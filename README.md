# Sub2API Ops Companion (Sub2Ops)

**简体中文** | [English](README.en.md)

**Sub2Ops** 是面向 Sub2API 的高性能 Rust 旁路运维服务与原生跨端客户端（macOS Apple Silicon & Android ARM64），提供**多 Sub2API 实例一键切换**、**OAuth / API Key 全周期额度监控**、**ModelTrace / LM Detector 模型验真与降智检测**、**7d 满额自动用卡**、**批量账号与代理运维**以及 **Bark 实时事件推送**。

> 客户端下载与使用说明见 [desktop/README.md](desktop/README.md)，版本演进记录见 [desktop/CHANGELOG.md](desktop/CHANGELOG.md)，升级与回退指南见 [UPGRADING.md](UPGRADING.md)。

---

## 核心特性

### 1. 多 Sub2API 实例管理与丝滑切换
- **多实例档案管理**：支持保存多个 Sub2API 实例（自定义备注名与实例地址），集中管理主力、备用或多区域节点。
- **系统级凭据隔离**：各实例的管理员 API Key 按规范化 `base_url` 独立存入系统安全凭据库（macOS Keychain / Android Keystore），绝不落盘明文配置文件。
- **全场景一键切换**：桌面侧栏、顶栏、设置页以及 **macOS 菜单栏快捷面板**均内置实例切换器，切换时自动调取本机凭据校验并刷新实时快照，无需重复输入密钥；凭据失效时自动弹出预填连接面板。

### 2. OAuth 与 API Key 智能额度监控与自动恢复
- **精准恢复监控**：自动追踪多平台（OpenAI、Claude、Gemini、Antigravity、Grok 等）OAuth 窗口恢复时间与冷却状态，到期后自动确认额度并执行恢复测活。
- **7d 满额 + 429 自动用卡**：当 OpenAI OAuth 账号 7d 原始用量达到 100% 且遭遇上游 429 限流时，按有效卡数自动触发用卡恢复（与原版自动用卡互斥防重）。
- **按账号精细保活**：支持按账号独立开关临时不可调度清除策略（默认关闭），严格保留人工调度、降智标记与恢复限额。
- **平台定制刷新**：支持单账号与全量 OAuth 并发刷新；Grok 仅刷新官方账单接口，不消耗模型请求额度。

### 3. ModelTrace 与 LM Detector 模型验真与降智检测
- **双重检测引擎**：结合分词器边界探测（LM Detector）与多维推理指纹分析（ModelTrace），识别渠道模型伪装、偷换与降智。
- **定时与手动巡检**：支持配置全局自动检测模型与定时巡检计划，实时展示接收与分析进度，异常结果自动记录并可联动调度处置。

### 4. 批量账号运维、优先级编排（`schema=2`）、宿主机配置与思考档位补全
- **高效日常管理与优先级拖拽编排**：提供分组动态、多维账号筛选排序、按平台/分组的优先级拖拽编排（`schema=2` 乐观并发对账）、批量测活、批量重授权、账号配置模板与代理质量体检。
- **宿主机 `config.yaml` 受保护下发与在线升级**：支持在客户端直接管理 Sub2API 宿主机配置、检查上游版本、单次安全提交在线升级并重启生效。
- **思考档位补全**：自动为新模型补全 Reasoning Effort（思考档位）映射与模型目录配置，按分组版本安全校验。

### 5. 原生跨端体验与 Bark 实时推送
- **macOS & Android 原生客户端**：基于 Tauri v2 构建，macOS 端提供常驻菜单栏快捷面板（支持置顶、分组概览、实例速切与快捷刷新），Android 端适配原生手势与安全存储。
- **Bark 告警通知**：实时推送 OAuth 额度恢复、每日测活失败、自动恢复失败、保活频率达限暂停及 401/402 认证异常。

---

## 专题文档索引

| 模块 | 说明文档 |
|---|---|
| 客户端使用说明 | [desktop/README.md](desktop/README.md) |
| OAuth 额度监控 | [docs/oauth-monitoring.md](docs/oauth-monitoring.md) |
| 模型验真与检测 | [docs/model-detection.md](docs/model-detection.md) |
| 账号保活机制 | [docs/keepalive.md](docs/keepalive.md) |
| 自动用卡机制 | [docs/automatic-credit.md](docs/automatic-credit.md) |
| 账号测试与操作 | [docs/account-operations.md](docs/account-operations.md) |
| 模型目录与档位 | [docs/model-catalog.md](docs/model-catalog.md) |
| Sub2API 宿主配置 | [docs/sub2api-config.md](docs/sub2api-config.md) |
| 升级与回退指南 | [UPGRADING.md](UPGRADING.md) |

---

## 快速开始与生产部署

### 启动服务

```bash
cp .env.example .env
docker compose up -d --build
```

- `.env.example` 仅提供本地开发占位值；生产部署前请替换数据库连接、Sub2API 地址和 Bark 配置，切勿将 `.env` 或 `data/` 纳入公开仓库或发布产物。
- 服务端源码位于 [`server/`](server/README.md)，正式生产镜像仅包含静态编译的 Rust 二进制与必要证书资源，不含 Python 运行时。
- 生产部署优先加载对应提交的 `Companion-linux-amd64` CI 成品镜像并设置 `OPS_IMAGE`；容器仅挂载 `/data` 目录。
- 预检与只读验收模式：
  - `--check-state`：校验现有配置、状态文件与数据库连接后立即退出。
  - `--read-only`：不派发后台任务、不写入业务库或状态文件的只读验收入口（需使用与正式服务一致的环境配置）。

### 网络与反向代理

- Compose 默认仅将端口发布到宿主机回环地址 `127.0.0.1:18081`（容器内监听 `0.0.0.0:18081`）。
- 直接在宿主机运行二进制时默认监听所有网卡；若仅限本机访问，请显式指定：
  ```bash
  sub2ops-server serve --host 127.0.0.1 --port 18081
  ```
- 生产环境建议通过 Nginx 挂载到 Sub2API 同域名的 `/sub2ops/` 路径下，并关闭该路径的 query access log（配置示例见 `deploy/nginx/sub2ops.location.conf`）。
- 桌面管理路由强制校验管理员 API Key；`connection-status` 保持 HTTP 200 返回脱敏鉴权诊断的契约（不回传地址、密钥或业务数据）；`/healthz` 与模型目录接口分别沿用健康检查和客户端鉴权语义。
- 数据库跨主机连接请启用 TLS、配置可信 CA 并验证服务端主机名（`sslmode=verify-full`）；`sslmode=disable` 仅适用于同一受控容器内网。

---

## 环境变量说明

| 变量 | 说明 |
|---|---|
| `SUB2API_CONFIG_PATH` | 独立连接配置文件，默认 `/data/sub2api-config.json`；JSON 中的 `base_url` / `verify_base_url` 优先于对应环境变量。 |
| `SUB2API_BASE_URL` | Sub2API 公网根地址。 |
| `SUB2API_VERIFY_BASE_URL` | 可选的服务端内网校验根地址。 |
| `GROUP_MODEL_CONFIG_PATH` | 分组思考档位补全文件，默认 `/data/group-model-config.json`（按分组版本校验）。 |
| `KEEPALIVE_CONFIG_PATH` | 账号保活设置、恢复预算与待核对记录，默认 `/data/keepalive-state.json`（默认全部关闭）。 |
| `DATABASE_URL` | Sub2API PostgreSQL 连接串。 |
| `BASE_PATH` | 历史兼容字段，默认 `/sub2ops`；外部前缀由 Nginx 配置并剥离，Rust 服务不据此挂载路由。 |
| `USAGE_QUERY_STATE_PATH` | OAuth 快照、管理员 API Key 与调度元数据，默认 `/data/usage-query-state.json`。 |
| `OAUTH_CONFIG_PATH` | OAuth 自动化配置文件，默认 `/data/oauth-config.json`；优先级：JSON > 环境变量 > 默认值。 |
| `BARK_CONFIG_PATH` | Bark 配置文件，默认 `/data/bark-config.json`。 |
| `BARK_ENABLED` | 是否启用事件 Bark 推送（含账号保活达到频率上限后的暂停通知），默认关闭。 |
| `BARK_DEVICE_KEY` | Bark Device Key；建议通过客户端设置写入权限为 `0600` 的配置文件。 |
| `BARK_SERVER_URL` | Bark 服务根 URL，默认 `https://api.day.app`；HTTP 仅允许 loopback。 |
| `OAUTH_RECOVERY_MONITOR_ENABLED` | 是否监控到期恢复，默认开启。 |
| `MODEL_DETECTION_MODEL_ID` | 全局自动检测模型，默认 `gpt-6-luna`（新任务生效，已有任务保留原模型）。 |
| `OAUTH_AUTO_RESET_CREDIT_ENABLED` | 7d 原始用量达 100% 且当前上游 429 限流时自动用卡，默认关闭（与 Sub2API 原自动用卡互斥）。 |
| `OAUTH_DAILY_TEST_ENABLED` | 是否启用每日 OpenAI OAuth 测活，默认开启（仅异常通过 Bark 推送）。 |
| `OAUTH_DAILY_TEST_TIME` | 每日测活时间（北京时间 `HH:MM`），默认 `05:00`；修改或重启后等待下一个未来时间点，错过不补跑。 |
| `OAUTH_USAGE_REFRESH_CONCURRENCY` | 活跃额度查询并发数，默认 `4`。 |
| `OAUTH_RECOVERY_TEST_CONCURRENCY` | 账号恢复测活并发数，默认 `2`。 |
| `OAUTH_EARLY_PROBE_BATCH_SIZE` | 历史提前探测参数，默认 `8`（保留读取与校验兼容，当前不提前探测）。 |
| `OAUTH_RECOVERY_TEST_MODEL_ID` | 恢复测活模型，默认 `gpt-5.6-luna`。 |

---

## 依赖维护与质量门禁

- **自动更新策略**：Dependabot 每周一 09:00（Asia/Taipei）检查两份 Cargo 锁文件、前端依赖、Python 测试工具、基础镜像、GitHub Actions 与 Android 应用库；普通版本观察 7 天后提出更新，安全修复立即响应。
- **锁文件与兼容边界**：Cargo 采用 `lockfile-only` 并限制跨越 `0.x` 兼容线的普通更新；React 与 React DOM 保持同版本；Tauri JS 包与 Rust 核心（`~2.12.5`）、构建辅助（`~2.7.4`）及插件成组对齐验证。
- **构建与包体积门禁**：桌面与 Android 构建统一启用调试符号裁剪，CI 自动执行 Rust/Node/Python 安全审计、契约一致性测试、模型目录集成测试、Android 模拟器真机级 instrumentation 冒烟测试及归档体积上限检查。
- **已知平台范围说明**：桌面锁文件中的 `glib 0.18.5`（`GHSA-wrw7-89jp-8q8g`）仅属于未交付的 Linux 桌面依赖，macOS ARM64、Android ARM64 与服务端实际依赖图均不包含该包；告警保持开放而不加入忽略名单。

---

## 安全规范

- **最小暴露面**：Companion 具备账号调度与配置写权限，切勿直接暴露于公网。请仅绑定回环地址或受控容器内网，并通过 TLS 反向代理向客户端提供服务。
- **凭据文件权限**：服务端管理员 API Key 与 Bark Device Key 写入时强制设置文件及锁权限为 `0600`，密钥绝不进入 URL、日志、审计输出或前端回显；客户端各实例的管理员 API Key 独立保存在系统级 Keychain / Keystore 中。
- **Bark SSRF 防护**：Bark HTTPS 目标严格拒绝非公网字面 IP（含 IPv4 映射及特殊进制写法），支持官方 `api.day.app` 与公网自建域名（保留本地测试所需的 HTTP loopback）。
