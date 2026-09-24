# Sub2Ops for macOS

Rust / Tauri 2 / React 运维工作台，首版支持 macOS 12 及以上的 Apple Silicon。

## 安装与连接

从本仓库 Releases 下载 `Sub2Ops_0.1.1_aarch64.dmg`，校验 `SHA256SUMS` 后将 Sub2Ops 拖入 Applications。客户端使用本机签名，未经 Apple 公证；检查更新只打开发布页，不自动安装。

填写 Companion 地址（例如 `https://661313.xyz/sub2ops`）和已有的 **Sub2API 管理员 API Key**。Key 存储在 macOS Keychain，服务为 `com.lich13.sub2ops`；偏好设置不含 Key。断开连接会删除当前连接对应的钥匙串条目。

- 总览：按真实使用日志的分组显示最近成功调用的三个不同账号、模型及北京时间。
- 账号：分组、平台、类型、状态筛选；用量窗口展示 OpenAI OAuth 的 5h/7d 及 Grok 上游请求数/Token 限额快照，不展示金额配额或账单替代值。调度开关只改变允许调度，不清除限流、冷却或认证错误。
- 事件：保留历史错误；查看错误码、脱敏摘要和请求 ID，并显示其后是否已有成功调用。
- 自动化：复用云端 OAuth、每日测活、两平台 Key 回退和模型降级保护配置。
- 设置：Bark、Telegram 查询配对与测试、连接、开机启动。Telegram 无账号操作。

左键菜单栏图标显示并聚焦快捷面板，星标分组排在前面；重复点击不会关闭。图钉控制失焦后是否关闭，Escape 或关闭按钮也可关闭。右键打开主窗口、检查更新或退出。Dock 不显示图标，关闭主窗口继续驻留菜单栏，开机启动默认关闭。

可见时每 2 秒读取状态，后台每 15 秒；恢复连接或重新打开窗口会刷新。离线保留最后快照并禁用写操作，不重放失败请求。刷新不调用额度查询或模型测活。

托管 Key 的手动切换需要确认解除该账号托管。解除失败不继续；调度失败会报告实际结果，不自动重新加入托管。网页或其他窗口已修改设置时，旧版本保存会被拒绝，刷新后重新编辑。

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

视觉规范见 [DESIGN.md](DESIGN.md)。`desktop-v*` 标签触发桌面构建与发布流程，附带 `.app.zip`、`.dmg` 和 SHA-256 校验值。

## 云端接口

`/api/desktop/v1` 只接受 `x-api-key` 管理员认证；服务端使用已配置的 Sub2API 内网地址验证权限。读取验证缓存 15 秒，写操作重新验证。账号 DTO 排除 credentials / extra，错误详情按需读取并移除请求正文。不存在桌面免认证模式。

配置通过共享 `ConfigService` 与网页保持一致，以 revision 防止覆盖并发修改。部署保持单个 Companion worker，与现有回退控制器共用进程内锁。接口不改变 Sub2API 表结构或源码。

`scripts/desktop_integration.py` 仅可在专用、空账号的隔离数据库运行，要求 `DESKTOP_QA_DISPOSABLE=sub2ops-desktop` 且数据库主机为 `pg`。它验证真实 Sub2API 鉴权、调度写入、分组证据、错误脱敏及并发保存；不得用于生产。
