use crate::{
    model::{self, now, Failure, Job},
    App, HashSet,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 200
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
}
fn scrub(error: &str) -> String {
    static PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> = std::sync::LazyLock::new(|| {
        [
        r"(?i)\bbearer\s+[^\s,;]+",
        r"(?i)\b(?:authorization|api[ _-]?key|access[ _-]?token|token|secret|password|cookie)\s*[:=]\s*[^\s,;]+",
        r"\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
        r"(?i)\b(?:sk-[A-Za-z0-9_-]{6,}|gh[pousr]_[A-Za-z0-9_]{10,})\b",r"https?://[^\s]+",
    ].into_iter().map(|p|regex::Regex::new(p).unwrap()).collect()
    });
    let mut text = error.to_string();
    for regex in PATTERNS.iter() {
        text = regex.replace_all(&text, "[redacted]").into_owned();
    }
    text.chars().filter(|c| !c.is_control()).take(240).collect()
}

pub async fn queue(app: App) {
    let mut tasks = tokio::task::JoinSet::new();
    let mut active = HashSet::<i64>::new();
    let mut jobs = HashSet::<String>::new();
    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Ok((jid, ids)) = result {
                jobs.remove(&jid);
                for id in ids {
                    active.remove(&id);
                }
            }
        }
        let data = app.store.lock().await.read();
        if let Ok(data) = data {
            for job in data.jobs {
                if tasks.len() >= app.concurrency {
                    break;
                }
                if job.status != "queued" || jobs.contains(&job.id) {
                    continue;
                }
                let ids = job.lock_ids();
                if ids.iter().any(|id| active.contains(id)) {
                    continue;
                }
                active.extend(ids.iter().copied());
                jobs.insert(job.id.clone());
                let owned = app.clone();
                tasks.spawn(async move {
                    if let Err(error) = run(&owned, &job).await {
                        let _ = owned
                            .store
                            .lock()
                            .await
                            .update(&job.id, |j| j.change("needs_confirmation", error.2));
                    }
                    (job.id, ids)
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn run(app: &App, job: &Job) -> Result<(), Failure> {
    let rows = app.rows(Some(&[job.account_id])).await?;
    if rows
        .first()
        .is_none_or(|row| model::version(row) != job.expected_version)
    {
        app.store.lock().await.update(&job.id, |j| {
            j.change("needs_confirmation", "相关字段、凭据或资格已变化")
        })?;
        return Ok(());
    }
    let key = app
        .keys
        .lock()
        .await
        .get(&job.batch_id)
        .cloned()
        .unwrap_or_else(|| app.admin_key.clone());
    if let Err(error) = app.authenticate(&key, true).await {
        app.store
            .lock()
            .await
            .update(&job.id, |j| j.change("failed", error.2))?;
        return Ok(());
    }
    // Match the Python prepare_test check after the fresh permission request.
    let latest = app.rows(Some(&[job.account_id])).await?;
    if latest
        .first()
        .is_none_or(|row| model::version(row) != job.expected_version)
    {
        app.store.lock().await.update(&job.id, |j| {
            j.change("needs_confirmation", "相关字段、凭据或资格已变化")
        })?;
        return Ok(());
    }
    {
        let store = app.store.lock().await;
        if store
            .read()?
            .jobs
            .iter()
            .find(|j| j.id == job.id)
            .is_none_or(|j| j.status != "queued")
        {
            return Ok(());
        }
        store.update(&job.id, |j| {
            j.change("running", "");
            j.started_at = Some(now());
        })?;
    }
    let result = probe(app, job, &key).await;
    let uncertain = result["uncertain"].as_bool() == Some(true);
    app.store.lock().await.update(&job.id, |j| {
        j.change(
            if uncertain {
                "needs_confirmation"
            } else {
                "completed"
            },
            if uncertain {
                result["error"].as_str().unwrap_or("")
            } else {
                ""
            },
        );
        j.result = Some(result);
    })?;
    Ok(())
}

pub async fn probe(app: &App, job: &Job, key: &str) -> Value {
    let started = Instant::now();
    let mut result =
        json!({"requested_model":job.requested["model_id"],"actual_model":null,"success":false});
    let outcome =
        tokio::time::timeout(Duration::from_secs(300), stream(app, job, key, &mut result)).await;
    match outcome {
        Ok(Ok(())) => (),
        _ => {
            result["uncertain"] = json!(true);
            result["error"] = json!("测试连接中断或超时，未重放请求");
        }
    }
    result["duration_ms"] = json!(started.elapsed().as_millis() as u64);
    result["completed_at"] = json!(now());
    result
}
async fn stream(app: &App, job: &Job, key: &str, result: &mut Value) -> Result<(), ()> {
    let response=app.client.post(format!("{}/api/v1/admin/accounts/{}/test",app.upstream,job.account_id))
        .header("x-api-key",key).header("X-Admin-UI-Request","1").header("Accept","text/event-stream")
        .json(&json!({"mode":job.requested["mode"],"model_id":job.requested["model_id"],"prompt":"","image_data_url":"","audio_data_url":""}))
        .send().await.map_err(|_|())?;
    if response.status() != 200 {
        result["error"] = json!(format!(
            "测试请求失败 [http_{}]",
            response.status().as_u16()
        ));
        return Ok(());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut total = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ())?;
        total += chunk.len();
        if total > 134_217_728 {
            return Err(());
        }
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line = buffer.drain(..=end).collect::<Vec<_>>();
            if event(&line, result) {
                return Ok(());
            }
        }
    }
    if !buffer.is_empty() && event(&buffer, result) {
        return Ok(());
    }
    result["uncertain"] = json!(true);
    result["error"] = json!("连接结束，未收到测试完成结果；未重放请求");
    Ok(())
}
fn event(line: &[u8], result: &mut Value) -> bool {
    let Ok(line) = std::str::from_utf8(line) else {
        return false;
    };
    let Some(data) = line.strip_prefix("data:") else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
        return false;
    };
    match value["type"].as_str() {
        Some("test_start") => {
            if let Some(model) = value["model"].as_str().filter(|m| valid_model(m)) {
                result["actual_model"] = json!(model);
            }
        }
        Some("test_complete") => {
            result["success"] = json!(value["success"].as_bool() == Some(true));
            if result["success"] != true {
                result["error"] = json!("上游未确认测试通过");
            }
            return true;
        }
        Some("error") => {
            result["error"] = json!(scrub(value["error"].as_str().unwrap_or("测试失败")));
            return true;
        }
        _ => (),
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_terminal_and_content_exclusion() {
        let mut result = json!({"success":false});
        assert!(!event(
            b"data: {\"type\":\"content\",\"text\":\"fixture-response-body\"}",
            &mut result
        ));
        assert!(event(
            b"data: {\"type\":\"test_complete\",\"success\":\"true\"}",
            &mut result
        ));
        assert_eq!(result["success"], false);
        assert!(!result.to_string().contains("fixture-response-body"));
    }
    #[test]
    fn native_error_redacts_secrets() {
        assert!(
            !scrub("Authorization: Bearer fixture-key secret=fixture-secret")
                .contains("fixture-key")
        );
        assert!(!scrub("token=fixture-secret").contains("fixture-secret"));
    }
}
