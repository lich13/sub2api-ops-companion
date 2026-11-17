# Sub2Ops 0.1.94

- macOS 客户端升级为纯原生 Swift 6 + SwiftUI / AppKit 界面（支持 macOS 14.0+ Apple Silicon）：原生菜单栏无抢焦点 `NSPanel` 快捷面板、主窗口 7 大分区与全部运维弹窗完整对齐，直接复用现有 Rust 业务核心、Keychain 凭据与本地偏好，移除 macOS WebView 开销；Android 客户端保持不变。
- 修复自动用卡在上游 `/reset-quota` 更新运行态 `accounts.extra` 字段后误判“账号已被其他操作修改”（`ownership_changed`）、导致扣卡成功后跳过恢复测活并使调度保持关闭的问题。
- 加固 `7d 100% 且 429` 来源识别：自动用卡要求 `ops_error_logs` 具备 7d 周窗口耗尽的正面证据（重置时间 ±300 秒对齐，排除容量过载、并发上限、RPM/TPM、5h 窗口及 `-spark` / `gpt-image` 等独立限流模型）；仅凭账号限流状态的回退只用于停止周期预约测活。
- 修复单账号连接测试在收到任务 ID 前点击“取消”仍继续在后台执行测试的竞态问题。
- 客户端需与服务端同版本：请先升级服务端再更新客户端；升级前已因历史 `ownership_changed` 保持关闭调度的账号请人工核对后手动开启。
