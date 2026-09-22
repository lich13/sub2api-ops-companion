#!/usr/bin/env python3
"""Build the public exact-price snapshot used by model_guard.

The input is Sub2API's deployed ``model_pricing.json``.  No HTTP pricing
endpoint is used because that endpoint may synthesize fallback prices.
"""
from __future__ import annotations

import hashlib
import json
import os
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path


def build(source: Path) -> dict[str, object]:
    raw = source.read_bytes()
    data = json.loads(raw.decode("utf-8"))
    if not isinstance(data, dict):
        raise ValueError("pricing file must be an object")
    prices: dict[str, dict[str, object]] = {}
    for name, item in data.items():
        if not isinstance(item, dict):
            continue
        provider = str(item.get("litellm_provider") or "").lower()
        if provider not in {"openai", "xai"}:
            continue
        try:
            input_price = float(item["input_cost_per_token"])
            output_price = float(item["output_cost_per_token"])
        except (KeyError, TypeError, ValueError):
            continue
        if input_price <= 0 or output_price <= 0:
            continue
        prices[str(name)] = {
            "input_price": input_price,
            "output_price": output_price,
            "unit": "token",
            "provider": provider,
        }
    return {
        "schema_version": 1,
        "source_sha256": hashlib.sha256(raw).hexdigest(),
        "collected_at": datetime.now(timezone.utc).isoformat(),
        "prices": prices,
    }


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: model_guard_price_snapshot.py SOURCE OUTPUT", file=sys.stderr)
        return 2
    source, output = map(Path, sys.argv[1:])
    payload = build(source)
    output.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(dir=output.parent, prefix=f".{output.name}.", suffix=".tmp")
    temporary = Path(name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            fd = -1
            json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(output)
        output.chmod(0o600)
    finally:
        if fd >= 0:
            os.close(fd)
        temporary.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
