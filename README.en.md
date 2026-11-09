# Sub2API Ops Companion (Sub2Ops)

[简体中文](README.md) | **English**

**Sub2Ops** is a high-performance Rust sidecar operations service and native cross-platform client (**macOS Apple Silicon** & **Android ARM64**) for Sub2API. It delivers **multi-Sub2API instance management with one-click switching**, **OAuth & API Key quota lifecycle monitoring**, **ModelTrace / LM Detector model authenticity & degradation detection**, **7-day 100% + 429 automatic credit reset**, **batch account & proxy operations**, and **real-time Bark push notifications**.

> See [desktop/README.md](desktop/README.md) for client downloads and usage, [desktop/CHANGELOG.md](desktop/CHANGELOG.md) for release notes, and [UPGRADING.md](UPGRADING.md) for upgrade and rollback procedures.

---

## Key Features

### 1. Multi-Sub2API Instance Management & Seamless Switching
- **Multiple Instance Profiles**: Save and manage multiple Sub2API instances with custom aliases and base URLs (e.g., primary, standby, or regional clusters).
- **OS-Level Credential Isolation**: Administrator API Keys are stored per normalized `base_url` in native system credential stores (**macOS Keychain** / **Android Keystore**) and are never written in plaintext to preference files.
- **One-Click Passwordless Switching**: Switch instances effortlessly from the desktop sidebar, top bar, Settings page, or the **macOS menu bar quick panel**. Sub2Ops automatically retrieves the stored credential, verifies connectivity, and refreshes live snapshots—falling back to a prefilled connection modal if a key is missing or rotated.

### 2. Intelligent OAuth & API Key Quota Monitoring
- **Automated Recovery Tracking**: Tracks OAuth quota reset windows and cooldown states across OpenAI, Claude, Gemini, Antigravity, Grok, and more—automatically verifying quota and running health checks once reset windows expire.
- **7d 100% + 429 Auto-Reset Credit**: Automatically triggers credit reset when an OpenAI OAuth account reaches 100% 7-day usage and encounters upstream HTTP 429 rate limits (mutually exclusive with native Sub2API auto-credit to prevent double consumption).
- **Per-Account Keepalive Controls**: Opt-in per-account keepalive clears eligible temporary unschedulable states while preserving manual scheduling locks, degradation flags, and recovery budgets.
- **Platform-Aware Refresh**: Supports single-account and concurrent batch OAuth refreshes; Grok refreshes official billing endpoints without consuming model inference tokens.

### 3. ModelTrace & LM Detector Authenticity & Degradation Detection
- **Dual Detection Engines**: Combines tokenizer boundary probing (**LM Detector**) with behavioral reasoning fingerprint analysis (**ModelTrace**) to detect upstream model spoofing, silent substitution, and quality degradation.
- **Scheduled & On-Demand Runs**: Configure global detection models and scheduled inspection runs with real-time in-memory progress streaming and automated scheduling actions on anomalies.

### 4. Batch Account Operations, `schema=2` Priority Orchestration, Host Config & Reasoning Catalog
- **Streamlined Daily Ops & Priority Drag-and-Drop**: Live group activity feeds, multi-dimensional account filtering/sorting, platform-scoped priority drag-and-drop orchestration (`schema=2` reconciled writes), batch health tests, batch re-authorization, reusable account templates, and proxy health checks.
- **Protected Host `config.yaml` Delivery & Online Upgrades**: Inspect and update Sub2API host configurations with optimistic concurrency guards, check upstream releases, and trigger one-click online upgrades and service restarts.
- **Reasoning Effort Completion**: Automatically supplements reasoning effort levels and model catalog entries for new upstream models with group-version concurrency guards.

### 5. Native Cross-Platform Clients & Bark Push Alerts
- **macOS & Android Native Apps**: Built with Tauri v2. Includes a resident macOS menu bar quick panel (pinning, group summaries, quick instance switching, and one-click quota refresh) and a mobile-optimized Android ARM64 app.
- **Real-Time Bark Notifications**: Instant push alerts for OAuth recovery, daily health check failures, auto-recovery failures, keepalive rate-limit pauses, and HTTP 401/402 authentication errors.

---

## Documentation Index

| Topic | Document |
|---|---|
| Desktop & Mobile Client Guide | [desktop/README.md](desktop/README.md) |
| OAuth Quota Monitoring | [docs/oauth-monitoring.md](docs/oauth-monitoring.md) |
| Model Detection (ModelTrace / LM Detector) | [docs/model-detection.md](docs/model-detection.md) |
| Account Keepalive | [docs/keepalive.md](docs/keepalive.md) |
| Automatic Credit Reset | [docs/automatic-credit.md](docs/automatic-credit.md) |
| Account Testing & Operations | [docs/account-operations.md](docs/account-operations.md) |
| Model Catalog & Reasoning Effort | [docs/model-catalog.md](docs/model-catalog.md) |
| Sub2API Host Configuration | [docs/sub2api-config.md](docs/sub2api-config.md) |
| Upgrade & Rollback Guide | [UPGRADING.md](UPGRADING.md) |

---

## Quick Start & Production Deployment

### Run with Docker Compose

```bash
cp .env.example .env
docker compose up -d --build
```

- `.env.example` contains local development placeholders only. Replace the database URL, Sub2API endpoints, and Bark settings before deploying to production. Never commit `.env` or `data/` to public repositories or release artifacts.
- Server source code resides in [`server/`](server/README.md). Production container images ship only the compiled Rust binary and license/resource files—no Python runtime is included.
- Production deployments should load the corresponding commit's `Companion-linux-amd64` CI artifact image and set `OPS_IMAGE`, mounting only `/data`.
- Validation & read-only modes:
  - `--check-state`: Validates configuration, state files, and database connectivity, then exits.
  - `--read-only`: Starts the server without background workers or database/state writes for safe acceptance verification.

### Networking & Reverse Proxy

- Docker Compose publishes port `18081` to host loopback `127.0.0.1:18081` by default (listening on `0.0.0.0:18081` inside the container).
- Running `sub2ops-server` directly on a host binds to all interfaces by default; restrict to loopback when needed:
  ```bash
  sub2ops-server serve --host 127.0.0.1 --port 18081
  ```
- In production, mount Sub2Ops under `/sub2ops/` on the same domain as Sub2API via Nginx and disable query access logs for that location (see `deploy/nginx/sub2ops.location.conf`).
- Client management routes require a valid administrator API Key. `/connection-status` returns HTTP 200 with redacted authentication diagnostics (never exposing URLs, keys, or business data). `/healthz` and model catalog routes follow health-check and client authentication semantics respectively.
- Use TLS with a trusted CA and hostname verification (`sslmode=verify-full`) for cross-host PostgreSQL connections; `sslmode=disable` is only appropriate inside an isolated container network.

---

## Environment Variables

| Variable | Description |
|---|---|
| `SUB2API_CONFIG_PATH` | Standalone connection config file (default `/data/sub2api-config.json`). JSON `base_url` / `verify_base_url` take precedence over environment variables. |
| `SUB2API_BASE_URL` | Public base URL of the Sub2API instance. |
| `SUB2API_VERIFY_BASE_URL` | Optional internal base URL used by the server for upstream verification. |
| `GROUP_MODEL_CONFIG_PATH` | Group reasoning-effort config file (default `/data/group-model-config.json`), guarded by group version. |
| `KEEPALIVE_CONFIG_PATH` | Per-account keepalive settings, recovery budgets, and pending checks (default `/data/keepalive-state.json`, disabled by default). |
| `DATABASE_URL` | Sub2API PostgreSQL connection string. |
| `BASE_PATH` | Legacy compatibility prefix (default `/sub2ops`); stripped by Nginx externally. |
| `USAGE_QUERY_STATE_PATH` | OAuth snapshots, admin API Key, and scheduling metadata (default `/data/usage-query-state.json`). |
| `OAUTH_CONFIG_PATH` | OAuth automation config file (default `/data/oauth-config.json`). Precedence: JSON > env vars > defaults. |
| `BARK_CONFIG_PATH` | Bark notification config file (default `/data/bark-config.json`). |
| `BARK_ENABLED` | Enable Bark push notifications (default `false`). |
| `BARK_DEVICE_KEY` | Bark Device Key (recommended to configure via client Settings, written with `0600` permissions). |
| `BARK_SERVER_URL` | Bark server base URL (default `https://api.day.app`; HTTP allowed only for loopback). |
| `OAUTH_RECOVERY_MONITOR_ENABLED` | Monitor expired quota windows for recovery (default `true`). |
| `MODEL_DETECTION_MODEL_ID` | Global model ID for automated ModelTrace detection (default `gpt-6-luna`). |
| `OAUTH_AUTO_RESET_CREDIT_ENABLED` | Auto-reset credit when 7d usage hits 100% with active 429 rate limits (default `false`). |
| `OAUTH_DAILY_TEST_ENABLED` | Enable daily OpenAI OAuth health checks (default `true`; pushes Bark alerts on failure only). |
| `OAUTH_DAILY_TEST_TIME` | Daily test time in `HH:MM` Asia/Shanghai (default `05:00`). Waits for the next future occurrence. |
| `OAUTH_USAGE_REFRESH_CONCURRENCY` | Concurrency limit for active usage refreshes (default `4`). |
| `OAUTH_RECOVERY_TEST_CONCURRENCY` | Concurrency limit for account recovery tests (default `2`). |
| `OAUTH_EARLY_PROBE_BATCH_SIZE` | Legacy early-probe batch size (default `8`; retained for config compatibility). |
| `OAUTH_RECOVERY_TEST_MODEL_ID` | Model ID used for recovery health checks (default `gpt-5.6-luna`). |

---

## Dependency Governance & Quality Gates

- **Automated Updates**: Dependabot checks both Cargo lockfiles, frontend packages, Python test tooling, base Docker images, GitHub Actions, and Android libraries every Monday at 09:00 (`Asia/Taipei`) with a 7-day cooldown for routine releases and immediate PRs for security advisories.
- **Lockfile & Compatibility Boundaries**: Cargo uses `lockfile-only` rules that prevent unreviewed `0.x` breaking bumps; React and React DOM are updated in lockstep; Tauri JS packages, Rust core (`~2.12.5`), build helpers (`~2.7.4`), and plugins are verified together across macOS and Android builds.
- **CI Verification Gates**: Every release passes Rust/Node/Python vulnerability audits, reference parity suites, disposable PostgreSQL + Sub2API model catalog integration tests, Android emulator instrumentation smoke tests, and bundle size gates.
- **Platform Scope Note**: `glib 0.18.5` (`GHSA-wrw7-89jp-8q8g`) in the desktop lockfile belongs solely to undelivered Linux desktop targets and is excluded from macOS ARM64, Android ARM64, and server dependency graphs.

---

## Security

- **Minimal Exposure**: Sub2Ops holds administrative scheduling privileges and must never be exposed directly to the public internet. Bind only to loopback or internal container networks behind a TLS reverse proxy.
- **Credential Protection**: Server state and config files (`USAGE_QUERY_STATE_PATH`, `BARK_CONFIG_PATH`) and their lockfiles are written atomically with `0600` permissions. Secrets are never logged or echoed. Client instance API Keys are stored per `base_url` in the OS Keychain / Keystore.
- **Bark SSRF Guards**: Bark HTTPS targets reject non-public literal IP addresses (including IPv4-mapped IPv6 and alternate numeric forms), supporting `api.day.app` and public self-hosted domains while preserving HTTP loopback for local testing.
