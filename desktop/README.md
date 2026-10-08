# Sub2Ops for macOS / Android

Rust / Tauri 2 / React 运维客户端，支持 macOS 12+ 的 Apple Silicon 和 Android 12+ 的 ARM64。

## 安装与连接

从本仓库 Releases 下载对应的 `.dmg` 或 `_arm64-v8a.apk`，校验 `SHA256SUMS`。Mac 将 Sub2Ops 拖入 Applications；Android 安装签名 APK。Mac 使用本机签名，未经 Apple 公证；Android 使用固定发布签名支持覆盖升级。Mac 检查更新打开对应 DMG，Android 打开本仓库发布页，不自动安装。

填写你部署的 Companion 地址（例如 `https://companion.example.com/sub2ops`）和已有的 **Sub2API 管理员 API Key**。Mac Key 存储在 Keychain（服务 `com.lich13.sub2ops`）；Android 使用 Keystore 的不可导出 AES-GCM 密钥加密后保存在应用私有、禁止备份的目录。偏好设置及前端持久化不含 Key。断开连接删除对应凭据。

- 账号：分组、平台、类型、状态筛选；用量栏对齐 Sub2API，包含窗口请求数、Token、A/U 费用。OpenAI OAuth 显示适用的 5h/7d；Grok 付费显示 7d/30d 和预付余额，免费显示 24h；API Key 显示今日统计及已配置的日/周/总配额。调度开关只改变允许调度，不清除限流、冷却或认证错误。
- 记录：北京时间日期筛选及用户实扣总消费，展示历史真实模型路由、Token、费用和延迟。用户、账户、API 密钥可搜索筛选，隐藏已删除候选但保留停用项；历史记录和消费统计不因实体删除而移除。总消费显示六位小数。
- 分组：Mac 独立页面、Android 账号页入口；Codex / Grok 以相交区域展示独有和共有账号。拖放或点击账号选择目标，统一应用草稿；范围外分组不变，保存冲突保留修改。
- 事件：降智错误、其他错误、恢复历史三个标签独立分页；保留真实历史、未关联及删除实体的错误，排除有明确证据的本地限流。详情区分检测线索与确认停调度后的通知状态。
- 功能：配置 OAuth 恢复、自动用卡、每日测活、Key 回退及分组模型。恢复账号可选择测试连接或一组模型验证，未选入的账号不自动恢复或用卡；托管 Key 可选择在 OAuth 可用时也启用。
- 账号模板：从账号页管理内置及自定义模板，编辑白名单与映射，按账号预览并手动应用。标记或自动检测不改写模型配置，空模板表示不限制模型。
- 设置：Bark、连接、版本检查；开机启动仅在 Mac 显示。

Mac 主窗口默认 1440×860，按屏幕可用区居中收缩，九列账号表完整显示；再次打开不重置本次手动调整。Android 使用五项底部导航、紧凑账号块、筛选与操作弹层；快捷面板与“分组动态”按当前成员最近调用排序，每组显示最近三个不同账号。详情、测试使用全屏面板，系统返回先关闭当前面板，再返回账号页或后台。

左键菜单栏图标显示并聚焦快捷面板，分组按最近调用排序；重复点击不会关闭。图钉控制失焦后是否关闭，Escape 或关闭按钮也可关闭。右键打开主窗口、检查更新或退出。主窗口打开或最小化时显示 Dock 图标，关闭后隐藏并继续驻留菜单栏；快捷面板不影响 Dock，开机启动默认关闭；启用后登录时静默驻留菜单栏，手动打开仍显示主窗口。

快捷面板宽 420px，高度随内容收缩、最大 520px；每组展示最近调用的三个不同有效账号、调度开关、最近调用和上次错误时间，以及 OAuth 微型用量条。工具栏合并为一行，账号行高 48px，超出内容滚动。全界面时间为北京时间 `MM-DD HH:mm:ss`。用量栏保留适用的查询、次数、重置、探测，排除点数与邀请。重置消耗次数、Grok 探测可能发送模型请求，均需确认；操作串行、失败不自动重放。

账号页支持当前筛选全选、单个及批量删除。确认时列出名称、编号及数量，托管 Key 先解除托管；保存失败中止删除。最多并发三个账号，逐项显示结果，失败项保留选择。恢复状态按钮只对错误、有效限流、过载、冷却或模型级限流显示，直接调用 Sub2API 的恢复状态接口，不额外查询额度或执行模型测试。

账号页支持优先级编辑及升降序排列；数值越小优先级越高。连接测试支持 OpenAI 普通、Compact、图像及 Grok 文本、图像、视频、搜索、TTS、STT、Realtime；弹窗内点击开始直接执行，取消不重放。流式文本保留空格、换行和缩进。测试媒体和正文只保留在当前窗口。全部额度刷新覆盖暂停在内的 OpenAI/Grok OAuth，Grok 仅刷新账单，显示逐账号进度及失败结果，批次结束停止进度轮询。

Mac 可见时每 2 秒读取状态，驻留后台每 15 秒。Android 前台每 2 秒合并刷新，后台停止读取和进度轮询，取消未完成的只读请求与测试流，返回前台立即刷新；云端批次及自动化继续运行。离线保留最后快照并禁用写操作，已发出的写操作不重放。刷新不调用额度查询或模型测活。

质量列支持红黄绿筛选、双向排序及评分明细，默认按最近调用时间倒序排列，无调用账号置底。评分只读近七天历史，错误、首字、输出速率权重为 50% / 25% / 25%，样本不足不显示数字。计算按需异步进行，最短间隔 30 秒，不阻塞快照；基准缓存五分钟，异常延迟明确显示，不影响账号优先级或调度。完整定义见 [QUALITY.md](QUALITY.md)。

托管 Key 的手动切换需要确认解除该账号托管。解除失败不继续；调度失败会报告实际结果，不自动重新加入托管。其他窗口已修改设置时，旧版本保存会被拒绝，刷新后重新编辑。

OpenAI OAuth／Key 的三类明确上游降智错误及慢首字预警作为检测线索，不直接推送 Bark。慢首字规则为最近 24 小时最近 10 条有效文本流式调用中，至少 8 条首字严格超过 10 秒。线索触发账号保存模型的自动检测，不要求启用定时检测，也不依赖 Bark 开关；不满足账号资格或仍在五分钟冷却内时忽略，不补跑。

自动检测第 1 组有效指纹最高匹配为 `gpt-5.6-luna` 时，保存降智标记并停止调度，重新读取确认后才发送 Bark 重要通知；失败按原重试节奏独立处理。定时、线索、恢复及用卡后的模型验证共用每账号五分钟门限，重启不清空。普通自动检测未命中时继续最多三组，恢复模型验证只执行一组。手动模型测试不自动处置；人工调度或标记变化优先，自动流程不能覆盖。

Codex 模型测试固定账号、有效凭据、分组精确白名单中的模型及指纹库版本，绕过账号模型映射。三组样本可选择 1／2／3 并发，失败重试不占用请求槽；只显示指纹推测、路由和进度摘要，不保存题目及原始输出。人工调度可在测试期间操作，不额外触发模型请求。

## 构建

需要 Node 22+、pnpm 10.27.0、Rust 1.94.1+、Xcode Command Line Tools。

```sh
pnpm install --frozen-lockfile
pnpm test
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings
pnpm bundle
```

产物位于 `src-tauri/target/aarch64-apple-darwin/release/bundle/`。`pnpm dev` 提供明确标记为“预览”的开发数据，只在开发构建中存在；生产客户端只使用 Rust HTTP 通道。

Android 固定工具链：JDK 21、SDK 36、Build Tools 36.0.0、NDK 28.2.13676358、Gradle 8.14.3、Rust 1.94.1 的 `aarch64-linux-android` 标准库。设好 `JAVA_HOME`、`ANDROID_HOME`、`NDK_HOME` 后：

```sh
node ../scripts/android_signing.mjs
# 设置上一步输出的 SUB2OPS_ANDROID_SIGNING_PROPERTIES 路径
pnpm bundle:android
python ../scripts/verify_android.py src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

签名材料只保存在仓库外的受保护目录；`android_signing.mjs --github` 将它经 stdin 配置为本仓库 CI secrets。不得替换已有签名。Android 仅构建 ARM64，发布前验证签名、最低 API、APK 及全部原生库的 16KB 对齐。

原生仪器测试使用 `node ../scripts/android_fixtures.mjs` 生成仅测试包包含的模拟数据，再构建 `assembleUniversalDebugAndroidTest`。在 ARM64 模拟器中分别运行 `AndroidRuntimeTest#persistAndLifecycle`、覆盖安装同签名 APK 后运行 `#restartUpgradeAndDisconnect`，以及 `#keyboardAndMediaPicker`。带硬件键盘的模拟器需启用软键盘。正式 APK 使用独立的 `releaseSmoke` 测试包，通过系统 UI 验证连接、五页、前后台、重启、覆盖升级和断开，不向正式应用注入测试运行器。

视觉规范见 [DESIGN.md](DESIGN.md)。`desktop-v*` 标签触发构建与发布流程，仅附带 Mac DMG、Android ARM64 APK 和统一 `SHA256SUMS`；不发布 ZIP，历史发布资产保留。

## 云端接口

`/api/desktop/v1` 只接受 `x-api-key` 管理员认证；服务端使用已配置的 Sub2API 内网地址验证权限。读取验证缓存 15 秒，写操作重新验证。账号 DTO 排除 credentials / extra，错误详情按需读取并移除请求正文。不存在桌面免认证模式。

配置通过 `ConfigService` 保存，以 revision 防止覆盖并发修改。部署保持单个 Companion worker，与现有回退控制器共用进程内锁。接口不改变 Sub2API 表结构或源码。

`scripts/desktop_integration.py` 仅可在专用、空账号的隔离数据库运行，要求 `DESKTOP_QA_DISPOSABLE=sub2ops-desktop` 且数据库主机为 `pg`。它验证真实 Sub2API 鉴权、调度写入、分组证据、错误脱敏及并发保存；不得用于生产。

菜单栏阶段诊断保存在应用配置目录的 `panel-diagnostics.log`，最多约 64 KiB，只包含点击、定位、显示、聚焦与隐藏状态，不包含连接或账号资料。

`network-diagnostics.log` 最多约 64 KiB，记录失败路径、HTTP 状态、响应类型、阶段和请求标识，不记录 Key、服务地址、查询参数或响应正文。瞬时失败保留最后快照，在下一刷新周期恢复；云端通过 `X-Request-ID` 关联安全错误日志。
