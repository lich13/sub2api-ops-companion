# Sub2API Ops Companion

旁路运维面板，用来补足 Sub2API 原面板里账号质量归因、`upstream_errors` 链路展示和账号调度禁用操作不够直观的问题。

设计边界：

- 不修改 Sub2API 原仓库和镜像。
- 只连接 Sub2API 的 PostgreSQL，读取 `usage_logs`、`ops_error_logs`、`accounts`、`groups` 等运行数据。
- 账号操作只更新 `accounts.schedulable`、`accounts.temp_unschedulable_*` 和 `accounts.updated_at`。
- 登录使用页面内强随机服务端 Cookie 会话，不触发浏览器 Basic Auth 弹窗。
- 操作审计写入本服务自己的 `/data/audit.jsonl`，不污染 Sub2API 源码。

## 功能

- 账号稳定性：按账号展开成功量、账号质量错误、错误率、`403 blocked`、余额/额度、限流、5xx/流式截断，并支持按错误率排序。
- 账号速度：按账号展示首 Token（秒）、平均耗时（秒）、tokens/秒和时间范围内消耗。
- 错误链路展开：把 `ops_error_logs.upstream_errors` 展开，显示同一次请求的 failover 账号链路。
- 请求定位：按 `request_id` 或 `client_request_id` 查询完整详情。
- 调度操作：一键暂停账号调度、临时冷却账号、恢复账号调度。
- 自动 Guard：后台定时扫描余额/额度不足错误，并永久暂停确定性坏账号；如果账号已被 Sub2API 临时冷却，也会升级为永久停调度，直到手动恢复。
- 定时恢复：在面板里直接配置 Sub2API 原生定时测试计划，支持每小时、每30分钟、每15分钟、每5分钟整点对齐检测；测试通过后自动清理可恢复异常状态。
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

默认监听 `127.0.0.1:18081`。生产环境建议通过 nginx 挂到 `/sub2ops/`，并保留 Basic Auth。nginx 片段见 `deploy/nginx/sub2ops.location.conf`。

## 环境变量

- `DATABASE_URL`：PostgreSQL 连接串。
- `OPS_BASIC_USER`：Basic Auth 用户名。
- `OPS_BASIC_PASSWORD`：页面登录密码，必须设置。
- `OPS_SESSION_SECRET`：Cookie 会话签名密钥。
- `OPS_SESSION_TTL_SECONDS`：页面登录 Cookie 会话有效期，默认 1 年（`31536000` 秒）。
- `OPS_SESSION_STORE_PATH`：服务端会话存储文件，默认 `/data/sessions.json`。
- `BASE_PATH`：反代路径前缀，例如 `/sub2ops`。
- `APP_PORT`：容器内监听端口，默认 `18081`。
- `GUARD_ENABLED`：是否启动后台 Guard，默认 `true`。
- `GUARD_INTERVAL_SECONDS`：Guard 扫描间隔，默认 `5` 秒；服务启动后会立即先扫一次，之后按该间隔轮询。
- `GUARD_LOOKBACK_MINUTES`：余额/额度错误扫描窗口，默认 `60`。
- `GUARD_BALANCE_ERROR_THRESHOLD`：触发自动处理的余额/额度错误次数，默认 `1`。
- `TELEGRAM_CONFIG_PATH`：Telegram 面板配置持久化文件，默认 `/data/telegram-config.json`。
- `TELEGRAM_BOT_TOKEN`：可选初始值。面板保存后以 `TELEGRAM_CONFIG_PATH` 文件为准。
- `TELEGRAM_STATE_PATH`：配对状态持久化文件，默认 `/data/telegram-state.json`。
- `OPS_UPDATE_ENABLED`：是否允许从面板执行更新，默认 `true`。
- `OPS_UPDATE_WORKDIR`：容器内 Git 工作树路径，默认 `/workspace`。
- `OPS_UPDATE_BRANCH`：更新跟踪分支，默认 `main`。
- `OPS_TURNSTILE_CONFIG_PATH`：Ops 登录防护配置文件，默认 `/data/turnstile-config.json`。通过 `/sub2ops/turnstile` 面板保存后立即作用于 Ops 自身登录。
- `OPS_TURNSTILE_VERIFY_TIMEOUT_SECONDS`：Cloudflare Turnstile 校验超时，默认 `5` 秒。
- `OPS_SSO_CONFIG_PATH`：Sub2API 免二次登录运行时配置，默认 `/data/sso-config.json`。可在 `/sub2ops/turnstile` 面板保存，不需要重启。
- `SUB2API_BASE_URL`：Sub2API 站点根地址，例如 `https://sub2api.example.com`。
- `SUB2API_SSO_ENABLED`：是否允许 Sub2API 自定义菜单 token 换取 Companion 会话，默认 `false`。
- `SUB2API_SSO_REQUIRED_ROLE`：允许进入 Companion 的 Sub2API 用户角色，默认 `admin`；设为 `*` 可放开角色校验，不建议。
- `SUB2API_SSO_SESSION_TTL_SECONDS`：SSO 换出的 Companion 会话有效期，默认 1 天，范围 `300` 到 `604800` 秒。
- `SUB2API_SSO_VERIFY_TIMEOUT_SECONDS`：Companion 调 Sub2API 验证 token 的超时，默认 `5` 秒。

## Sub2API 免二次登录

这个模式不改 Sub2API 源码，只依赖 Sub2API 现有自定义菜单 iframe 会自动追加 `token`、`user_id`、`ui_mode=embedded` 等参数。

1. 进入 `/sub2ops/turnstile` 的“Sub2API 免二次登录”区块，开启后填写 Sub2API 站点地址，例如：

```text
https://你的-sub2api-域名
```

也可以用 `.env` 兜底配置：

```bash
SUB2API_BASE_URL=https://你的-sub2api-域名
SUB2API_SSO_ENABLED=true
SUB2API_SSO_REQUIRED_ROLE=admin
```

2. 在 Sub2API 管理后台的自定义菜单里新增管理端菜单，URL 填面板生成的地址：

```text
https://你的-sub2api-域名/sub2ops/sso/start
```

3. 管理员从 Sub2API 菜单进入后，Companion 会用传入的 JWT 调 `SUB2API_BASE_URL/api/v1/auth/me` 验证身份。验证成功后立即写入自己的强随机会话 Cookie，并 303 跳转到干净的 `/sub2ops/`。Cookie 只包含随机会话 ID 和 HMAC，真实用户信息保存在服务端 `/data`，浏览器端没有可读明文。

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

不会再首次自动绑定陌生会话；重新生成配对码后旧码立即失效，已绑定会话继续可用。Telegram 侧不再提供账号命令菜单。后台会按 `ops_error_logs.id` 做增量扫描，默认每 2 秒检查一次新错误链路，每批最多处理 50 条错误日志；首次启动只记录当前最大 id，避免历史错误刷屏。之后每条带账号的错误链路会推送到当前绑定的 Telegram 会话，并在消息下方附加“暂停”“冷却 5m”“冷却 15m”“冷却 30m”“恢复”“查看详情”等账号操作按钮。

如果 Sub2API 定时测试计划开启了 `auto_recover`，测试成功并实际清理账号运行态后，companion 也会按 `scheduled_test_results.id` 增量推送“账号已自动恢复”通知，并附带同样的账号操作按钮。

## 定时恢复

进入 `/sub2ops/scheduled-tests` 可以给账号创建或更新上游 `scheduled_test_plans`：

- 频率固定为每小时、每30分钟、每15分钟、每5分钟，对应 `0 * * * *`、`*/30 * * * *`、`*/15 * * * *`、`*/5 * * * *`，都以整点分钟栅格对齐。
- 模型可以留空；留空时由 Sub2API 使用对应平台的默认测试模型。
- 开启“测试通过后自动恢复”会写入 `auto_recover=true`。实际测试和恢复仍由 Sub2API 自己的 scheduled test runner 执行。
- 自动恢复能清理上游可恢复运行态：`status=error`、rate-limit、overload、临时不可调度、模型级限流等。手动永久停调度仍需要人工恢复。

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

这个服务具备暂停和恢复账号调度的能力，不应该裸露在公网。至少启用 Basic Auth；更稳妥的方式是只监听本机，通过 nginx 内部路径访问。

启用 Sub2API SSO 后，Companion 的独立登录仍可作为回退入口；如果你希望只能从 Sub2API 进入，可以在公网侧只暴露 `/sub2ops/sso/start` 和认证后的 `/sub2ops/`，并把容器端口继续限制在 `127.0.0.1:18081` 或 Docker 内网。

## 调度问题排查口径

如果 OpenAI 同优先级账号没有轮询，先检查 `settings.openai_advanced_scheduler_enabled`。该值为 `false` 时，Sub2API 会使用默认 OpenAI 账号选择路径，实际行为可能明显偏向单个账号。

高级调度开启后仍需要保证最高优先级下至少有两个健康可调度账号。如果最高优先级只剩一个账号，轮询无从发生；应恢复健康账号或把健康账号提升到同一优先级。

如果错误超过阈值但不切换，先看日志是否出现 `openai.upstream_failover_switching`。当前线上证据显示 `429` 和部分 `502` 会进入 failover，而大量 `500/503/504` 只记录 `openai.forward_failed` 并直接返回，不会自动把账号改成不可调度。

自动 Guard 的边界是余额/额度不足类确定性错误，例如 `INSUFFICIENT_BALANCE`、`insufficient_user_quota`、`pre_consume_token_quota_failed`、`token quota is not enough`、`用户额度不足`、`额度已用尽`、`RemainQuota = -...`、`预扣费额度失败`、`剩余额度`、`not enough credits`。它会把可调度或临时冷却中的账号永久停调度，不设置冷却时间；不自动处理 403 blocked、5xx 或限流。
