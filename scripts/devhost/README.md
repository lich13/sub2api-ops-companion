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

SSH 失联时，在 Grok 云端终端运行一个恢复入口：

```sh
/workspace/devhost/bin/devhost-recover
```

默认只恢复连接组件、同一私网身份和 SSH 服务，不升级开发工具链、不重新安装 Android、不重置仓库。系统层丢失导致 Java、Tauri 系统库或 npm/pnpm 缺失时，运行 `devhost-recover --full` 按清单补齐开发环境；仅补回固定工具链的缺失文件，已有不同内容会停止。运行副本也缺失时，先在此仓库运行上面的 `devhost.py install`，再执行恢复入口。出现注册交接或主机指纹变化时停止自动处理，不清空身份或跳过指纹校验。

连接身份、SSH 主机私钥和恢复配置保存在开发用户的 `~/.local/state/devhost/transport`，由 root 管理，目录 700、文件 600；不放在仓库或 `/workspace`。系统层软件丢失、但该用户目录保留时，恢复命令会复用原身份。用户目录也丢失时可以从本机私有备份恢复原身份；没有有效备份或原节点已被控制面撤销时，才需要重新注册并独立核对指纹。不能承诺供应商 Reset 会保留这些数据。

连接服务使用 systemd；没有 systemd 时由独立 supervisord 管理。只监听私网 SSH，允许密钥认证、本地端口转发和 SFTP。实例停止或重建仍需从 Grok 入口执行恢复；未实际测试的供应商恢复能力不得标为通过。

连接健康检查启动 15 秒后开始、每 30 秒运行一次。已登录客户端连续 4 次离线后，直接重启云端受管 Tailscale 进程，至少间隔 10 分钟；认证探测临时失败时最多沿用 5 分钟内的已知状态，明确退出登录后不再自动重启。SSH listener 连续 4 次无法返回 SSH banner 时单独重启；SSH 探测和备份异常不会计为 Tailscale 故障，重启失败也不会令监控进程退出。supervisor 负责拉起同一身份的进程。诊断仅保存时间和事件类别。

## 自动备份与一键恢复

备份只覆盖连接必需的状态：私网节点身份、SSH 主机私钥、授权公钥、连接配置和工具链锁定清单。源码继续以 GitHub 为恢复源，软件包按锁定清单重装；不备份依赖、编译缓存、浏览器登录或 GitHub Token。

- 云端每次成功启动连接后备份一次；现有连接健康进程每 6 小时再检查一次。副本存入开发用户的 ~/.local/state/devhost/transport-backups，由 root 管理，目录 700、文件 600，保留最近两代不同内容。
- 缺少必要源文件、损坏备份、软链接或校验失败会停止操作，不用空文件覆盖好备份。每一代带 SHA-256 校验；发布新一代后才回收旧一代。
- 恢复只补缺失文件，不回滚仍存在的节点状态和配置，不重置仓库。软件恢复后启动同一节点和同一 SSH 身份。

云端终端运行一个命令，补回配置并恢复连接和缺失的开发环境：

    /workspace/devhost/bin/devhost-restore --full

只恢复连接时省略 --full。选择上一代可用 --previous；导入本机副本用 --archive /protected/path/backup.tar.gz。私有 archive 只能放受保护的用户目录，不能上传到仓库或 /workspace。

Mac 上安装独立副本机制（将这两个 Python 脚本下载到同一临时目录后执行；无需安装依赖）：

    python3 macos_transport_backup.py install

它安装用户 LaunchAgent，登录后及每 6 小时通过 SSH 拉取一次已校验的小型备份，保存在本机 Library/Application Support/grok-cloud-recovery，保留最近两代。离线时保留已有副本，不唤醒云电脑，也不输出原始错误或凭据。Downloads 中生成 恢复grok-cloud.command：SSH 可用时执行完整恢复；不可用时复制云端恢复命令，提示从 Grok 终端执行。程序无法在供应商停机、且所有远程入口都关闭时凭空执行命令。

本机副本导入入口：

    python3 macos_transport_backup.py restore --from-local

导入前校验 archive 的成员白名单、大小和哈希；通过 SSH 加密传输，临时文件位于云端用户目录并在完成后移除。SSH 完全不可用时，先从 Grok 终端恢复连接；云端用户目录也丢失时，需先通过可信控制台传入本机私有副本。权限保护不能隔离拥有 root/sudo 的其他 Bot。隔夜和供应商整机重建仍需独立验收。

## 仓库与工具链

十三个仓库均使用 `main → origin/main`。修改前 fetch 并确认工作树干净、主线未分叉；测试和敏感信息检查后在云端提交并 `git push origin HEAD:main`。不强推，不覆盖其他写入者的修改。`NexusHub` 自动识别已有小写目录 `nexushub`。

`toolchains.lock.json` 固定官方下载来源、精确版本和校验值。首次安装解析 Python 补丁版本后，需将运行副本生成的完整清单审查并提交；恢复不重新选择版本。

- Node 24.13.1，Rust 1.94.1，JDK 21；Gradle 使用项目 Wrapper。
- 包管理器由 `repositories.json` 指定：pnpm 按项目版本选择；npm 项目使用 Node 配套 npm 11.8.0 和 `npm ci`，不转换锁文件。
- Python 3.12、uv、Go 版本见锁定清单。sub2api、Pylon、Settlement 使用独立 `devhost/venvs/<repo>`；插件验证复用 Python 标准库。
- Android 36、Build Tools 35.0.0/36.0.0、NDK 28.2.13676358，包清单见 `android-packages.txt`。不安装模拟器或 AVD。

交互式终端可 source `devhost-env`；切换目录后重新 source，自动识别已注册仓库。自动化命令通过 `devhost-run` 获取一致环境：

```sh
devhost-run --repo NexusHub -- cargo test --workspace
devhost-run --repo sub2api-ops-companion --cwd desktop -- pnpm install --frozen-lockfile
devhost-run --repo lich13studio -- pnpm tauri:build:android
```

`repositories.json` 是仓库目录、包管理器、安装/检查命令和 CI 平台的唯一配置。`devhost-prepare --repo <repo>` 按需安装该项目依赖；`devhost-check --repo <repo>` 在构建锁和空间门禁内依次执行检查，首个失败即停止。SlimBrave 只在 Windows CI 检查，云端检查入口返回 78。

Python 和 pnpm 版本按项目选择；npm 不接收 pnpm 的配置变量。Tauri 打包使用项目本身的 CLI（`pnpm exec tauri` 或 `npm exec --offline -- tauri`）。浏览器引擎只在 CI 安装；日常浏览器交互由本机现有浏览器经 SSH 端口转发完成。

连接 VS Code Remote-SSH 后运行 `devhost-editor`，安装锁定版本的远端扩展并生成 `devhost/workspaces/<repo>.code-workspace`。打开对应工作区可获得 Rust/Python 导航、调试和 prepare/check 任务；机器设置与工作区均通过 `devhost-rust-analyzer` 管理编译；保存时自动检查默认关闭，手动检查和构建脚本遵守资源门禁。扩展、语言服务和调试适配器留在云端。

新增覆盖：Pylon、Settlement、Apple Music 工具、CloudTune、Obsidian 扩展、Design Director 插件、Open Computer Use 插件及 SlimBrave。macOS/Windows 原生构建和 ARM64 容器构建交给对应 CI；生产服务、浏览器登录状态、签名私钥和数据库不会随开发环境迁移。

同一仓库只允许一个写入者。构建经资源锁串行执行，Rust 和 Gradle 默认两个 worker。SSH 非交互命令直接调用 `/workspace/devhost/bin/devhost-run`，不依赖交互式 shell 配置。

## Linux 原生构建依赖

系统包统一以 `toolchains.lock.json` 的 `apt_packages` 为准，bootstrap 和完整恢复安装整份清单。截图和 Wayland 依赖包含 PipeWire、SPA、Clang、GBM 和 XCB RandR。仅用于 Windows 的依赖不作为 Linux 系统包安装依据。GTK/WebKit、OpenSSL、D-Bus、Wayland 和 X11 同样进行实际可用性检查。

只读检查系统包版本、pkg-config 元数据和共享库加载：

```sh
python3 -B /workspace/devhost/lib/toolchains.py --check-system
```

仅补齐清单中的系统包：

```sh
python3 -B /workspace/devhost/lib/toolchains.py --system-only
```

安装使用构建锁和现有空间门禁，不重装 Node、Rust 或 Android。已有版本与锁定清单不一致时报告差异，不自动降级或全量升级。`devhost-status` 的 `system_dependencies.ready` 来自实际探测；包管理器显示已安装但 `.pc` 文件或共享库缺失时，仍会报告未就绪。Linux 编译通过不代表已验收真实屏幕捕获、音频设备或 macOS 原生权限。

## 空间保护

```sh
devhost-clean --mode=preflight --dry-run
devhost-clean --mode=postbuild --apply
devhost-clean --mode=emergency --apply
```

开始构建要求至少 8 GiB 可用空间、1% 可用 inode；缓存容量没有硬上限。低于 12 GiB 或 2% 可用 inode 才清理，恢复到至少 16 GiB 且 3% 可用 inode 后停止。空间充足时 preflight/postbuild/emergency 均不删除缓存，也不执行定期 pnpm prune。运行中每秒检查：低于 3 GiB、0.2% 可用 inode 或遇到文件系统异常，只终止本次任务；3 秒内未退出则强制停止该任务进程组，不自动重试。

低空间时按顺序回收：30 天旧日志、30 天闲置 Rust target、90 天旧下载缓存、Gradle 旧缓存、明确标记的 14 天临时成果，仍不足时调用 pnpm 原生 prune（最多每天一次）。Gradle released wrapper/版本缓存保留 90 天，snapshot、build cache 和 daemon 日志保留 30 天。关闭 Gradle User Home 的独立定期清理，统一由 devhost 的空间检查触发；清理只涉及可再生目录，保留最新使用时间判断和路径保护。

全局锁和仓库锁保护活动构建；未受管理构建存在时拒绝清理。源码、Git、凭据、虚拟环境、当前工具链、正式成果、symlink 和真实挂载点不进入回收范围。挂载边界读取 Linux mountinfo，兼容 overlay 保留文件的设备编号差异。Android 旧版本仅通过显式 `devhost-sdk-prune --package <package> --apply` 删除；当前包清单受保护。

缓存统一放在 `/workspace/cache`，Rust 工具链在 `/workspace/devhost/toolchains/rustup`。诊断同时统计仓库、工具链和成果占用。回收后仍低于 8 GiB 或 1% inode 才拒绝构建；无法阻止其他 Bot 绕过入口写盘，也不承诺修复供应商硬件故障。

运行记录只保存资源、仓库名和退出码，不记录命令参数、环境或输出正文。

## 验证

```sh
python3 -B -m unittest discover -s scripts/devhost -p 'test_*.py'
```

测试使用隔离 fixture，覆盖活动锁、路径保护、低空间与 inode 门禁、失败后清理和环境选择。真实 SSH、Remote-SSH、Android 构建、隔夜重连和重建恢复分别验收。

## Rust 依赖缓存修复

`cc`、`openssl-sys` 等 crate 的 `build`、`src/target`、`dist` 目录可能包含编译器实际读取的源码，不能按目录名递归清理。空间回收只处理已确认的缓存根目录和完整缓存单元。

遇到缓存文件缺失时运行：

```sh
cargo-cache-doctor --online
cargo-cache-doctor --online --repair
```

默认只检查；`--repair` 才会补回缺失文件或替换与归档不一致的缓存文件。先验证 `.crate` 的 SHA-256 与 registry 索引一致，再逐文件比对和原子修复。`--online` 仅在本地索引缺少校验值时通过官方 HTTPS 查询。下载包校验失败、活动构建、Cargo 锁、软链接或挂载边界冲突时停止对应操作；不删除项目源码、额外文件或凭据，不重试构建。大范围检查按需执行，不增加日常构建的全量扫描。
