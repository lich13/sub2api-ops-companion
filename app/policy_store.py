"""Small durable control records. No credentials or model outputs."""
from __future__ import annotations
import copy
import fcntl
import json
import threading
from contextlib import contextmanager
from pathlib import Path
from .atomic_config import write_json

_LOCK = threading.RLock()

class PolicyStore:
    def __init__(self, path, defaults):
        self.path, self.defaults = Path(path), defaults
        self.existed = self.path.exists() or self.path.with_suffix('.lock').exists()

    def read(self):
        try:
            data = json.loads(self.path.read_text())
        except FileNotFoundError:
            if self.existed:
                raise ValueError('控制状态文件丢失') from None
            return {'version': 1, **copy.deepcopy(self.defaults)}
        if not isinstance(data, dict) or data.get('version') != 1:
            raise ValueError('控制状态文件无效')
        self.existed = True
        return data

    @contextmanager
    def transaction(self):
        with _LOCK:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            with self.path.with_suffix('.lock').open('a+') as lock:
                self.path.with_suffix('.lock').chmod(0o600)
                fcntl.flock(lock, fcntl.LOCK_EX)
                try:
                    data = self.read()
                    yield data
                    write_json(self.path, data)
                    self.existed = True
                finally:
                    fcntl.flock(lock, fcntl.LOCK_UN)
