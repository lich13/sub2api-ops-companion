from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime


IGNORED_CATEGORIES = {"client_pre_route", "client_request", "client_bad_request"}


@dataclass(slots=True)
class GuardPolicy:
    hard_pause_enabled: bool = True
    rate_limit_enabled: bool = True
    unstable_enabled: bool = True
    failure_threshold: int = 4
    success_threshold: int = 2
    circuit_timeout_seconds: int = 60
    error_rate_threshold: float = 0.6
    min_requests: int = 10
    rate_limit_cooldowns: tuple[int, ...] = (1, 3, 5)
    unstable_cooldowns: tuple[int, ...] = (1, 3, 5)
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
        if circuit.state == "open":
            circuit.state = "half_open"
        if circuit.state == "half_open" and circuit.consecutive_successes >= policy.success_threshold:
            circuit.state = "closed"
            circuit.opened_at = ""
        return _none(signal, "success recorded"), _remember(circuit, signal)

    circuit.total_requests += 1
    circuit.failed_requests += 1
    circuit.consecutive_failures += 1
    circuit.consecutive_successes = 0

    if signal.category == "provider_balance_or_quota":
        if not policy.hard_pause_enabled:
            return _none(signal, "hard-pause category disabled by guard policy"), _remember(circuit, signal)
        if circuit.consecutive_failures < policy.balance_pause_threshold:
            return _none(signal, "balance/quota signal recorded below threshold"), _remember(circuit, signal)
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
        if not policy.hard_pause_enabled:
            return _none(signal, "hard-pause category disabled by guard policy"), _remember(circuit, signal)
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
        if not policy.rate_limit_enabled:
            return _none(signal, "rate-limit category disabled by guard policy"), _remember(circuit, signal)
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
        if not policy.unstable_enabled:
            return _none(signal, "unstable-upstream category disabled by guard policy"), _remember(circuit, signal)
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
