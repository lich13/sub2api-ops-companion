use crate::{
    compat::{datetime, integer, parse_datetime, string, truthy},
    error::{Error, Result},
    oauth_queries::{QueryOptions, Source},
    runtime::Runtime,
};
use chrono::Timelike;
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

impl Runtime {
    pub fn quota_progress(&self) -> Value {
        self.quota_batch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn start_quota_refresh(self: &Arc<Self>, key: &str) -> Result<Value> {
        self.writable()?;
        // Serialize acceptance without holding a synchronous mutex over SQL.
        let _start = self.operations.submit_lock.lock().await;
        if self
            .quota_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Ok(self.quota_progress());
        }
        let rows = self
            .actions
            .queries
            .accounts()
            .await?
            .into_iter()
            .filter(|row| {
                matches!(string(&row["platform"]), "openai" | "grok") && row["type"] == "oauth"
            })
            .collect::<Vec<_>>();
        let batch = json!({"id":uuid::Uuid::new_v4().simple().to_string(),"status":"running","started_at":crate::compat::now(),"completed_at":null,
            "total":rows.len(),"completed":0,"items":rows.iter().map(|row|json!({"account_id":row["id"],"account_name":row["name"],"platform":row["platform"],"status":"pending"})).collect::<Vec<_>>()});
        *self.quota_batch.lock().unwrap_or_else(|e| e.into_inner()) = batch.clone();
        let runtime = self.clone();
        let key = key.to_owned();
        let requested = Instant::now();
        let handle = self.actions.supervisor.spawn("quota_refresh", async move {
            let limit = runtime
                .config
                .current()
                .ok()
                .and_then(|v| integer(&v["oauth_usage_refresh_concurrency"]))
                .unwrap_or(4)
                .clamp(1, 16) as usize;
            let openai = Arc::new(tokio::sync::Semaphore::new(limit));
            let grok = Arc::new(tokio::sync::Semaphore::new(limit));
            stream::iter(rows.into_iter().map(|row| {
                let runtime=runtime.clone();let key=key.clone();let capacity=if row["platform"]=="openai" {openai.clone()}else {grok.clone()};async move {
                    let _platform=capacity.acquire_owned().await;
                    let id=integer(&row["id"]).unwrap_or(0);
                    let result=runtime.refresh_quota_item(&row,&key,requested).await;
                    runtime.quota_item(id,match result {
                        Ok(result)=>result,
                        Err(error)=>json!({"status":"failed","error":crate::security::safe_error(&error.detail,200),"error_code":"quota_query_failed"}),
                    },true);
                    runtime.invalidate().await;
                }
            })).buffer_unordered(limit*2).collect::<Vec<_>>().await;
            let mut batch = runtime
                .quota_batch
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for item in batch["items"].as_array_mut().into_iter().flatten() {
                if matches!(string(&item["status"]), "pending" | "queued" | "running") {
                    item.as_object_mut().unwrap().extend(json!({"status":"failed","error":"本轮未取得结果，未重复请求","error_code":"batch_incomplete"}).as_object().unwrap().clone());
                }
            }
            batch["status"] = "completed".into();
            batch["completed"] = batch["total"].clone();
            batch["completed_at"] = crate::compat::now().into();
        });
        *self.quota_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(batch)
    }
    fn quota_item(&self, id: i64, changes: Value, completed: bool) {
        let mut batch = self.quota_batch.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(item) = batch["items"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .find(|item| item["account_id"] == id)
        {
            item.as_object_mut()
                .unwrap()
                .extend(changes.as_object().unwrap().clone());
        }
        if completed {
            batch["completed"] = (integer(&batch["completed"]).unwrap_or(0) + 1).into();
        }
    }
    async fn refresh_quota_item(
        &self,
        original: &Value,
        key: &str,
        requested: Instant,
    ) -> Result<Value> {
        let id = integer(&original["id"]).ok_or_else(Error::invalid)?;
        let _lease = loop {
            if self.closing.load(Ordering::Acquire) || self.actions.supervisor.is_stopping() {
                return Err(Error::new(503, "服务正在停止"));
            }
            let row = self.actions.account(id, None).await?;
            match self.monitor.lease(&row) {
                Ok(lease) => break lease,
                Err(error) if error.status.as_u16() == 409 => {
                    self.quota_item(id, json!({"status":"queued"}), false);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        };
        let _budget = self
            .budget
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::internal())?;
        let row = self.actions.account(id, None).await?;
        if row["platform"] != original["platform"] || row["type"] != "oauth" {
            return Err(Error::new(409, "账号类型已变化"));
        }
        self.quota_item(id, json!({"status":"running"}), false);
        let at = chrono::Utc::now();
        let data = if row["platform"] == "openai" {
            let result = self
                .monitor
                .queries
                .query(
                    id,
                    key,
                    QueryOptions {
                        source: Source::Manual,
                        reason: "batch_refresh".into(),
                        at,
                        requested_at: Some(requested),
                        timeout: 30,
                        generation: None,
                    },
                )
                .await;
            if result["success"] != true {
                return Err(Error::new(
                    502,
                    result
                        .get("error")
                        .cloned()
                        .unwrap_or(json!("额度查询失败")),
                ));
            }
            result["data"].clone()
        } else {
            self.actions
                .json(
                    reqwest::Method::GET,
                    &format!("accounts/{id}/usage?source=active&force=true"),
                    key,
                    None,
                    30,
                )
                .await?
        };
        let live = self.actions.account(id, None).await?;
        if live["platform"] != row["platform"] || live["type"] != "oauth" {
            return Err(Error::new(409, "账号已删除或类型变化，结果未采用"));
        }
        if !data.is_object() {
            return Err(Error::new(502, "额度响应格式无效"));
        }
        let billing = &data["grok_billing"];
        let partial = row["platform"] == "grok"
            && (!billing.is_object()
                || truthy(&billing["partial"])
                || truthy(&billing["failed_windows"])
                || parse_datetime(&billing["fetched_at"])
                    .is_none_or(|fetched| fetched < at.with_nanosecond(0).unwrap()));
        let mut result =
            json!({"status":if partial {"partial"}else {"success"},"queried_at":datetime(at)});
        if partial {
            result["error"] = "没有完整的新鲜账单窗口".into();
            result["error_code"] = "partial_billing".into();
        }
        Ok(result)
    }
}
