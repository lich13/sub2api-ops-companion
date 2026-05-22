# Sub2API Ops Companion

旁路运维面板，用来补足 Sub2API 原面板里账号质量归因、`upstream_errors` 链路展示和账号调度禁用操作不够直观的问题。

设计边界：

- 不修改 Sub2API 原仓库和镜像。
- 只连接 Sub2API 的 PostgreSQL，读取 `usage_logs`、`ops_error_logs`、`accounts`、`groups` 等运行数据。
- 账号操作只更新现有运行态/调度字段：`accounts.status`、`accounts.schedulable`、`accounts.temp_unschedulable_*`、`accounts.rate_limited_at`、`accounts.rate_limit_reset_at`、`accounts.overload_until`、`accounts.error_message`、`accounts.updated_at`、`accounts.priority`，以及上游数据库存在时的 `accounts.load_factor`。
- 不提供独立账号密码登录页，只接受 Sub2API 管理后台 SSO 首跳换取本服务 Cookie 会话。
- 操作审计写入本服务自己的 `/data/audit.jsonl`，不污染 Sub2API 源码。

## 功能

- 账号稳定性：按账号展开成功量、账号质量错误、错误率、`403 blocked`、余额/额度、限流、5xx/流式截断，并支持按错误率排序。
- 账号速度：按账号展示首 Token（秒）、平均耗时（秒）、tokens/秒、时间范围内消耗，以及可选的实时剩余额度快照。
- 额度查询：每个账号可配置 Sub2API、NewAPI 或自定义 `request/extractor` 模板，保存 API Key/Access Token、上游倍率和自动查询间隔；速度列表会展示原始剩余额度和按 `剩余额度 / 上游倍率` 还原的实际可用量。
- 错误链路展开：把 `ops_error_logs.upstream_errors` 展开，显示同一次请求的 failover 账号链路。
- 请求定位：按 `request_id` 或 `client_request_id` 查询完整详情。
- 调度操作：一键暂停账号调度、临时冷却账号、恢复账号调度。
- 自动 Guard：读取全部账号，按 `ops_error_logs.id` 增量处理错误链路，余额/额度/403 硬停、429 短冷却、5xx/流式冷却三类动作可独立开关；429/5xx/流式中断先把 `accounts.load_factor` 降到 1 软降载，再按 1m/3m/5m 短冷却，并清掉上游 rate-limit/overload 运行态；追加的余额/额度扫尾只处理最近 24 小时内仍有异常状态的账号，避免很久以前的过期报错触发补处理；后台自动 Guard 不处理 `type=oauth` 账号。
- Guard P1/P2 队列：Guard 面板可以按分组切换队列视图，直接把账号设为 P1/P2/备用/降载观察；自动调整会按成功样本把健康账号排到 P1/P2，并把异常、冷却或已停账号降到低优先级和 `load_factor=1`（上游字段存在时）；自动调整会跳过 `type=oauth` 账号。
- 定时恢复：在面板里直接配置 Sub2API 原生定时测试计划，支持每小时、每30分钟、每15分钟、每5分钟整点对齐检测；测试通过后由 companion 自动清理可恢复异常状态。
- Telegram 远程运维：保存 Bot Token 后生成随机配对码，私聊 `/pair 配对码` 才能绑定；新错误链路会实时推送，并直接附带账号暂停/恢复/冷却按钮。
- Sub2API 免二次登录：Sub2API 自定义菜单 iframe 可进入 `/sub2ops/sso/start`，Companion 调 Sub2API `/api/v1/auth/me` 验证管理员 JWT 后换成本服务的不可伪造会话。
- 面板版本更新：左上角显示当前版本，支持检查 GitHub main 分支并从面板拉取更新后自重启。

## 运行

复制 `.env.example` 为 `.env` 并设置强密码：

```bash
cp .env.example .env
```

启动：

```bash
docker compose up -d --build
```

默认监听 `127.0.0.1:18081`。生产环境建议通过 nginx 挂到 Sub2API 同域的 `/sub2ops/`，并关闭该路径的 query access log，避免首跳 token 写入日志。nginx 片段见 `deploy/nginx/sub2ops.location.conf`。

## 环境变量

- `DATABASE_URL`：PostgreSQL 连接串。
- `OPS_SESSION_SECRET`：Cookie 会话签名密钥。
- `OPS_SESSION_TTL_SECONDS`：兼容旧会话校验的最大有效期，默认 1 年（`31536000` 秒）。
- `OPS_SESSION_STORE_PATH`：服务端会话存储文件，默认 `/data/sessions.json`。
- `BASE_PATH`：反代路径前缀，例如 `/sub2ops`。
- `APP_PORT`：容器内监听端口，默认 `18081`。
- `GUARD_ENABLED`：是否启动后台 Guard，默认 `true`。
- `GUARD_INTERVAL_SECONDS`：Guard 扫描间隔，默认 `5` 秒；服务启动后会立即先扫一次，之后按该间隔轮询。
- `GUARD_BALANCE_ERROR_THRESHOLD`：触发自动处理的余额/额度错误次数，默认 `1`。
- `GUARD_BALANCE_ERROR_MAX_AGE_HOURS`：余额/额度追加扫尾只处理最近多少小时内的最后报错，默认 `24`，范围 `1-720`。
- `GUARD_STATE_PATH`：Guard cursor、circuit 和策略状态文件，默认 `/data/guard-state.json`。
- `GUARD_EVENT_BATCH_SIZE`：每轮 Guard 最多处理的错误/成功事件数，默认 `100`。
- `USAGE_QUERY_STATE_PATH`：账号额度查询配置、密钥和最新快照文件，默认 `/data/usage-query-state.json`；文件会按 `0600` 写入。
- `TELEGRAM_CONFIG_PATH`：Telegram 面板配置持久化文件，默认 `/data/telegram-config.json`。
- `TELEGRAM_BOT_TOKEN`：可选初始值。面板保存后以 `TELEGRAM_CONFIG_PATH` 文件为准。
- `TELEGRAM_STATE_PATH`：配对状态持久化文件，默认 `/data/telegram-state.json`。
- `OPS_UPDATE_ENABLED`：是否允许从面板执行更新，默认 `true`。
- `OPS_UPDATE_WORKDIR`：容器内 Git 工作树路径，默认 `/workspace`。
- `OPS_UPDATE_BRANCH`：更新跟踪分支，默认 `main`。
- `OPS_SSO_CONFIG_PATH`：Sub2API 免二次登录运行时配置，默认 `/data/sso-config.json`。可在 `/sub2ops/sso` 面板保存，不需要重启。
- `SUB2API_BASE_URL`：Sub2API 菜单公网根地址，例如 `https://sub2api.example.com`。
- `SUB2API_VERIFY_BASE_URL`：可选的 Ops 服务端校验根地址；Docker 同网部署时可填 `http://sub2api:8080`，避免服务端绕公网/Cloudflare 回源。
- `SUB2API_SSO_ENABLED`：是否允许 Sub2API 自定义菜单 token 换取 Companion 会话；SSO-only 部署应设为 `true`。
- `SUB2API_SSO_REQUIRED_ROLE`：允许进入 Companion 的 Sub2API 用户角色，默认 `admin`；设为 `*` 可放开角色校验，不建议。
- `SUB2API_SSO_SESSION_TTL_SECONDS`：SSO 换出的 Companion 会话有效期，默认 1 天，范围 `300` 到 `604800` 秒。
- `SUB2API_SSO_VERIFY_TIMEOUT_SECONDS`：Companion 调 Sub2API 验证 token 的超时，默认 `5` 秒。

## Sub2API 免二次登录

这个模式不改 Sub2API 源码，只依赖 Sub2API 现有自定义菜单 iframe 会自动追加 `token`、`user_id`、`ui_mode=embedded` 等参数。

1. 首次部署先通过 `.env` 配好 `SUB2API_BASE_URL` 和 `SUB2API_SSO_ENABLED=true`，或直接写入 `OPS_SSO_CONFIG_PATH` 指向的配置文件。成功从 Sub2API 菜单进入后，可以继续在 `/sub2ops/sso` 的“Sub2API 免二次登录”区块里调整配置，例如：

```text
https://你的-sub2api-域名
```

也可以用 `.env` 兜底配置：

```bash
SUB2API_BASE_URL=https://你的-sub2api-域名
# 可选：Ops 和 Sub2API 在同一个 Docker 网络时，服务端验 token 可走内网。
SUB2API_VERIFY_BASE_URL=http://sub2api:8080
SUB2API_SSO_ENABLED=true
SUB2API_SSO_REQUIRED_ROLE=admin
```

2. 在 Sub2API 管理后台的自定义菜单里新增管理端菜单，URL 填面板生成的地址：

```text
https://你的-sub2api-域名/sub2ops/sso/start
```

3. 管理员从 Sub2API 菜单进入后，Companion 会用传入的 JWT 调 `SUB2API_VERIFY_BASE_URL/api/v1/auth/me` 验证身份；未配置 `SUB2API_VERIFY_BASE_URL` 时回退到 `SUB2API_BASE_URL`。验证成功后立即写入自己的强随机会话 Cookie，并 303 跳转到干净的 `/sub2ops/`。Cookie 只包含随机会话 ID 和 HMAC，真实用户信息保存在服务端 `/data`，浏览器端没有可读明文。

4. 因为 Sub2API 现有 iframe 机制会把 JWT 放在首次 GET 的 query string 里，生产环境应使用 HTTPS，并避免 nginx 记录 `/sub2ops/` 的 query 日志。仓库里的 nginx 示例已经对该路径关闭 access log。

## 面板版本更新

当前版本定义为 `0.1.0`。服务通过左上角版本徽标展示版本状态：

- `已是最新版本`：容器内 Git 工作树和 GitHub `main` 分支一致。
- `发现可更新版本`：GitHub `main` 分支有新提交，可以点击“立即更新”。
- 更新动作会在容器内执行 `git fetch` 和 `git reset --hard origin/main`，然后退出当前进程；Docker 的 `restart: unless-stopped` 会拉起新版代码。

生产部署需要满足两点：

- `/srv/sub2api-ops-companion` 是 Git clone，而不是 rsync 出来的普通目录。
- `docker-compose.yml` 已把项目目录挂载到容器内 `/workspace`。
- 仓库保持公开，容器通过 HTTPS origin 直接读取 GitHub，无需 SSH deploy key。

## Telegram 远程控制

进入 `/sub2ops/telegram` 后保存 Bot Token。保存后面板会生成随机配对码，Bot 会热重启，不需要手动改 `.env` 或重启容器。

配置完成后，在 Telegram 私聊里给 Bot 发送面板显示的配对命令：

```text
/pair ABCD-EFGH
```

不会再首次自动绑定陌生会话；重新生成配对码后旧码立即失效，已绑定会话继续可用。Telegram 侧不再提供账号命令菜单，但已配对会话可发送 `/quota`、`/usage` 或 `额度`，立即查询所有已启用额度查询的账号，并逐行返回可用额度和总额度。后台会按 `ops_error_logs.id` 做增量扫描，默认每 2 秒检查一次新错误链路，每批最多处理 50 条错误日志；首次启动只记录当前最大 id，避免历史错误刷屏。之后每条带账号的错误链路会推送到当前绑定的 Telegram 会话，并在消息下方附加“暂停”“冷却 5m”“冷却 15m”“冷却 30m”“恢复”“查看详情”等账号操作按钮。

如果 Sub2API 定时测试计划开启了 `auto_recover`，companion 会按 `scheduled_test_results.id` 增量读取成功结果；只要账号当时仍有停调度、rate-limit、overload、临时不可调度或错误状态，就会清理这些运行态并推送“账号已自动恢复”通知，消息下方附带同样的账号操作按钮。

## 账号额度查询

进入 `/sub2ops/speed` 后，在“额度”列展开单个账号的“配置额度查询”：

- `Sub2API` 模板默认请求 `{{baseUrl}}/v1/usage`，使用 `Authorization: Bearer {{apiKey}}`，兼容 `remaining`、`quota.remaining` 和 `balance` 字段；若接口没有直接返回 `total`，会尝试用 `remaining + usage.total.actual_cost/cost` 推导总额。
- `NewAPI` 模板默认请求 `{{baseUrl}}/api/user/self`，使用 `Authorization: Bearer {{accessToken}}` 和 `New-Api-User: {{userId}}`，把 `quota`、`used_quota` 除以 `500000` 后展示为 USD。
- `自定义` 模板使用和 cc-switch 一致的 `({ request, extractor })` 形态：JS 只声明请求和提取器，HTTP 请求由 Companion 后端统一执行，返回对象或对象数组字段可包含 `planName`、`extra`、`isValid`、`invalidMessage`、`total`、`used`、`remaining`、`unit`。
- 勾选“读取账号 Base URL / API Key”后，查询时会直接读取 Sub2API `accounts.credentials.base_url` 和 `accounts.credentials.api_key`；表单里手动填写的 Base URL/API Key 优先级更高。
- `上游倍率` 用于还原实际可用量，展示值为 `remaining / upstream_multiplier`；例如倍率 `0.5`、剩余额度 `12` 时，实际可用量显示为 `24`。
- 开启“可用量≤0 时 Auto Guard 硬停”后，后台 Guard 会按账号配置的自动查询间隔刷新额度；只有查询成功且实际可用量小于等于 `0`，才会把非 OAuth 账号硬停调度。查询失败只保存失败快照，不会当作额度耗尽处理。

密钥只保存在 `USAGE_QUERY_STATE_PATH` 指向的本服务 JSON 文件或上游账号 `credentials` 中，不会写入审计明文，也不会渲染回页面；表单留空会保留已保存密钥。自定义模板允许管理员填写完整 `http/https` 请求 URL，因此只应在可信管理员环境中使用。若接口没有返回总额度且无法从已用量推导，面板和 Telegram 会把总额显示为 `-`。

## 定时恢复

进入 `/sub2ops/scheduled-tests` 可以给账号创建或更新上游 `scheduled_test_plans`：

- 频率固定为每小时、每30分钟、每15分钟、每5分钟，对应 `0 * * * *`、`*/30 * * * *`、`*/15 * * * *`、`*/5 * * * *`，都以整点分钟栅格对齐。
- 模型可以留空；留空时由 Sub2API 使用对应平台的默认测试模型。
- 开启“测试通过后自动恢复”会写入 `auto_recover=true`。实际测试由 Sub2API 的 scheduled test runner 执行；成功结果由 companion 接管恢复写回。
- 自动恢复能清理上游可恢复运行态：`status=error`、rate-limit、overload、临时不可调度、错误状态等；如果账号确实恢复可用，会重新设为可调度并推送 Telegram。

## 质量统计口径

账号质量错误会展开 `ops_error_logs.upstream_errors` 后按实际 `account_id` 归因。
`account_id` 为空、`none`、`null` 或客户端请求错误不会进入账号质量表，也不会在面板展示或统计。

计入账号质量问题：

- `403` 且消息包含 `blocked`。
- 余额或额度类错误，例如 `insufficient_user_quota`、`pre_consume_token_quota_failed`、`token quota is not enough`、`用户额度不足`、`额度已用尽`、`RemainQuota = -...`、`预扣费额度失败`、`剩余额度`、`insufficient balance`。
- `429`、`rate limit`、`Too many pending requests`。
- `500` 到 `599`。
- 流式截断或终止事件缺失，例如 `stream ended before a terminal event`。

不计入账号质量问题：

- `account_id IS NULL` 的预路由错误。
- `error_owner=client` 或 `error_source=client_request`。
- 常见客户端坏请求，例如 `Input must be a list`、`Instructions are required`。

## 安全说明

这个服务具备暂停和恢复账号调度的能力，不应该作为独立站点裸露在公网。当前实现是 SSO-only：未带合法 Sub2API token 的直接访问会返回 `403`，不会展示账号密码登录表单。

更稳妥的部署方式是让 Companion 继续只监听 `127.0.0.1:18081` 或 Docker 内网，由 Sub2API 同域 nginx 暴露 `/sub2ops/`。响应会附加 `frame-ancestors 'self' <Sub2API 菜单域名>`，使 Companion 只面向 Sub2API 管理后台 iframe 接入。

## 调度问题排查口径

如果 OpenAI 同优先级账号没有轮询，先检查 `settings.openai_advanced_scheduler_enabled`。该值为 `false` 时，Sub2API 会使用默认 OpenAI 账号选择路径，实际行为可能明显偏向单个账号。

高级调度开启后仍需要保证最高优先级下至少有两个健康可调度账号。如果最高优先级只剩一个账号，轮询无从发生；应恢复健康账号或把健康账号提升到同一优先级。

如果错误超过阈值但不切换，先看日志是否出现 `openai.upstream_failover_switching`。当前线上证据显示 `429` 和部分 `502` 会进入 failover，而大量 `500/503/504` 只记录 `openai.forward_failed` 并直接返回，不会自动把账号改成不可调度。

自动 Guard 的边界是控制面 future-request failover，不是同请求内重试。余额/额度不足类确定性错误，例如 `INSUFFICIENT_BALANCE`、`insufficient_user_quota`、`pre_consume_token_quota_failed`、`token quota is not enough`、`用户额度不足`、`额度已用尽`、`RemainQuota = -...`、`预扣费额度失败`、`剩余额度`、`not enough credits`，会把命中的活跃非 OAuth 账号永久停调度，不设置冷却时间。即使账号已被上游 rate-limit 标记成不可调度，也会被升级为明确的硬暂停；但追加扫尾只看最近 `GUARD_BALANCE_ERROR_MAX_AGE_HOURS` 小时内的最后报错，旧报错过期后不会再补处理。`403 blocked` 会硬停；`429`、`5xx`、流式截断类错误会先尝试 `load_factor=1` 软降载，再按 1m/3m/5m 短冷却。Guard 面板里的三类开关可以分别停用硬停、429 冷却、5xx/流式冷却；停用后只记录信号和游标，不会改账号调度状态。白名单账号可在 Guard 策略里通过下拉菜单勾选；白名单账号的 403、429、5xx、流式错误只记录信号，不自动暂停或冷却，Telegram 错误链路推送也会跳过这些普通报错；只有连续额度/余额错误达到白名单额度阈值（默认 10）时才会硬停。余额/额度兜底扫尾会跳过白名单账号以保留连续计数语义。速度页额度查询的“可用量≤0 时 Auto Guard 硬停”是逐账号显式开关，查询失败不会停号；启用后若查询成功且实际可用量小于等于 0，会硬停对应非 OAuth 账号。后台自动 Guard、余额/额度兜底扫尾、额度查询硬停、定时测试自动恢复和队列自动调整都会跳过 `type=oauth` 账号，避免自动改动 OAuth 账号状态。

现有自动恢复依赖 Sub2API 原生 `scheduled_test_plans` / `scheduled_test_results`：面板创建定时测试计划，Sub2API runner 到点执行，Companion 只消费 `auto_recover=true` 且结果为 `success` 的记录来清理账号异常运行态并推送 Telegram。当前不会由 Auto Guard 自动创建短期临时测试，也不会在恢复后自动删除临时计划。
