# Sub2API Ops Companion

Sub2API 的 Rust 旁路运维服务，提供 OAuth 额度监控、Bark 事件推送、桌面账号管理和新模型思考档位补全。

**Sub2Ops** 支持 macOS Apple Silicon 和 Android ARM64，提供分组动态、账号管理、错误详情及 Ops 设置；Mac 另有菜单栏快捷面板。下载与使用见 [客户端说明](desktop/README.md)，版本变化见 [发布记录](desktop/CHANGELOG.md)。仅通过 App 管理；网页、SSO 和旧表单接口已下线。

- OAuth 额度监控：等待恢复时间后确认额度，统一限制自动查询并执行恢复后测活。
- Bark：推送 OAuth 恢复、测活失败、自动恢复失败和 401/402 认证异常。
- OAuth 额度查询：客户端支持单账号查询和全部 OAuth 刷新；Grok 只刷新官方账单，不发送模型请求。
- 账号保活：按账号选择是否清除符合条件的临时不可调度，所有平台默认关闭；保留人工调度、降智标记及恢复限额。

机制说明：[OAuth 监控](docs/oauth-monitoring.md)、[模型检测](docs/model-detection.md)、[账号保活](docs/keepalive.md)、[自动用卡](docs/automatic-credit.md)、[账号测试](docs/account-operations.md)、[模型目录](docs/model-catalog.md)。升级和回退见 [UPGRADING.md](UPGRADING.md)。

## 运行

```bash
cp .env.example .env
docker compose up -d --build
```

`.env.example` 只提供本地开发占位值；部署前请替换数据库连接、Sub2API 地址和 Bark 配置，并不要把 `.env` 或 `data/` 纳入公开发布。

服务源码位于 [`server/`](server/README.md)，正式镜像不含 Python 运行时。生产部署优先加载对应提交的 `Companion-linux-amd64` CI 成品，并设置 `OPS_IMAGE`；镜像只挂载 `/data`。`--check-state` 检查现有配置、状态和数据库后退出，`--read-only` 提供不派发后台任务、不写业务或状态的验收入口。两种模式都必须使用与正式服务相同的环境配置。

Compose 默认只将端口发布到宿主机 `127.0.0.1:18081`；容器内服务默认监听 `0.0.0.0:18081`。生产环境建议通过 nginx 挂载到 Sub2API 同域的 `/sub2ops/`，并关闭该路径的 query access log。nginx 示例见 `deploy/nginx/sub2ops.location.conf`。

直接运行二进制也默认监听所有网卡；仅本机访问时使用 `sub2ops-server serve --host 127.0.0.1 --port 18081`，不要把容器端口发布限制误认为二进制的监听限制。桌面业务管理路由验证管理员 API Key；`connection-status` 沿用 HTTP 200 携带脱敏鉴权失败诊断的契约，不返回地址、密钥或业务数据。`/healthz` 与模型目录入口分别沿用健康检查和客户端鉴权语义。

示例数据库连接的口令必须替换，`sslmode=disable` 仅适用于同一受控 Docker 网络。跨主机连接应启用 TLS、使用可信 CA 并验证服务端主机名（`sslmode=verify-full`）。

## 环境变量

| 变量 | 说明 |
|---|---|
| `SUB2API_CONFIG_PATH` | 独立连接配置文件，默认 `/data/sub2api-config.json`，JSON 的 `base_url` / `verify_base_url` 优先于对应环境变量。 |
| `SUB2API_BASE_URL` | Sub2API 公网根地址。 |
| `SUB2API_VERIFY_BASE_URL` | 可选的服务端内网校验根地址。 |
| `GROUP_MODEL_CONFIG_PATH` | 分组思考档位补全文件，默认 `/data/group-model-config.json`；按分组版本校验。 |
| `KEEPALIVE_CONFIG_PATH` | 账号保活设置、恢复预算和待核对记录，默认 `/data/keepalive-state.json`；所有账号默认关闭。 |
| `DATABASE_URL` | Sub2API PostgreSQL 连接串。 |
| `BASE_PATH` | 历史兼容字段，默认 `/sub2ops`；外部前缀须由 nginx 配置并剥离，Rust 不据此挂载路由。 |
| `USAGE_QUERY_STATE_PATH` | OAuth 快照、管理员 API Key 和调度元数据，默认 `/data/usage-query-state.json`。 |
| `OAUTH_CONFIG_PATH` | OAuth 自动化配置，默认 `/data/oauth-config.json`；JSON 优先于环境变量，环境变量优先于默认值。 |
| `BARK_CONFIG_PATH` | Bark 配置文件，默认 `/data/bark-config.json`。 |
| `BARK_ENABLED` | 是否启用事件 Bark 推送（含账号保活达到频率上限后的暂停通知），默认关闭。 |
| `BARK_DEVICE_KEY` | Bark Device Key；生产环境建议通过桌面设置写入权限为 `0600` 的配置文件。 |
| `BARK_SERVER_URL` | Bark 服务根 URL，默认 `https://api.day.app`；HTTP 只允许 loopback。桌面端保留当前运行时 URL。 |
| `OAUTH_RECOVERY_MONITOR_ENABLED` | 是否监控到期恢复，默认开启。 |
| `MODEL_DETECTION_MODEL_ID` | 全局自动检测模型，默认 `gpt-6-luna`；新任务生效，已有任务保留原模型。 |
| `OAUTH_AUTO_RESET_CREDIT_ENABLED` | 7d 原始用量达到 100% 且当前上游 429 限流时自动用卡，默认关闭；与 Sub2API 原自动用卡互斥。 |
| `OAUTH_DAILY_TEST_ENABLED` | 是否启用每日 OpenAI OAuth 测活，默认开启；仅异常通过 Bark 推送。 |
| `OAUTH_DAILY_TEST_TIME` | 每日测活时间（北京时间 `HH:MM`），默认 `05:00`。修改时间、启用或重启后均等待下一个未来时间点，错过不补跑。 |
| `OAUTH_USAGE_REFRESH_CONCURRENCY` | active usage 并发，默认 `4`。 |
| `OAUTH_RECOVERY_TEST_CONCURRENCY` | account test 并发，默认 `2`。 |
| `OAUTH_EARLY_PROBE_BATCH_SIZE` | 历史提前探测参数，默认 `8`；保留读取、保存和校验兼容，当前不提前探测，也不用于后台派发上限。 |
| `OAUTH_RECOVERY_TEST_MODEL_ID` | 恢复测活模型，默认 `gpt-5.6-luna`。 |

`OAUTH_CONFIG_PATH` 中的 JSON 优先于环境变量，环境变量优先于默认值。状态和配置文件在实际写入时统一为 `0600`，不继承旧文件的宽权限；只读检查不修改权限。

## 安全

Companion 具有账号调度操作权限，不应独立暴露在公网。仅向回环地址或 Docker 内网开放服务，桌面接口经 TLS 反代并验证管理员权限。管理员 API Key 保存在 `USAGE_QUERY_STATE_PATH`，Bark Device Key 保存在 `BARK_CONFIG_PATH`；写入文件及使用的文件锁统一为 `0600`，未写入的历史文件仅给出安全诊断，密钥不写入 URL、页面、日志或审计明文。不要公开 `.env`、`data/` 或生产日志。

Bark 的 HTTPS 目标拒绝非公网字面 IP（含 IPv4 映射及数字写法），支持 `api.day.app` 和公网自建服务；现有 HTTP loopback 方式保留。不进行 DNS 解析校验，域名仍需由部署者配置为可信服务。

配置管理说明：[Sub2API 配置](docs/sub2api-config.md)。
