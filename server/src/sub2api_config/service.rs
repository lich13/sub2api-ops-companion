use super::host::{read_frame, write_frame};
use crate::{
    compat::{sha256, string},
    error::{Error, Result},
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::net::UnixStream;

pub struct Service {
    socket: Option<PathBuf>,
    owner: Mutex<Option<(String, Instant, bool)>>,
    apply_gate: tokio::sync::Mutex<()>,
}
impl Service {
    pub fn new() -> Self {
        Self {
            socket: std::env::var_os("SUB2API_CONFIG_SOCKET").map(PathBuf::from),
            owner: Mutex::new(None),
            apply_gate: tokio::sync::Mutex::new(()),
        }
    }
    pub fn available(&self) -> bool {
        self.socket.as_ref().is_some_and(|p| p.exists())
    }
    pub fn owns_operation(&self, key: &str) -> bool {
        self.owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|(k, t, _)| *k == sha256(key) && *t > Instant::now())
    }
    pub async fn request(
        &self,
        action: &str,
        target: &str,
        key: &str,
        payload: &Value,
    ) -> Result<Value> {
        if action == "operation" {
            let owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
            match owner.as_ref() {
                Some((hash, deadline, _)) if *deadline > Instant::now() && *hash != sha256(key) => {
                    return Err(Error::new(403, "只有发起本次应用的管理员可以查询操作结果"))
                }
                None => return Ok(json!({"status":"idle"})),
                Some((_, deadline, _)) if *deadline <= Instant::now() => {
                    return Ok(json!({"status":"idle"}))
                }
                _ => {}
            }
        }
        let Some(socket) = &self.socket else {
            return if action == "read" {
                Ok(
                    json!({"available":false,"editable":false,"reason":"此服务尚未配置宿主机配置接口"}),
                )
            } else {
                Err(Error::new(503, "宿主机配置接口不可用"))
            };
        };
        let _gate = if action == "apply" {
            Some(
                self.apply_gate
                    .try_lock()
                    .map_err(|_| Error::conflict("已有配置请求正在提交"))?,
            )
        } else {
            None
        };
        let prior = if action == "apply" {
            let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
            owner.replace((
                sha256(key),
                Instant::now() + Duration::from_secs(900),
                false,
            ))
        } else {
            None
        };
        let call = async {
            let mut stream = UnixStream::connect(socket)
                .await
                .map_err(|_| Error::new(503, "无法连接宿主机配置接口"))?;
            write_frame(
                &mut stream,
                &json!({"action":action,"target":target,"payload":payload}),
            )
            .await?;
            let result = read_frame(&mut stream).await?;
            if result["ok"] != true {
                if action == "apply" {
                    *self.owner.lock().unwrap_or_else(|e| e.into_inner()) = prior;
                }
                return Err(Error::new(
                    result["status"].as_u64().unwrap_or(503) as u16,
                    result["detail"].clone(),
                ));
            }
            Ok(result["value"].clone())
        };
        let value = tokio::time::timeout(Duration::from_secs(55), call)
            .await
            .map_err(|_| Error::new(504, "配置接口超时，请查询操作状态；未重放"))??;
        if action == "operation"
            && self.owns_operation(key)
            && !matches!(
                string(&value["status"]),
                "saving" | "restarting" | "restoring"
            )
        {
            let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, deadline, terminal)) = owner.as_mut() {
                if !*terminal {
                    let elapsed = value["updated_at"]
                        .as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|t| {
                            (chrono::Utc::now() - t.with_timezone(&chrono::Utc))
                                .num_seconds()
                                .max(0) as u64
                        })
                        .unwrap_or(0);
                    *deadline =
                        Instant::now() + Duration::from_secs(600u64.saturating_sub(elapsed));
                    *terminal = true;
                }
            }
            if owner
                .as_ref()
                .is_some_and(|(_, deadline, _)| *deadline <= Instant::now())
            {
                return Ok(json!({"status":"idle"}));
            }
        }
        Ok(value)
    }
}
impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn sub2api_config_operation_is_bound_to_owner_and_expires_after_completion() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let service = Service {
            socket: Some(socket),
            owner: Mutex::new(Some((
                sha256("fixture-owner"),
                Instant::now() + Duration::from_secs(900),
                false,
            ))),
            apply_gate: tokio::sync::Mutex::new(()),
        };
        assert!(service.owns_operation("fixture-owner"));
        assert!(!service.owns_operation("fixture-other"));
        assert_eq!(
            service
                .request(
                    "operation",
                    "http://example.invalid",
                    "fixture-other",
                    &json!({})
                )
                .await
                .unwrap_err()
                .status
                .as_u16(),
            403
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
            write_frame(&mut stream,&json!({"ok":true,"value":{"status":"succeeded","updated_at":(chrono::Utc::now()-chrono::Duration::minutes(11)).to_rfc3339()}})).await.unwrap();
        });
        assert_eq!(
            service
                .request(
                    "operation",
                    "http://example.invalid",
                    "fixture-owner",
                    &json!({})
                )
                .await
                .unwrap()["status"],
            "idle"
        );
        assert!(!service.owns_operation("fixture-owner"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sub2api_config_rejected_apply_preserves_previous_owner() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let service = Service {
            socket: Some(socket),
            owner: Mutex::new(Some((
                sha256("fixture-owner"),
                Instant::now() + Duration::from_secs(900),
                false,
            ))),
            apply_gate: tokio::sync::Mutex::new(()),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
            write_frame(
                &mut stream,
                &json!({"ok":false,"status":409,"detail":"fixture conflict"}),
            )
            .await
            .unwrap();
        });
        assert!(service
            .request(
                "apply",
                "http://example.invalid",
                "fixture-other",
                &json!({})
            )
            .await
            .is_err());
        assert!(service.owns_operation("fixture-owner"));
        assert!(!service.owns_operation("fixture-other"));
        server.await.unwrap();
    }
}
