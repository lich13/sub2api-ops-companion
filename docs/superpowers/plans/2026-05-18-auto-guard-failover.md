# Auto Guard Failover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve Sub2API Ops Companion automatic Guard with cc-switch-inspired failover and circuit-breaker behavior, and let the Guard panel edit account priority/load factor so Guard can degrade unhealthy accounts before cooling or pausing them.

**Architecture:** Ops Companion remains a sidecar control plane: it reads Sub2API database telemetry, classifies account-attributed error/success events, writes account schedulability/cooldown state plus safe account tuning fields through existing `accounts` columns, and stores Guard health state in Companion-owned files under `/data`. It cannot do same-request retry or provider switching unless a future sidecar proxy is placed in the request path; this plan therefore implements fast future-request suppression, load-factor throttling, recovery, Telegram visibility, and operator controls.

**Tech Stack:** Python 3, FastAPI, Jinja templates, PostgreSQL SQL strings, JSON state files under `/data`, existing Telegram bot loop, existing Sub2API scheduled-test tables.

---

## Source Findings

### cc-switch Behavior To Reuse

- `/tmp/cc-switch/src-tauri/src/proxy/provider_router.rs` uses current provider only when auto-failover is disabled, and failover queue order only when enabled.
- `/tmp/cc-switch/src-tauri/src/database/dao/failover.rs` orders failover queue by `COALESCE(sort_index, 999999), id ASC`.
- `/tmp/cc-switch/src-tauri/src/proxy/forwarder.rs` treats `max_retries` as retries after the first attempt, so total attempts are `max_retries + 1`, capped by provider count.
- `/tmp/cc-switch/src-tauri/src/proxy/circuit_breaker.rs` defaults are `failure_threshold=4`, `success_threshold=2`, `timeout_seconds=60`, `error_rate_threshold=0.6`, `min_requests=10`.
- cc-switch circuit states are `Closed`, `Open`, `HalfOpen`; half-open allows exactly one probe at a time.
- Half-open failure immediately reopens the circuit; enough half-open successes close the circuit.
- `/tmp/cc-switch/src-tauri/src/proxy/forwarder.rs` retries timeouts, forwarding failures, provider unhealthy/config/auth/transform errors, stream idle timeouts, all 5xx, and most 4xx except `400/405/406/413/414/415/422/501`.
- cc-switch can mark request success conservatively because it owns forwarding: non-streaming buffers the whole body, streaming waits for first chunk.
- `/tmp/cc-switch/src-tauri/src/proxy/failover_switch.rs` deduplicates switch actions by `app_type:provider_id` and only switches when proxy takeover is enabled.

### Ops Companion Current Guard

- `app/main.py` owns `guard_state`, `run_auto_guard_once()`, `run_auto_guard_threaded()`, `auto_guard_loop()`, and the `/guard` routes.
- `app/sql.py` defines `QUALITY_SQL`, `TELEGRAM_ERROR_ALERTS_SQL`, and `GUARD_BALANCE_CANDIDATES_SQL`.
- `GUARD_BALANCE_CANDIDATES_SQL` scans recent `ops_error_logs`, expands `upstream_errors`, and auto-pauses only balance/quota faults.
- `pause_guard_candidate()` permanently pauses by setting `accounts.schedulable=false`, clearing `temp_unschedulable_until`, and writing `temp_unschedulable_reason`.
- `guard_suggestion()` already suggests non-automatic actions for `provider_blocked_403`, high error rate, `upstream_unstable_5xx_stream`, and `provider_rate_limit`.
- Telegram error push already scans `ops_error_logs.id` incrementally and sends account action buttons.
- Scheduled-test recovery UI writes upstream `scheduled_test_plans`; Sub2API remains the runner and recovery owner.
- Recovery push already scans `scheduled_test_results.id` incrementally and only notifies successful `auto_recover=true` rows that actually updated the account.

### Sub2API Account Priority And Load Factor

- Ops Companion currently projects `accounts.priority AS account_priority` and `account_groups.priority AS group_priority`, but only displays them; it does not edit either field.
- Ops Companion currently has no `load_factor` reference in SQL, routes, templates, tests, or README.
- Upstream Sub2API has a real nullable `accounts.load_factor` column from migration `067_add_account_load_factor.sql`.
- Upstream Sub2API account schema defines `load_factor` as optional/nillable and `priority` as the account scheduler priority where smaller numbers are higher priority.
- Upstream `EffectiveLoadFactor()` semantics are: positive `load_factor` overrides concurrency capacity, while `NULL`, `0`, or negative values fall back to `accounts.concurrency`.
- Upstream gateway and OpenAI scheduler use `EffectiveLoadFactor()` as account max concurrency for load-aware routing, so Ops Companion can use `load_factor` as a soft degradation knob before cooldown or hard pause.
- This plan edits `accounts.priority` and `accounts.load_factor` only. It does not edit `account_groups.priority` unless a later requirement explicitly asks for group-level priority editing.

## Hard Boundary

Ops Companion must not modify Sub2API source files, Sub2API container image layers, or Sub2API runtime code. It may only:

- Read existing Sub2API database tables.
- Write existing account state columns already controlled by the panel: `schedulable`, `temp_unschedulable_until`, `temp_unschedulable_reason`, `updated_at`.
- Write existing account routing columns after schema capability detection: `accounts.priority` and, when present, `accounts.load_factor`.
- Write existing scheduled-test plan rows through the already-supported Sub2API tables.
- Store Companion-owned Guard state/config/audit files under `/data`.
- Send Telegram notifications through the existing Companion bot.

This means the default implementation is not true same-request failover. It is fast future-request failover by suppressing unhealthy accounts before the next Sub2API scheduling decision. Same-request retry would require a separate optional sidecar proxy in front of Sub2API, listed at the end as an out-of-default-scope phase.

## File Structure

- Create `app/guard_classifier.py`: Python classifier shared by Guard, UI suggestions, and tests. It mirrors the SQL categories and prevents one-off string drift.
- Create `app/guard_policy.py`: pure decision engine for category-to-action policy, circuit state transitions, cooldown escalation, and recovery decisions.
- Create `app/guard_store.py`: Companion-owned JSON state store for cursors, per-account circuit state, dedupe keys, and runtime policy config.
- Create `app/guard_engine.py`: orchestrates database event polling, classifier/policy/store, account updates, audit, and Telegram action summaries.
- Modify `app/sql.py`: add event-oriented SQL for incremental Guard polling and success polling, expose `account_priority` and capability-gated `load_factor`, and keep existing `GUARD_BALANCE_CANDIDATES_SQL` as a compatibility fallback.
- Modify `app/main.py`: replace inline Guard logic with `guard_engine`, keep route names and public behavior stable, add policy save endpoint and Guard account-routing save endpoint.
- Modify `app/settings.py`: add `guard_state_path` and `guard_event_batch_size`, and keep current environment defaults.
- Modify `app/account_ops.py`: add narrow helpers for Guard-owned pause/cooldown/resume and account routing field updates, reusing existing update semantics.
- Modify `app/templates/guard.html`: make the Guard page a practical operator surface: policy summary, circuit/account state, priority/load-factor controls, actions, and compact config controls.
- Modify `app/static/style.css`: style the Guard policy panel and table without nested cards.
- Modify `app/telegram_bot.py`: add Guard action/recovery notifications with account operation buttons, preserving pairing and cursor behavior.
- Add tests:
  - `tests/test_guard_classifier.py`
  - `tests/test_guard_policy.py`
  - `tests/test_guard_store.py`
  - `tests/test_guard_engine.py`
  - `tests/test_guard_event_sql.py`
  - `tests/test_guard_account_routing.py`
- Modify existing tests:
  - `tests/test_guard_quota_sql.py`
  - `tests/test_telegram_pairing.py`
  - `tests/test_scheduled_tests.py`

---

## Task 1: Extract A Shared Guard Classifier

**Files:**
- Create: `app/guard_classifier.py`
- Create: `tests/test_guard_classifier.py`
- Modify: `app/sql.py`

- [ ] **Step 1: Write classifier tests**

Create `tests/test_guard_classifier.py` with these cases:

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
    row = event(
        status_code=403,
        search_text='{"code":"pre_consume_token_quota_failed","message":"token quota is not enough"}',
    )
    assert classify_guard_event(row) == "provider_balance_or_quota"


def test_positive_remain_quota_is_not_balance_signal():
    row = event(search_text="RemainQuota = 12.30")
    assert classify_guard_event(row) == "account_other_error"


def test_negative_remain_quota_is_balance_signal():
    row = event(search_text="RemainQuota = -0.01")
    assert classify_guard_event(row) == "provider_balance_or_quota"


def test_client_bad_request_never_counts_against_account():
    row = event(status_code=400, search_text="Input must be a list")
    assert classify_guard_event(row) == "client_bad_request"


def test_retryable_provider_categories():
    assert classify_guard_event(event(status_code=429, search_text="rate limit")) == "provider_rate_limit"
    assert classify_guard_event(event(status_code=500, search_text="upstream reset")) == "upstream_unstable_5xx_stream"
    assert classify_guard_event(event(status_code=403, search_text="blocked by policy")) == "provider_blocked_403"
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
python3 -m unittest tests.test_guard_classifier -v
```

Expected before implementation: import failure for `app.guard_classifier`.

- [ ] **Step 3: Implement `app/guard_classifier.py`**

Use a pure function that accepts a database row dict and returns one of the existing category names:

```python
from __future__ import annotations

import re
from typing import Any


CLIENT_BAD_REQUEST_TERMS = (
    "input must be a list",
    "instructions are required",
)

BALANCE_OR_QUOTA_TERMS = (
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

RATE_LIMIT_TERMS = (
    "rate limit",
    "too many pending",
)

UNSTABLE_TERMS = (
    "terminal event",
    "missing terminal event",
    "truncated",
)

NEGATIVE_REMAIN_QUOTA_RE = re.compile(r"RemainQuota\s*=\s*-", re.IGNORECASE)


def _text(row: dict[str, Any]) -> str:
    parts = [
        row.get("kind"),
        row.get("message"),
        row.get("search_text"),
        row.get("error_message"),
        row.get("error_body"),
    ]
    return " ".join(str(part or "") for part in parts)


def classify_guard_event(row: dict[str, Any]) -> str:
    text = _text(row)
    lower = text.lower()
    status_code = int(row.get("status_code") or row.get("upstream_status_code") or 0)

    if not row.get("account_id"):
        return "client_pre_route"
    if row.get("error_owner") == "client" or row.get("error_source") == "client_request":
        return "client_request"
    if status_code == 400 and any(term in lower for term in CLIENT_BAD_REQUEST_TERMS):
        return "client_bad_request"
    if any(term in lower for term in BALANCE_OR_QUOTA_TERMS) or NEGATIVE_REMAIN_QUOTA_RE.search(text):
        return "provider_balance_or_quota"
    if status_code == 403 and "blocked" in lower:
        return "provider_blocked_403"
    if status_code == 429 or any(term in lower for term in RATE_LIMIT_TERMS) or "quota" in lower:
        return "provider_rate_limit"
    if 500 <= status_code <= 599 or any(term in lower for term in UNSTABLE_TERMS):
        return "upstream_unstable_5xx_stream"
    return "account_other_error"
```

- [ ] **Step 4: Keep SQL categories in sync**

Add a short comment above `QUALITY_SQL` and `GUARD_BALANCE_CANDIDATES_SQL`:

```python
# Keep category names and quota terms aligned with app.guard_classifier.
```

Do not remove SQL classification in this step; SQL is still used for dashboard aggregation.

- [ ] **Step 5: Run classifier and existing quota tests**

Run:

```bash
python3 -m unittest tests.test_guard_classifier tests.test_guard_quota_sql -v
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add app/guard_classifier.py app/sql.py tests/test_guard_classifier.py
git commit -m "Extract guard error classifier"
```

---

## Task 2: Add Guard Policy And Circuit State

**Files:**
- Create: `app/guard_policy.py`
- Create: `tests/test_guard_policy.py`

- [ ] **Step 1: Write policy tests**

Create `tests/test_guard_policy.py`:

```python
from datetime import datetime, timezone

from app.guard_policy import (
    GuardCircuit,
    GuardPolicy,
    GuardSignal,
    apply_signal,
)


def now():
    return datetime(2026, 5, 18, 10, 0, tzinfo=timezone.utc)


def test_balance_quota_immediately_hard_pauses():
    policy = GuardPolicy()
    circuit = GuardCircuit(account_id=9)
    action, updated = apply_signal(
        policy,
        circuit,
        GuardSignal(account_id=9, category="provider_balance_or_quota", event_key="error:101:1", event_id=101, created_at=now()),
        now(),
    )
    assert action.kind == "pause"
    assert action.hard is True
    assert updated.state == "open"


def test_rate_limit_escalates_to_allowed_cooldown_slots():
    policy = GuardPolicy()
    circuit = GuardCircuit(account_id=9)
    action1, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:101:1", now(), event_id=101), now())
    action2, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:102:1", now(), event_id=102), now())
    action3, circuit = apply_signal(policy, circuit, GuardSignal(9, "provider_rate_limit", "error:103:1", now(), event_id=103), now())
    assert [action1.minutes, action2.minutes, action3.minutes] == [5, 15, 30]


def test_unstable_errors_open_after_failure_threshold():
    policy = GuardPolicy(failure_threshold=4)
    circuit = GuardCircuit(account_id=9)
    action = None
    for index in range(4):
        action, circuit = apply_signal(
            policy,
            circuit,
            GuardSignal(9, "upstream_unstable_5xx_stream", f"error:{200 + index}:1", now(), event_id=200 + index),
            now(),
        )
    assert action.kind == "cooldown"
    assert action.minutes == 30
    assert circuit.state == "open"


def test_client_errors_are_ignored():
    policy = GuardPolicy()
    circuit = GuardCircuit(account_id=9)
    action, updated = apply_signal(policy, circuit, GuardSignal(9, "client_bad_request", "error:101:1", now(), event_id=101), now())
    assert action.kind == "none"
    assert updated.state == "closed"


def test_successes_close_half_open_after_threshold():
    policy = GuardPolicy(success_threshold=2)
    circuit = GuardCircuit(account_id=9, state="half_open", consecutive_failures=0)
    _, circuit = apply_signal(policy, circuit, GuardSignal(9, "success", "success:9:2026-05-18T10:00:00+00:00", now()), now())
    assert circuit.state == "half_open"
    _, circuit = apply_signal(policy, circuit, GuardSignal(9, "success", "success:9:2026-05-18T10:01:00+00:00", now()), now())
    assert circuit.state == "closed"
```

- [ ] **Step 2: Run the failing test**

```bash
python3 -m unittest tests.test_guard_policy -v
```

Expected before implementation: import failure for `app.guard_policy`.

- [ ] **Step 3: Implement policy dataclasses**

Create `app/guard_policy.py`:

```python
from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta


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

- [ ] **Step 4: Implement transition helpers**

Add below the dataclasses:

```python
def _slot(slots: tuple[int, ...], level: int) -> int:
    index = max(0, min(level, len(slots) - 1))
    return int(slots[index])


def _none(signal: GuardSignal, reason: str) -> GuardAction:
    return GuardAction(kind="none", account_id=signal.account_id, reason=reason, event_id=signal.event_id)


def _remember(circuit: GuardCircuit, signal: GuardSignal) -> GuardCircuit:
    processed = [*circuit.processed_event_keys, signal.event_key][-200:]
    if signal.event_id is not None:
        circuit.last_event_id = max(circuit.last_event_id, int(signal.event_id))
    circuit.last_event_key = signal.event_key
    circuit.last_category = signal.category
    circuit.last_message = signal.message
    circuit.processed_event_keys = processed
    return circuit


def apply_signal(
    policy: GuardPolicy,
    circuit: GuardCircuit,
    signal: GuardSignal,
    now: datetime,
) -> tuple[GuardAction, GuardCircuit]:
    if signal.event_key in set(circuit.processed_event_keys):
        return _none(signal, "duplicate guard event"), circuit

    if signal.category in IGNORED_CATEGORIES:
        return _none(signal, "client-side error ignored by guard"), _remember(circuit, signal)

    if signal.category == "success":
        circuit.total_requests += 1
        circuit.consecutive_failures = 0
        circuit.consecutive_successes += 1
        if circuit.state == "half_open" and circuit.consecutive_successes >= policy.success_threshold:
            circuit.state = "closed"
            circuit.opened_at = ""
        elif circuit.state == "open":
            circuit.state = "half_open"
        return _none(signal, "success recorded"), _remember(circuit, signal)

    circuit.total_requests += 1
    circuit.failed_requests += 1
    circuit.consecutive_failures += 1
    circuit.consecutive_successes = 0

    if signal.category == "provider_balance_or_quota":
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        return (
            GuardAction(
                kind="pause",
                account_id=signal.account_id,
                hard=True,
                event_id=signal.event_id,
                reason=f"auto guard: balance/quota fault; {signal.message or signal.category}",
            ),
            _remember(circuit, signal),
        )

    if signal.category == "provider_blocked_403" and circuit.consecutive_failures >= policy.blocked_403_threshold:
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        return (
            GuardAction(
                kind="pause",
                account_id=signal.account_id,
                hard=True,
                event_id=signal.event_id,
                reason=f"auto guard: blocked 403; {signal.message or signal.category}",
            ),
            _remember(circuit, signal),
        )

    if signal.category == "provider_rate_limit":
        minutes = _slot(policy.rate_limit_cooldowns, circuit.rate_limit_level)
        load_factor = _slot(policy.rate_limit_load_factor_steps, circuit.rate_limit_level)
        circuit.rate_limit_level += 1
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        circuit.last_applied_load_factor = load_factor
        return (
            GuardAction(
                kind="cooldown",
                account_id=signal.account_id,
                minutes=minutes,
                load_factor=load_factor,
                event_id=signal.event_id,
                reason=f"auto guard: provider rate limit; load_factor {load_factor}, cooldown {minutes}m",
            ),
            _remember(circuit, signal),
        )

    if signal.category == "upstream_unstable_5xx_stream":
        if circuit.consecutive_failures < policy.failure_threshold:
            return _none(signal, "unstable signal recorded below threshold"), _remember(circuit, signal)
        minutes = _slot(policy.unstable_cooldowns, circuit.unstable_level)
        load_factor = _slot(policy.unstable_load_factor_steps, circuit.unstable_level)
        circuit.unstable_level += 1
        circuit.state = "open"
        circuit.opened_at = now.isoformat()
        circuit.last_applied_load_factor = load_factor
        return (
            GuardAction(
                kind="cooldown",
                account_id=signal.account_id,
                minutes=minutes,
                load_factor=load_factor,
                event_id=signal.event_id,
                reason=f"auto guard: unstable upstream; load_factor {load_factor}, cooldown {minutes}m",
            ),
            _remember(circuit, signal),
        )

    return _none(signal, "category recorded without automatic account action"), _remember(circuit, signal)
```

- [ ] **Step 5: Run policy tests**

```bash
python3 -m unittest tests.test_guard_policy -v
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add app/guard_policy.py tests/test_guard_policy.py
git commit -m "Add guard policy circuit state"
```

---

## Task 3: Add Companion-Owned Guard State Store

**Files:**
- Create: `app/guard_store.py`
- Create: `tests/test_guard_store.py`
- Modify: `app/settings.py`

- [ ] **Step 1: Write store tests**

Create `tests/test_guard_store.py`:

```python
from pathlib import Path
from tempfile import TemporaryDirectory

from app.guard_policy import GuardCircuit
from app.guard_store import GuardStore


def test_store_round_trips_cursors_and_circuits():
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
        assert reloaded.circuit(9).consecutive_failures == 4


def test_store_handles_missing_file_as_empty_state():
    with TemporaryDirectory() as tmp:
        store = GuardStore(str(Path(tmp) / "missing.json"))
        assert store.error_cursor() == 0
        assert store.success_cursor() == ""
        assert store.circuit(9).state == "closed"
```

- [ ] **Step 2: Run failing test**

```bash
python3 -m unittest tests.test_guard_store -v
```

Expected before implementation: import failure for `app.guard_store`.

- [ ] **Step 3: Add settings fields**

Modify `Settings` in `app/settings.py`:

```python
    guard_state_path: str = "/data/guard-state.json"
    guard_event_batch_size: int = 100
```

Add to `load_settings()`:

```python
        guard_state_path=os.getenv("GUARD_STATE_PATH", "/data/guard-state.json"),
        guard_event_batch_size=int_env("GUARD_EVENT_BATCH_SIZE", 100, 1, 500),
```

- [ ] **Step 4: Implement JSON store**

Create `app/guard_store.py`:

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
            return {"cursors": {}, "circuits": {}}
        if not isinstance(data, dict):
            return {"cursors": {}, "circuits": {}}
        data.setdefault("cursors", {})
        data.setdefault("circuits", {})
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
        if not isinstance(raw, dict):
            raw = {}
        return GuardCircuit(account_id=int(account_id), **{k: v for k, v in raw.items() if k != "account_id"})

    def save_circuit(self, circuit: GuardCircuit) -> None:
        self._data.setdefault("circuits", {})[str(int(circuit.account_id))] = asdict(circuit)
        self._write()
```

- [ ] **Step 5: Run store tests**

```bash
python3 -m unittest tests.test_guard_store -v
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add app/settings.py app/guard_store.py tests/test_guard_store.py
git commit -m "Persist guard circuit state"
```

---

## Task 4: Add Incremental Guard Event SQL

**Files:**
- Modify: `app/sql.py`
- Create: `tests/test_guard_event_sql.py`

- [ ] **Step 1: Write SQL shape tests**

Create `tests/test_guard_event_sql.py`:

```python
from app.sql import GUARD_ERROR_EVENTS_SQL, GUARD_SUCCESS_EVENTS_SQL


def test_guard_error_events_are_cursor_based_and_expand_attempts():
    assert "e.id > %(cursor_id)s::bigint" in GUARD_ERROR_EVENTS_SQL
    assert "jsonb_array_elements" in GUARD_ERROR_EVENTS_SQL
    assert "error_log_id" in GUARD_ERROR_EVENTS_SQL
    assert "attempt_no" in GUARD_ERROR_EVENTS_SQL
    assert "account_priority" in GUARD_ERROR_EVENTS_SQL
    assert "load_factor" in GUARD_ERROR_EVENTS_SQL
    assert "ORDER BY e.id ASC" in GUARD_ERROR_EVENTS_SQL


def test_guard_error_events_include_full_payload_search_text():
    assert "x.elem::text" in GUARD_ERROR_EVENTS_SQL
    assert "e.upstream_errors::text" in GUARD_ERROR_EVENTS_SQL
    assert "upstream_response_body" in GUARD_ERROR_EVENTS_SQL


def test_guard_success_events_are_incremental():
    assert "usage_logs" in GUARD_SUCCESS_EVENTS_SQL
    assert "created_at > %(cursor_created_at)s::timestamptz" in GUARD_SUCCESS_EVENTS_SQL
    assert "success_event_key" in GUARD_SUCCESS_EVENTS_SQL
    assert "account_id IS NOT NULL" in GUARD_SUCCESS_EVENTS_SQL
```

- [ ] **Step 2: Run failing test**

```bash
python3 -m unittest tests.test_guard_event_sql -v
```

Expected before SQL constants exist: import failure.

- [ ] **Step 3: Add `GUARD_ERROR_EVENTS_SQL`**

Add to `app/sql.py`:

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
  concat_ws(
    ' ',
    NULLIF(x.elem->>'detail',''),
    NULLIF(x.elem->>'message',''),
    NULLIF(x.elem->>'upstream_response_body',''),
    e.upstream_error_message,
    e.error_message,
    e.error_body,
    x.elem::text,
    e.upstream_errors::text
  ) AS search_text
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

- [ ] **Step 4: Add `GUARD_SUCCESS_EVENTS_SQL`**

Add to `app/sql.py`:

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

- [ ] **Step 5: Run SQL tests**

```bash
python3 -m unittest tests.test_guard_event_sql tests.test_guard_quota_sql -v
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add app/sql.py tests/test_guard_event_sql.py
git commit -m "Add incremental guard event SQL"
```

---

## Task 5: Implement Guard Engine Without Changing Route Contracts

**Files:**
- Create: `app/guard_engine.py`
- Modify: `app/main.py`
- Modify: `app/account_ops.py`
- Create: `tests/test_guard_engine.py`

- [ ] **Step 1: Write engine tests with a fake database**

Create `tests/test_guard_engine.py`:

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
        self.queries = []
        self.updates = []

    def fetch_all(self, sql, params=None):
        self.queries.append((sql, params or {}))
        return list(self.rows)

    def fetch_one(self, sql, params=None):
        self.updates.append((sql, params or {}))
        if "UPDATE accounts" in sql:
            return {"id": params["account_id"], "name": "wong", "schedulable": False}
        return None


def row(**overrides):
    base = {
        "error_log_id": 101,
        "created_at": datetime(2026, 5, 18, 10, 0, tzinfo=timezone.utc),
        "account_id": 9,
        "account_name": "wong",
        "status_code": 403,
        "kind": "http_error",
        "error_owner": "provider",
        "error_source": "upstream",
        "message": "token quota is not enough",
        "search_text": "pre_consume_token_quota_failed token quota is not enough",
    }
    base.update(overrides)
    return base


def test_engine_pauses_quota_fault_and_advances_cursor():
    with TemporaryDirectory() as tmp:
        db = FakeDB([row()])
        store = GuardStore(str(Path(tmp) / "state.json"))
        engine = GuardEngine(db=db, store=store, audit_path=str(Path(tmp) / "audit.jsonl"), policy=GuardPolicy())
        actions = engine.run_once(actor="test")
        assert len(actions) == 1
        assert actions[0]["action"] == "pause"
        assert actions[0]["account_id"] == 9
        assert store.error_cursor() == 101


def test_engine_ignores_client_error_but_advances_cursor():
    with TemporaryDirectory() as tmp:
        db = FakeDB([row(error_owner="client", error_source="client_request", search_text="bad user input")])
        store = GuardStore(str(Path(tmp) / "state.json"))
        engine = GuardEngine(db=db, store=store, audit_path=str(Path(tmp) / "audit.jsonl"), policy=GuardPolicy())
        actions = engine.run_once(actor="test")
        assert actions == []
        assert store.error_cursor() == 101
        assert db.updates == []
```

- [ ] **Step 2: Run failing test**

```bash
python3 -m unittest tests.test_guard_engine -v
```

Expected before implementation: import failure for `app.guard_engine`.

- [ ] **Step 3: Add account update helpers**

Keep current `pause_account()`, `cooldown_account()`, and `resume_account()` behavior. Add Guard-specific wrappers in `app/account_ops.py` so Guard writes can have a distinct audit event name:

```python
def guard_pause_account(db: Database, account_id: int, reason: str) -> dict[str, Any] | None:
    return db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = false,
            temp_unschedulable_until = NULL,
            temp_unschedulable_reason = %(reason)s,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
          AND (schedulable = true OR temp_unschedulable_until IS NOT NULL)
        RETURNING id, name, schedulable, temp_unschedulable_until, temp_unschedulable_reason
        """,
        {"account_id": account_id, "reason": reason},
    )


def guard_cooldown_account(db: Database, account_id: int, minutes: int, reason: str) -> dict[str, Any] | None:
    minutes = max(1, min(1440, int(minutes or 15)))
    return db.fetch_one(
        """
        UPDATE accounts
        SET schedulable = true,
            temp_unschedulable_until = now() + (%(minutes)s::text || ' minutes')::interval,
            temp_unschedulable_reason = %(reason)s,
            updated_at = now()
        WHERE id = %(account_id)s
          AND deleted_at IS NULL
          AND schedulable = true
        RETURNING id, name, schedulable, temp_unschedulable_until, temp_unschedulable_reason
        """,
        {"account_id": account_id, "minutes": minutes, "reason": reason},
    )
```

- [ ] **Step 4: Implement `GuardEngine`**

Create `app/guard_engine.py`:

```python
from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

from . import account_ops
from .audit import write_audit
from .guard_classifier import classify_guard_event
from .guard_policy import GuardPolicy, GuardSignal, apply_signal
from .guard_store import GuardStore
from .sql import GUARD_ERROR_EVENTS_SQL, GUARD_SUCCESS_EVENTS_SQL


class GuardEngine:
    def __init__(self, db: Any, store: GuardStore, audit_path: str, policy: GuardPolicy, batch_size: int = 100) -> None:
        self.db = db
        self.store = store
        self.audit_path = audit_path
        self.policy = policy
        self.batch_size = batch_size

    def run_once(self, actor: str = "auto_guard") -> list[dict[str, Any]]:
        actions: list[dict[str, Any]] = []
        rows = self.db.fetch_all(
            GUARD_ERROR_EVENTS_SQL,
            {"cursor_id": self.store.error_cursor(), "limit": self.batch_size},
        )
        max_cursor = self.store.error_cursor()
        for row in rows:
            max_cursor = max(max_cursor, int(row.get("error_log_id") or 0))
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
            if applied:
                actions.append(applied)
        self.store.set_error_cursor(max_cursor)
        self._record_successes()
        if actions:
            write_audit(self.audit_path, "guard_auto_actions", {"actor": actor, "actions": actions})
        return actions

    def _apply_action(self, action: Any, actor: str, source_row: dict[str, Any]) -> dict[str, Any] | None:
        if action.kind == "pause":
            updated = account_ops.guard_pause_account(self.db, action.account_id, action.reason)
            if not updated:
                return None
            result = {
                "account_id": action.account_id,
                "name": updated.get("name") or source_row.get("account_name"),
                "action": "pause",
                "reason": action.reason,
                "event_id": action.event_id,
                "updated": updated,
                "actor": actor,
            }
            write_audit(self.audit_path, "guard_auto_pause_account", result)
            return result
        if action.kind == "cooldown":
            updated = account_ops.guard_cooldown_account(self.db, action.account_id, int(action.minutes or 15), action.reason)
            if not updated:
                return None
            result = {
                "account_id": action.account_id,
                "name": updated.get("name") or source_row.get("account_name"),
                "action": "cooldown",
                "minutes": action.minutes,
                "reason": action.reason,
                "event_id": action.event_id,
                "updated": updated,
                "actor": actor,
            }
            write_audit(self.audit_path, "guard_auto_cooldown_account", result)
            return result
        return None

    def _record_successes(self) -> None:
        rows = self.db.fetch_all(
            GUARD_SUCCESS_EVENTS_SQL,
            {"cursor_created_at": self.store.success_cursor(), "limit": self.batch_size},
        )
        latest = self.store.success_cursor()
        for row in rows:
            account_id = row.get("account_id")
            created_at = row.get("success_created_at")
            if not account_id or not created_at:
                continue
            signal = GuardSignal(
                account_id=int(account_id),
                category="success",
                event_key=str(row.get("success_event_key") or f"success:{account_id}:{created_at}"),
                created_at=created_at,
                message=f"{row.get('success_count') or 0} successful requests",
            )
            circuit = self.store.circuit(int(account_id))
            _, circuit = apply_signal(self.policy, circuit, signal, datetime.now(timezone.utc))
            self.store.save_circuit(circuit)
            latest = str(created_at)
        self.store.set_success_cursor(latest)
```

- [ ] **Step 5: Wire `app/main.py` to `GuardEngine`**

Replace inline `pause_guard_candidate()` use with a module-level engine factory:

```python
from .guard_engine import GuardEngine
from .guard_policy import GuardPolicy
from .guard_store import GuardStore
```

Add:

```python
def guard_engine() -> GuardEngine:
    return GuardEngine(
        db=db,
        store=GuardStore(settings.guard_state_path),
        audit_path=settings.audit_path,
        policy=GuardPolicy(),
        batch_size=settings.guard_event_batch_size,
    )
```

Change `run_auto_guard_once()` to:

```python
def run_auto_guard_once(actor: str = "auto_guard") -> list[dict[str, Any]]:
    guard_state["running"] = True
    guard_state["last_error"] = ""
    try:
        actions = guard_engine().run_once(actor)
        guard_state.update(
            {
                "last_run_at": datetime.now(timezone.utc).isoformat(),
                "last_actions": actions[:10],
            }
        )
        if not actions:
            write_audit(settings.audit_path, "guard_auto_noop", {"actor": actor, "reason": "no rows updated"})
        return actions
    except Exception as exc:
        guard_state["last_error"] = str(exc)
        write_audit(settings.audit_path, "guard_auto_error", {"actor": actor, "error": str(exc)})
        raise
    finally:
        guard_state["running"] = False
```

Keep the old `GUARD_BALANCE_CANDIDATES_SQL` fallback available until Task 11 validates production behavior.

- [ ] **Step 6: Run engine tests**

```bash
python3 -m unittest tests.test_guard_engine tests.test_guard_policy tests.test_guard_classifier -v
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add app/account_ops.py app/guard_engine.py app/main.py tests/test_guard_engine.py
git commit -m "Run guard from incremental event engine"
```

---

## Task 6: Add Runtime Policy Config

**Files:**
- Modify: `app/guard_store.py`
- Modify: `app/main.py`
- Modify: `app/templates/guard.html`
- Modify: `tests/test_guard_store.py`

- [ ] **Step 1: Extend store tests for policy config**

Add to `tests/test_guard_store.py`:

```python
def test_store_round_trips_policy_config():
    with TemporaryDirectory() as tmp:
        path = Path(tmp) / "guard-state.json"
        store = GuardStore(str(path))
        store.save_policy({"failure_threshold": 8, "success_threshold": 3, "rate_limit_cooldowns": [5, 15, 30]})
        reloaded = GuardStore(str(path))
        assert reloaded.policy_config()["failure_threshold"] == 8
        assert reloaded.policy_config()["success_threshold"] == 3
        assert reloaded.policy_config()["rate_limit_cooldowns"] == [5, 15, 30]
```

- [ ] **Step 2: Implement store policy methods**

Add to `GuardStore`:

```python
    def policy_config(self) -> dict[str, Any]:
        raw = self._data.get("policy") or {}
        return raw if isinstance(raw, dict) else {}

    def save_policy(self, policy: dict[str, Any]) -> None:
        self._data["policy"] = dict(policy)
        self._write()
```

- [ ] **Step 3: Add policy loader in `app/main.py`**

Add:

```python
def guard_policy_from_store() -> GuardPolicy:
    raw = GuardStore(settings.guard_state_path).policy_config()
    return GuardPolicy(
        failure_threshold=int_param(str(raw.get("failure_threshold")), 4, 1, 50),
        success_threshold=int_param(str(raw.get("success_threshold")), 2, 1, 20),
        circuit_timeout_seconds=int_param(str(raw.get("circuit_timeout_seconds")), 60, 5, 3600),
        blocked_403_threshold=int_param(str(raw.get("blocked_403_threshold")), 1, 1, 20),
        balance_pause_threshold=int_param(str(raw.get("balance_pause_threshold")), 1, 1, 20),
    )
```

Use `guard_policy_from_store()` in `guard_engine()`.

- [ ] **Step 4: Add route to save policy**

Add to `app/main.py`:

```python
@app.post("/guard/policy")
def guard_policy_save(
    user: AuthUser,
    failure_threshold: int = Form(4),
    success_threshold: int = Form(2),
    circuit_timeout_seconds: int = Form(60),
    blocked_403_threshold: int = Form(1),
    balance_pause_threshold: int = Form(1),
) -> Response:
    payload = {
        "failure_threshold": int_param(str(failure_threshold), 4, 1, 50),
        "success_threshold": int_param(str(success_threshold), 2, 1, 20),
        "circuit_timeout_seconds": int_param(str(circuit_timeout_seconds), 60, 5, 3600),
        "blocked_403_threshold": int_param(str(blocked_403_threshold), 1, 1, 20),
        "balance_pause_threshold": int_param(str(balance_pause_threshold), 1, 1, 20),
        "updated_by": user,
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    GuardStore(settings.guard_state_path).save_policy(payload)
    write_audit(settings.audit_path, "guard_policy_update", payload)
    return RedirectResponse(f"{settings.base_path}/guard?msg={quote('Guard 策略已保存')}", status_code=303)
```

- [ ] **Step 5: Show policy in `guard_config()`**

Add:

```python
        "policy": guard_policy_from_store(),
```

If Jinja cannot directly render dataclass fields in the target runtime, convert with `dataclasses.asdict()`.

- [ ] **Step 6: Run config tests**

```bash
python3 -m unittest tests.test_guard_store -v
python3 -m compileall app tests
```

Expected: all tests and compile pass.

- [ ] **Step 7: Commit**

```bash
git add app/guard_store.py app/main.py app/templates/guard.html tests/test_guard_store.py
git commit -m "Add guard runtime policy config"
```

---

## Task 7: Add Guard Account Routing Controls

**Files:**
- Modify: `app/sql.py`
- Modify: `app/account_ops.py`
- Modify: `app/main.py`
- Modify: `app/templates/guard.html`
- Modify: `app/static/style.css`
- Create: `tests/test_guard_account_routing.py`
- Modify: `tests/test_time_range_filters.py`

- [ ] **Step 1: Write capability and route tests**

Create `tests/test_guard_account_routing.py`:

```python
from pathlib import Path
from tempfile import TemporaryDirectory

from app.account_ops import normalize_load_factor_value, normalize_priority_value
from app.sql import ACCOUNT_ROUTING_CAPABILITY_SQL, GUARD_ACCOUNT_ROUTING_UPDATE_SQL, QUALITY_SQL


def test_account_routing_sql_uses_existing_sub2api_columns():
    assert "table_name = 'accounts'" in ACCOUNT_ROUTING_CAPABILITY_SQL
    assert "column_name = 'priority'" in ACCOUNT_ROUTING_CAPABILITY_SQL
    assert "column_name = 'load_factor'" in ACCOUNT_ROUTING_CAPABILITY_SQL
    assert "UPDATE accounts" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "priority = %(priority)s::int" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "load_factor = NULL" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "load_factor = %(load_factor)s::int" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL
    assert "deleted_at IS NULL" in GUARD_ACCOUNT_ROUTING_UPDATE_SQL


def test_quality_sql_exposes_account_priority_and_load_factor():
    assert "a.priority AS account_priority" in QUALITY_SQL
    assert "a.load_factor" in QUALITY_SQL
    assert "effective_load_factor" in QUALITY_SQL


def test_priority_validation_matches_sub2api_scheduler_range():
    assert normalize_priority_value("1") == 1
    assert normalize_priority_value("50") == 50
    assert normalize_priority_value("999") == 100
    assert normalize_priority_value("bad") == 50


def test_load_factor_validation_allows_clear_and_positive_values():
    assert normalize_load_factor_value("") is None
    assert normalize_load_factor_value("0") is None
    assert normalize_load_factor_value("-1") is None
    assert normalize_load_factor_value("1") == 1
    assert normalize_load_factor_value("20") == 20
```

- [ ] **Step 2: Run failing tests**

```bash
python3 -m unittest tests.test_guard_account_routing -v
```

Expected before implementation: imports fail for the new SQL constants and helper functions.

- [ ] **Step 3: Add capability SQL and routing update SQL**

Add to `app/sql.py`:

```python
ACCOUNT_ROUTING_CAPABILITY_SQL = """
SELECT
  EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'accounts'
      AND column_name = 'priority'
  ) AS account_priority_column_exists,
  EXISTS (
    SELECT 1
    FROM information_schema.columns
    WHERE table_schema = 'public'
      AND table_name = 'accounts'
      AND column_name = 'load_factor'
  ) AS account_load_factor_column_exists;
"""


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
```

Important implementation note: only run `GUARD_ACCOUNT_ROUTING_UPDATE_SQL` when `ACCOUNT_ROUTING_CAPABILITY_SQL.account_load_factor_column_exists` is true. If the live DB lacks `accounts.load_factor`, use a priority-only SQL constant:

```python
GUARD_ACCOUNT_PRIORITY_UPDATE_SQL = """
UPDATE accounts
SET priority = %(priority)s::int,
    updated_at = now()
WHERE id = %(account_id)s::bigint
  AND deleted_at IS NULL
RETURNING id, name, priority AS account_priority, concurrency, updated_at;
"""
```

- [ ] **Step 4: Project load factor in account rows**

Modify `QUALITY_SQL` group account projection:

```sql
    a.concurrency,
    a.load_factor,
    COALESCE(NULLIF(a.load_factor, 0), NULLIF(a.concurrency, 0), 1) AS effective_load_factor,
```

Modify `fallback_account()` and scheduled-test account SQL to include:

```sql
          load_factor,
          COALESCE(NULLIF(load_factor, 0), NULLIF(concurrency, 0), 1) AS effective_load_factor,
```

If an older live DB lacks `load_factor`, hide the field in Guard via capability detection rather than removing the SQL from the plan. The implementation can use a compatibility SELECT only for old schemas.

- [ ] **Step 5: Add validation and update helper**

Add to `app/account_ops.py`:

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
    write_audit(
        audit_path,
        "guard_account_routing_update",
        {"user": actor, "account": row, "params": params, "load_factor_supported": load_factor_supported, "reason": reason},
    )
    return row
```

Add imports for `GUARD_ACCOUNT_PRIORITY_UPDATE_SQL` and `GUARD_ACCOUNT_ROUTING_UPDATE_SQL`.

- [ ] **Step 6: Add capability loader and POST route**

Add to `app/main.py`:

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
        account_ops.normalize_priority_value(priority),
        account_ops.normalize_load_factor_value(load_factor),
        capability["load_factor"],
        reason,
    )
    if not updated:
        raise HTTPException(status_code=404, detail="account not found")
    return RedirectResponse(f"{settings.base_path}/guard?msg={quote('账号优先级/负载因子已保存')}", status_code=303)
```

Add `"account_routing": account_routing_capability()` to `guard_config()`.

- [ ] **Step 7: Use load factor as Guard soft landing**

In `GuardEngine._apply_action()`, when `action.kind == "cooldown"` and `action.load_factor` is positive:

```python
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

Then apply cooldown. This makes rate-limit/unstable accounts receive less traffic before or during cooldown, while balance/quota remains hard pause.

- [ ] **Step 8: Add Guard table controls**

In `app/templates/guard.html`, add a column `调度参数` to the suggestions/account table. Each row should show a compact form:

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

When load factor is unsupported, show a small disabled note: `当前 Sub2API 数据库没有 accounts.load_factor，无法降载，只能改优先级。`

- [ ] **Step 9: Add stable CSS for routing controls**

Add to `app/static/style.css`:

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

- [ ] **Step 10: Run routing tests**

```bash
python3 -m unittest tests.test_guard_account_routing tests.test_time_range_filters -v
python3 -m compileall app tests
```

Expected: all tests pass and Python compilation succeeds.

- [ ] **Step 11: Commit**

```bash
git add app/sql.py app/account_ops.py app/main.py app/templates/guard.html app/static/style.css tests/test_guard_account_routing.py tests/test_time_range_filters.py
git commit -m "Add guard account routing controls"
```

---

## Task 8: Redesign Guard Panel As An Operator Surface

**Files:**
- Modify: `app/templates/guard.html`
- Modify: `app/static/style.css`
- Modify: `app/main.py`

- [ ] **Step 1: Replace ambiguous copy with boundary-aware copy**

The page title text should say:

```html
<p>自动 Guard 读取错误链路和成功记录，快速暂停确定性坏账号、冷却临时异常账号，并用定时测试结果恢复；不接管 Sub2API 请求转发。</p>
```

- [ ] **Step 2: Add compact policy controls**

Place one full-width `section.panel.guard-policy-panel` below metrics. Use compact grid fields, not nested cards:

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
  <label>403 阈值
    <input type="number" name="blocked_403_threshold" min="1" max="20" value="{{ guard.policy.blocked_403_threshold }}" />
  </label>
  <label>额度阈值
    <input type="number" name="balance_pause_threshold" min="1" max="20" value="{{ guard.policy.balance_pause_threshold }}" />
  </label>
  <button class="primary" type="submit">保存策略</button>
</form>
```

- [ ] **Step 3: Show actions and state in separate tables**

Keep the existing suggestions table, but rename the first table to `自动动作与人工建议`. Add columns:

```html
<th data-col="state">状态</th>
<th data-col="signal">最近信号</th>
<th data-col="next">下一次恢复</th>
```

Populate these from `row.guard_circuit` after Task 9 adds row enrichment.

- [ ] **Step 4: Add CSS with stable dimensions**

Add to `app/static/style.css`:

```css
.guard-policy-panel {
  display: grid;
  gap: 14px;
}

.guard-policy-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  align-items: end;
  gap: 12px;
}

.guard-policy-grid label {
  min-width: 0;
}

.guard-policy-grid input {
  width: 100%;
}

.guard-policy-grid .primary {
  min-height: 38px;
}

.guard-state-pill {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 0 8px;
  border-radius: 999px;
  font-size: 12px;
  white-space: nowrap;
}
```

- [ ] **Step 5: Run template checks**

```bash
python3 -m compileall app tests
git diff --check
```

Expected: compile passes and diff check has no whitespace errors.

- [ ] **Step 6: Commit**

```bash
git add app/templates/guard.html app/static/style.css app/main.py
git commit -m "Improve guard operator panel"
```

---

## Task 9: Add Guard State To Rows And Telegram Messages

**Files:**
- Modify: `app/main.py`
- Modify: `app/telegram_bot.py`
- Modify: `tests/test_telegram_pairing.py`

- [ ] **Step 1: Add row enrichment**

In `app/main.py`, add:

```python
def enrich_guard_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    store = GuardStore(settings.guard_state_path)
    for row in rows:
        row["guard_circuit"] = store.circuit(int(row.get("id") or 0))
    return rows
```

Use it in `guard_view()` immediately after `load_quality()`.

- [ ] **Step 2: Add Telegram action formatting tests**

Extend `tests/test_telegram_pairing.py` with:

```python
from app.telegram_bot import format_guard_actions


def test_guard_action_message_includes_circuit_action_and_account_buttons_context():
    text = format_guard_actions(
        "自动 Guard 已处理",
        [{"account_id": 9, "name": "wong", "action": "cooldown", "minutes": 15, "reason": "auto guard: provider rate limit"}],
    )
    assert "自动 Guard 已处理" in text
    assert "#9 wong" in text
    assert "cooldown" in text or "冷却" in text
    assert "provider rate limit" in text
```

- [ ] **Step 3: Keep Telegram buttons tied to accounts**

Verify `notify_account_alerts()` uses `account_actions_keyboard(account_id)`. If it does not for Guard actions, change it so every Guard notification with `account_id` has:

```python
account_actions_keyboard(int(item["account_id"]))
```

The buttons should remain the practical actions already used by error alerts: pause, resume, cooldown 5m, cooldown 15m, cooldown 30m.

- [ ] **Step 4: Notify automatic Guard actions from loop**

In `auto_guard_loop()`, after `actions = await run_auto_guard_threaded()`, call:

```python
if actions:
    await notify_telegram_account_alerts("自动 Guard 已处理账号异常", actions)
```

Keep exception notification unchanged.

- [ ] **Step 5: Run Telegram tests**

```bash
python3 -m unittest tests.test_telegram_pairing -v
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add app/main.py app/telegram_bot.py tests/test_telegram_pairing.py
git commit -m "Notify telegram for guard actions"
```

---

## Task 10: Integrate Scheduled-Test Recovery With Guard Circuits

**Files:**
- Modify: `app/main.py`
- Modify: `app/guard_engine.py`
- Modify: `tests/test_scheduled_tests.py`
- Modify: `tests/test_guard_policy.py`

- [ ] **Step 1: Add recovery state test**

Add to `tests/test_guard_policy.py`:

```python
def test_recovery_success_closes_open_circuit_after_threshold():
    policy = GuardPolicy(success_threshold=1)
    circuit = GuardCircuit(account_id=9, state="open", consecutive_failures=4)
    _, circuit = apply_signal(policy, circuit, GuardSignal(9, "success", "recovery:999", now(), event_id=999), now())
    assert circuit.state in {"half_open", "closed"}
```

For `success_threshold=1`, adjust `apply_signal()` so an open circuit goes to `closed` on a trusted scheduled-test recovery success.

- [ ] **Step 2: Add an explicit trusted recovery helper**

In `app/guard_engine.py`, add:

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

- [ ] **Step 3: Call recovery helper inside Telegram recovery loop**

In `telegram_recovery_alert_loop()`, after rows are loaded and before notification:

```python
engine = guard_engine()
for row in rows:
    engine.record_recovery_success(
        int(row["account_id"]),
        int(row["result_id"]),
        f"scheduled test success: {row.get('model_id') or ''}",
    )
```

- [ ] **Step 4: Preserve recovery notification behavior**

Run:

```bash
python3 -m unittest tests.test_scheduled_tests tests.test_telegram_pairing tests.test_guard_policy -v
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add app/main.py app/guard_engine.py tests/test_scheduled_tests.py tests/test_guard_policy.py
git commit -m "Close guard circuits on scheduled recovery"
```

---

## Task 11: Keep Balance/Quota Hard Pause Backward Compatible

**Files:**
- Modify: `app/main.py`
- Modify: `tests/test_guard_quota_sql.py`
- Modify: `README.md`

- [ ] **Step 1: Add a regression for the user sample**

In `tests/test_guard_quota_sql.py`, keep the existing `pre_consume_token_quota_failed` sample and add this exact assertion:

```python
def test_user_sample_quota_not_enough_remains_hard_pause_signal(self) -> None:
    sample = """
    {"error":{"code":"pre_consume_token_quota_failed","message":"token quota is not enough, token remain quota: ＄0.060062, need quota: ＄0.198744"}}
    """
    combined = QUALITY_SQL + GUARD_BALANCE_CANDIDATES_SQL
    self.assertIn("pre_consume_token_quota_failed", combined)
    self.assertIn("token quota is not enough", combined)
    self.assertNotIn("provider_rate_limit' THEN 'provider_balance_or_quota", combined)
    self.assertIn("provider_balance_or_quota", combined)
```

- [ ] **Step 2: Keep fallback scan available**

If incremental polling fails, `run_auto_guard_once()` should run the existing `GUARD_BALANCE_CANDIDATES_SQL` fallback before raising. Add:

```python
def run_guard_balance_fallback(actor: str) -> list[dict[str, Any]]:
    candidates = db.fetch_all(
        GUARD_BALANCE_CANDIDATES_SQL,
        {
            "lookback_minutes": settings.guard_lookback_minutes,
            "threshold": settings.guard_balance_error_threshold,
        },
    )
    return [pause_guard_candidate(row, actor) for row in candidates if row.get("id")]
```

Use it only inside the exception path for event polling failures, and audit `guard_auto_fallback_balance_scan`.

- [ ] **Step 3: Update README Guard section**

Document:

```markdown
自动 Guard 现在有两层：

1. 增量错误链路 Guard：按 `ops_error_logs.id` 近实时处理账号错误。
2. 余额/额度兜底扫描：如果增量读取异常，仍按最近窗口扫描确定性余额/额度错误并永久停调度。

余额/额度错误仍是硬暂停；429/5xx/流式中断先把 `accounts.load_factor` 降到 1 作为软降载，再按 5m/15m/30m 临时冷却；定时测试成功后会关闭 Guard circuit 并推送 Telegram。`accounts.priority` 控制调度顺序，数字越小越优先；`accounts.load_factor` 是有效负载容量，正数覆盖 `concurrency`，清空/0/负数回退到 `concurrency`。
```

- [ ] **Step 4: Run regression tests**

```bash
python3 -m unittest tests.test_guard_quota_sql tests.test_guard_engine -v
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add app/main.py tests/test_guard_quota_sql.py README.md
git commit -m "Preserve quota hard pause fallback"
```

---

## Task 12: Full Verification And Rollout

**Files:**
- No code files unless verification finds issues.

- [ ] **Step 1: Run unit tests**

```bash
python3 -m unittest discover -s tests -v
```

Expected: all tests pass.

- [ ] **Step 2: Compile Python**

```bash
python3 -m compileall app tests
```

Expected: compile completes with no syntax errors.

- [ ] **Step 3: Check whitespace**

```bash
git diff --check
```

Expected: no output and exit code 0.

- [ ] **Step 4: Confirm Sub2API source stayed untouched**

Run from `/Users/gosu/Documents/sub2api-ops-companion`:

```bash
git status --short
```

Expected: only Companion repo files changed. There must be no Sub2API repo paths in the output.

- [ ] **Step 5: Local smoke test with app server**

Start the app with the same local environment normally used for this repo. Then verify:

```text
/sub2ops/guard loads
Guard policy save redirects back to /sub2ops/guard
Guard account priority and load factor save redirects back to /sub2ops/guard
Manual "立即扫描并执行" still works
Telegram test push still works if token is configured
Scheduled-test page still loads and existing plans render
```

- [ ] **Step 6: Commit final fixes if needed**

If verification required fixes:

```bash
git add app tests README.md
git commit -m "Stabilize guard failover implementation"
```

- [ ] **Step 7: Push only when the user asks for remote update**

The user has explicitly said not to create branches. If pushing is requested, push current `main` directly:

```bash
git push origin main
```

Expected: remote `main` accepts the push.

---

## Optional Phase: True Same-Request Failover Proxy

This is out of the default plan because it changes the network architecture, even though it still can preserve the no-Sub2API-source-change rule.

To make Ops Companion match cc-switch's request-level behavior, add a separate sidecar proxy that sits in front of Sub2API or between Sub2API and upstream providers. That proxy would own:

- Per-attempt provider/account selection.
- Request body transformation and retry stop conditions.
- Streaming first-byte and idle timeout detection.
- Half-open probe permits.
- Same-request retry before the client sees an error.

This should be a separate plan because it requires deployment changes, endpoint routing changes, and much higher blast radius. The control-plane Guard in this plan should be implemented first because it improves the current stack without changing request topology.

## Acceptance Criteria

- No Sub2API source files are edited.
- No new Git branch is created.
- Guard panel can edit `accounts.priority` for a non-deleted account.
- Guard panel can edit `accounts.load_factor` when the live database exposes the column, and shows a disabled state when it does not.
- Account routing edits are validated, audited as `guard_account_routing_update`, and visible after refresh.
- Stability/speed panel default sorting remains schedulable-first and sample/success-oriented; adding priority editing must not restore priority-first visible sorting.
- Balance/quota errors such as `pre_consume_token_quota_failed` and `token quota is not enough` hard-pause the account within one Guard poll.
- Client-side request errors never pause or cooldown accounts.
- 429/rate-limit errors first apply load-factor soft landing when supported, then use temporary cooldown escalation: 5m, then 15m, then 30m.
- 5xx/stream/truncated errors open the circuit only after the configured failure threshold and then apply load-factor soft landing when supported.
- Scheduled-test success closes or advances the account circuit and still sends Telegram recovery notification.
- Guard automatic actions send Telegram notifications with account operation buttons.
- `/sub2ops/guard` remains usable on desktop and cloud deployments.
- `python3 -m unittest discover -s tests -v`, `python3 -m compileall app tests`, and `git diff --check` pass before any commit is pushed.
