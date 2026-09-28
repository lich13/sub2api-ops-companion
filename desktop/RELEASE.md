Sub2Ops 0.1.7 增加分组模型配置，并将管理入口统一为桌面 App。

- 登录开机后静默驻留菜单栏；手动打开主窗口，保留原开机启动选择和 Dock 行为。
- 新增“模型”页：分组白名单、模型信息、JSON 编辑、最终预览、只读上游导入及 models.dev 搜索。保存使用版本校验，冲突保留草稿。
- Companion 在原 Sub2API 鉴权后应用目录元数据，故障回退原目录；不修改 Sub2API 源码、镜像、表结构、账号映射或计费。
- 网页面板、SSO、浏览器会话及热更新入口已移除。需同步部署 Companion 0.1.7 并迁移连接配置。
- 保留 Keychain、账号管理、自动恢复、每日测活、Bark、Key 回退与质量评分。

系统要求：macOS 12+，Apple Silicon。本版为本机签名，未经 Apple Developer ID 签名或公证。下载 DMG 与 SHA256SUMS，运行 shasum -a 256 -c SHA256SUMS 校验。后续发布不再提供 ZIP，历史发布资产不变。
