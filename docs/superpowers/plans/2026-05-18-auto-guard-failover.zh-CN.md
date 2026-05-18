# 自动 Guard 故障转移实施方案

> **给 agentic workers 的要求：** 实施本方案时必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans`，逐任务执行。任务使用 checkbox（`- [ ]`）追踪。

**目标：** 在不修改 Sub2API 源码的前提下，优化 Sub2API Ops Companion 的自动 Guard：借鉴 `cc-switch` 的故障转移/熔断思路，并允许在 Guard 面板直接修改账号优先级和负载因子，让 Guard 可以先“降载”，再冷却或停用异常账号。

**架构：** Ops Companion 继续作为 sidecar 控制面存在：读取 Sub2API 数据库中的错误链路、成功日志和账号状态，写回 Sub2API 已存在的账号调度字段，并把 Companion 自己的 Guard 状态、策略和审计写到 `/data`。Companion 不在请求链路内，所以默认不做同请求重试；本方案实现的是近实时 future-request failover，也就是尽快影响下一次 Sub2API 调度。

**技术栈：** Python 3、FastAPI、Jinja 模板、PostgreSQL SQL 字符串、`/data` JSON 状态文件、现有 Telegram bot、现有 Sub2API 定时测试表。

---

## 关键结论

### `cc-switch` 可借鉴点

- `cc-switch` 只有在代理接管开启后才做真正的同请求 failover。
- `max_retries` 表示首次尝试后的重试次数，所以最大尝试数是 `max_retries + 1`。
- 熔断状态是 `Closed / Open / HalfOpen`，默认阈值是连续失败 4 次、半开成功 2 次、Open 等待 60 秒、错误率阈值 0.6、最小样本 10。
- 半开状态只允许一个探测请求。
- 可重试错误包括超时、转发失败、上游不健康、配置/认证/转换问题、流式 idle timeout、全部 5xx 和大多数 4xx；`400/405/406/413/414/415/422/501` 不重试。
- 这些能力依赖 `cc-switch` 拥有转发链路；Companion 不接管转发时不能声称做到同请求内 retry。

### Ops Companion 当前状态

- 当前 Guard 主逻辑在 `app/main.py`：`run_auto_guard_once()`、`pause_guard_candidate()`、`auto_guard_loop()`。
- 当前 Guard SQL 在 `app/sql.py`：`QUALITY_SQL`、`TELEGRAM_ERROR_ALERTS_SQL`、`GUARD_BALANCE_CANDIDATES_SQL`。
- 当前自动 Guard 只对余额/额度不足做永久停调度。
- 当前面板只展示 `accounts.priority AS account_priority` 和 `account_groups.priority AS group_priority`，不能编辑。
- 当前 Companion 没有任何 `load_factor` 读取、展示或写入。
- 当前账号操作只写 `schedulable`、`temp_unschedulable_until`、`temp_unschedulable_reason`、`updated_at`。

### Sub2API 上游字段

- 上游 Sub2API 真实存在 `accounts.load_factor`，迁移文件是 `backend/migrations/067_add_account_load_factor.sql`。
- 上游账号 schema 定义 `load_factor` 为可空整数，`priority` 为账号优先级，数字越小越优先。
- 上游 `EffectiveLoadFactor()` 语义：
  - `load_factor > 0`：使用 `load_factor` 作为有效负载容量。
  - `load_factor IS NULL`、`0`、负数：回退到 `accounts.concurrency`。
  - `concurrency <= 0` 时最终至少按 `1` 处理。
- 上游 OpenAI/Gateway 调度会用 `EffectiveLoadFactor()` 参与负载感知调度，所以 Companion 可以用 `load_factor=1` 作为软降载手段。

## 边界

Ops Companion 不修改 Sub2API 源码、不改 Sub2API 镜像、不改 Sub2API runtime。允许的操作只有：

- 读取现有 Sub2API 数据库表。
- 写现有账号状态字段：`schedulable`、`temp_unschedulable_until`、`temp_unschedulable_reason`、`updated_at`。
- 在能力探测通过后写现有账号调度字段：`accounts.priority`、`accounts.load_factor`。
- 写现有定时测试计划表。
- 写 Companion 自己的 `/data` 状态、策略和审计文件。
- 通过现有 Telegram bot 推送通知。

默认实现不是同请求内 failover，而是近实时控制面 failover：通过停用、冷却、降低负载因子，影响下一次 Sub2API 账号选择。同请求 retry 只能作为后续 sidecar proxy 阶段单独设计。

## 文件结构

- 新建 `app/guard_classifier.py`：Guard 错误分类器，统一 Python 侧分类。
- 新建 `app/guard_policy.py`：纯策略引擎，处理熔断、软降载、冷却、停用和恢复状态。
- 新建 `app/guard_store.py`：Companion 自有 JSON 状态存储。
- 新建 `app/guard_engine.py`：增量读取错误/成功事件，执行策略，写账号状态和审计。
- 修改 `app/sql.py`：增加 Guard 增量事件 SQL、成功事件 SQL、账号调度能力探测 SQL、账号调度更新 SQL，并投影 `account_priority`、`load_factor`、`effective_load_factor`。
- 修改 `app/account_ops.py`：增加 Guard 专用暂停、冷却、恢复、优先级/负载因子更新 helper。
- 修改 `app/main.py`：接入 `GuardEngine`，增加 `/guard/policy` 和 `/guard/account-routing`。
- 修改 `app/templates/guard.html`：Guard 面板增加策略、熔断状态、账号优先级/负载因子编辑。
- 修改 `app/static/style.css`：补齐 Guard 面板和小型调度参数表单样式。
- 修改 `app/telegram_bot.py`：自动 Guard 动作和恢复动作推送 Telegram，并附账号操作按钮。
- 新增测试：
  - `tests/test_guard_classifier.py`
  - `tests/test_guard_policy.py`
  - `tests/test_guard_store.py`
  - `tests/test_guard_engine.py`
  - `tests/test_guard_event_sql.py`
  - `tests/test_guard_account_routing.py`

---

## Task 1: 抽出 Guard 错误分类器

**文件：**
- 新建：`app/guard_classifier.py`
- 新建：`tests/test_guard_classifier.py`
- 修改：`app/sql.py`

- [ ] **Step 1: 写分类器测试**

`tests/test_guard_classifier.py` 覆盖这些情况：

```python
from app.guard_classifier import classify_guard_event


def event(**overrides):
    base = {
        "account_id": 9,
        "status_code": 403,
        "kind": "http_error:request_body_truncated",
        "error_owner": "provider",
        "error_source": "upstream",
        "message": "",
        "search_text": "",
    }
    base.update(overrides)
    return base


def test_pre_consume_quota_is_balance_before_rate_limit():
    row = event(search_text='{"code":"pre_consume_token_quota_failed","message":"token quota is not enough"}')
    assert classify_guard_event(row) == "provider_balance_or_quota"


def test_positive_remain_quota_is_not_balance_signal():
    assert classify_guard_event(event(search_text="RemainQuota = 12.30")) == "account_other_error"


def test_negative_remain_quota_is_balance_signal():
    assert classify_guard_event(event(search_text="RemainQuota = -0.01")) == "provider_balance_or_quota"


def test_client_error_is_ignored():
    assert classify_guard_event(event(status_code=400, search_text="Input must be a list")) == "client_bad_request"
```

- [ ] **Step 2: 运行失败测试**

```bash
python3 -m unittest tests.test_guard_classifier -v
```

预期：`app.guard_classifier` 不存在，测试失败。

- [ ] **Step 3: 实现分类器**

`app/guard_classifier.py` 使用纯函数实现，分类名必须和 SQL 保持一致：

```python
from __future__ import annotations

import re
from typing import Any


IGNORED_CLIENT_TERMS = ("input must be a list", "instructions are required")
BALANCE_TERMS = (
    "用户额度不足",
    "额度不足",
    "额度已用尽",
    "令牌额度已用尽",
    "预扣费额度失败",
    "剩余额度",
    "insufficient_user_quota",
    "insufficient balance",
    "insufficient_balance",
    "not enough credits",
    "pre_consume_token_quota_failed",
    "token quota is not enough",
    "quota exceeded",
)
RATE_LIMIT_TERMS = ("rate limit", "too many pending")
UNSTABLE_TERMS = ("terminal event", "missing terminal event", "truncated")
NEGATIVE_REMAIN_QUOTA_RE = re.compile(r"RemainQuota\s*=\s*-", re.IGNORECASE)


def _search_text(row: dict[str, Any]) -> str:
    return " ".join(
        str(row.get(key) or "")
        for key in ("kind", "message", "search_text", "error_message", "error_body")
    )


def classify_guard_event(row: dict[str, Any]) -> str:
    text = _search_text(row)
    lower = text.lower()
    status_code = int(row.get("status_code") or row.get("upstream_status_code") or 0)
    if not row.get("account_id"):
        return "client_pre_route"
    if row.get("error_owner") == "client" or row.get("error_source") == "client_request":
        return "client_request"
    if status_code == 400 and any(term in lower for term in IGNORED_CLIENT_TERMS):
        return "client_bad_request"
    if any(term in lower for term in BALANCE_TERMS) or NEGATIVE_REMAIN_QUOTA_RE.search(text):
        return "provider_balance_or_quota"
    if status_code == 403 and "blocked" in lower:
        return "provider_blocked_403"
    if status_code == 429 or any(term in lower for term in RATE_LIMIT_TERMS) or "quota" in lower:
        return "provider_rate_limit"
    if 500 <= status_code <= 599 or any(term in lower for term in UNSTABLE_TERMS):
        return "upstream_unstable_5xx_stream"
    return "account_other_error"
```

- [ ] **Step 4: SQL 加同步注释**

在 `QUALITY_SQL` 和 `GUARD_BALANCE_CANDIDATES_SQL` 前增加：

```python
# Keep category names and quota terms aligned with app.guard_classifier.
```

- [ ] **Step 5: 验证**

```bash
python3 -m unittest tests.test_guard_classifier tests.test_guard_quota_sql -v
```

预期：全部通过。

---

## Task 2: 增加 Guard 策略和软降载状态

**文件：**
- 新建：`app/guard_policy.py`
- 新建：`tests/test_guard_policy.py`

- [ ] **Step 1: 写策略测试**

重点覆盖余额硬停、429 软降载和冷却、5xx 达阈值后熔断、客户端错误忽略、成功恢复：

```python
from datetime import datetime, timezone

from app.guard_policy import GuardCircuit, GuardPolicy, GuardSignal, apply_signal


def now():
    return datetime(2026, 5, 18, 10, 0, tzinfo=timezone.utc)


def test_balance_quota_hard_pause():
    action, circuit = apply_signal(
        GuardPolicy(),
        GuardCircuit(account_id=9),
        GuardSignal(9, "provider_balance_or_quota", "error:101:1", now(), event_id=101),
        now(),
    )
    assert action.kind == "pause"
    assert action.hard is True
    assert circuit.state == "open"


def test_rate_limit_soft_lands_with_load_factor_then_cooldown():
    policy = GuardPolicy()
    action, circuit = apply_signal(
        policy,
        GuardCircuit(account_id=9),
        GuardSignal(9, "provider_rate_limit", "error:102:1", now(), event_id=102),
        now(),
    )
    assert action.kind == "cooldown"
    assert action.load_factor == 1
    assert action.minutes == 5
    assert circuit.last_applied_load_factor == 1


def test_client_bad_request_ignored():
    action, circuit = apply_signal(
        GuardPolicy(),
        GuardCircuit(account_id=9),
        GuardSignal(9, "client_bad_request", "error:103:1", now(), event_id=103),
        now(),
    )
    assert action.kind == "none"
    assert circuit.state == "closed"
```

- [ ] **Step 2: 运行失败测试**

```bash
python3 -m unittest tests.test_guard_policy -v
```

预期：`app.guard_policy` 不存在。

- [ ] **Step 3: 实现 dataclass**

`app/guard_policy.py` 核心结构：

```python
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime


IGNORED_CATEGORIES = {"client_pre_route", "client_request", "client_bad_request"}


@dataclass(slots=True)
class GuardPolicy:
    failure_threshold: int = 4
    success_threshold: int = 2
    circuit_timeout_seconds: int = 60
    error_rate_threshold: float = 0.6
    min_requests: int = 10
    rate_limit_cooldowns: tuple[int, ...] = (5, 15, 30)
    unstable_cooldowns: tuple[int, ...] = (5, 15, 30)
    rate_limit_load_factor_steps: tuple[int, ...] = (1, 1, 1)
    unstable_load_factor_steps: tuple[int, ...] = (1, 1, 1)
    blocked_403_threshold: int = 1
    balance_pause_threshold: int = 1


@dataclass(slots=True)
class GuardSignal:
    account_id: int
    category: str
    event_key: str
    created_at: datetime
    event_id: int | None = None
    message: str = ""


@dataclass(slots=True)
class GuardAction:
    kind: str
    account_id: int
    reason: str
    minutes: int | None = None
    load_factor: int | None = None
    hard: bool = False
    event_id: int | None = None


@dataclass(slots=True)
class GuardCircuit:
    account_id: int
    state: str = "closed"
    consecutive_failures: int = 0
    consecutive_successes: int = 0
    total_requests: int = 0
    failed_requests: int = 0
    rate_limit_level: int = 0
    unstable_level: int = 0
    original_load_factor: int | None = None
    last_applied_load_factor: int | None = None
    opened_at: str = ""
    last_event_id: int = 0
    last_event_key: str = ""
    last_category: str = ""
    last_message: str = ""
    processed_event_keys: list[str] = field(default_factory=list)
```

- [ ] **Step 4: 实现转移逻辑**

核心规则：

```python
def _slot(values: tuple[int, ...], level: int) -> int:
    return int(values[max(0, min(level, len(values) - 1))])


def apply_signal(policy: GuardPolicy, circuit: GuardCircuit, signal: GuardSignal, now: datetime) -> tuple[GuardAction, GuardCircuit]:
    if signal.event_key in set(circuit.processed_event_keys):
        return GuardAction("none", signal.account_id, "duplicate guard event", event_id=signal.event_id), circuit

    circuit.processed_event_keys = [*circuit.processed_event_keys, signal.event_key][-200:]
    circuit.last_event_key = signal.event_key
    circuit.last_category = signal.category
    circuit.last_message = signal.message
    if signal.event_id is not None:
        circuit.last_event_id = max(circuit.last_event_id, signal.event_id)

    if signal.category in IGNORED_CATEGORIES:
        return GuardAction("none", signal.account_id, "client-side error ignored", event_id=signal.event_id), circuit

    if signal.category == "success":
        circuit.consecutive_failures = 0
        circuit.consecutive_successes += 1
        if circuit.state == "half_open" and circuit.consecutive_successes >= policy.success_threshold:
            circuit.state = "closed"
            circuit.opened_at = ""
        elif circuit.state == "open":
            circuit.state = "half_open"
        return GuardAction("none", signal.account_id, "success recorded"), circuit

    circuit.total_requests += 1
    circuit.failed_requests += 1
    circuit.consecutive_failures += 1
    circuit.consecutive_successes = 0

    if signal.category == "provider_balance_or_quota":
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        return GuardAction("pause", signal.account_id, f"auto guard: balance/quota fault; {signal.message}", hard=True, event_id=signal.event_id), circuit

    if signal.category == "provider_rate_limit":
        minutes = _slot(policy.rate_limit_cooldowns, circuit.rate_limit_level)
        load_factor = _slot(policy.rate_limit_load_factor_steps, circuit.rate_limit_level)
        circuit.rate_limit_level += 1
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        circuit.last_applied_load_factor = load_factor
        return GuardAction("cooldown", signal.account_id, f"auto guard: provider rate limit; load_factor {load_factor}, cooldown {minutes}m", minutes=minutes, load_factor=load_factor, event_id=signal.event_id), circuit

    if signal.category == "upstream_unstable_5xx_stream" and circuit.consecutive_failures >= policy.failure_threshold:
        minutes = _slot(policy.unstable_cooldowns, circuit.unstable_level)
        load_factor = _slot(policy.unstable_load_factor_steps, circuit.unstable_level)
        circuit.unstable_level += 1
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        circuit.last_applied_load_factor = load_factor
        return GuardAction("cooldown", signal.account_id, f"auto guard: unstable upstream; load_factor {load_factor}, cooldown {minutes}m", minutes=minutes, load_factor=load_factor, event_id=signal.event_id), circuit

    return GuardAction("none", signal.account_id, "category recorded without automatic account action", event_id=signal.event_id), circuit
```

- [ ] **Step 5: 验证**

```bash
python3 -m unittest tests.test_guard_policy -v
```

---

## Task 3: 增加 Guard 状态存储

**文件：**
- 新建：`app/guard_store.py`
- 新建：`tests/test_guard_store.py`
- 修改：`app/settings.py`

- [ ] **Step 1: 写存储测试**

```python
from pathlib import Path
from tempfile import TemporaryDirectory

from app.guard_policy import GuardCircuit
from app.guard_store import GuardStore


def test_store_round_trips_state():
    with TemporaryDirectory() as tmp:
        path = Path(tmp) / "guard-state.json"
        store = GuardStore(str(path))
        store.set_error_cursor(42)
        store.set_success_cursor("2026-05-18T10:00:00+00:00")
        store.save_circuit(GuardCircuit(account_id=9, state="open", consecutive_failures=4))

        reloaded = GuardStore(str(path))
        assert reloaded.error_cursor() == 42
        assert reloaded.success_cursor() == "2026-05-18T10:00:00+00:00"
        assert reloaded.circuit(9).state == "open"
```

- [ ] **Step 2: 实现 `GuardStore`**

```python
from __future__ import annotations

import json
from dataclasses import asdict
from pathlib import Path
from typing import Any

from .guard_policy import GuardCircuit


class GuardStore:
    def __init__(self, path: str) -> None:
        self.path = Path(path)
        self._data = self._read()

    def _read(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return {"cursors": {}, "circuits": {}, "policy": {}}
        if not isinstance(data, dict):
            return {"cursors": {}, "circuits": {}, "policy": {}}
        data.setdefault("cursors", {})
        data.setdefault("circuits", {})
        data.setdefault("policy", {})
        return data

    def _write(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(self.path.suffix + ".tmp")
        tmp.write_text(json.dumps(self._data, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")
        tmp.replace(self.path)

    def error_cursor(self) -> int:
        return max(0, int((self._data.get("cursors") or {}).get("error_log_id") or 0))

    def set_error_cursor(self, value: int) -> None:
        self._data.setdefault("cursors", {})["error_log_id"] = max(0, int(value or 0))
        self._write()

    def success_cursor(self) -> str:
        return str((self._data.get("cursors") or {}).get("success_created_at") or "")

    def set_success_cursor(self, value: str) -> None:
        self._data.setdefault("cursors", {})["success_created_at"] = str(value or "")
        self._write()

    def circuit(self, account_id: int) -> GuardCircuit:
        raw = (self._data.get("circuits") or {}).get(str(int(account_id))) or {}
        return GuardCircuit(account_id=int(account_id), **{k: v for k, v in raw.items() if k != "account_id"})

    def save_circuit(self, circuit: GuardCircuit) -> None:
        self._data.setdefault("circuits", {})[str(int(circuit.account_id))] = asdict(circuit)
        self._write()

    def policy_config(self) -> dict[str, Any]:
        raw = self._data.get("policy") or {}
        return raw if isinstance(raw, dict) else {}

    def save_policy(self, policy: dict[str, Any]) -> None:
        self._data["policy"] = dict(policy)
        self._write()
```

- [ ] **Step 3: 增加设置项**

`app/settings.py`：

```python
guard_state_path: str = "/data/guard-state.json"
guard_event_batch_size: int = 100
```

`load_settings()`：

```python
guard_state_path=os.getenv("GUARD_STATE_PATH", "/data/guard-state.json"),
guard_event_batch_size=int_env("GUARD_EVENT_BATCH_SIZE", 100, 1, 500),
```

- [ ] **Step 4: 验证**

```bash
python3 -m unittest tests.test_guard_store -v
```

---

## Task 4: 增加增量 Guard SQL 和账号调度字段投影

**文件：**
- 修改：`app/sql.py`
- 新建：`tests/test_guard_event_sql.py`

- [ ] **Step 1: 写 SQL 形状测试**

```python
from app.sql import GUARD_ERROR_EVENTS_SQL, GUARD_SUCCESS_EVENTS_SQL


def test_guard_error_events_are_cursor_based():
    assert "e.id > %(cursor_id)s::bigint" in GUARD_ERROR_EVENTS_SQL
    assert "jsonb_array_elements" in GUARD_ERROR_EVENTS_SQL
    assert "attempt_no" in GUARD_ERROR_EVENTS_SQL
    assert "account_priority" in GUARD_ERROR_EVENTS_SQL
    assert "load_factor" in GUARD_ERROR_EVENTS_SQL


def test_guard_success_events_are_incremental():
    assert "usage_logs" in GUARD_SUCCESS_EVENTS_SQL
    assert "created_at > %(cursor_created_at)s::timestamptz" in GUARD_SUCCESS_EVENTS_SQL
    assert "success_event_key" in GUARD_SUCCESS_EVENTS_SQL
```

- [ ] **Step 2: 增加错误事件 SQL**

```python
GUARD_ERROR_EVENTS_SQL = """
SELECT
  e.id AS error_log_id,
  e.created_at,
  e.request_id,
  e.client_request_id,
  e.platform,
  e.model,
  x.ordinality AS attempt_no,
  COALESCE(
    CASE WHEN coalesce(x.elem->>'account_id','') ~ '^[0-9]+$' THEN (x.elem->>'account_id')::bigint END,
    e.account_id
  ) AS account_id,
  COALESCE(
    CASE
      WHEN lower(trim(coalesce(x.elem->>'account_name',''))) IN ('', 'none', 'null') THEN NULL
      ELSE x.elem->>'account_name'
    END,
    a.name
  ) AS account_name,
  a.priority AS account_priority,
  a.load_factor,
  COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
  COALESCE(
    CASE WHEN coalesce(x.elem->>'upstream_status_code','') ~ '^[0-9]+$' THEN (x.elem->>'upstream_status_code')::int END,
    e.upstream_status_code,
    e.status_code
  ) AS status_code,
  COALESCE(NULLIF(x.elem->>'kind',''), e.error_type) AS kind,
  e.error_owner,
  e.error_source,
  COALESCE(
    NULLIF(x.elem->>'detail',''),
    NULLIF(x.elem->>'message',''),
    NULLIF(x.elem->>'upstream_response_body',''),
    e.upstream_error_message,
    e.error_message,
    e.error_body,
    ''
  ) AS message,
  concat_ws(' ', x.elem::text, e.upstream_errors::text, e.upstream_error_message, e.error_message, e.error_body) AS search_text
FROM ops_error_logs e
LEFT JOIN accounts a ON a.id = e.account_id
LEFT JOIN LATERAL jsonb_array_elements(
  CASE
    WHEN jsonb_typeof(e.upstream_errors) = 'array' AND jsonb_array_length(e.upstream_errors) > 0 THEN e.upstream_errors
    ELSE '[{}]'::jsonb
  END
) WITH ORDINALITY AS x(elem, ordinality) ON true
WHERE e.id > %(cursor_id)s::bigint
ORDER BY e.id ASC, x.ordinality ASC
LIMIT %(limit)s::int;
"""
```

- [ ] **Step 3: 增加成功事件 SQL**

```python
GUARD_SUCCESS_EVENTS_SQL = """
SELECT
  account_id,
  max(created_at) AS success_created_at,
  ('success:' || account_id::text || ':' || max(created_at)::text) AS success_event_key,
  count(*) AS success_count,
  COALESCE(sum(output_tokens), 0) AS output_tokens,
  round(avg(duration_ms)::numeric, 0) AS avg_duration_ms,
  round(avg(first_token_ms)::numeric, 0) AS avg_first_token_ms
FROM usage_logs
WHERE account_id IS NOT NULL
  AND (%(cursor_created_at)s = '' OR created_at > %(cursor_created_at)s::timestamptz)
GROUP BY account_id
ORDER BY max(created_at) ASC
LIMIT %(limit)s::int;
"""
```

- [ ] **Step 4: 在质量 SQL 中投影负载因子**

`QUALITY_SQL` 的账号字段增加：

```sql
a.load_factor,
COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
```

如果旧部署没有 `accounts.load_factor`，实现时通过 Task 6 的 capability 检测走兼容查询或禁用 UI，不要修改 Sub2API schema。

- [ ] **Step 5: 验证**

```bash
python3 -m unittest tests.test_guard_event_sql tests.test_guard_quota_sql -v
```

---

## Task 5: 实现 Guard Engine

**文件：**
- 新建：`app/guard_engine.py`
- 修改：`app/main.py`
- 修改：`app/account_ops.py`
- 新建：`tests/test_guard_engine.py`

- [ ] **Step 1: 写 engine 测试**

```python
from datetime import datetime, timezone
from pathlib import Path
from tempfile import TemporaryDirectory

from app.guard_engine import GuardEngine
from app.guard_policy import GuardPolicy
from app.guard_store import GuardStore


class FakeDB:
    def __init__(self, rows):
        self.rows = rows
        self.updates = []

    def fetch_all(self, sql, params=None):
        return list(self.rows)

    def fetch_one(self, sql, params=None):
        self.updates.append((sql, params or {}))
        if "UPDATE accounts" in sql:
            return {"id": params["account_id"], "name": "wong", "schedulable": False, "priority": 50, "load_factor": 1}
        return None


def test_engine_pauses_quota_fault_and_advances_cursor():
    with TemporaryDirectory() as tmp:
        db = FakeDB([{
            "error_log_id": 101,
            "attempt_no": 1,
            "created_at": datetime(2026, 5, 18, 10, 0, tzinfo=timezone.utc),
            "account_id": 9,
            "account_name": "wong",
            "account_priority": 50,
            "load_factor": None,
            "status_code": 403,
            "message": "token quota is not enough",
            "search_text": "pre_consume_token_quota_failed token quota is not enough",
        }])
        engine = GuardEngine(db, GuardStore(str(Path(tmp) / "state.json")), str(Path(tmp) / "audit.jsonl"), GuardPolicy())
        actions = engine.run_once("test")
        assert actions[0]["action"] == "pause"
```

- [ ] **Step 2: 实现 engine**

`GuardEngine.run_once()`：

```python
rows = self.db.fetch_all(GUARD_ERROR_EVENTS_SQL, {"cursor_id": self.store.error_cursor(), "limit": self.batch_size})
for row in rows:
    account_id = row.get("account_id")
    if not account_id:
        continue
    category = classify_guard_event(row)
    signal = GuardSignal(
        account_id=int(account_id),
        category=category,
        event_key=f"error:{int(row.get('error_log_id') or 0)}:{int(row.get('attempt_no') or 1)}",
        created_at=row.get("created_at") or datetime.now(timezone.utc),
        event_id=int(row.get("error_log_id") or 0),
        message=str(row.get("message") or ""),
    )
    circuit = self.store.circuit(int(account_id))
    action, circuit = apply_signal(self.policy, circuit, signal, datetime.now(timezone.utc))
    self.store.save_circuit(circuit)
    applied = self._apply_action(action, actor, row)
```

`_apply_action()`：

```python
if action.kind == "pause":
    updated = account_ops.guard_pause_account(self.db, action.account_id, action.reason)
elif action.kind == "cooldown":
    if action.load_factor:
        self.apply_load_factor_soft_landing(action, actor, source_row)
    updated = account_ops.guard_cooldown_account(self.db, action.account_id, int(action.minutes or 15), action.reason)
```

- [ ] **Step 3: 接入 `app/main.py`**

```python
def guard_engine() -> GuardEngine:
    return GuardEngine(
        db=db,
        store=GuardStore(settings.guard_state_path),
        audit_path=settings.audit_path,
        policy=guard_policy_from_store(),
        batch_size=settings.guard_event_batch_size,
    )
```

`run_auto_guard_once()` 改为调用 `guard_engine().run_once(actor)`。

- [ ] **Step 4: 验证**

```bash
python3 -m unittest tests.test_guard_engine tests.test_guard_policy tests.test_guard_classifier -v
```

---

## Task 6: Guard 面板账号优先级和负载因子

**文件：**
- 修改：`app/sql.py`
- 修改：`app/account_ops.py`
- 修改：`app/main.py`
- 修改：`app/templates/guard.html`
- 修改：`app/static/style.css`
- 新建：`tests/test_guard_account_routing.py`

- [ ] **Step 1: 写账号调度字段测试**

```python
from app.account_ops import normalize_load_factor_value, normalize_priority_value
from app.sql import ACCOUNT_ROUTING_CAPABILITY_SQL, GUARD_ACCOUNT_PRIORITY_UPDATE_SQL, GUARD_ACCOUNT_ROUTING_UPDATE_SQL, QUALITY_SQL


def test_capability_and_update_sql_use_existing_columns():
    assert "table_name = 'accounts'" in ACCOUNT_ROUTING_CAPABILITY_SQL
    assert "column_name = 'priority'" in ACCOUNT_ROUTING_CAPABILITY_SQL
    assert "column_name = 'load_factor'" in ACCOUNT_ROUTING_CAPABILITY_SQL
    assert "UPDATE accounts" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "deleted_at IS NULL" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "load_factor = NULL" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "UPDATE accounts" in GUARD_ACCOUNT_PRIORITY_UPDATE_SQL


def test_quality_sql_exposes_routing_fields():
    assert "a.priority AS account_priority" in QUALITY_SQL
    assert "a.load_factor" in QUALITY_SQL
    assert "effective_load_factor" in QUALITY_SQL


def test_validation():
    assert normalize_priority_value("1") == 1
    assert normalize_priority_value("999") == 100
    assert normalize_priority_value("bad") == 50
    assert normalize_load_factor_value("") is None
    assert normalize_load_factor_value("0") is None
    assert normalize_load_factor_value("20") == 20
```

- [ ] **Step 2: 增加 capability SQL**

`app/sql.py`：

```python
ACCOUNT_ROUTING_CAPABILITY_SQL = """
SELECT
  EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'accounts'
      AND column_name = 'priority'
  ) AS account_priority_column_exists,
  EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'accounts'
      AND column_name = 'load_factor'
  ) AS account_load_factor_column_exists;
"""
```

- [ ] **Step 3: 增加更新 SQL**

```python
GUARD_ACCOUNT_ROUTING_UPDATE_SQL = """
UPDATE accounts
SET priority = %(priority)s::int,
    load_factor = CASE
      WHEN %(load_factor)s::int IS NULL OR %(load_factor)s::int <= 0 THEN NULL
      ELSE %(load_factor)s::int
    END,
    updated_at = now()
WHERE id = %(account_id)s::bigint
  AND deleted_at IS NULL
RETURNING id, name, priority AS account_priority, load_factor, concurrency, updated_at;
"""


GUARD_ACCOUNT_PRIORITY_UPDATE_SQL = """
UPDATE accounts
SET priority = %(priority)s::int,
    updated_at = now()
WHERE id = %(account_id)s::bigint
  AND deleted_at IS NULL
RETURNING id, name, priority AS account_priority, concurrency, updated_at;
"""
```

只在 capability 显示 `load_factor` 存在时使用 `GUARD_ACCOUNT_ROUTING_UPDATE_SQL`。

- [ ] **Step 4: 增加 helper**

`app/account_ops.py`：

```python
def normalize_priority_value(value: object, default: int = 50) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = default
    return max(1, min(100, parsed))


def normalize_load_factor_value(value: object) -> int | None:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        return None
    return parsed if parsed > 0 else None


def guard_update_account_routing(
    db: Database,
    audit_path: str,
    account_id: int,
    actor: str,
    priority: int,
    load_factor: int | None,
    load_factor_supported: bool,
    reason: str,
) -> dict[str, Any] | None:
    sql = GUARD_ACCOUNT_ROUTING_UPDATE_SQL if load_factor_supported else GUARD_ACCOUNT_PRIORITY_UPDATE_SQL
    params = {
        "account_id": int(account_id),
        "priority": normalize_priority_value(priority),
        "load_factor": normalize_load_factor_value(load_factor),
    }
    row = db.fetch_one(sql, params)
    write_audit(audit_path, "guard_account_routing_update", {
        "user": actor,
        "account": row,
        "params": params,
        "load_factor_supported": load_factor_supported,
        "reason": reason,
    })
    return row
```

- [ ] **Step 5: 增加 route**

`app/main.py`：

```python
def account_routing_capability() -> dict[str, bool]:
    row = db.fetch_one(ACCOUNT_ROUTING_CAPABILITY_SQL) or {}
    return {
        "priority": bool(row.get("account_priority_column_exists")),
        "load_factor": bool(row.get("account_load_factor_column_exists")),
    }


@app.post("/guard/account-routing")
def guard_account_routing_save(
    user: AuthUser,
    account_id: int = Form(...),
    priority: int = Form(50),
    load_factor: str = Form(""),
    reason: str = Form("manual guard routing update"),
) -> Response:
    capability = account_routing_capability()
    if not capability["priority"]:
        raise HTTPException(status_code=400, detail="accounts.priority column is not available")
    updated = account_ops.guard_update_account_routing(
        db,
        settings.audit_path,
        account_id,
        user,
        priority,
        account_ops.normalize_load_factor_value(load_factor),
        capability["load_factor"],
        reason,
    )
    if not updated:
        raise HTTPException(status_code=404, detail="account not found")
    return RedirectResponse(f"{settings.base_path}/guard?msg={quote('账号优先级/负载因子已保存')}", status_code=303)
```

`guard_config()` 增加：

```python
"account_routing": account_routing_capability(),
```

- [ ] **Step 6: Guard 面板增加调度参数表单**

`app/templates/guard.html` 的账号行增加：

```html
<form class="routing-mini-form" method="post" action="{{ base_path }}/guard/account-routing">
  <input type="hidden" name="account_id" value="{{ row.id or item.account_id }}" />
  <input type="hidden" name="reason" value="guard panel account routing update" />
  <label>优先级
    <input type="number" name="priority" min="1" max="100" value="{{ row.account_priority or 50 }}" />
  </label>
  <label>负载因子
    <input
      type="number"
      name="load_factor"
      min="1"
      value="{{ row.load_factor or '' }}"
      placeholder="{{ row.concurrency or 1 }}"
      {{ 'disabled' if not guard.account_routing.load_factor else '' }}
    />
  </label>
  <button type="submit">保存</button>
</form>
```

如果不支持 `load_factor`，显示：

```html
<span class="cell-sub">当前 Sub2API 数据库没有 accounts.load_factor，无法降载，只能改优先级。</span>
```

- [ ] **Step 7: 样式**

`app/static/style.css`：

```css
.routing-mini-form {
  display: grid;
  grid-template-columns: repeat(2, minmax(74px, 1fr)) auto;
  gap: 8px;
  align-items: end;
  min-width: 250px;
}

.routing-mini-form label {
  display: grid;
  gap: 4px;
  font-size: 12px;
  min-width: 0;
}

.routing-mini-form input {
  width: 100%;
  min-width: 0;
}

.routing-mini-form button {
  min-height: 34px;
  white-space: nowrap;
}
```

- [ ] **Step 8: Guard 自动软降载**

`GuardEngine._apply_action()` 在 cooldown 前执行：

```python
if action.kind == "cooldown" and action.load_factor:
    capability = self.account_routing_capability()
    if capability.get("load_factor"):
        account_ops.guard_update_account_routing(
            self.db,
            self.audit_path,
            action.account_id,
            actor,
            int(source_row.get("account_priority") or source_row.get("priority") or 50),
            int(action.load_factor),
            True,
            f"{action.reason}; automatic load-factor soft landing",
        )
```

余额/额度错误仍然直接 hard pause，不做软降载。

- [ ] **Step 9: 验证**

```bash
python3 -m unittest tests.test_guard_account_routing tests.test_time_range_filters -v
python3 -m compileall app tests
```

---

## Task 7: 重做 Guard 面板为操作面

**文件：**
- 修改：`app/templates/guard.html`
- 修改：`app/static/style.css`
- 修改：`app/main.py`

- [ ] **Step 1: 改文案**

标题说明改为：

```html
<p>自动 Guard 读取错误链路和成功记录，先对临时异常账号降载/冷却，对确定性坏账号停调度，并用定时测试结果恢复；不接管 Sub2API 请求转发。</p>
```

- [ ] **Step 2: 增加策略面板**

在 metrics 下方放置 `section.panel.guard-policy-panel`，保持紧凑，不嵌套卡片：

```html
<form class="guard-policy-grid" method="post" action="{{ base_path }}/guard/policy">
  <label>连续失败阈值
    <input type="number" name="failure_threshold" min="1" max="50" value="{{ guard.policy.failure_threshold }}" />
  </label>
  <label>恢复成功阈值
    <input type="number" name="success_threshold" min="1" max="20" value="{{ guard.policy.success_threshold }}" />
  </label>
  <label>半开等待秒数
    <input type="number" name="circuit_timeout_seconds" min="5" max="3600" value="{{ guard.policy.circuit_timeout_seconds }}" />
  </label>
  <button class="primary" type="submit">保存策略</button>
</form>
```

- [ ] **Step 3: 列表列**

Guard 表格保留账号、状态、最近信号、建议/自动动作、调度参数、执行。调度参数列使用 Task 6 的 `routing-mini-form`。

- [ ] **Step 4: 验证**

```bash
python3 -m compileall app tests
git diff --check
```

---

## Task 8: 行数据富化和 Telegram 推送

**文件：**
- 修改：`app/main.py`
- 修改：`app/telegram_bot.py`
- 修改：`tests/test_telegram_pairing.py`

- [ ] **Step 1: Guard 行富化**

```python
def enrich_guard_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    store = GuardStore(settings.guard_state_path)
    for row in rows:
        row["guard_circuit"] = store.circuit(int(row.get("id") or 0))
    return rows
```

用在 `guard_view()` 的 `load_quality()` 后。

- [ ] **Step 2: 自动 Guard 动作推送**

`auto_guard_loop()`：

```python
actions = await run_auto_guard_threaded()
if actions:
    await notify_telegram_account_alerts("自动 Guard 已处理账号异常", actions)
```

- [ ] **Step 3: Telegram 文案覆盖负载因子**

Guard 动作格式中包含：

```text
动作：冷却 15m
软降载：load_factor=1
原因：auto guard: provider rate limit
```

- [ ] **Step 4: 验证**

```bash
python3 -m unittest tests.test_telegram_pairing -v
```

---

## Task 9: 定时测试恢复联动 Guard circuit

**文件：**
- 修改：`app/main.py`
- 修改：`app/guard_engine.py`
- 修改：`tests/test_scheduled_tests.py`
- 修改：`tests/test_guard_policy.py`

- [ ] **Step 1: 增加恢复 helper**

```python
def record_recovery_success(self, account_id: int, result_id: int, message: str = "scheduled test recovered") -> None:
    signal = GuardSignal(
        account_id=int(account_id),
        category="success",
        event_key=f"recovery:{int(result_id)}",
        created_at=datetime.now(timezone.utc),
        event_id=int(result_id),
        message=message,
    )
    circuit = self.store.circuit(int(account_id))
    circuit.state = "half_open"
    _, circuit = apply_signal(self.policy, circuit, signal, datetime.now(timezone.utc))
    self.store.save_circuit(circuit)
```

- [ ] **Step 2: 在恢复推送循环中调用**

`telegram_recovery_alert_loop()` 在发送恢复通知前：

```python
engine = guard_engine()
for row in rows:
    engine.record_recovery_success(
        int(row["account_id"]),
        int(row["result_id"]),
        f"scheduled test success: {row.get('model_id') or ''}",
    )
```

- [ ] **Step 3: 验证**

```bash
python3 -m unittest tests.test_scheduled_tests tests.test_telegram_pairing tests.test_guard_policy -v
```

---

## Task 10: 保持余额/额度硬暂停兼容

**文件：**
- 修改：`app/main.py`
- 修改：`tests/test_guard_quota_sql.py`
- 修改：`README.md`

- [ ] **Step 1: 增加用户样例回归**

```python
def test_user_sample_quota_not_enough_remains_hard_pause_signal(self) -> None:
    sample = '{"error":{"code":"pre_consume_token_quota_failed","message":"token quota is not enough, token remain quota: ＄0.060062, need quota: ＄0.198744"}}'
    combined = QUALITY_SQL + GUARD_BALANCE_CANDIDATES_SQL
    self.assertIn("pre_consume_token_quota_failed", combined)
    self.assertIn("token quota is not enough", combined)
    self.assertIn("provider_balance_or_quota", combined)
```

- [ ] **Step 2: 兜底扫描**

增量 Guard 异常时，仍运行旧的余额/额度窗口扫描：

```python
def run_guard_balance_fallback(actor: str) -> list[dict[str, Any]]:
    candidates = db.fetch_all(
        GUARD_BALANCE_CANDIDATES_SQL,
        {"lookback_minutes": settings.guard_lookback_minutes, "threshold": settings.guard_balance_error_threshold},
    )
    return [pause_guard_candidate(row, actor) for row in candidates if row.get("id")]
```

- [ ] **Step 3: README 写明语义**

```markdown
自动 Guard 现在有三层：

1. 增量错误链路 Guard：按 `ops_error_logs.id` 近实时处理账号错误。
2. 软降载：429/5xx/流式错误先把 `accounts.load_factor` 降到 1，降低后续流量。
3. 兜底扫描：如果增量读取异常，仍按最近窗口扫描余额/额度错误并永久停调度。

`accounts.priority` 控制调度顺序，数字越小越优先；`accounts.load_factor` 控制有效负载容量，正数覆盖 `concurrency`，清空/0/负数回退到 `concurrency`。
```

- [ ] **Step 4: 验证**

```bash
python3 -m unittest tests.test_guard_quota_sql tests.test_guard_engine -v
```

---

## Task 11: 全量验证和上线

**文件：**
- 没有固定文件；验证发现问题时再修。

- [ ] **Step 1: 跑单元测试**

```bash
python3 -m unittest discover -s tests -v
```

预期：全部通过。

- [ ] **Step 2: 编译 Python**

```bash
python3 -m compileall app tests
```

预期：无语法错误。

- [ ] **Step 3: 空白检查**

```bash
git diff --check
```

预期：无输出，退出码 0。

- [ ] **Step 4: 确认没有污染 Sub2API**

```bash
git status --short
```

预期：只出现 Companion 仓库文件，不能出现 Sub2API 源码路径。

- [ ] **Step 5: 本地烟测**

```text
/sub2ops/guard 正常加载
保存 Guard 策略后回到 /sub2ops/guard
保存账号优先级/负载因子后回到 /sub2ops/guard
数据库没有 accounts.load_factor 时，负载因子输入禁用但优先级可保存
手动“立即扫描并执行”可用
Telegram 测试推送可用
定时恢复页面可用
```

- [ ] **Step 6: 提交**

```bash
git add app tests README.md
git commit -m "Improve guard failover with account routing controls"
```

- [ ] **Step 7: 只有用户明确要求时推送 main**

```bash
git push origin main
```

---

## 可选后续阶段：真正同请求故障转移代理

如果要做到 `cc-switch` 一样的同请求 retry，需要单独加 sidecar proxy，把它放在 Sub2API 前面或 Sub2API 到上游之间。它需要负责：

- 每次请求的账号/供应商选择。
- 每次 attempt 的错误分类。
- 流式首包和 idle timeout。
- HalfOpen 探测名额。
- 客户端看到错误前的同请求 retry。

这个阶段不默认实施，因为它改变网络拓扑和部署方式，风险比当前控制面 Guard 大。

## 验收标准

- 不修改 Sub2API 源码。
- 不创建新分支。
- Guard 面板可以编辑非删除账号的 `accounts.priority`。
- live DB 有 `accounts.load_factor` 时，Guard 面板可以编辑负载因子；没有该列时，负载因子输入禁用且页面不报错。
- 账号调度参数修改会写审计事件 `guard_account_routing_update`。
- 稳定性/速度面板默认排序仍保持“可调度优先 + 成功/样本优先”，不能因为新增优先级编辑而回到 priority-first。
- `pre_consume_token_quota_failed`、`token quota is not enough` 等余额/额度错误仍然硬暂停。
- 客户端错误不影响账号状态。
- 429/rate-limit 先做负载因子软降载，再按 5m/15m/30m 冷却。
- 5xx/stream/truncated 达阈值后熔断，并在支持时做负载因子软降载。
- 定时测试恢复成功会推进/关闭 Guard circuit，并推送 Telegram。
- 自动 Guard 动作会推送 Telegram，且附带账号操作按钮。
- `/sub2ops/guard` 在桌面和云机部署中都能正常使用。
- `python3 -m unittest discover -s tests -v`、`python3 -m compileall app tests`、`git diff --check` 通过后才能推送。
