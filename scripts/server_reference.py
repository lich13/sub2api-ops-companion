#!/usr/bin/env python3
"""Immutable Python reference shared by all server contract tools."""
from __future__ import annotations

import json
from pathlib import Path

REFERENCE_SHA = "6be7c291ed38dcc337cb88f693639040a3463899"


def check_fixtures(root: Path) -> list[str]:
    errors: list[str] = []

    def visit(value: object, path: Path) -> None:
        if isinstance(value, dict):
            for key, item in value.items():
                if key in ("reference_sha", "python_reference_sha") and item != REFERENCE_SHA:
                    errors.append(f"{path.relative_to(root)}: pinned reference differs")
                visit(item, path)
        elif isinstance(value, list):
            for item in value:
                visit(item, path)

    for path in sorted((root / "tests/contracts").rglob("*.json")):
        visit(json.loads(path.read_text(encoding="utf-8")), path)
    return sorted(set(errors))


if __name__ == "__main__":
    import sys

    try:
        failures = check_fixtures(Path(__file__).resolve().parents[1])
    except (OSError, ValueError):
        failures = ["reference fixtures cannot be read"]
    for failure in failures:
        print(failure, file=sys.stderr)
    if not failures:
        print("pinned Python reference fixtures are consistent")
    raise SystemExit(bool(failures))
