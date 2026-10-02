# 云端开发机

支持 Linux x86_64。源码从 `main` 恢复；不保存凭据、不安装服务，不处理 NexusHub。

```sh
python3 -B scripts/devhost/devhost.py install
/workspace/devhost/bin/devhost-up
```

首次 Android 安装若返回 `ANDROID_LICENSE_HANDOFF_REQUIRED`，由用户在云端终端阅读并交互接受协议，然后重新执行 `devhost-up`。不得自动输入 `yes` 或复制 license 哈希。Command-Line Tools 使用 Google 官方 `15859902` Linux 包并校验官网 SHA-256；SDK 36、Build Tools 35.0.0/36.0.0、NDK 28.2.13676358、JDK 21、Rust 1.94.1 / `aarch64-linux-android`。35.0.0 为现有 Android Gradle 构建所需，两个版本均受清理保护。不装模拟器。

每次安装依赖、测试或构建均使用受控入口；`--cwd` 相对仓库根目录：

```sh
devhost-run --repo sub2api-ops-companion --cwd desktop -- pnpm install --frozen-lockfile
devhost-run --repo sub2api-ops-companion --cwd desktop -- pnpm tauri android build --target aarch64 --apk --ci
devhost-status
devhost-clean --mode=preflight --dry-run
devhost-clean --mode=postbuild --apply
```

无需发布密钥可先用同一构建命令生成未签名 release APK；设备安装验证使用 `--debug` 产物。正式签名仍由现有 CI 处理。

启动、构建前后检查空间、inode 和缓存预算。开始构建要求可用空间至少 40 GiB、可用 inode 至少 10%、缓存不超过 50 GiB；低于 30 GiB 使用 emergency 清理。构建期间每 2 秒检查空间；低于 15 GiB、inode 低于 2% 或文件系统异常时，只终止本次任务进程组，20 秒后仍未结束才强制终止；不自动重试。

全局 flock 将受控构建和清理串行化；另有仓库锁及未受管理构建检测。脚本不能阻止绕过入口的其他 Bot 写满共享磁盘，也不能修复供应商硬件或文件系统故障；检测到只读、I/O 错误或无法回收时停止并报告。

回收顺序：7 天旧日志（活动 tunnel.log 除外）→14 天未用的已纳管 Rust target →30 天旧下载缓存与 pnpm 原生 prune →Gradle 保留周期 →带 `.devhost-temporary` 标记的 `artifacts/tmp` 7 天旧目录。未知路径、symlink、挂载点、源码、Git、凭据、当前工具链、虚拟环境、正式产物均保留。NexusHub 不纳管。缓存年龄使用子树最新修改时间。

Gradle 保留 released wrapper 45 天、snapshot 10 天、build cache 5 天、daemon 日志 14 天；`devhost-run` 禁用常驻 Gradle daemon。当前 SDK 包受保护；旧 SDK 必须显式指定：

```sh
devhost-sdk-prune --package 'build-tools;34.0.0'
# 检查 dry-run 后才添加 --apply。
```

`devhost-status` 零写入。运行记录只保存空间、路径类别、退出码，最多约 2 MiB；不记录命令参数、环境内容或构建输出。Reset/Recreate 后重新 clone main、执行 install/up 并完成必要登录；生产 Reset 恢复必须实测才算通过。

测试：`python3 -B scripts/devhost/test_devhost.py -v`（仅临时 fixture，不接触真实缓存）。
