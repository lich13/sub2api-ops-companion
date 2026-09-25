"""One-time 0.1.4 migration. Run with the old service stopped and its environment.

No bot credentials are copied. Runtime imports never invoke this migration.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import tempfile

from app.config_service import OAUTH_FIELDS
from app.settings import Settings, bool_value, daily_test_time, int_value


def read_object(path: Path) -> dict:
    if path.is_symlink():
        raise ValueError("migration refuses symbolic links")
    if not path.exists():
        return {}
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("configuration must be an object")
    return value


def atomic(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(dir=path.parent, prefix=".oauth-migration-")
    temporary = Path(name)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def remove_derived(value):
    if isinstance(value, dict):
        return {k: remove_derived(v) for k, v in value.items() if k != "telegram_windows"}
    if isinstance(value, list):
        return [remove_derived(v) for v in value]
    return value


def migrate(env: dict[str, str], env_path: Path) -> dict:
    legacy_path = Path(env.get("TELEGRAM_CONFIG_PATH", "/data/telegram-config.json"))
    target = Path(env.get("OAUTH_CONFIG_PATH", "/data/oauth-config.json"))
    state_path = Path(env.get("USAGE_QUERY_STATE_PATH", "/data/usage-query-state.json"))
    legacy, current = read_object(legacy_path), read_object(target)
    bot_state = Path(str(legacy.get("state_path") or env.get("TELEGRAM_STATE_PATH") or "/data/telegram-state.json"))
    protected = {target.resolve(), state_path.resolve(), env_path.resolve()}
    if legacy_path.resolve() in protected or bot_state.resolve() in protected or bot_state.is_symlink() or env_path.is_symlink():
        raise ValueError("migration paths overlap or use symbolic links")
    for path in (legacy_path, bot_state):
        if path.exists() and not path.is_file():
            raise ValueError("retired path is not a file")
    limits = {"oauth_usage_refresh_concurrency": (1, 16), "oauth_recovery_test_concurrency": (1, 8),
              "oauth_early_probe_batch_size": (1, 50), "oauth_7d_probe_interval_seconds": (60, 86400)}
    result = {}
    for key in sorted(OAUTH_FIELDS):
        default = getattr(Settings, key)
        value = current.get(key, legacy.get(key, env.get(key.upper(), env.get("TELEGRAM_" + key.upper(), default))))
        if isinstance(default, bool):
            value = bool_value(value, default)
        elif key in limits:
            value = int_value(value, default, *limits[key])
        elif key == "oauth_daily_test_time":
            value = daily_test_time(value)
        else:
            value = str(value or default).strip() or default
        result[key] = value
    raw_env = env_path.read_text(encoding="utf-8")
    neutral_env = "".join(line for line in raw_env.splitlines(keepends=True)
                          if not re.match(r"\s*(?:export\s+)?TELEGRAM_[A-Z0-9_]+\s*=", line))
    if not re.search(r"(?m)^\s*(?:export\s+)?OAUTH_CONFIG_PATH\s*=", neutral_env):
        neutral_env = neutral_env.rstrip("\n") + "\nOAUTH_CONFIG_PATH=" + str(target) + "\n"
    state = read_object(state_path)
    cleaned = remove_derived(state)
    # Validate every input before changing anything; partial commits can be rerun.
    serialized = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    atomic(target, serialized)
    if read_object(target) != result:
        raise ValueError("OAuth configuration readback mismatch")
    if cleaned != state:
        atomic(state_path, json.dumps(cleaned, ensure_ascii=False, indent=2) + "\n")
    atomic(env_path, neutral_env)
    for path in dict.fromkeys((legacy_path, bot_state)):
        if path.exists():
            if not path.is_file():
                raise ValueError("retired path is not a file")
            path.unlink()
    return {"migrated": True, "fields": len(result), "mode": oct(target.stat().st_mode & 0o777),
            "legacy_removed": not legacy_path.exists() and not bot_state.exists()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--env-file", type=Path, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(migrate(dict(os.environ), args.env_file)))
    except Exception as exc:
        # Exceptions may contain source values; report the category only.
        raise SystemExit("OAuth migration failed: " + type(exc).__name__) from None
