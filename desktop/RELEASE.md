Sub2Ops 0.1.9 增加 Android ARM64 客户端，并修复 Mac 账号表右侧裁切。

- Mac 主窗口默认 1440×860，限制在屏幕可用区并居中；完整显示九列，保留手动调整、菜单栏与静默启动。
- Android 提供账号、模型、事件、自动化、设置五页，包含批量删除、优先级、质量、连接测试、额度操作与分组动态。
- 安卓 Key 使用 Android Keystore 加密保存，禁止系统备份，断开连接删除；Mac Keychain 保留。
- 安卓前台每两秒合并刷新，后台暂停读取与测试流，返回前台读取实际状态；不重放已发出的写操作。
- 复用现有 Companion 接口，本版无需更新云端，不改变账号配置或调度状态。

系统要求：macOS 12+ / Apple Silicon，或 Android 12+ / ARM64。Mac 为本机签名，未经 Apple Developer ID 公证；APK 使用固定发布签名，支持后续覆盖升级及 16KB 内存页。下载对应 DMG 或 APK，使用 SHA256SUMS 校验；不提供 ZIP。
