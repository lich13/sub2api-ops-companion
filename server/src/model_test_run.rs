use crate::{
    audit,
    compat::{integer, string, truthy},
    error::{Error, Result},
    model_downgrade::{diagnose_prediction, is_downgrade, valid_report},
    model_target,
    model_test_stream::{self, Failure, Progress},
    model_tests::{error_text, Context, ModelTests},
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

struct Run {
    groups: Mutex<Vec<Value>>,
    returned: Mutex<BTreeSet<String>>,
    attempts: AtomicUsize,
    early: AtomicBool,
    fatal: Mutex<Option<Failure>>,
    disposition: Mutex<Value>,
    analysis: tokio::sync::Mutex<()>,
    started: Instant,
}
impl ModelTests {
    fn progress(&self, id: &str, run: &Run) -> Result<()> {
        self.update(
            id,
            json!({
                "groups": run.groups.lock().unwrap_or_else(|e|e.into_inner()).clone(),
                "attempts": run.attempts.load(Ordering::Acquire),
                "duration_ms": run.started.elapsed().as_millis(),
            }),
        )?;

        Ok(())
    }
    fn group(&self, id: &str, run: &Run, index: usize, changes: Value) -> Result<()> {
        run.groups.lock().unwrap_or_else(|e| e.into_inner())[index]
            .as_object_mut()
            .ok_or_else(Error::internal)?
            .extend(changes.as_object().cloned().ok_or_else(Error::internal)?);
        self.progress(id, run)
    }
    pub(crate) async fn run(&self, job: Value, context: Arc<Context>) -> Result<()> {
        let id = string(&job["id"]);
        let aid = integer(&job["account_id"]).ok_or_else(Error::internal)?;
        let run = Arc::new(Run {
            groups: Mutex::new(
                job["groups"]
                    .as_array()
                    .cloned()
                    .ok_or_else(Error::internal)?,
            ),
            returned: Mutex::new(
                job["returned_models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            ),
            attempts: AtomicUsize::new(0),
            early: AtomicBool::new(false),
            fatal: Mutex::new(None),
            disposition: Mutex::new(Value::Null),
            analysis: tokio::sync::Mutex::new(()),
            started: Instant::now(),
        });
        let client = context
            .target
            .client()
            .map_err(|_| Error::new(502, "账号代理或连接不可用"))?;
        let group_slots = Arc::new(tokio::sync::Semaphore::new(
            integer(&job["concurrency"]).unwrap_or(1).clamp(1, 3) as usize,
        ));
        let mut pending = (0..integer(&job["planned_groups"]).unwrap_or(3).clamp(1, 3) as usize)
            .filter(|index| {
                !context
                    .samples
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(index)
            })
            .collect::<Vec<_>>();
        if job["automatic"] == true && !pending.is_empty() {
            let index = pending.remove(0);
            self.sample(
                &job,
                context.clone(),
                run.clone(),
                index,
                &client,
                group_slots.clone(),
            )
            .await?;
        }
        if !run.early.load(Ordering::Acquire)
            && run
                .fatal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            let mut children = FuturesUnordered::new();
            for index in pending {
                children.push(self.sample(
                    &job,
                    context.clone(),
                    run.clone(),
                    index,
                    &client,
                    group_slots.clone(),
                ));
            }
            while let Some(result) = children.next().await {
                result?;
                if run.early.load(Ordering::Acquire)
                    || run
                        .fatal
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some()
                {
                    break;
                }
            }
        }
        let early = run.early.load(Ordering::Acquire);
        let fatal = run.fatal.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut groups = run.groups.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for group in &mut groups {
            if matches!(
                string(&group["status"]),
                "queued" | "running" | "receiving" | "retrying" | "analyzing"
            ) {
                group["status"] = if early { "skipped" } else { "cancelled" }.into();
            }
        }
        let mut current = self.get(id)?;
        let samples = context
            .samples
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        let mut disposition = run
            .disposition
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let retryable = fatal.is_none()
            && !early
            && groups.iter().any(|g| {
                g["retryable"] == true && g["status"] == "failed" && g["uncertain"] != true
            });
        let status = if fatal.is_some() || samples == 0 && !early {
            "failed"
        } else {
            "completed"
        };
        if crate::lm_verdict::is_lm(&current) {
            let mut finished = current.clone();
            finished["status"] = status.into();
            finished["groups"] = json!(groups);
            finished["can_retry"] = retryable.into();
            finished["uncertain"] = fatal.as_ref().is_some_and(|f| f.uncertain).into();
            finished["error_code"] = json!(fatal.as_ref().map(|f| f.code));
            let verdict = crate::lm_verdict::classify(&finished);
            current = self.update(id, json!({"classification":verdict,"groups":groups}))?;
            if current["automatic"] == true && verdict["outcome"] == "degraded" {
                finished["classification"] = verdict.clone();
                finished["disposition_basis"] = verdict["basis"].clone();
                disposition = self.hooks.verdict(finished,current["report"].clone()).await
                    .unwrap_or_else(|error|json!({"status":"overridden","reason":crate::security::safe_error(&error.detail,200)}));
                if disposition.is_object() {
                    disposition["basis"] = verdict["basis"].clone();
                    disposition["group_indexes"] = verdict["group_indexes"].clone();
                }
            }
        }
        if !crate::lm_verdict::is_lm(&current)
            && status == "completed"
            && current["automatic"] == true
            && crate::model_tests::ACTIVE.contains(&string(&current["status"]))
            && !truthy(&disposition)
            && !truthy(&current["automatic_disposition"])
            && valid_report(&current["report"])
            && is_downgrade(
                string(&current["requested_model"]),
                string(&current["report"]["prediction"]),
            )
        {
            let mut verdict_job = current.clone();
            verdict_job["disposition_basis"] = "aggregate".into();
            disposition = self.hooks.verdict(verdict_job, current["report"].clone()).await
                .unwrap_or_else(|error| json!({"status":"overridden","reason":crate::security::safe_error(&error.detail,200)}));
            if disposition.is_object() {
                disposition["basis"] = "aggregate".into();
            }
        }
        let error = if let Some(fatal) = &fatal {
            error_text(fatal.code)
        } else if truthy(&current["report"]) {
            ""
        } else {
            "有效样本不足，无法识别"
        };
        let reason = if truthy(&disposition) {
            "automatic_degradation"
        } else if early {
            "confidence_99"
        } else if fatal.is_some() {
            "fatal_error"
        } else {
            "samples_finished"
        };
        let probe = current["tokenizer_probe"] == true
            && fatal.is_none()
            && groups
                .iter()
                .all(|g| g["status"] == "completed" && g["transport_complete"] == true);
        self.update(
            id,
            json!({
                "status": if probe { "running" } else { status },
                "phase": if probe { "tokenizer" } else { "finished" },
                "error": error,
                "error_code": fatal.as_ref().map(|f|f.code),
                "uncertain": fatal.as_ref().is_some_and(|f|f.uncertain),
                "can_retry": retryable,
                "completion_reason": reason,
                "automatic_disposition": disposition,
                "groups": groups,
                "completed_at": crate::compat::now(),
                "duration_ms": run.started.elapsed().as_millis(),
                "attempts": run.attempts.load(Ordering::Acquire),
            }),
        )?;

        if probe {
            self.update(
                id,
                json!({"tokenizer":{"status":"running","requests":0,"report":null}}),
            )?;
            if let Err(error) = self.run_tokenizer(&current, context.clone()).await {
                self.update(id,json!({"tokenizer":{"status":"failed","error":crate::security::safe_error(&error.detail,200)}}))?;
            }
            self.update(id,json!({"status":status,"phase":"finished","completed_at":crate::compat::now(),"duration_ms":run.started.elapsed().as_millis()}))?;
        }
        audit::write(
            &self.actions.settings.audit_path,
            "model_test_result",
            json!({
                "account_id": aid,
                "job_id": id,
                "status": status,
                "attempts": run.attempts.load(Ordering::Acquire),
                "valid_groups": current["valid_groups"],
                "completion_reason": reason,
                "engine": current["engine"],
                "sample_count": current["planned_groups"],
                "resource_revision": current["resource_revision"],
                "analysis_status": current["report"]["decision"],
                "classification": current["classification"],
                "disposition_basis": disposition["basis"],
            }),
            false,
        );
        self.contexts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|key, old| key == id || old.row["id"] != aid);
        Ok(())
    }
    async fn sample(
        &self,
        job: &Value,
        context: Arc<Context>,
        run: Arc<Run>,
        index: usize,
        client: &reqwest::Client,
        group_slots: Arc<tokio::sync::Semaphore>,
    ) -> Result<()> {
        let id = string(&job["id"]);
        let started = Instant::now();
        let result = self
            .sample_inner(
                job,
                context,
                run.clone(),
                index,
                client,
                group_slots,
                started,
            )
            .await;
        if let Err(failure) = result {
            self.group(
                id,
                &run,
                index,
                json!({
                    "status":"failed", "error":error_text(failure.code), "retryable":false,
                    "uncertain":failure.uncertain
                }),
            )?;
            *run.fatal.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure);
        }
        self.group(
            id,
            &run,
            index,
            json!({"duration_ms":started.elapsed().as_millis()}),
        )?;
        Ok(())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "A sample owns independent job, cancellation, concurrency and timing contexts"
    )]
    async fn sample_inner(
        &self,
        job: &Value,
        context: Arc<Context>,
        run: Arc<Run>,
        index: usize,
        client: &reqwest::Client,
        group_slots: Arc<tokio::sync::Semaphore>,
        started: Instant,
    ) -> std::result::Result<(), Failure> {
        let internal = |_: Error| Failure::new("internal_error", false);
        let id = string(&job["id"]);
        let aid = integer(&job["account_id"]).unwrap();
        let (expected, prompt) = &context.challenges[index];
        for attempt in 0..3 {
            let permit = group_slots
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Failure::new("internal_error", false))?;
            if run.early.load(Ordering::Acquire)
                || run
                    .fatal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some()
            {
                return Ok(());
            }
            // ModelTrace keeps the caller's 1–3 sample concurrency, within its
            // two-job limit and the shared task budget. Native connection
            // probes have a separate configurable recovery-test limit.
            if job["automatic"] == true {
                self.hooks
                    .guard(self.get(id).map_err(internal)?)
                    .await
                    .map_err(|_| Failure::new("account_changed", false))?;
            }
            if model_target::signature(
                &model_target::account(&self.actions, aid)
                    .await
                    .map_err(internal)?,
            ) != model_target::signature(&context.row)
                || context.owner["id"] != aid
                    && model_target::signature(
                        &model_target::account(
                            &self.actions,
                            integer(&context.owner["id"]).unwrap(),
                        )
                        .await
                        .map_err(internal)?,
                    ) != model_target::signature(&context.owner)
            {
                return Err(Failure::new("account_changed", false));
            }
            if self.actions.supervisor.is_stopping() {
                return Err(Failure::new("internal_error", false));
            }
            if job["automatic"] == true && run.attempts.load(Ordering::Acquire) == 0 {
                self.hooks
                    .register_dispatch(&self.get(id).map_err(internal)?)
                    .map_err(internal)?;
            }
            run.attempts.fetch_add(1, Ordering::AcqRel);
            self.group(
                id,
                &run,
                index,
                json!({
                    "status": "running",
                    "attempts": attempt+1,
                    "error": "",
                    "ttft_ms": null,
                    "retryable": false,
                    "started_at": crate::compat::now(),
                    "diagnostics": {},
                }),
            )
            .map_err(internal)?;

            audit::write(
                &self.actions.settings.audit_path,
                "model_test_attempt",
                json!({"account_id":aid,"job_id":id,"group":index+1,"attempt":attempt+1,"engine":job["engine"],"sample_count":job["planned_groups"],"resource_revision":job["resource_revision"]}),
                false,
            );
            let callback: Progress = {
                let this = self.clone();
                let id = id.to_owned();
                let run = run.clone();
                Arc::new(move |changes| {
                    let this = this.clone();
                    let id = id.clone();
                    let run = run.clone();
                    Box::pin(async move { this.group(&id, &run, index, changes) })
                })
            };
            let mut diagnostics = json!({});
            let received = tokio::time::timeout(
                Duration::from_secs(180),
                model_test_stream::execute(
                    &context.target,
                    prompt,
                    *expected,
                    client,
                    Some(callback),
                    &mut diagnostics,
                ),
            )
            .await
            .unwrap_or_else(|_| Err(Failure::new("timeout", false)));
            self.group(id, &run, index, json!({"diagnostics":diagnostics}))
                .map_err(internal)?;
            let mut transport = diagnostics.clone();
            transport.as_object_mut().unwrap().extend(
                json!({"account_id":aid,"job_id":id,"group":index+1,"attempt":attempt+1,"engine":job["engine"],"sample_count":job["planned_groups"],"resource_revision":job["resource_revision"]})
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            audit::write(
                &self.actions.settings.audit_path,
                "model_test_transport",
                transport,
                false,
            );
            match received {
                Ok((text, model)) => {
                    if let Some(model) =
                        model.filter(|v| crate::account_templates::MODEL_ID.is_match(v))
                    {
                        let safe = context
                            .target
                            .headers
                            .iter()
                            .filter(|(key, _)| {
                                matches!(key.as_str(), "authorization" | "chatgpt-account-id")
                            })
                            .all(|(_, value)| {
                                value.to_str().ok().is_none_or(|value| {
                                    !model.contains(value.strip_prefix("Bearer ").unwrap_or(value))
                                })
                            });
                        if safe {
                            run.returned
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(model);
                        }
                    }
                    context
                        .samples
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(index, text);
                    let error = match string(&diagnostics["end_reason"]) {
                        "max_output_tokens" | "max_tokens" | "length" => {
                            "达到输出预算，已保留收到的样本"
                        }
                        "content_filter" => "上游内容过滤，已保留收到的样本",
                        _ => "",
                    };
                    self.group(id, &run, index, json!({
                        "status":"analyzing", "duration_ms":started.elapsed().as_millis(), "error":error
                    })).map_err(internal)?;
                    let _analysis = run.analysis.lock().await;
                    let analysis_started = Instant::now();
                    let samples = context
                        .samples
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let scorer = context.clone();
                    let scoring_samples = samples.clone();
                    let report = crate::supervision::join(tokio::task::spawn_blocking(move || {
                        scorer.analyze(&scoring_samples)
                    }))
                    .await;
                    let report = match report {
                        Ok(Ok(report)) => report,
                        _ => {
                            self.group(
                                id,
                                &run,
                                index,
                                json!({"status":"completed","error":"样本已完成，本地分析失败","transport_complete":matches!(string(&diagnostics["end_reason"]),"completed"|"stop"|"done")}),
                            )
                            .map_err(internal)?;
                            return Ok(());
                        }
                    };
                    if job["automatic"] == true {
                        diagnose_prediction(
                            string(&job["requested_model"]),
                            string(&report["prediction"]),
                        );
                    }
                    // Retried first groups must be scored independently even
                    // when later groups already contributed to the aggregate.
                    let independent = if context.lm.is_some()
                        || index == 0 && job["automatic"] == true && samples.len() > 1
                    {
                        let scorer = context.clone();
                        let one = BTreeMap::from([(index, samples[&index].clone())]);
                        match crate::supervision::join(tokio::task::spawn_blocking(move || {
                            scorer.analyze(&one)
                        }))
                        .await
                        {
                            Ok(Ok(report)) => report,
                            _ if context.lm.is_some() => Value::Null,
                            Ok(Err(error)) => return Err(internal(error)),
                            Err(_) => return Err(Failure::new("internal_error", false)),
                        }
                    } else {
                        report.clone()
                    };
                    let first = independent.clone();
                    self.group(id, &run, index, json!({
                        "status":"completed", "analysis_ms":analysis_started.elapsed().as_millis(),
                        "error": if context.lm.is_some() && independent.is_null() { "样本已完成，本地分析失败" } else { error },
                        "report": if context.lm.is_some() { independent } else { Value::Null },
                        "transport_complete": matches!(string(&diagnostics["end_reason"]),"completed"|"stop"|"done"),
                    })).map_err(internal)?;
                    let current = self.get(id).map_err(internal)?;
                    let first_group_valid = if index == 0 {
                        valid_report(&first)
                    } else {
                        truthy(&current["first_group_valid"])
                    };
                    let first_group_prediction = if index == 0 {
                        &first["prediction"]
                    } else {
                        &current["first_group_prediction"]
                    };
                    self.update(id, json!({
                        "completed_groups": samples.len(),
                        "valid_groups": integer(&report["used_outputs"]).unwrap_or(0),
                        "returned_models": run.returned.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                        "report": report,
                        "first_group_valid": first_group_valid,
                        "first_group_prediction": first_group_prediction,
                        "first_group_probability": if index == 0 { &first["probability"] } else { &current["first_group_probability"] },
                    })).map_err(internal)?;
                    if context.lm.is_none()
                        && index == 0
                        && job["automatic"] == true
                        && valid_report(&first)
                        && is_downgrade(
                            string(&job["requested_model"]),
                            string(&first["prediction"]),
                        )
                    {
                        run.early.store(true, Ordering::Release);
                        let disposition = self
                            .hooks
                            .verdict(self.get(id).map_err(internal)?, first)
                            .await
                            .unwrap_or_else(|error| {
                                json!({
                                    "status": "overridden",
                                    "basis": "first_group",
                                    "reason": crate::security::safe_error(&error.detail, 200),
                                })
                            });

                        *run.disposition.lock().unwrap_or_else(|e| e.into_inner()) =
                            disposition.clone();
                        self.update(id, json!({
                            "automatic_disposition":disposition, "completion_reason":"automatic_degradation"
                        })).map_err(internal)?;
                    }
                    if context.lm.is_none()
                        && report["probability"]
                            .as_f64()
                            .is_some_and(|value| value >= 0.99)
                    {
                        run.early.store(true, Ordering::Release);
                    }
                    return Ok(());
                }
                Err(failure) => {
                    self.group(
                        id,
                        &run,
                        index,
                        json!({
                            "error": error_text(failure.code),
                            "retryable": failure.retryable && !failure.uncertain,
                            "uncertain": failure.uncertain,
                        }),
                    )
                    .map_err(internal)?;

                    // A missing terminal can represent a billed request. Only a
                    // definite upstream rejection is eligible for the old retry loop.
                    if failure.uncertain || !failure.retryable {
                        return Err(failure);
                    }
                    if attempt == 2 {
                        self.group(id, &run, index, json!({"status":"failed"}))
                            .map_err(internal)?;
                        return Ok(());
                    }
                    self.group(id, &run, index, json!({"status":"retrying"}))
                        .map_err(internal)?;
                }
            }
            drop(permit);
            tokio::time::sleep(Duration::from_secs([1, 3][attempt])).await;
        }
        Ok(())
    }
}
