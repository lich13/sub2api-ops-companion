# Sub2Ops 0.1.104

- 「Sub2API 配置」目录与说明同步至上游 Sub2API `0.2.25`（`34899726`）：`concurrency.ping_interval` 支持 `0`（关闭保活 ping）或 `5..30` 秒，`gateway.request_body_memory_budget_bytes` 对齐自适应内存上限 1/4（不低于 1 GiB）的默认行为，`pricing.override_file` 补充 DeepSeek 显式覆写最终计费说明，同步脚本补齐源码行号锚点自动定位与校验。
- 「添加账号」目录与创建流程同步至 Sub2API `0.2.25`：支持 API Key 账号 `upstream_billing_group_sync_enabled`（联动上游计费额度探测）与第二步手动「同步上游分组名称与备注」（`POST /api/desktop/v1/account-creation/upstream-group`）、OpenAI 透传渠道 `extra.openai_passthrough_model_filter`、自定义请求头魔法变量（`{{user_id}}`、`{{api_key_id}}`、`{{group_id}}`、`{{request_id}}`）校验及快捷插入，并更新 Claude 5.2、GPT-6.2 与 Gemini 4 模型及映射预设。
- 修复 `basis: "sample_family"` 模型测试自动处置与「校验中 / 已复核」检测日志在客户端的展示与解码，修复「Sub2API 配置」清空数值草稿误提交旧整数、切换连接残留旧更新横幅及 macOS「优先级」保存失败提示闪退的问题；统一 `sub2ops-protocol` 生成双端模型测试与检测日志结构，清理重复错误文本清洗与历史失效配置引用，并合并 Rust 依赖更新（含 `webpki-roots 1.0.12`）。
- 请先升级 Companion 与配置宿主，再更新 macOS 和 Android 客户端；沿用现有业务数据库与运行状态格式，回退 0.1.103 可直接使用保留镜像与当前数据。
