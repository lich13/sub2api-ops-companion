# 批量连接测试：Rust 隔离原型

这是独立实验服务，不读取生产配置、Keychain、业务状态或账号凭据，也不接入生产路由。仅接受显式 `SUB2OPS_PROBE_ISOLATED=fixture-only`、loopback 上游和以 `probe_fixture_` 开头的临时 PostgreSQL 数据库。不要向它提供生产配置。

Python 对照直接复用正式版本的 `AccountOperations`、`ConnectionBatches`、`DesktopActions` 和原子状态存储，只将数据库投影替换为共用合成账号表。Rust 使用 Axum／Tokio、Reqwest／Rustls、SQLx PostgreSQL，数据库连接池同为 1–8，批测并发同为 2，派发周期同为 250 ms；两者都在派发前 fsync 保存状态，重启不重放未知请求。

HTTP 合同与正式客户端一致：

| 请求 | 用途 |
|---|---|
| `GET /api/desktop/v1/accounts` | 合成账号和字段版本 |
| `POST /api/desktop/v1/account-operations/batch` | `action=test`，返回 202；请求 ID 去重，最多 1000 账号 |
| `GET /api/desktop/v1/account-operations?batch_id=…` | 逐项状态及必要测试摘要 |
| `POST /api/desktop/v1/account-operations/batches/…/cancel` | 只取消尚未派发项 |

业务请求使用 `x-api-key`，经模拟 Sub2API 管理员接口验证。请求模型在接受时冻结，实际模型来自原生 `test_start`；不查模型目录、不改白名单、不保存正文。任务完成与测试通过分别判断。连接中断、超时、缺少终态或运行中崩溃均不得自动重测。

## 复现

使用项目的 Python 3.12 依赖和 Rust 1.94.1。先在独立 PostgreSQL 16 实例准备可以创建临时数据库的 fixture 管理账号，将 loopback 管理连接 URL 放入 `PROBE_QA_DATABASE_URL`。驱动只创建和删除自身带随机后缀的临时数据库、状态目录及进程。

```sh
python -m pip install -r requirements.txt
cargo test --manifest-path experiments/connection_probe/rust/Cargo.toml --locked
cargo clippy --manifest-path experiments/connection_probe/rust/Cargo.toml --locked --all-targets -- -D warnings
cargo build --manifest-path experiments/connection_probe/rust/Cargo.toml --release --locked
python scripts/connection_probe_check.py --mode contract --implementation python --command '["python", "-m", "experiments.connection_probe.python_slice"]' --database-url "$PROBE_QA_DATABASE_URL"
python scripts/connection_probe_check.py --mode contract --implementation rust --command '["experiments/connection_probe/rust/target/release/sub2ops-probe"]' --database-url "$PROBE_QA_DATABASE_URL"
```

Linux x86_64、Docker 和 cgroup v2 的同环境测量：

```sh
docker build -f experiments/connection_probe/Dockerfile.python -t sub2ops-probe-python .
docker build -f experiments/connection_probe/Dockerfile.rust -t sub2ops-probe-rust .
docker build -t sub2ops-probe-full .
python scripts/connection_probe_check.py --mode contract --implementation python --image sub2ops-probe-python --database-url "$PROBE_QA_DATABASE_URL"
python scripts/connection_probe_check.py --mode contract --implementation rust --image sub2ops-probe-rust --database-url "$PROBE_QA_DATABASE_URL"
python scripts/connection_probe_check.py --mode benchmark --implementation python --image sub2ops-probe-python --database-url "$PROBE_QA_DATABASE_URL" --full-image sub2ops-probe-full
python scripts/connection_probe_check.py --mode benchmark --implementation rust --image sub2ops-probe-rust --database-url "$PROBE_QA_DATABASE_URL" --binary-path /usr/local/bin/sub2ops-probe
```

CI 的 `prototype` 作业执行同一组命令并在日志中输出 JSON 指标。20／100 账号使用相同合成记录、50 ms 上游延迟、并发 2，各运行三次；1000 项只测整批接受边界，随后停止未派发项。记录 Docker `image inspect` 的 `Size`、Rust 二进制、容器空闲／峰值内存、CPU 用时、批次耗时和 HTTP 请求延迟。合同另覆盖鉴权、幂等冲突、原生映射、混合平台、版本变化、共享凭据互斥、取消、SSE 截断、超时及重启恢复。

## 如何解释结果

Docker 报告的镜像 `Size` 不代表独占磁盘占用，也不据此推断压缩传输或展开后的大小；容器内存不是整机用量。功能切片结果反映这两个具体实现及其依赖、连接复用和存储开销，不能全部归因于语言。正式 Python 服务另测空数据库下的完整运行时参考；启动公共指纹库同步结束后才采空闲值，并记录同步状态，不能直接与带业务历史的生产内存相比。

原型仅实现本轮批测链路：没有自动恢复、模型检测、预算／冷却、用卡、通知、历史迁移和多实例调度。其状态格式属于实验格式，不可替换生产状态。Rust 的文件锁限制同一状态目录只有一个实例；资源收益不等于完整重写已经具备上线条件。

后续迁移按只读查询、手动操作队列、模型检测、恢复／用卡／通知推进。每一步须通过共享合同和真实入口验收，重点保持状态兼容、人工控制优先、预算与冷却，以及未知结果禁止重放；达到这些条件后才决定是否扩大替换范围。
