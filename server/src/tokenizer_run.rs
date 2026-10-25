use crate::{
    compat::{integer, string},
    error::{Error, Result},
    model_target, model_test_stream,
    model_tests::{Context, ModelTests},
    tokenizer::{self, Observation, Posterior},
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

impl ModelTests {
    pub(crate) async fn run_tokenizer(&self, job: &Value, context: Arc<Context>) -> Result<()> {
        let Some(snapshot) = context.lm.clone() else {
            return Ok(());
        };
        let id = string(&job["id"]);
        let account = integer(&job["account_id"]).ok_or_else(Error::internal)?;
        let client = context
            .target
            .client()
            .map_err(|_| Error::new(502, "账号代理或连接不可用"))?;
        let started = Instant::now();
        let mut observations: Vec<Observation> = vec![];
        let mut steps: Vec<Value> = vec![];
        let mut posterior: Option<Posterior> = None;
        let mut asked = BTreeSet::new();
        let mut error = String::new();
        let mut baseline_drift = false;
        let mut next: Vec<Option<String>> = vec![None];
        let mut ending = false;
        loop {
            let mut pending = FuturesUnordered::new();
            for probe in &next {
                let index = steps.len();
                steps.push(json!({"probe":probe,"status":"requesting"}));
                if let Some(probe) = probe {
                    asked.insert(probe.clone());
                }
                let prompt = snapshot.tokenizer.text(probe.as_deref())?;
                let client = client.clone();
                let context = context.clone();
                let probe = probe.clone();
                pending.push(async move {
                    let owner = integer(&context.owner["id"]).unwrap_or(account);
                    let valid = async {
                        let row = model_target::account(&self.actions, account).await?;
                        let credentials = model_target::account(&self.actions, owner).await?;
                        Ok::<_, Error>(
                            model_target::signature(&row) == model_target::signature(&context.row)
                                && model_target::signature(&credentials)
                                    == model_target::signature(&context.owner),
                        )
                    }
                    .await;
                    if !matches!(valid, Ok(true)) {
                        return (index, probe, Err("账号配置已变化".to_string()));
                    }
                    let mut diag = json!({});
                    let value = tokio::time::timeout(
                        Duration::from_secs(90),
                        model_test_stream::execute_probe(
                            &context.target,
                            &prompt,
                            &client,
                            &mut diag,
                        ),
                    )
                    .await;
                    let value = match value {
                        Ok(Ok(n)) => Ok(n),
                        Ok(Err(f)) => Err(if f.code == "no_usage" {
                            "接口未提供输入 Token 用量，无法探测"
                        } else {
                            crate::model_tests::error_text(f.code)
                        }
                        .into()),
                        Err(_) => Err("分词器请求超时".into()),
                    };
                    (index, probe, value)
                });
            }
            self.update(id,json!({"tokenizer":{"status":"running","steps":steps,"requests":steps.len(),"report":Value::Null}}))?;
            while let Some((index, probe, result)) = pending.next().await {
                match result {
                    Ok(tokens) => {
                        steps[index] =
                            json!({"probe":probe,"status":"completed","input_tokens":tokens});
                        observations.push(Observation { probe, tokens });
                    }
                    Err(reason) => {
                        steps[index] = json!({"probe":probe,"status":"failed","error":reason});
                        if error.is_empty() {
                            error = reason;
                        }
                    }
                }
                let bank = snapshot.clone();
                let values = observations.clone();
                posterior = crate::supervision::join(tokio::task::spawn_blocking(move || {
                    bank.tokenizer.posterior(&values)
                }))
                .await
                .map_err(|_| Error::internal())?;
                let report = posterior.as_ref().filter(|p| p.answered > 0).map(|p| {
                    snapshot
                        .tokenizer
                        .report(p, string(&job["requested_model"]))
                });
                self.update(id,json!({"tokenizer":{"status":"running","steps":steps,"requests":steps.len(),"report":report,"error":error}}))?;
            }
            if !error.is_empty() || ending {
                break;
            }
            let finished = posterior.as_ref().is_some_and(tokenizer::done);
            let bank = snapshot.clone();
            let p = posterior.clone();
            let seen = asked.clone();
            let count = usize::from(asked.len() < 20);
            let probes = if finished {
                vec![]
            } else {
                crate::supervision::join(tokio::task::spawn_blocking(move || {
                    bank.tokenizer.next(p.as_ref(), &seen, count)
                }))
                .await
                .map_err(|_| Error::internal())?
            };
            if probes.is_empty() {
                ending = true;
                next = vec![None];
            } else {
                next = probes.into_iter().map(Some).collect();
            }
        }
        let baselines = observations
            .iter()
            .filter(|o| o.probe.is_none())
            .map(|o| o.tokens)
            .collect::<Vec<_>>();
        if baselines.len() == 2 && baselines[0] != baselines[1] {
            baseline_drift = true;
        }
        let report = posterior.as_ref().filter(|p| p.answered > 0).map(|p| {
            snapshot
                .tokenizer
                .report(p, string(&job["requested_model"]))
        });
        self.update(id,json!({"tokenizer":{"status":if error.is_empty(){"completed"}else{"failed"},"steps":steps,"requests":steps.len(),"report":report,"baseline_drift":baseline_drift,"error":error,"duration_ms":started.elapsed().as_millis(),"bank_version":snapshot.version}}))?;
        Ok(())
    }
}
