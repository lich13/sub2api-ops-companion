//! A local, fixed-target authority. No request can supply a file or command.
use super::{effective, environment_override, get, parse_yaml, prepare, CATALOG, MAX_CONFIG_BYTES};
use crate::{
    compat::{sha256, string},
    error::{Error, Result},
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

const MAX_FRAME: usize = 4 * 1024 * 1024;
pub(super) async fn read_frame(stream: &mut UnixStream) -> Result<Value> {
    let n = stream.read_u32().await? as usize;
    if n > MAX_FRAME {
        return Err(Error::invalid());
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| Error::invalid())
}
pub(super) async fn write_frame(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(Error::invalid());
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}
#[derive(Clone)]
pub struct Observed {
    pub version: String,
    pub commit: String,
    pub process: String,
    pub healthy: bool,
    pub env: BTreeMap<String, String>,
}
pub trait Driver: Send + Sync {
    fn observe(&self) -> BoxFuture<'_, Result<Observed>>;
    fn restart(&self) -> BoxFuture<'_, Result<()>>;
}
struct Docker {
    container: String,
    config: PathBuf,
}
async fn command(args: &[&str], seconds: u64) -> Result<Vec<u8>> {
    let (bin, args) = args.split_first().ok_or_else(Error::invalid)?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(seconds), cmd.output())
        .await
        .map_err(|_| Error::new(504, "宿主机操作超时"))??;
    if !out.status.success() {
        return Err(Error::new(503, "无法读取目标进程或执行服务重启"));
    }
    Ok(out.stdout)
}
impl Driver for Docker {
    fn observe(&self) -> BoxFuture<'_, Result<Observed>> {
        Box::pin(async move {
            let raw = command(&["docker", "inspect", &self.container], 10).await?;
            let values: Value = serde_json::from_slice(&raw)?;
            let d = &values[0];
            if d["State"]["Running"] != true {
                return Err(Error::new(503, "Sub2API 尚未运行"));
            }
            let pid = d["State"]["Pid"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or_else(Error::internal)?;
            let root = PathBuf::from(format!("/proc/{pid}"));
            let bytes = std::fs::read(root.join("environ"))?;
            let env: BTreeMap<String, String> = bytes
                .split(|b| *b == 0)
                .filter_map(|p| {
                    std::str::from_utf8(p)
                        .ok()?
                        .split_once('=')
                        .map(|(k, v)| (k.into(), v.into()))
                })
                .collect();
            let cwd = std::fs::read_link(root.join("cwd"))?;
            let resolve = |p: PathBuf| if p.is_absolute() { p } else { cwd.join(p) };
            let container_path =
                if let Some(p) = env.get("CONFIG_FILE").filter(|p| !p.trim().is_empty()) {
                    resolve(PathBuf::from(p.trim()))
                } else {
                    let mut candidates = vec![];
                    if let Some(dir) = env.get("DATA_DIR").filter(|p| !p.trim().is_empty()) {
                        candidates.push(resolve(PathBuf::from(dir.trim()).join("config.yaml")));
                    }
                    candidates.extend([
                        PathBuf::from("/app/data/config.yaml"),
                        cwd.join("config.yaml"),
                        cwd.join("config/config.yaml"),
                        PathBuf::from("/etc/sub2api/config.yaml"),
                    ]);
                    candidates
                        .into_iter()
                        .find(|p| {
                            root.join("root")
                                .join(p.strip_prefix("/").unwrap_or(p))
                                .is_file()
                        })
                        .ok_or_else(|| Error::new(409, "目标进程没有可确认的配置文件"))?
                };
            if !container_path.is_absolute() {
                return Err(Error::new(
                    409,
                    "相对 CONFIG_FILE 路径需要先在部署配置中明确",
                ));
            }
            let running_file = std::fs::metadata(
                root.join("root")
                    .join(container_path.strip_prefix("/").unwrap()),
            )?;
            let local_file = std::fs::metadata(&self.config)?;
            if (running_file.dev(), running_file.ino()) != (local_file.dev(), local_file.ino()) {
                return Err(Error::new(409, "指定文件与目标进程配置文件不一致"));
            }
            let executable = root.join("exe");
            // -version writes through Go's logger to stderr. Execute the running
            // inode, including when the on-disk binary has already been replaced.
            let mut cmd = tokio::process::Command::new(&executable);
            cmd.arg("-version").kill_on_drop(true);
            let out = tokio::time::timeout(Duration::from_secs(8), cmd.output())
                .await
                .map_err(|_| Error::new(504, "读取运行版本超时"))??;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            static VERSION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
                regex::Regex::new(r"Sub2API\s+(\S+)\s+\(commit:\s*([a-f0-9]{7,40}),").unwrap()
            });
            let cap = VERSION_RE
                .captures(&text)
                .filter(|_| out.status.success())
                .ok_or_else(|| Error::new(409, "无法确认运行版本及提交"))?;
            let commit = cap[2].to_string();
            let commit = if commit.len() >= 7 && string(&CATALOG["revision"]).starts_with(&commit) {
                string(&CATALOG["revision"]).to_string()
            } else {
                commit
            };
            let after = std::fs::read_link(&executable)?;
            let exe = std::fs::metadata(&executable)?;
            let process = sha256(format!(
                "{}:{}:{}:{}:{}",
                d["Id"],
                pid,
                d["State"]["StartedAt"],
                exe.ino(),
                after.display()
            ));
            Ok(Observed {
                version: cap[1].trim_start_matches('v').into(),
                commit,
                process,
                healthy: d["State"]["Health"]["Status"] == "healthy",
                env,
            })
        })
    }
    fn restart(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            command(&["docker", "restart", "--time", "15", &self.container], 40).await?;
            Ok(())
        })
    }
}
pub struct Host {
    config: PathBuf,
    driver: Arc<dyn Driver>,
    targets: Vec<String>,
    gate: Arc<tokio::sync::Mutex<()>>,
    operation: Mutex<Value>,
    health_wait: Duration,
}
impl Host {
    fn new(config: PathBuf, driver: Arc<dyn Driver>, targets: Vec<String>) -> Self {
        Self {
            config,
            driver,
            targets,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            operation: Mutex::new(json!({"status":"idle"})),
            health_wait: Duration::from_secs(90),
        }
    }
    fn read(&self) -> Result<Vec<u8>> {
        let m = std::fs::symlink_metadata(&self.config)?;
        if !m.is_file() || m.len() > MAX_CONFIG_BYTES as u64 {
            return Err(Error::new(409, "配置文件类型或大小不受支持"));
        }
        Ok(std::fs::read(&self.config)?)
    }
    fn update(&self, status: &str, message: &str) {
        *self.operation.lock().unwrap_or_else(|e| e.into_inner()) =
            json!({"status":status,"message":message,"updated_at":crate::compat::now()});
    }
    async fn current(&self) -> Result<(Observed, Vec<u8>)> {
        let o = self.driver.observe().await?;
        let bytes = self.read()?;
        Ok((o, bytes))
    }
    async fn snapshot(&self) -> Result<Value> {
        let (o, bytes) = self.current().await?;
        let config = parse_yaml(&bytes)?;
        let supported = o.version == CATALOG["version"] && o.commit == CATALOG["revision"];
        let effective = effective(&config, &o.env);
        let mut values = json!({});
        for f in CATALOG["fields"].as_array().unwrap() {
            if f["internal"] == true {
                continue;
            }
            let path = string(&f["path"]);
            let overridden = environment_override(f, &config, &o.env);
            values[path] = json!({"configured":get(&config,path).is_some(),"yaml":get(&config,path),"effective":get(&effective,path),"source":if overridden.is_some(){"environment"}else if get(&config,path).is_some(){"yaml"}else{"default"},"environment":overridden.as_ref().map(|(k,_)|k),"editable":supported&&f["editable"]==true&&overridden.is_none()});
        }
        let mut unknown = vec![];
        fn walk(v: &Value, path: &str, out: &mut Vec<Value>) {
            if super::field(path).is_some() {
                return;
            }
            if let Some(map) = v.as_object() {
                for (k, v) in map {
                    walk(
                        v,
                        &if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        },
                        out,
                    );
                }
            } else {
                out.push(json!({"path":path,"message":CATALOG["obsolete"][path].as_str().unwrap_or("当前目录未识别此字段；保存时保留原值。" )}));
            }
        }
        walk(&config, "", &mut unknown);
        Ok(
            json!({"available":true,"editable":supported,"reason":if supported{""}else{"运行版本没有匹配的配置目录，请更新 Sub2Ops"},"version":o.version,"commit":o.commit,"healthy":o.healthy,"runtime_revision":o.process,"revision":sha256(&bytes),"catalog":*CATALOG,"values":values,"unknown":unknown}),
        )
    }
    async fn checked(&self, payload: &Value) -> Result<(Observed, Vec<u8>, Vec<u8>)> {
        let (o, before) = self.current().await?;
        if o.version != CATALOG["version"] || o.commit != CATALOG["revision"] {
            return Err(Error::new(409, "运行版本与配置目录不匹配"));
        }
        if o.process != payload["runtime_revision"] {
            return Err(Error::conflict("运行进程已变化，请刷新配置"));
        }
        let (after, _) = prepare(&before, &o.env, payload)?;
        Ok((o, before, after))
    }
    async fn wait_healthy(&self, previous: &Observed) -> Result<Observed> {
        let until = Instant::now() + self.health_wait;
        loop {
            if let Ok(o) = self.driver.observe().await {
                if o.healthy
                    && o.process != previous.process
                    && o.version == previous.version
                    && o.commit == previous.commit
                {
                    return Ok(o);
                }
            }
            if Instant::now() >= until {
                return Err(Error::new(504, "重启后未在期限内恢复健康"));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    async fn apply(
        self: Arc<Self>,
        original: Observed,
        before: Vec<u8>,
        after: Vec<u8>,
        _lock: tokio::sync::OwnedMutexGuard<()>,
    ) {
        let backup = self.config.with_extension("yaml.sub2ops-rollback");
        let result = async {
            atomic(&backup, &before, &self.config)?;
            if self.read()? != before {
                return Err(Error::conflict("保存前文件发生变化，已停止"));
            }
            // Confirm identity again immediately before replacing the document.
            if self.driver.observe().await?.process != original.process {
                return Err(Error::conflict("保存前进程发生变化，已停止"));
            }
            atomic(&self.config, &after, &self.config)?;
            self.update("restarting", "已保存，正在重启 Sub2API");
            self.driver.restart().await?;
            self.wait_healthy(&original).await?;
            if self.read()? != after {
                return Err(Error::conflict("重启后文件被外部修改"));
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                self.update("succeeded", "配置已应用，Sub2API 已恢复健康");
                let _ = std::fs::remove_file(&backup);
            }
            Err(error) => {
                let error = error.detail.as_str().unwrap_or("配置应用失败");
                let current = self.read();
                if current.as_ref().is_ok_and(|b| *b == before) {
                    self.update("failed", error);
                    let _ = std::fs::remove_file(&backup);
                    return;
                }
                if !current.is_ok_and(|b| b == after) {
                    self.update("conflict", "配置已被外部修改，未覆盖；应用前副本已保留");
                    return;
                }
                if self
                    .driver
                    .observe()
                    .await
                    .is_ok_and(|o| o.version != original.version || o.commit != original.commit)
                {
                    self.update("conflict", "运行版本发生变化，未覆盖配置；应用前副本已保留");
                    return;
                }
                self.update("restoring", "应用失败，正在恢复上次配置");
                let rollback = async {
                    let previous = self.driver.observe().await.ok();
                    if self.read()? != after {
                        return Err(Error::conflict("恢复前文件发生变化"));
                    }
                    atomic(&self.config, &before, &self.config)?;
                    self.driver.restart().await?;
                    self.wait_healthy(previous.as_ref().unwrap_or(&original))
                        .await?;
                    if self.read()? != before {
                        return Err(Error::conflict("恢复后文件被外部修改"));
                    }
                    Ok(())
                }
                .await;
                match rollback {
                    Ok(()) => {
                        self.update("restored", &format!("{error}；已恢复上次配置并恢复健康"));
                        let _ = std::fs::remove_file(&backup);
                    }
                    Err(_) => self.update(
                        "restore_failed",
                        &format!("{error}；上次配置恢复或健康检查失败，请检查目标服务"),
                    ),
                }
            }
        }
    }
    async fn handle(self: &Arc<Self>, request: &Value) -> Result<Value> {
        if !self.targets.iter().any(|s| s == string(&request["target"])) {
            return Err(Error::new(409, "当前连接与宿主机绑定的 Sub2API 不一致"));
        }
        match string(&request["action"]) {
            "read" => self.snapshot().await,
            "operation" => Ok(self
                .operation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()),
            "validate" => match self.checked(&request["payload"]).await {
                Ok(_) => Ok(json!({"valid":true,"errors":[]})),
                Err(e) => Ok(
                    json!({"valid":false,"errors":e.detail.get("errors").cloned().unwrap_or_else(||json!([{"path":"","message":e.detail.as_str().unwrap_or("配置校验失败")}]))}),
                ),
            },
            "apply" => {
                let lock = self
                    .gate
                    .clone()
                    .try_lock_owned()
                    .map_err(|_| Error::conflict("已有配置应用正在执行"))?;
                if self.config.with_extension("yaml.sub2ops-rollback").exists() {
                    return Err(Error::conflict(
                        "存在尚未处理的恢复副本，请先检查上次应用结果",
                    ));
                }
                let (o, before, after) = self.checked(&request["payload"]).await?;
                self.update("saving", "正在保存配置");
                // Own the transaction independently of the socket lifetime.
                tokio::spawn(self.clone().apply(o, before, after, lock));
                Ok(json!({"status":"saving","message":"正在保存配置"}))
            }
            _ => Err(Error::invalid()),
        }
    }
}
fn atomic(path: &Path, bytes: &[u8], original: &Path) -> Result<()> {
    let metadata = std::fs::metadata(original)?;
    let parent = path.parent().ok_or_else(Error::invalid)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(metadata.mode() & 0o777))?;
    use std::os::fd::AsRawFd;
    // Preserve the file's actual owner, even though the host helper is root.
    if unsafe { libc::fchown(tmp.as_file().as_raw_fd(), metadata.uid(), metadata.gid()) } != 0 {
        return Err(Error::internal());
    }
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|_| Error::internal())?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub async fn run(args: &[String]) -> Result<()> {
    let mut options = BTreeMap::new();
    let mut it = args.iter();
    while let Some(k) = it.next() {
        if !matches!(
            k.as_str(),
            "--socket"
                | "--container"
                | "--config"
                | "--client-uid"
                | "--upstream-url"
                | "--public-url"
        ) {
            return Err(Error::invalid());
        }
        options.insert(k.as_str(), it.next().ok_or_else(Error::invalid)?.clone());
    }
    let required = |key| options.get(key).cloned().ok_or_else(Error::invalid);
    let socket = PathBuf::from(required("--socket")?);
    let config = PathBuf::from(required("--config")?);
    let container = required("--container")?;
    if !config.is_absolute()
        || !socket.is_absolute()
        || !regex::Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_.-]*$")
            .unwrap()
            .is_match(&container)
    {
        return Err(Error::invalid());
    }
    let uid = required("--client-uid")?
        .parse::<u32>()
        .map_err(|_| Error::invalid())?;
    let targets = [options.get("--upstream-url"), options.get("--public-url")]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err(Error::invalid());
    }
    let host = Arc::new(Host::new(
        config.clone(),
        Arc::new(Docker { container, config }),
        targets,
    ));
    let parent = socket.parent().ok_or_else(Error::invalid)?;
    std::fs::create_dir_all(parent)?;
    if socket.exists() {
        if UnixStream::connect(&socket).await.is_ok() {
            return Err(Error::conflict("宿主机配置接口已在运行"));
        }
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    let name = std::ffi::CString::new(socket.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::invalid())?;
    let directory = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::invalid())?;
    if unsafe { libc::chown(name.as_ptr(), 0, uid) } != 0
        || unsafe { libc::chown(directory.as_ptr(), 0, uid) } != 0
    {
        return Err(Error::internal());
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        let Ok(cred) = stream.peer_cred() else {
            continue;
        };
        if cred.uid() != uid {
            continue;
        }
        let host = host.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(10),
                read_frame(&mut stream),
            )
            .await
            {
                Ok(Ok(r)) => host.handle(&r).await,
                _ => Err(Error::invalid()),
            };
            let reply = match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(e) => json!({"ok":false,"status":e.status.as_u16(),"detail":e.detail}),
            };
            let _ = write_frame(&mut stream, &reply).await;
        });
    }
}

#[cfg(test)]
mod tests;
