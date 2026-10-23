# 账号用量

Sub2Ops 对齐 Sub2API 0.2.11 的账号用量展示。`/snapshot` 保留今日统计、配置额度及被动快照；`GET /api/desktop/v1/accounts/{id}/usage` 仅返回统一的 `UsageSummary`，不转发完整账号或凭据。账号页前台刷新时读取原生数据并复用上游缓存，查询按钮和“刷新全部用量”可主动刷新。

| 类别 | 来源 |
|---|---|
| 普通 Key、upstream、Bedrock | 今日请求、含缓存 Token、A/U 费用及配置的日、周、总额度 |
| Anthropic OAuth / Setup Token | 被动 5h、7d、Fable 快照；OAuth 主动查询同时读取原生返回的 Sonnet 窗口 |
| OpenAI / Grok | 原有订阅额度、账单及统计窗口；Key 使用今日统计和配置额度 |
| Gemini | 原生返回的共享、Pro、Flash 日与分钟配额，标明估算 |
| Antigravity | 原生模型配额、重置时间及 AI Credits |
| Kimi / Zhipu / MiniMax | coding 模式的 5h、周、月套餐窗口 |
| Kimi / DeepSeek | 按量模式的余额及原始币种，保留多币种和零余额 |
| Ollama Cloud / OpenCode Go | 原生快照、适用窗口、采集与重置时间、授权状态 |

今日统计按原站 UTC+8 日界计算，Token 包括输入、输出及缓存；A 为 `coalesce(account_stats_cost,total_cost) × account_rate_multiplier`，U 为 `actual_cost`。统计仍属于实际调用账号，原生用量请求保留账号 ID，由原站解析父子账号关系。API Key 余额额外使用下述直连查询。

窗口记录来源、采集时间、重置时间及状态。失败时保留已有数据并标异常；无数据、采集时间缺失、过期和授权失效分别展示，不生成虚假的零用量。余额也保留采集时间和状态。原生接口未提供窗口时，有今日统计的账号仍正常显示统计。

用量记录费用浮层完全使用该条记录字段，无额外接口请求。Token 单价按对应费用除以 Token 数计算，图片 Token 与文本 Token 分开，图片及按次计费使用记录计费模式。费用保留 8 位，每百万 Token 单价保留 4 位；缺失值显示破折号，零 Token 不计算单价。

## API Key 余额（0.1.57）

`UsageSummary.key_quota` 为可选余额对象，包含 `state`（idle / ok / error / unsupported）、`source`、`checked_at`、`success_at`、`retry_at`、`next_refresh_at`、`stale`、`error`、`key_status`、`plans`、`subscriptions`、`expires_at`、`expires_at_unix`、`today` 和 `total_usage`。时间戳为 Unix 秒，字符串时间为 ISO 日期。Plan 保留 `name`、`remaining`、`used`、`total`、`unit`、`unlimited` 和 `reset_at`；缺失数字为 null，不转换成零。订阅保留名称、状态、是否生效、到期及各额度窗口。

查询移植 lich13-switch 的 Sub2API / New API 协议解析。部署路径前缀保留，自动识别 `/v1/usage` 与 `/api/usage/token/`；New API 的 `/api/status` 决定单位换算，换算规则不足时显示原始额度单位。钱包、账户余额、Key 配额与本地今日费用分别展示，不合并不同币种。套餐、窗口、到期与错误均直接位于用量列，上游统计可折叠。

服务端读取账号的地址与 Key，沿用父账号凭据继承规则；额外账户访问令牌按当前账号隔离，仅用于 New API `/api/user/self`，不会作为模型 API Key 使用。余额请求直连、严格校验证书、不跟随重定向；不使用账号代理，不调用模型，不改变调度。自动与手动查询复用同一内存缓存及进行中请求，全局最多并发三个，遵守 `Retry-After`。失败保留上次成功值，地址、凭据及父账号关系改变后旧结果失效。重启后缓存为空。

新增接口前缀均为 `/api/desktop/v1`，使用现有管理员认证与只读模式约束：

| 接口 | 请求 / 响应 |
|---|---|
| `GET /config/key-quota` | `{ seconds, revision }`，默认 60 秒 |
| `PUT /config/key-quota` | `{ expected_revision, seconds }`，0 关闭，或整数 10–86400 |
| `GET /accounts/{id}/key-quota-settings` | `{ account_id, has_access_token, revision }`，不返回令牌 |
| `PUT /accounts/{id}/key-quota-settings` | `{ expected_revision, access_token? }`，省略保留，非空替换，空字符串清除 |

修订冲突返回 409，非法输入返回 422。配置由现有 Store 写入数据目录中的 `api-key-quota-config.json`，权限 0600，不改业务数据库。账号页用量工具栏提供自动刷新设置，余额行提供独立令牌入口。快捷面板在账号名下显示简要余额、未知／不可查询或异常状态，悬停查看来源与更新时间；相同账号跨分组复用进行中的查询。账号页或快捷面板的分组页前台可见时查询到期结果；隐藏、离开这些页面或关闭自动刷新后停止自动请求，手动刷新及“刷新全部用量”仍可使用。
