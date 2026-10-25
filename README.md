# Sub2API Ops Companion

Sub2API 的 Rust 旁路运维服务，提供 OAuth 额度监控、Bark 事件推送、桌面账号管理和新模型思考档位补全。

**Sub2Ops** 支持 macOS Apple Silicon 和 Android ARM64，提供分组动态、账号管理、错误详情及 Ops 设置；Mac 另有菜单栏快捷面板。下载与使用见 [客户端说明](desktop/README.md)，版本变化见 [发布记录](desktop/CHANGELOG.md)。仅通过 App 管理；网页、SSO 和旧表单接口已下线。

- OAuth 额度监控：等待恢复时间后确认额度，统一限制自动查询并执行恢复后测活。
- Bark：推送 OAuth 恢复、测活失败、自动恢复失败和 401/402 认证异常。
- OAuth 额度查询：客户端支持单账号查询和全部 OAuth 刷新；Grok 只刷新官方账单，不发送模型请求。
- 账号保活：按账号选择是否清除符合条件的临时不可调度，所有平台默认关闭；保留人工调度、降智标记及恢复限额。

机制说明：[OAuth 监控](docs/oauth-monitoring.md)、[模型检测](docs/model-detection.md)、[账号保活](docs/keepalive.md)、[自动用卡](docs/automatic-credit.md)、[账号测试](docs/account-operations.md)、[模型目录](docs/model-catalog.md)。升级和回退见 [UPGRADING.md](UPGRADING.md)。

## 运行

```bash
cp .env.example .env
docker compose up -d --build
```

`.env.example` 只提供本地开发占位值；部署前请替换数据库连接、Sub2API 地址和 Bark 配置，并不要把 `.env` 或 `data/` 纳入公开发布。

服务源码位于 [`server/`](server/README.md)，正式镜像不含 Python 运行时。生产部署优先加载对应提交的 `Companion-linux-amd64` CI 成品，并设置 `OPS_IMAGE`；镜像只挂载 `/data`。`--check-state` 检查现有配置、状态和数据库后退出，`--read-only` 提供不派发后台任务、不写业务或状态的验收入口。两种模式都必须使用与正式服务相同的环境配置。

Compose 默认只将端口发布到宿主机 `127.0.0.1:18081`；容器内服务默认监听 `0.0.0.0:18081`。生产环境建议通过 nginx 挂载到 Sub2API 同域的 `/sub2ops/`，并关闭该路径的 query access log。nginx 示例见 `deploy/nginx/sub2ops.location.conf`。

直接运行二进制也默认监听所有网卡；仅本机访问时使用 `sub2ops-server serve --host 127.0.0.1 --port 18081`，不要把容器端口发布限制误认为二进制的监听限制。桌面业务管理路由验证管理员 API Key；`connection-status` 沿用 HTTP 200 携带脱敏鉴权失败诊断的契约，不返回地址、密钥或业务数据。`/healthz` 与模型目录入口分别沿用健康检查和客户端鉴权语义。

示例数据库连接的口令必须替换，`sslmode=disable` 仅适用于同一受控 Docker 网络。跨主机连接应启用 TLS、使用可信 CA 并验证服务端主机名（`sslmode=verify-full`）。

## 依赖维护

Dependabot 每周一 09:00（Asia/Taipei）检查两份 Cargo 锁文件、前端、Python 测试工具、基础镜像、GitHub Actions 和 Android 应用库；普通版本观察七天后提出更新。Cargo、Python、镜像、Actions、Android 各最多一个普通 PR，前端按 React、Tauri、其他依赖最多三个。安全修复不等待普通周期，所有合并仍由维护者完成。仓库设置需开启漏洞告警、自动安全修复及合并后删分支；无告警不等于依赖图已成功识别。

Cargo 使用 `lockfile-only`，包含直接和间接依赖，并显式限制普通更新候选，避免解析器尝试跨越 `0.x` 兼容线；各生态的 `0.x` 包默认只提出补丁更新，已确认的稳定依赖族允许兼容小版本。新增包默认仅开放补丁，确认兼容策略后再扩充普通小版本更新名单。React 与 React DOM 保持相同版本，类型包一起检查；Tauri JS 包一起更新并与 Rust 侧及 Mac／Android 构建联动验证；普通自动更新仅开放已验证系列的补丁，新的小版本由维护者把 JS 包、Rust 核心、构建辅助和插件成组更新。安全修复需要跨兼容线时单独验证，不通过忽略漏洞、放宽检查或自动合并处理。

pnpm 保留 `packageManager` 中的精确版本，由 Corepack 和 CI 的 pnpm 安装步骤选择该版本。`pmOnFail: ignore` 关闭 pnpm 自身的版本切换，使锁文件保持单文档，供 GitHub 依赖图识别；本机在 `desktop` 目录运行 Corepack；从父目录使用 `pnpm --dir` 不会让 Corepack 读取子目录的版本声明。直接调用其他 pnpm 二进制前先核对版本。依赖变更后使用指定版本重新生成锁文件并验证 `(cd desktop && corepack pnpm install --frozen-lockfile)`，不手工删除锁文件中的文档。

每月集中核对 Rust、Node、pnpm、cargo-audit、CI 浏览器工具、Gradle／AGP／Kotlin／JDK／SDK／NDK 和 QA 镜像；有安全公告时提前处理。保留 Action SHA 和镜像摘要，Rust 构建镜像只自动更新当前标签摘要，编译器版本与 `rust-toolchain.toml`、CI、manifest 最低版本及说明一起更新。Gradle 工具链和 QA Compose 镜像由维护者成组更新；Python 驱动中的重复 psycopg 版本同步维护。Gradle 间接依赖安全检查不能只依赖 GitHub 自动告警，需按月核对解析结果和上游公告。

维护时在现有约束内更新 Cargo 锁文件；前端精确固定的版本需显式选择兼容版本，并检查间接依赖。运行现有 Rust 审计、`(cd desktop && corepack pnpm audit)`，以及对 `requirements.txt` 完整解析结果的 Python 漏洞检查，再跑受影响测试与版本一致性检查。`webpki-roots` 的转导例外绑定准确版本、注册表校验和及依赖边；升级时重新核实并同步审计测试，不扩大例外。

当前延期：jsdom 30、lucide-react 1.x、应用直接使用的 reqwest 0.13，以及其他没有安全或实际功能需要的跨兼容线升级。每月复核延期项和维护状态；生产服务仍为 Rust，Python 依赖仅用于隔离测试及固定历史参考代码。

Tauri 当前使用 2.12 核心／运行库与 2.7 构建辅助组件；其内部接口不能与旧 2.11 核心混用，更新时保持整组一致。Python 参考测试及依赖审计使用现有 Python 3.12 环境，例如 `uvx --python 3.12 pip-audit -r requirements.txt --progress-spinner off`，避免审计工具自动选择尚无依赖轮子的更新解释器。

桌面锁文件中的 `glib 0.18.5` 有 `GHSA-wrw7-89jp-8q8g` 告警，但它仅属于未交付的 Linux 桌面依赖；Mac ARM64、Android ARM64 的实际依赖图和服务端均不包含它。告警保持开放，不加入审计忽略；新增 Linux 桌面支持前必须更新其 GTK／glib 依赖链。

## 环境变量

| 变量 | 说明 |
|---|---|
| `SUB2API_CONFIG_PATH` | 独立连接配置文件，默认 `/data/sub2api-config.json`，JSON 的 `base_url` / `verify_base_url` 优先于对应环境变量。 |
| `SUB2API_BASE_URL` | Sub2API 公网根地址。 |
| `SUB2API_VERIFY_BASE_URL` | 可选的服务端内网校验根地址。 |
| `GROUP_MODEL_CONFIG_PATH` | 分组思考档位补全文件，默认 `/data/group-model-config.json`；按分组版本校验。 |
| `KEEPALIVE_CONFIG_PATH` | 账号保活设置、恢复预算和待核对记录，默认 `/data/keepalive-state.json`；所有账号默认关闭。 |
| `DATABASE_URL` | Sub2API PostgreSQL 连接串。 |
| `BASE_PATH` | 历史兼容字段，默认 `/sub2ops`；外部前缀须由 nginx 配置并剥离，Rust 不据此挂载路由。 |
| `USAGE_QUERY_STATE_PATH` | OAuth 快照、管理员 API Key 和调度元数据，默认 `/data/usage-query-state.json`。 |
| `OAUTH_CONFIG_PATH` | OAuth 自动化配置，默认 `/data/oauth-config.json`；JSON 优先于环境变量，环境变量优先于默认值。 |
| `BARK_CONFIG_PATH` | Bark 配置文件，默认 `/data/bark-config.json`。 |
| `BARK_ENABLED` | 是否启用事件 Bark 推送（含账号保活达到频率上限后的暂停通知），默认关闭。 |
| `BARK_DEVICE_KEY` | Bark Device Key；生产环境建议通过桌面设置写入权限为 `0600` 的配置文件。 |
| `BARK_SERVER_URL` | Bark 服务根 URL，默认 `https://api.day.app`；HTTP 只允许 loopback。桌面端保留当前运行时 URL。 |
| `OAUTH_RECOVERY_MONITOR_ENABLED` | 是否监控到期恢复，默认开启。 |
| `MODEL_DETECTION_MODEL_ID` | 全局自动检测模型，默认 `gpt-6-luna`；新任务生效，已有任务保留原模型。 |
| `OAUTH_AUTO_RESET_CREDIT_ENABLED` | 7d 原始用量达到 100% 且当前上游 429 限流时自动用卡，默认关闭；与 Sub2API 原自动用卡互斥。 |
| `OAUTH_DAILY_TEST_ENABLED` | 是否启用每日 OpenAI OAuth 测活，默认开启；仅异常通过 Bark 推送。 |
| `OAUTH_DAILY_TEST_TIME` | 每日测活时间（北京时间 `HH:MM`），默认 `05:00`。修改时间、启用或重启后均等待下一个未来时间点，错过不补跑。 |
| `OAUTH_USAGE_REFRESH_CONCURRENCY` | active usage 并发，默认 `4`。 |
| `OAUTH_RECOVERY_TEST_CONCURRENCY` | account test 并发，默认 `2`。 |
| `OAUTH_EARLY_PROBE_BATCH_SIZE` | 历史提前探测参数，默认 `8`；保留读取、保存和校验兼容，当前不提前探测，也不用于后台派发上限。 |
| `OAUTH_RECOVERY_TEST_MODEL_ID` | 恢复测活模型，默认 `gpt-5.6-luna`。 |

`OAUTH_CONFIG_PATH` 中的 JSON 优先于环境变量，环境变量优先于默认值。状态和配置文件在实际写入时统一为 `0600`，不继承旧文件的宽权限；只读检查不修改权限。

## 安全

Companion 具有账号调度操作权限，不应独立暴露在公网。仅向回环地址或 Docker 内网开放服务，桌面接口经 TLS 反代并验证管理员权限。管理员 API Key 保存在 `USAGE_QUERY_STATE_PATH`，Bark Device Key 保存在 `BARK_CONFIG_PATH`；写入文件及使用的文件锁统一为 `0600`，未写入的历史文件仅给出安全诊断，密钥不写入 URL、页面、日志或审计明文。不要公开 `.env`、`data/` 或生产日志。

Bark 的 HTTPS 目标拒绝非公网字面 IP（含 IPv4 映射及数字写法），支持 `api.day.app` 和公网自建服务；现有 HTTP loopback 方式保留。不进行 DNS 解析校验，域名仍需由部署者配置为可信服务。

配置管理说明：[Sub2API 配置](docs/sub2api-config.md)。
