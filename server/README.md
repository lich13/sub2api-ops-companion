# Companion 服务

Axum／Tokio 提供 HTTP 服务，Reqwest／Rustls 连接 Sub2API 和通知服务，SQLx 使用现有 PostgreSQL 业务表。生产不加载 Python、不转发至旧 Companion，也不创建替代业务表。

## 构建与运行

工具链与桌面一致，使用 Rust stable 和提交的 Cargo.lock。

```bash
cargo build --release --locked --manifest-path server/Cargo.toml
server/target/release/sub2ops-server --version
```

配置变量见根目录 README。配置文件优先于环境变量，路径和默认值沿用 0.1.39。配置完成后：

```bash
server/target/release/sub2ops-server serve
```

每次启动都会校验配置、状态文件和数据库连接，并取得进程独占锁；状态损坏或已有状态丢失时拒绝初始化覆盖。

接口保持 `api_version=1`，0.1.84 及以后的客户端可继续连接；账号写操作统一经操作队列（`POST /accounts/{id}/operations` 与 `/account-operations/batch`）提交。`capabilities` 的 `implementation` 和 `build_revision` 用于核对实际服务；正式构建通过 `SUB2OPS_BUILD_REVISION` 注入提交。

## 验证

Python 仅用于测试驱动，需要项目的 Python 测试依赖及 PostgreSQL 的 `initdb`／`pg_ctl`。测试使用回环模拟上游与临时数据库，不读取生产配置。

```bash
cargo fetch --locked --manifest-path server/Cargo.toml
python scripts/check_release_versions.py
cargo fmt --check --manifest-path server/Cargo.toml
python scripts/server_test.py
cargo clippy --locked --manifest-path server/Cargo.toml --all-targets --features test-fixtures -- -D warnings
cargo build --locked --manifest-path server/Cargo.toml
python -m unittest discover -s scripts -p test_dependency_audit.py
python scripts/server_dependency_audit.py
python -m unittest discover -s scripts -p test_http_client.py
python scripts/server_http_contract.py --command-json '["server/target/debug/sub2ops-server"]'
python scripts/server_mutation_contract.py --command-json '["server/target/debug/sub2ops-server"]'
```

`scripts/server_test.py` 支持 `--test <套件>`、`--lib` 和 `--filter <名称>` 缩小范围。`tests/contracts/` 下的固定数据向量覆盖哈希、时间和数值序列化；进程测试覆盖接口、请求次数和未知结果恢复。

依赖审计完整扫描两个锁文件，不忽略通告或限定平台。SQLx 0.8 使用的 `webpki-roots 0.26.11` 仅重新导出 1.x 证书数据；针对 `RUSTSEC-2026-0381`，脚本只接受该兼容包与已修复的 `1.0.10` 的精确注册源、校验和及依赖关系。旧根证书、关系变化或其他漏洞仍阻断发布。依据为[兼容包源码](https://docs.rs/webpki-roots/0.26.11/src/webpki_roots/lib.rs.html)和[上游证书修复](https://github.com/rustls/webpki-roots/pull/133)。

测试驱动启用 `test-fixtures`，允许将模型请求的最终地址指向回环模拟服务，保持原有鉴权、账号与配置校验、请求正文和 SSE 处理。正式构建不启用此功能，也不提供运行时覆盖上游地址的配置。

`scripts/server_runtime_smoke.py` 使用临时 PostgreSQL、合成配置和隔离容器验证非 root 运行、Compose 健康检查及后台故障后非零退出与重启。故障来自单独编译的 `supervision_fixture`，该测试程序不会进入正式镜像或其归档层；生产服务没有故障注入开关。结果保存在 `Companion-validation` 成品的 `runtime-smoke.json`。

## 部署和回退

`Companion-linux-amd64` CI 成品包含镜像归档、SHA256SUMS 和记录版本与提交的 manifest。先核对摘要、加载镜像，并保留当前版本的回退镜像。

切换时停止旧服务接收和派发任务，最多等待 60 秒收束；确认停止后使用同一状态目录启动新镜像。核对公网鉴权、实现和提交、账号与历史查询以及后台状态。启动或入口检查失败时切回旧镜像，继续使用当前状态；不要恢复过期快照。正常运行与回退均不得同时启动两个写服务。

Rust 的状态写入沿用 JSON 结构和版本，使用文件锁、临时文件、原子替换以及文件和目录同步。未派发任务可以接续；已派发的请求先回读核对，未知测活、用卡或不可逆写入不得重放。人工控制版本、预算、冷却、通知去重和已送达记录持续生效。
