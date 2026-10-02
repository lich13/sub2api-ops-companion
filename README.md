# Sub2API Ops Companion

Sub2API 的旁路 OAuth 运维服务，提供 OAuth 额度监控、Bark 事件推送、桌面账号管理和新模型思考档位补全。

**Sub2Ops** 支持 macOS Apple Silicon 和 Android ARM64，提供分组动态、账号管理、错误详情及 Ops 设置；Mac 另有菜单栏快捷面板。下载与使用见 [客户端说明](desktop/README.md)，版本变化见 [发布记录](desktop/CHANGELOG.md)。仅通过 App 管理；网页、SSO 和旧表单接口已下线。

## 功能

- OAuth 额度监控：等待恢复时间后确认额度，统一限制自动查询并执行恢复后测活。
- Bark：推送 OAuth 恢复、测活失败、自动恢复失败和 401/402 认证异常。
- OAuth 额度查询：客户端支持单账号查询和全部 OAuth 刷新；Grok 只刷新官方账单，不发送模型请求。
- Key 调度回退：OpenAI、Grok 分平台控制选定的 apikey。某平台全部 OAuth 账号不可用时开启该平台的 Key，存在可用 OAuth 时关闭；没有 OAuth 或无法判断时保持原状态。Grok 根据 Sub2API 的调度、冷却、限流、到期和重新认证状态判断，不额外查询额度。

## OAuth 监控机制

- 普通后台额度刷新已移除；客户端自动更新只读已有快照与使用日志。
- 不提前探测。已耗尽的必要窗口以最晚恢复时间为准，在 `reset_at + 60秒` 后确认。
- 每账号自动查询至少间隔 1 小时，滚动 24 小时最多 6 次；失败、超时和到点仍未释放按 1、3、6、12 小时退避，之后保持 12 小时。
- 预算在请求前持久化，重启保留；401/402 暂停自动查询，凭据变化或手动成功后解除。正常账号不轮询，新账号无额度数据时才初始化查询。
- 手动单查和全部刷新可强制查询，不消耗或清空自动预算；结果共享并刷新自动冷却，同账号并发查询合并。
- `free` 只要求 7d 窗口；其他套餐同时要求 5h 和 7d。7d 无余量时不测活。
- 恢复监控全天运行；每日测活默认北京时间 `05:00`，先复用一小时内的完整额度，补查仍受限频约束；等待恢复或受限账号跳过，错过不补跑。
- 额度确认恢复后调用 Sub2API account test，默认模型为 `gpt-5.6-luna`。
- active usage 同一账号不会并发重复请求；请求前登记预算，结果原子写入状态文件。
- 测活结果先持久化为待推送事件。Bark 完整发送失败只重试发送，不重复测活；Bark 关闭时事件按 suppressed 语义确认。

客户端自动刷新只读取已有快照；用户点击额度查询才主动刷新。未知额度不会显示为 0%。

## 运行

```bash
cp .env.example .env
docker compose up -d --build
```

`.env.example` 只提供本地开发占位值；部署前请替换数据库连接、Sub2API 地址和 Bark 配置，并不要把 `.env` 或 `data/` 纳入公开发布。

默认监听 `127.0.0.1:18081`。生产环境建议通过 nginx 挂载到 Sub2API 同域的 `/sub2ops/`，并关闭该路径的 query access log。nginx 示例见 `deploy/nginx/sub2ops.location.conf`。

## 环境变量

- `SUB2API_CONFIG_PATH`：独立连接配置文件，默认 `/data/sub2api-config.json`，JSON 的 `base_url` / `verify_base_url` 优先于对应环境变量。
- `GROUP_MODEL_CONFIG_PATH`：分组思考档位补全文件；权限 `0600`，按分组版本校验。

- `DATABASE_URL`：Sub2API PostgreSQL 连接串。
- `BASE_PATH`：反代路径前缀，默认 `/sub2ops`。
- `USAGE_QUERY_STATE_PATH`：OAuth 快照、管理员 API Key 和调度元数据，默认 `/data/usage-query-state.json`。
- `OAUTH_CONFIG_PATH`：OAuth 自动化配置，默认 `/data/oauth-config.json`，权限 `0600`；JSON 优先于环境变量，环境变量优先于默认值。
- `BARK_CONFIG_PATH`：Bark 配置文件，默认 `/data/bark-config.json`。
- `BARK_ENABLED`：是否启用 OAuth 事件的 Bark 推送，默认关闭。
- `BARK_DEVICE_KEY`：Bark Device Key；生产环境建议通过桌面设置写入权限为 `0600` 的配置文件。
- `BARK_SERVER_URL`：Bark 服务根 URL，默认 `https://api.day.app`；HTTP 只允许 loopback。桌面端保留当前运行时 URL。

Codex OAuth 指定容量错误由独立采集任务读取 `ops_error_logs`，每两秒增量采集并回看五分钟处理延迟落库；每条新错误单独发送 Bark `critical` 重要警告。游标、去重、重试和手动降智标记持久保存在 `USAGE_QUERY_STATE_PATH` 同目录的 `capacity-alert-state.json`，权限 `0600`；不新增数据库表，不发起额度或模型请求。失败按 5/30/120/600 秒重试，首次启动不推送历史记录，手动标记立即取消该账号待发报警。
- `KEY_FALLBACK_CONFIG_PATH`：Key 调度回退配置文件，默认 `/data/key-fallback-config.json`，权限 `0600`。
- `OAUTH_RECOVERY_MONITOR_ENABLED`：是否监控到期恢复。
- `OAUTH_AUTO_RESET_CREDIT_ENABLED`：7d 原始用量达到 100% 且当前上游 429 限流时自动用卡，默认关闭；与 Sub2API 原自动用卡互斥。
- `OAUTH_DAILY_TEST_ENABLED`：是否启用每日 OpenAI OAuth 测活，默认开启；仅异常通过 Bark 推送。
- `OAUTH_DAILY_TEST_TIME`：每日测活时间（北京时间 `HH:MM`），默认 `05:00`。修改时间、启用或重启后均等待下一个未来时间点，错过不补跑。
- `OAUTH_USAGE_REFRESH_CONCURRENCY`：active usage 并发，默认 `4`。
- `OAUTH_RECOVERY_TEST_CONCURRENCY`：account test 并发，默认 `2`。
- `OAUTH_EARLY_PROBE_BATCH_SIZE`：每轮最多处理的 OAuth 账号数，默认 `8`。
- `OAUTH_RECOVERY_TEST_MODEL_ID`：恢复测活模型，默认 `gpt-5.6-luna`。
- `SUB2API_BASE_URL`：Sub2API 公网根地址。
- `SUB2API_VERIFY_BASE_URL`：可选的服务端内网校验根地址。

## 新模型思考档位

桌面“模型”页只维护需要补全的新模型。输入精确 ID 后读取真实上游，填写支持的思考档位与默认值；缺失信息不猜测。只写入 `supported_reasoning_levels` 和 `default_reasoning_level`，保存至 `GROUP_MODEL_CONFIG_PATH`（默认 `/data/group-model-config.json`，0600）。白名单阻挡时经明确确认只追加当前模型，保留其他条目和开关；冲突、失败保留草稿。

补全绑定普通或 Composite 分组的真实路由与账号映射。路由变化后停止应用；目录缺少新条目时只使用其自身的真实描述。当前已核对原版 Sub2API 0.2.9 / 0.2.10 的 Responses 转发规则：未知 Grok 型号会丢弃思考强度，部分档位会被改写；分组强度策略也可能限制请求。这些情况显示“转发受限”，未知版本或证据缺失显示“转发未核实”，均只存草稿。上游原生支持后可“恢复原生”，仅移除 Companion 补全，不撤销白名单或账号配置。升级不自动添加补全项，不修改 Sub2API 源码、镜像或表结构。

兼容入口仅处理带 `client_version` 的 GET `/v1/models`、`/models` 及 `/backend-api/codex/models`。每次请求先交给原 Sub2API 校验实际调用者，之后应用当前分组覆盖；异常返回原目录。nginx 配置见 `deploy/nginx/codex-models.location.conf`，只开放 API 的 IP 入口使用 `codex-models-raw-ip.location.conf` 保留普通 `/models` 的关闭状态。原模型调用和普通列表不经过兼容层。

## 升级迁移

从 0.1.6 升级前，使用旧服务环境运行 `PYTHONPATH=/workspace python /workspace/scripts/migrate_desktop_only.py --env-file /workspace/.env`。脚本迁移有效 Sub2API 连接地址并验证权限，删除退休的浏览器配置、会话和环境变量，精确移除旧自定义菜单入口。先验证迁移成功，再启动新版；OAuth 和 Bark 配置保持原值。nginx 的模型目录接管配置需单独安装，回滚时先撤回目录接管。

夜间恢复冷却已退役；即时恢复全天运行。旧夜间配置字段在升级时移除，遗留延后意图重新进入严格恢复检查。每日测活使用独立批次记录，按北京时间日期去重，中断批次不补测；人工暂停或冷却、额度不足和不可调度的账号跳过。

首次启动新版时会把 `/data/usage-query-state.json` 收缩为管理员 API Key、OAuth 快照和调度元数据。同时幂等删除历史的一分钟自动恢复计划并清理旧状态文件。数据库清理失败时不写完成标记，下次启动会继续重试。审计历史不会被删除。

## 安全

Companion 具有账号调度操作权限，不应独立暴露在公网。仅在 `127.0.0.1` 或 Docker 内网监听，桌面接口经 TLS 反代并验证管理员权限。管理员 API Key 只保存在 `USAGE_QUERY_STATE_PATH`，Bark Device Key 只保存在 `BARK_CONFIG_PATH`；两个文件权限均为 `0600`，密钥不写入 URL、页面、日志或审计明文。
