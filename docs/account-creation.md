# 添加账号

账号页工具栏的“添加账号”先填写基本信息、分组、代理及账号参数，再填写凭据或完成授权。桌面使用表单弹窗，Android 使用全屏表单。字段和原生接口基于 Sub2API 0.2.11（`96f4c115c9749078f90cbf210a01d39baf3f53b6`）的 [CreateAccountModal](https://github.com/Wei-Shaw/sub2api/blob/96f4c115c9749078f90cbf210a01d39baf3f53b6/frontend/src/components/account/CreateAccountModal.vue)。

| 平台 | 创建类型 | 授权或导入入口 |
| --- | --- | --- |
| Claude | OAuth、Setup Token、API Key、Bedrock、Vertex Service Account | 授权码、Cookie / Session Key；Bedrock 支持 Signature V4 和 API Key |
| OpenAI | OAuth、API Key | 授权码、Refresh Token、Mobile Refresh Token、Codex Session、Agent Identity、Codex PAT |
| Gemini | OAuth、API Key、Vertex Service Account | Code Assist、Google One、已配置的 AI Studio OAuth |
| Antigravity | OAuth、Upstream | 授权码、Refresh Token；Upstream 按原生规则创建为 API Key 类型 |
| Grok | OAuth、API Key | 授权码、Refresh Token、SSO Cookie |
| Kimi、智谱、DeepSeek、MiniMax、OpenCode | API Key | 平台账号模式、协议与对应 API 地址 |

Refresh Token、Cookie、Grok SSO 支持逐行批量输入，Codex Session 和 Agent Identity 支持原生批量格式和多文件导入。后两者沿用 Sub2API 的 `update_existing` 行为，匹配的已有账号会更新。授权链接由系统浏览器打开，授权后将授权码或完整回调地址粘贴回表单。

“复用配置”只提供当前连接中 `platform + type` 相同的账号。有一个候选时自动选中，多个候选可搜索。点击后读取来源详情，并填入新建支持的配置；保留新账号名称和已输入凭据。身份凭据、运行状态、历史用量及 Sub2Ops 独立配置不参与复用。字段目录位于 `server/src/account_creation_fields.json`，由服务端、表单及客户端响应校验共用。

创建直接调用原生接口，不进入持久化任务队列。全部成功后关闭表单并刷新账号；部分失败保留输入和结果，只重试明确失败项。结果未知的请求不会重复发送，应刷新列表核对。原生批量导入部分失败后，仅将失败项重新填入再提交。创建后的模型同步和上游倍率探测失败会提示对应操作，已创建账号不会再次创建。

## 接口

以下路径相对于 `/api/desktop/v1`，沿用管理员认证和写入权限。授权和导入仅使用固定上游路由，不接受任意 URL 或路径。凭据只在当前请求与表单内存中使用，不记录到操作历史、本地草稿或日志。响应均禁止缓存。

| 接口 | 用途 |
| --- | --- |
| `GET /account-creation/options` | 分组与代理选项，排除代理认证字段 |
| `GET /account-creation/catalog/:kind` | 原生能力、TLS、全局开关和默认模型映射 |
| `GET /account-creation/source/:id` | 来源账号中允许复用的新建配置 |
| `POST /account-creation` | 原生 `CreateAccountRequest` 创建与后续模型同步、倍率探测；已预览模型时使用 `?sync_models=true` 保留原生同步行为 |
| `POST /account-creation/check-mixed-channel` | Claude / Antigravity 混合渠道风险，需用户确认后继续 |
| `POST /account-creation/models` | 使用新填写凭据读取上游模型 |
| `POST /account-creation/auth/:provider/:action` | 固定平台的授权链接、代码兑换、Cookie 或 Refresh Token |
| `POST /account-creation/import/:kind` | Codex Session / Agent Identity、Codex PAT、Grok SSO |
| `GET /accounts/:id/key-quota-settings/access-token` | 用户点击“显示并复制”时读取已保存的账户访问令牌 |

余额设置中的令牌默认遮蔽。“显示并复制”优先使用当前编辑值，未编辑时才读取服务端保存值；复制失败时保留显示以便手动复制。查看、复制和隐藏均不保存设置，关闭表单清理临时明文。留空保存保留原令牌，输入新值保存后替换。普通设置接口与账号快照仍只返回是否已配置。
