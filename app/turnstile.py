from __future__ import annotations

from typing import Any, Protocol


TURNSTILE_ENABLED_KEY = "turnstile_enabled"
TURNSTILE_SITE_KEY = "turnstile_site_key"
TURNSTILE_SECRET_KEY = "turnstile_secret_key"
TURNSTILE_SETTING_KEYS = (TURNSTILE_ENABLED_KEY, TURNSTILE_SITE_KEY, TURNSTILE_SECRET_KEY)

TURNSTILE_SETTINGS_SELECT_SQL = """
SELECT key, value, updated_at
FROM settings
WHERE key = ANY(%(keys)s::text[])
ORDER BY key
"""

TURNSTILE_SETTING_UPSERT_SQL = """
INSERT INTO settings (key, value, updated_at)
VALUES (%(key)s, %(value)s, now())
ON CONFLICT (key)
DO UPDATE SET value = EXCLUDED.value, updated_at = now()
RETURNING key, value, updated_at
"""


class SettingsDatabase(Protocol):
    def fetch_all(self, sql: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        ...

    def fetch_one(self, sql: str, params: dict[str, Any] | None = None) -> dict[str, Any] | None:
        ...


def setting_bool(value: Any) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on"}


def clean_setting_value(value: Any) -> str:
    return str(value or "").strip()


def turnstile_setting_values(
    enabled: bool,
    site_key: str,
    secret_key: str | None,
) -> list[tuple[str, str]]:
    values = [
        (TURNSTILE_ENABLED_KEY, "true" if enabled else "false"),
        (TURNSTILE_SITE_KEY, site_key.strip()),
    ]
    if secret_key is not None:
        values.append((TURNSTILE_SECRET_KEY, secret_key.strip()))
    return values


def build_turnstile_config(rows: list[dict[str, Any]]) -> dict[str, Any]:
    by_key = {str(row.get("key")): row for row in rows}
    secret_key = clean_setting_value(by_key.get(TURNSTILE_SECRET_KEY, {}).get("value"))
    updated_values = [row.get("updated_at") for row in rows if row.get("updated_at")]
    return {
        "enabled": setting_bool(by_key.get(TURNSTILE_ENABLED_KEY, {}).get("value")),
        "site_key": clean_setting_value(by_key.get(TURNSTILE_SITE_KEY, {}).get("value")),
        "site_key_set": bool(clean_setting_value(by_key.get(TURNSTILE_SITE_KEY, {}).get("value"))),
        "secret_key_set": bool(secret_key),
        "updated_at": max(updated_values) if updated_values else None,
        "setting_keys": TURNSTILE_SETTING_KEYS,
    }


def load_turnstile_config(db: SettingsDatabase) -> dict[str, Any]:
    return build_turnstile_config(
        db.fetch_all(TURNSTILE_SETTINGS_SELECT_SQL, {"keys": list(TURNSTILE_SETTING_KEYS)})
    )


def save_turnstile_config(
    db: SettingsDatabase,
    *,
    enabled: bool,
    site_key: str,
    secret_key: str | None,
) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for key, value in turnstile_setting_values(enabled, site_key, secret_key):
        row = db.fetch_one(TURNSTILE_SETTING_UPSERT_SQL, {"key": key, "value": value})
        if row:
            rows.append(row)
    return rows
