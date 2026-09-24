Sub2Ops 0.1.1 修复菜单栏左键显示与失焦竞态，Dock 不再显示图标。快捷面板重新设计为分组与异常两页，支持固定、关闭与错误详情。

每组显示最近调用的三个不同账号；账号表的“用量窗口”对齐 Sub2API，显示 OpenAI 5h/7d 与 Grok 请求数/Token 上游快照。所有时间使用准确北京时间。精简解释性文案，移除独立的后台定时额度刷新；保留额度查询、每日测活及到期恢复所需流程。

使用已有 Sub2API 管理员 API Key，保存至 macOS Keychain。云端需部署本次配套 Companion 接口；现有网页、SSO 和自动化继续保留。

系统要求：macOS 12+，Apple Silicon。首版使用本机签名，未经 Apple Developer ID 签名或公证。检查更新会打开发布页，不自动安装。

安装前可使用 `shasum -a 256 -c SHA256SUMS` 验证下载文件。
