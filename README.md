# Sub2API Ops Companion

Sub2API 的旁路 OAuth 运维服务，提供 OAuth 额度监控、Bark 事件推送、桌面账号管理和 Sub2API SSO 接入。

macOS Apple Silicon 客户端 **Sub2Ops** 提供菜单栏快捷面板、分组最近成功调用、账号调度、错误详情及 Ops 设置。下载与使用见 [客户端说明](desktop/README.md)。现有网页和 SSO 入口继续保留。

## 功能

- OAuth 额度监控：统一调度 active usage，支持准确恢复时间到点查询、7d 提前重置探测和恢复后测活。
- Bark：推送 OAuth 恢复、测活失败、自动恢复失败和 401/402 认证异常。
- OAuth 额度查询：客户端支持单账号查询和全部 OAuth 刷新；Grok 只刷新官方账单，不发送模型请求。
- Key 调度回退：OpenAI、Grok 分平台控制选定的 apikey。某平台全部 OAuth 账号不可用时开启该平台的 Key，存在可用 OAuth 时关闭；没有 OAuth 或无法判断时保持原状态。Grok 根据 Sub2API 的调度、冷却、限流、到期和重新认证状态判断，不额外查询额度。
- Sub2API SSO：从 Sub2API 自定义菜单进入，验证管理员 JWT 后换取 Companion 本地会话。
- 面板更新：显示当前版本，可检查 `origin/main` 并在源码无依赖变更时热更新。

## OAuth 监控机制

- 普通后台额度刷新已移除；客户端自动更新只读已有快照与使用日志。
- 7d 已耗尽且恢复时间未到时，默认每 3600 秒探测一次提前重置。
- 当前已耗尽的必要窗口有准确 `reset_at` 时，以最晚恢复时间为准，到点立即查询。
- `free` 只要求 7d 窗口；其他套餐同时要求 5h 和 7d。7d 无余量时不测活。
- 即时恢复全天运行；额外每日测活默认北京时间 `05:00`，仅异常推送 Bark，错过不补跑。
- 额度确认恢复后调用 Sub2API account test，默认模型为 `gpt-5.6-luna`。
- active usage 同一账号不会并发重复请求；一轮结果集中写入状态文件。
- 测活结果先持久化为待推送事件。Bark 完整发送失败只重试发送，不重复测活；Bark 关闭时事件按 suppressed 语义确认。

客户端自动刷新只读取已有快照；用户点击额度查询才主动刷新。未知额度不会显示为 0%。

## 运行

```bash
cp .env.example .env
docker compose up -d --build
```

默认监听 `127.0.0.1:18081`。生产环境建议通过 nginx 挂载到 Sub2API 同域的 `/sub2ops/`，并关闭该路径的 query access log。nginx 示例见 `deploy/nginx/sub2ops.location.conf`。

## 环境变量

- `DATABASE_URL`：Sub2API PostgreSQL 连接串。
- `OPS_SESSION_SECRET`：Companion 会话密钥。
- `OPS_SESSION_TTL_SECONDS`：会话最大有效期。
- `OPS_SESSION_STORE_PATH`：会话状态文件。
- `BASE_PATH`：反代路径前缀，默认 `/sub2ops`。
- `USAGE_QUERY_STATE_PATH`：OAuth 快照、管理员 API Key 和调度元数据，默认 `/data/usage-query-state.json`。
- `OAUTH_CONFIG_PATH`：OAuth 自动化配置，默认 `/data/oauth-config.json`，权限 `0600`；JSON 优先于环境变量，环境变量优先于默认值。
- `BARK_CONFIG_PATH`：Bark 面板配置文件，默认 `/data/bark-config.json`。
- `BARK_ENABLED`：是否启用 OAuth 事件的 Bark 推送，默认关闭。
- `BARK_DEVICE_KEY`：Bark Device Key；生产环境建议通过面板写入权限为 `0600` 的配置文件。
- `BARK_SERVER_URL`：Bark 服务根 URL，默认 `https://api.day.app`；HTTP 只允许 loopback。面板不再展示或提交该字段，缺少表单值时保留当前运行时 URL。
- `KEY_FALLBACK_CONFIG_PATH`：Key 调度回退配置文件，默认 `/data/key-fallback-config.json`，权限 `0600`。
- `OAUTH_RECOVERY_MONITOR_ENABLED`：是否监控恢复和 7d 提前重置。
- `OAUTH_DAILY_TEST_ENABLED`：是否启用每日 OpenAI OAuth 测活，默认开启；仅异常通过 Bark 推送。
- `OAUTH_DAILY_TEST_TIME`：每日测活时间（北京时间 `HH:MM`），默认 `05:00`。修改时间、启用或重启后均等待下一个未来时间点，错过不补跑。
- `OAUTH_USAGE_REFRESH_CONCURRENCY`：active usage 并发，默认 `4`。
- `OAUTH_RECOVERY_TEST_CONCURRENCY`：account test 并发，默认 `2`。
- `OAUTH_EARLY_PROBE_BATCH_SIZE`：每轮最多处理的 OAuth 账号数，默认 `8`。
- `OAUTH_7D_PROBE_INTERVAL_SECONDS`：7d 提前重置探测间隔，默认 `3600`。
- `OAUTH_RECOVERY_TEST_MODEL_ID`：恢复测活模型，默认 `gpt-5.6-luna`。
- `OPS_SSO_CONFIG_PATH`：Sub2API SSO 运行时配置文件。
- `SUB2API_BASE_URL`：Sub2API 公网根地址。
- `SUB2API_VERIFY_BASE_URL`：可选的服务端内网校验根地址。
- `SUB2API_SSO_ENABLED`：是否允许 SSO 换取 Companion 会话。
- `OPS_UPDATE_ENABLED`、`OPS_UPDATE_WORKDIR`、`OPS_UPDATE_BRANCH`：面板更新配置。

## Sub2API SSO

在 Sub2API 自定义菜单中配置：

```text
https://你的-sub2api-域名/sub2ops/sso/start
```

Companion 使用首跳参数中的 JWT 请求 `SUB2API_VERIFY_BASE_URL/api/v1/auth/me`，验证成功后写入本地会话 Cookie 并跳转到 `/sub2ops/ops`。生产环境必须使用 HTTPS，并避免记录首跳 query string。

## 升级迁移

从 0.1.3 升级前停止旧服务，使用旧服务环境运行 `PYTHONPATH=/workspace python /workspace/scripts/migrate_oauth_config.py --env-file /workspace/.env`。脚本迁移有效 OAuth 设置并验证，之后删除退休的 Bot 配置、配对状态和环境变量，不复制 Bot 密钥。迁移失败不得启动新版；运行时不读取旧设置。网页入口为 `/ops`，OAuth 保存入口为 `/oauth/settings`。

夜间恢复冷却已退役；即时恢复全天运行。旧夜间配置字段在升级时移除，遗留延后意图重新进入严格恢复检查。每日测活使用独立批次记录，按北京时间日期去重，中断批次不补测；人工暂停或冷却、额度不足和不可调度的账号跳过。

首次启动新版时会把 `/data/usage-query-state.json` 收缩为管理员 API Key、OAuth 快照和调度元数据。同时幂等删除历史的一分钟自动恢复计划并清理旧状态文件。数据库清理失败时不写完成标记，下次启动会继续重试。审计历史不会被删除。

## 安全

Companion 具有账号调度操作权限，不应独立暴露在公网。建议仅在 `127.0.0.1` 或 Docker 内网监听，并通过 Sub2API 同域 iframe 进入。管理员 API Key 只保存在 `USAGE_QUERY_STATE_PATH`，Bark Device Key 只保存在 `BARK_CONFIG_PATH`；两个文件权限均为 `0600`，密钥不写入 URL、页面、日志或审计明文。
