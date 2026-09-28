"""One-time 0.1.7 migration. Run with the old deployment's environment.

Copies only the effective connection URLs, then retires browser secrets. Never
backs up browser credentials or prints config contents. Safe to rerun.
"""
from __future__ import annotations

import argparse
import json
import os
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

import psycopg

from app.atomic_config import write_json
from app.connection_config import normalize_url


def migrate(env_file: Path, *, remove_menu: bool = True) -> dict:
    old_path = Path(os.environ.get("OPS_SSO_CONFIG_PATH", "/data/sso-config.json"))
    new_path = Path(os.environ.get("SUB2API_CONFIG_PATH", "/data/sub2api-config.json"))
    retired = [old_path, Path(os.environ.get("OPS_SESSION_STORE_PATH", "/data/sessions.json"))]
    if new_path in retired:
        raise RuntimeError("Connection path overlaps retired file")
    source = old_path if old_path.exists() else new_path
    data = json.loads(source.read_text()) if source.exists() else {}
    effective = {"base_url": normalize_url(data.get("base_url", os.environ.get("SUB2API_BASE_URL", ""))),
                 "verify_base_url": normalize_url(data.get("verify_base_url", os.environ.get("SUB2API_VERIFY_BASE_URL", "")))}
    if not (effective["verify_base_url"] or effective["base_url"]):
        raise RuntimeError("Sub2API connection is missing; migration aborted")
    write_json(new_path, effective)
    assert json.loads(new_path.read_text()) == effective
    assert new_path.stat().st_mode & 0o777 == 0o600
    models = Path(os.environ.get("GROUP_MODEL_CONFIG_PATH", str(new_path.with_name("group-model-config.json"))))
    if not models.exists():
        write_json(models, {"schema": 1, "groups": {}})
    removed_menu = 0
    if remove_menu:
        with psycopg.connect(os.environ["DATABASE_URL"]) as db:
            db.execute("SET LOCAL lock_timeout='5s'")
            row = db.execute("SELECT value FROM settings WHERE key='custom_menu_items' FOR UPDATE").fetchone()
            if row:
                items = json.loads(row[0])
                if not isinstance(items, list):
                    raise RuntimeError("Custom menu format unknown; migration aborted")
                kept = [item for item in items if not (isinstance(item, dict) and
                        urlsplit(str(item.get("url") or "")).path.rstrip("/") == "/sub2ops/sso/start" and
                        urlsplit(str(item.get("url") or "")).hostname in {None, urlsplit(effective["base_url"]).hostname})]
                removed_menu = len(items) - len(kept)
                if removed_menu:
                    db.execute("UPDATE settings SET value=%s,updated_at=now() WHERE key='custom_menu_items'",
                               (json.dumps(kept, ensure_ascii=False),))
    # Keep all non-retired lines byte-for-byte, including unrelated secrets.
    if env_file.exists():
        lines = env_file.read_text().splitlines(keepends=True)
        kept = [line for line in lines if not line.lstrip().split("=", 1)[0].startswith(("OPS_SESSION_", "OPS_SSO_", "SUB2API_SSO_", "OPS_UPDATE_"))]
        fd, name = tempfile.mkstemp(prefix=".desktop-env.", dir=env_file.parent)
        temporary = Path(name)
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, "w") as out:
                out.writelines(kept)
                out.flush()
                os.fsync(out.fileno())
            temporary.replace(env_file)
        finally:
            temporary.unlink(missing_ok=True)
    released = 0
    for path in retired:
        if path == new_path:
            raise RuntimeError("Connection path overlaps retired file")
        if path.exists():
            released += path.stat().st_size
            path.unlink()
    return {"connection_migrated": True, "menu_items_removed": removed_menu, "retired_bytes": released}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--env-file", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(migrate(args.env_file)))
