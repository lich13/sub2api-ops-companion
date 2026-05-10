from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def write_audit(path: str, event: str, payload: dict[str, Any]) -> None:
    audit_path = Path(path)
    record = {
        "ts": datetime.now(timezone.utc).isoformat(),
        "event": event,
        "payload": payload,
    }
    try:
        audit_path.parent.mkdir(parents=True, exist_ok=True)
        with audit_path.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(record, ensure_ascii=False, default=str) + "\n")
    except OSError:
        # Audit is best-effort; it must not block operational controls.
        return


def read_audit(path: str, limit: int = 20, event_prefix: str | None = None) -> list[dict[str, Any]]:
    audit_path = Path(path)
    try:
        lines = audit_path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return []

    records: list[dict[str, Any]] = []
    for line in reversed(lines):
        if len(records) >= limit:
            break
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event_prefix and not str(record.get("event", "")).startswith(event_prefix):
            continue
        records.append(record)
    return records
