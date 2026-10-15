# 新模型思考档位

“功能”页的模型配置只维护需要补全的新模型。输入精确 ID 后读取真实上游，填写支持的思考档位与默认值；缺失信息不猜测。只写入 `supported_reasoning_levels` 和 `default_reasoning_level`，保存至 `GROUP_MODEL_CONFIG_PATH`（默认 `/data/group-model-config.json`，0600）。白名单阻挡时经明确确认只追加当前模型，保留其他条目和开关；冲突、失败保留草稿。

补全绑定普通或 Composite 分组的真实路由与账号映射。路由变化后停止应用；目录缺少新条目时只使用其自身的真实描述。当前已核对原版 Sub2API 0.2.9 / 0.2.10 的 Responses 转发规则：未知 Grok 型号会丢弃思考强度，部分档位会被改写；分组强度策略也可能限制请求。这些情况显示“转发受限”，未知版本或证据缺失显示“转发未核实”，均只存草稿。上游原生支持后可“恢复原生”，仅移除 Companion 补全，不撤销白名单或账号配置。升级不自动添加补全项，不修改 Sub2API 源码、镜像或表结构。

兼容入口仅处理带 `client_version` 的 GET `/v1/models`、`/models` 及 `/backend-api/codex/models`。每次请求先交给原 Sub2API 校验实际调用者，之后应用当前分组覆盖；异常返回原目录。nginx 配置见 `deploy/nginx/codex-models.location.conf`，只开放 API 的 IP 入口使用 `codex-models-raw-ip.location.conf` 保留普通 `/models` 的关闭状态。原模型调用和普通列表不经过兼容层。
