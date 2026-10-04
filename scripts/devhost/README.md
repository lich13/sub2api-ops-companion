# 云端开发机

日常通过 `grok-cloud` SSH 或 VS Code Remote-SSH 打开远端仓库。源码、依赖和构建留在云端；macOS 应用由 macOS CI 构建。

## 初始化与恢复

从 GitHub `main` 恢复仓库后，在 Linux x86_64 云电脑执行：

```sh
python3 -B scripts/devhost/devhost.py install
/workspace/devhost/bin/devhost-bootstrap
```

首次注册通过受保护的临时文件提供控制面地址和 SSH 公钥：`--control-url-file`、`--public-key-file`。地址、认证状态和私钥不进入源码或 `/workspace`。优先复用已有授权；新节点由现有 Headscale 管理员注册。Android 出现新许可证时由用户阅读接受。

`devhost-bootstrap --transport-only` 只准备连接。已有环境使用 `devhost-up` 恢复连接并检查空间；它不重新安装 Android，不默认启动 Tunnel。`devhost-status` 只读诊断。备用 Tunnel 由 `devhost-tunnel-fallback` 显式启动。

连接服务使用 systemd；没有 systemd 时由独立 supervisord 管理。只监听私网 SSH，允许密钥认证、本地端口转发和 SFTP。实例停止或重建仍需从 Grok 入口执行恢复；未实际测试的供应商恢复能力不得标为通过。

连接健康检查每 30 秒运行一次。已登录客户端连续 4 次离线、且控制面 HTTPS 正常时，只重启本机受管 Tailscale 进程，至少间隔 10 分钟；未登录时不循环重启。诊断仅保存时间和事件类别。

## 仓库与工具链

十三个仓库均使用 `main → origin/main`。修改前 fetch 并确认工作树干净、主线未分叉；测试和敏感信息检查后在云端提交并 `git push origin HEAD:main`。不强推，不覆盖其他写入者的修改。`NexusHub` 自动识别已有小写目录 `nexushub`。

`toolchains.lock.json` 固定官方下载来源、精确版本和校验值。首次安装解析 Python 补丁版本后，需将运行副本生成的完整清单审查并提交；恢复不重新选择版本。

- Node 24.13.1，Rust 1.94.1，JDK 21；Gradle 使用项目 Wrapper。
- 包管理器由 `repositories.json` 指定：pnpm 按项目版本选择；npm 项目使用 Node 配套 npm 11.8.0 和 `npm ci`，不转换锁文件。
- Python 3.12、uv、Go 版本见锁定清单。sub2api、Pylon、Settlement 使用独立 `devhost/venvs/<repo>`；插件验证复用 Python 标准库。
- Android 36、Build Tools 35.0.0/36.0.0、NDK 28.2.13676358，包清单见 `android-packages.txt`。不安装模拟器或 AVD。

交互式终端可 source `devhost-env`；切换项目后设置对应 `DEVHOST_REPO` 并重新 source。自动化命令通过 `devhost-run` 获取一致环境：

```sh
devhost-run --repo NexusHub -- cargo test --workspace
devhost-run --repo sub2api-ops-companion --cwd desktop -- pnpm install --frozen-lockfile
devhost-run --repo lich13studio -- pnpm tauri:build:android
```

`repositories.json` 是仓库目录、包管理器、安装/检查命令和 CI 平台的唯一配置。`devhost-prepare --repo <repo>` 按需安装该项目依赖；`devhost-check --repo <repo>` 在构建锁和空间门禁内依次执行检查，首个失败即停止。SlimBrave 只在 Windows CI 检查，云端检查入口返回 78。

Python 和 pnpm 版本按项目选择；npm 不接收 pnpm 的配置变量。Tauri 打包使用项目本身的 CLI（`pnpm exec tauri` 或 `npm exec --offline -- tauri`）。浏览器引擎只在 CI 安装；日常浏览器交互由本机现有浏览器经 SSH 端口转发完成。

连接 VS Code Remote-SSH 后运行 `devhost-editor`，安装锁定版本的远端扩展并生成 `devhost/workspaces/<repo>.code-workspace`。打开对应工作区可获得 Rust/Python 导航、调试和 prepare/check 任务；Rust 默认关闭保存时编译，构建脚本检查经过 `devhost-run`。扩展、语言服务和调试适配器留在云端。

新增覆盖：Pylon、Settlement、Apple Music 工具、CloudTune、Obsidian 扩展、Design Director 插件、Open Computer Use 插件及 SlimBrave。macOS/Windows 原生构建和 ARM64 容器构建交给对应 CI；生产服务、浏览器登录状态、签名私钥和数据库不会随开发环境迁移。

同一仓库只允许一个写入者。构建经资源锁串行执行，Rust 和 Gradle 默认两个 worker。SSH 非交互命令直接调用 `/workspace/devhost/bin/devhost-run`，不依赖交互式 shell 配置。

## 空间保护

```sh
devhost-clean --mode=preflight --dry-run
devhost-clean --mode=postbuild --apply
devhost-clean --mode=emergency --apply
```

开始大型构建要求至少 40 GiB 可用空间、10% 可用 inode，缓存不超过 50 GiB。低于 30 GiB 进入 emergency 清理。运行中低于 15 GiB、2% 可用 inode，或遇到文件系统异常，只终止本次任务；不自动重试。

回收顺序：7 天旧日志、14 天闲置 Rust target、30 天旧下载缓存、pnpm 原生 prune、Gradle 旧缓存、明确标记的 7 天临时成果。Gradle released wrapper 保留 45 天、snapshot 10 天、build cache 5 天、daemon 日志 14 天。

全局锁和仓库锁保护活动构建；未受管理构建存在时拒绝清理。源码、Git、凭据、虚拟环境、当前工具链、正式成果、symlink 和挂载点不进入回收范围。Android 旧版本仅通过显式 `devhost-sdk-prune --package <package> --apply` 删除；当前包清单受保护。

缓存统一放在 `/workspace/cache`，Rust 工具链在 `/workspace/devhost/toolchains/rustup`。诊断同时统计仓库、工具链和成果占用。清理不足时拒绝构建，不降低保护标准；无法阻止其他 Bot 绕过入口写盘，也不承诺修复供应商硬件故障。

运行记录只保存资源、仓库名和退出码，不记录命令参数、环境或输出正文。

## 验证

```sh
python3 -B -m unittest discover -s scripts/devhost -p 'test_*.py'
```

测试使用隔离 fixture，覆盖活动锁、路径保护、低空间与 inode 门禁、失败后清理和环境选择。真实 SSH、Remote-SSH、Android 构建、隔夜重连和重建恢复分别验收。

Remote-SSH 的机器设置和生成的工作区均将 rust-analyzer 编译交给 `devhost-rust-analyzer`；保存时不自动检查，手动检查及构建脚本仍遵守 devhost 资源门禁。
