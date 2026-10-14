# Companion 服务

Axum／Tokio 提供 HTTP 服务，Reqwest／Rustls 连接 Sub2API 和通知服务，SQLx 使用现有 PostgreSQL 业务表。生产不加载 Python、不转发至旧 Companion，也不创建替代业务表。

## 构建与运行

工具链与桌面一致，使用 Rust 1.94.1 和提交的 Cargo.lock。

```bash
cargo build --release --locked --manifest-path server/Cargo.toml
server/target/release/sub2ops-server --version
```

配置变量见根目录 README。配置文件优先于环境变量，路径和默认值沿用 0.1.39。配置完成后：

```bash
server/target/release/sub2ops-server --check-state
server/target/release/sub2ops-server --read-only --host 127.0.0.1 --port 18082
server/target/release/sub2ops-server serve
```

`--check-state` 与 `--read-only` 不创建锁或状态文件、不迁移数据、不派发后台工作。HTTP 写操作返回 `read_only`；PostgreSQL 会话也设置为只读。正常运行使用进程独占锁，状态损坏或已有状态丢失时拒绝初始化覆盖。

旧客户端继续使用 `api_version=1`。管理员鉴权、路径与别名、字段版本、幂等请求、原生模型映射及 SSE 完成语义保持兼容。`capabilities` 的 `implementation` 和 `build_revision` 用于核对实际服务；正式构建通过 `SUB2OPS_BUILD_REVISION` 注入提交。

## 验证

Python 仅用于隔离测试驱动。驱动从 Git 固定提交 `6be7c291ed38dcc337cb88f693639040a3463899` 临时读取旧实现；需要完整 Git 历史、项目 Python 测试依赖及 PostgreSQL 的 `initdb`／`pg_ctl`。测试使用回环模拟上游与临时数据库，不读取生产配置。

```bash
cargo fetch --locked --manifest-path server/Cargo.toml
python scripts/server_test.py
cargo clippy --locked --manifest-path server/Cargo.toml --all-targets --features test-fixtures -- -D warnings
python scripts/server_reference_tests.py
python scripts/server_http_contract.py
python scripts/server_http_contract.py --command-json '["server/target/debug/sub2ops-server"]'
python scripts/server_mutation_contract.py
python scripts/server_mutation_contract.py --command-json '["server/target/debug/sub2ops-server"]'
```

`tests/contracts/migration-map.json` 记录旧测试与新断言的对应关系。固定数据向量覆盖跨语言哈希、时间和数值序列化；进程测试覆盖接口、请求次数和未知结果恢复。测试通过与尚未覆盖的范围分别记录。

测试驱动启用 `test-fixtures`，允许将模型请求的最终地址指向回环模拟服务，保持原有鉴权、账号与配置校验、请求正文和 SSE 处理。正式构建不启用此功能，也不提供运行时覆盖上游地址的配置。

CI 在同一 Linux x86_64 主机构建两个完整服务镜像，使用相同账号、开关、并发及模拟延迟运行 20／100 账号批次和 1000 账号接受边界。结果保存在 `Companion-validation` 中，包含镜像与二进制体积、空闲／峰值内存、CPU、耗时和请求延迟。这里的自动任务开关关闭，所有运行时模块仍加载；结果不等同于生产业务负载。

`scripts/server_history_memory.py` 另用十万条合成用量、三千条错误和已完成操作历史，对比两个完整服务连续刷新质量统计时的资源开销。检查统计确实完成、没有额外业务请求，并限制 Rust 的内存峰值及刷新后的持续增长；结果保存在同一成品的 `history-memory.json`。

镜像体积统一读取同一 CI 主机的 `docker image inspect Size`；不同 Docker 存储后端的这个值可能与 `docker system df` 不同，不能用它推算主机实际磁盘占用。部署后的磁盘统计需单独测量，共享层也不能重复相加。

## 部署和回退

`Companion-linux-amd64` CI 成品包含镜像归档、SHA256SUMS 和提交／二进制摘要。先核对摘要、加载镜像，用现有配置、数据目录和数据库执行只读检查。保留旧镜像至稳定观察结束。

切换时停止旧服务接收和派发任务，最多等待 60 秒收束；确认停止后使用同一状态目录启动新镜像。核对公网鉴权、实现和提交、账号与历史查询以及后台状态。启动或入口检查失败时切回旧镜像，继续使用当前状态；不要恢复过期快照。正常运行与回退均不得同时启动两个写服务。

Rust 的状态写入沿用 JSON 结构和版本，使用文件锁、临时文件、原子替换以及文件和目录同步。未派发任务可以接续；已派发的请求先只读核对，未知测活、用卡或不可逆写入不得重放。人工控制版本、预算、冷却、通知去重和已送达记录持续生效。
