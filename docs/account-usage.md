# 账号用量

Sub2Ops 0.1.53 对齐 Sub2API 0.2.11 的账号用量展示。`/snapshot` 保留今日统计、配置额度及被动快照；`GET /api/desktop/v1/accounts/{id}/usage` 仅返回统一的 `UsageSummary`，不转发完整账号或凭据。账号页前台刷新时读取原生数据并复用上游缓存，查询按钮和“刷新全部用量”可主动刷新，不新增调度器或状态文件。

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

今日统计按原站 UTC+8 日界计算，Token 包括输入、输出及缓存；A 为 `coalesce(account_stats_cost,total_cost) × account_rate_multiplier`，U 为 `actual_cost`。统计仍属于实际调用账号，原生用量请求保留账号 ID，由原站解析父子账号关系。

窗口记录来源、采集时间、重置时间及状态。失败时保留已有数据并标异常；无数据、采集时间缺失、过期和授权失效分别展示，不生成虚假的零用量。余额也保留采集时间和状态。原生接口未提供窗口时，有今日统计的账号仍正常显示统计。

用量记录费用浮层完全使用该条记录字段，无额外接口请求。Token 单价按对应费用除以 Token 数计算，图片 Token 与文本 Token 分开，图片及按次计费使用记录计费模式。费用保留 8 位，每百万 Token 单价保留 4 位；缺失值显示破折号，零 Token 不计算单价。
