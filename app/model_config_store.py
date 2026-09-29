from __future__ import annotations

import fcntl
import json
import os
import threading
from datetime import datetime, timezone
from pathlib import Path

from fastapi import HTTPException

from .atomic_config import write_json
from .model_rules import revision, validate_overrides
from .model_reasoning import model_id, reasoning_fields


class ModelConfigStore:
    def __init__(self, path: str):
        self.path = Path(path)
        self.lock = threading.RLock()

    def read(self) -> dict:
        try:
            if self.path.stat().st_size > 16 * 1024 * 1024:
                raise ValueError("模型配置文件过大")
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return {"schema": 1, "groups": {}}
        if not isinstance(data, dict) or data.get("schema") != 1 or not isinstance(data.get("groups"), dict):
            raise ValueError("模型配置文件无效")
        for value in data["groups"].values():
            if not isinstance(value, dict):
                raise ValueError("模型配置文件无效")
            validate_overrides(value.get("overrides", {}))
            entries = value.get("reasoning", {})
            if not isinstance(entries, dict) or len(entries) > 500:
                raise ValueError("思考档位配置无效")
            for model, entry in entries.items():
                model_id(model)
                if not isinstance(entry, dict):
                    raise ValueError("思考档位配置无效")
                reasoning_fields(entry.get("efforts"), entry.get("default_effort"))
        return data

    def group(self, group_id: int) -> dict:
        saved = self.read()["groups"].get(str(group_id), {})
        return {**saved, "overrides": saved.get("overrides", {}), "revision": revision(saved)}

    def save_reasoning(self, group_id: int, model: str, entry: dict | None, expected: str) -> dict:
        model_id(model)
        if entry is not None:
            reasoning_fields(entry.get("efforts"), entry.get("default_effort"))
        with self.lock:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            fd = os.open(str(self.path) + ".lock", os.O_CREAT | os.O_RDWR, 0o600)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX)
                data = self.read()
                old = data["groups"].get(str(group_id), {})
                if revision(old) != expected:
                    raise HTTPException(409, "模型配置已变更，请刷新后重试")
                updated = {**old, "reasoning": dict(old.get("reasoning", {}))}
                if entry is None:
                    updated["reasoning"].pop(model, None)
                else:
                    updated["reasoning"][model] = entry
                # Legacy non-reasoning fields remain intact. The new editor
                # cannot write or accidentally erase those unrelated values.
                overrides = {k: dict(v) for k, v in old.get("overrides", {}).items()}
                if model in overrides:
                    for field in ("supported_reasoning_levels", "default_reasoning_level"):
                        overrides[model].pop(field, None)
                    if not overrides[model]:
                        del overrides[model]
                updated["overrides"] = overrides
                updated["updated_at"] = datetime.now(timezone.utc).isoformat()
                data["groups"][str(group_id)] = updated
                if len(json.dumps(data).encode()) > 16 * 1024 * 1024:
                    raise ValueError("模型配置文件过大")
                write_json(self.path, data)
                return self.group(group_id)
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
                os.close(fd)

    def save(self, group_id: int, overrides: dict, expected: str) -> dict:
        validate_overrides(overrides)
        with self.lock:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            fd = os.open(str(self.path) + ".lock", os.O_CREAT | os.O_RDWR, 0o600)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX)
                data = self.read()
                old = data["groups"].get(str(group_id), {})
                if revision(old) != expected:
                    raise HTTPException(409, "模型配置已变更，请刷新后重试")
                data["groups"][str(group_id)] = {"overrides": overrides, "updated_at": datetime.now(timezone.utc).isoformat()}
                write_json(self.path, data)
                return self.group(group_id)
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
                os.close(fd)
