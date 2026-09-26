"""Replay quality scoring against existing logs with read-only transactions.

Prints aggregate evidence and numeric account IDs, never raw errors or keys.
Run from the repository: python -m scripts.quality_replay
"""
import json
import os
import time
from collections import Counter
from datetime import datetime, timezone

from app.account_quality import calculate, read_evidence
from app.db import Database


def main():
    db = Database(os.environ["DATABASE_URL"])
    db.open()
    try:
        now, started = datetime.now(timezone.utc), time.monotonic()
        accounts, usage, errors = read_evidence(db, now)
        read_ms = (time.monotonic() - started) * 1000
        results, _ = calculate(accounts, usage, errors, now)
        print(json.dumps({"accounts": len(accounts), "success_logs": len(usage), "error_logs": len(errors),
            "read_ms": round(read_ms, 1), "total_ms": round((time.monotonic() - started) * 1000, 1),
            "grades": dict(Counter(v["grade"] for v in results.values())),
            "sample_status": dict(Counter(v["sample_status"] for v in results.values())),
            "quality": [{"id": k, "score": v["score"], "reasons": v["reasons"], "samples": v["reliability"]["total"],
                "failures": v["reliability"]["failures"], "failure_rate": v["reliability"]["effective_rate"],
                "coverage": v["coverage"]} for k,v in results.items()]}, ensure_ascii=False))
    finally:
        db.close()


if __name__ == "__main__":
    main()
