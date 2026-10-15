# Sub2API Ops Companion

Sub2API 的 Rust 旁路运维服务，提供 OAuth 额度监控、Bark 事件推送、桌面账号管理和新模型思考档位补全。

**Sub2Ops** 支持 macOS Apple Silicon 和 Android ARM64，提供分组动态、账号管理、错误详情及 Ops 设置；Mac 另有菜单栏快捷面板。下载与使用见 [客户端说明](desktop/README.md)，版本变化见 [发布记录](desktop/CHANGELOG.md)。仅通过 App 管理；网页、SSO 和旧表单接口已下线。

- OAuth 额度监控：等待恢复时间后确认额度，统一限制自动查询并执行恢复后测活。
- Bark：推送 OAuth 恢复、测活失败、自动恢复失败和 401/402 认证异常。
- OAuth 额度查询：客户端支持单账号查询和全部 OAuth 刷新；Grok 只刷新官方账单，不发送模型请求。
- Key 调度回退：OpenAI、Grok 分平台控制选定的 apikey。某平台全部 OAuth 账号不可用时开启该平台的 Key；存在可用 OAuth 时，勾选“OAuth 可用时也启用”的 Key 保持开启，其余关闭。没有 OAuth 或无法判断时保持原状态。Grok 根据 Sub2API 的调度、冷却、限流、到期和重新认证状态判断，不额外查询额度。

机制说明：[OAuth 监控](docs/oauth-monitoring.md)、[模型检测](docs/model-detection.md)、[自动用卡](docs/automatic-credit.md)、[账号测试](docs/account-operations.md)、[模型目录](docs/model-catalog.md)。升级和回退见 [UPGRADING.md](UPGRADING.md)。

## 运行

```bash
cp .env.example .env
docker compose up -d --build
```

`.env.example` 只提供本地开发占位值；部署前请替换数据库连接、Sub2API 地址和 Bark 配置，并不要把 `.env` 或 `data/` 纳入公开发布。

服务源码位于 [`server/`](server/README.md)，正式镜像不含 Python 运行时。生产部署优先加载对应提交的 `Companion-linux-amd64` CI 成品，并设置 `OPS_IMAGE`；镜像只挂载 `/data`。`--check-state` 检查现有配置、状态和数据库后退出，`--read-only` 提供不派发后台任务、不写业务或状态的验收入口。两种模式都必须使用与正式服务相同的环境配置。

Compose 默认只将端口发布到宿主机 `127.0.0.1:18081`；容器内服务默认监听 `0.0.0.0:18081`。生产环境建议通过 nginx 挂载到 Sub2API 同域的 `/sub2ops/`，并关闭该路径的 query access log。nginx 示例见 `deploy/nginx/sub2ops.location.conf`。

## 环境变量

| 变量 | 说明 |
|---|---|
| `SUB2API_CONFIG_PATH` | 独立连接配置文件，默认 `/data/sub2api-config.json`，JSON 的 `base_url` / `verify_base_url` 优先于对应环境变量。 |
| `SUB2API_BASE_URL` | Sub2API 公网根地址。 |
| `SUB2API_VERIFY_BASE_URL` | 可选的服务端内网校验根地址。 |
| `GROUP_MODEL_CONFIG_PATH` | 分组思考档位补全文件，默认 `/data/group-model-config.json`；按分组版本校验。 |
| `DATABASE_URL` | Sub2API PostgreSQL 连接串。 |
| `BASE_PATH` | 反代路径前缀，默认 `/sub2ops`。 |
| `USAGE_QUERY_STATE_PATH` | OAuth 快照、管理员 API Key 和调度元数据，默认 `/data/usage-query-state.json`。 |
| `OAUTH_CONFIG_PATH` | OAuth 自动化配置，默认 `/data/oauth-config.json`；JSON 优先于环境变量，环境变量优先于默认值。 |
| `BARK_CONFIG_PATH` | Bark 配置文件，默认 `/data/bark-config.json`。 |
| `BARK_ENABLED` | 是否启用 OAuth 事件的 Bark 推送，默认关闭。 |
| `BARK_DEVICE_KEY` | Bark Device Key；生产环境建议通过桌面设置写入权限为 `0600` 的配置文件。 |
| `BARK_SERVER_URL` | Bark 服务根 URL，默认 `https://api.day.app`；HTTP 只允许 loopback。桌面端保留当前运行时 URL。 |
| `KEY_FALLBACK_CONFIG_PATH` | Key 调度回退配置文件，默认 `/data/key-fallback-config.json`。 |
| `OAUTH_RECOVERY_MONITOR_ENABLED` | 是否监控到期恢复，默认开启。 |
| `OAUTH_AUTO_RESET_CREDIT_ENABLED` | 7d 原始用量达到 100% 且当前上游 429 限流时自动用卡，默认关闭；与 Sub2API 原自动用卡互斥。 |
| `OAUTH_DAILY_TEST_ENABLED` | 是否启用每日 OpenAI OAuth 测活，默认开启；仅异常通过 Bark 推送。 |
| `OAUTH_DAILY_TEST_TIME` | 每日测活时间（北京时间 `HH:MM`），默认 `05:00`。修改时间、启用或重启后均等待下一个未来时间点，错过不补跑。 |
| `OAUTH_USAGE_REFRESH_CONCURRENCY` | active usage 并发，默认 `4`。 |
| `OAUTH_RECOVERY_TEST_CONCURRENCY` | account test 并发，默认 `2`。 |
| `OAUTH_EARLY_PROBE_BATCH_SIZE` | 历史提前探测参数，默认 `8`；保留读取、保存和校验兼容，当前不提前探测，也不用于后台派发上限。 |
| `OAUTH_RECOVERY_TEST_MODEL_ID` | 恢复测活模型，默认 `gpt-5.6-luna`。 |

`OAUTH_CONFIG_PATH` 中的 JSON 优先于环境变量，环境变量优先于默认值。新建状态和配置文件默认 `0600`，已有权限保持不变。

## 安全

Companion 具有账号调度操作权限，不应独立暴露在公网。仅向回环地址或 Docker 内网开放服务，桌面接口经 TLS 反代并验证管理员权限。管理员 API Key 保存在 `USAGE_QUERY_STATE_PATH`，Bark Device Key 保存在 `BARK_CONFIG_PATH`；新文件默认 `0600`，已有文件保留权限并给出安全诊断，密钥不写入 URL、页面、日志或审计明文。不要公开 `.env`、`data/` 或生产日志。
