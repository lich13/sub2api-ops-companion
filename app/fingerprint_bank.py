"""Durable, data-only ModelTrace fingerprint bank updates.

Remote updates are treated as untrusted data.  The pinned analyzer and challenge
contract in ``modeltrace_data/manifest.json`` must remain compatible before a new
bank can replace the bundled or last-known-good snapshot.
"""
from __future__ import annotations

import copy
import fcntl
import hashlib
import json
import re
import threading
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

import httpx

from .atomic_config import write_json
from .audit import write_audit

FINGERPRINT_SYNC_INTERVAL_SECONDS = 60 * 60
MAX_BYTES = 8 * 1024 * 1024
SHA1 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


class IncompatibleFingerprintBankError(ValueError):
    """The downloaded data belongs to an incompatible analyzer contract."""


def fingerprint_digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def git_blob_digest(raw: bytes) -> str:
    return hashlib.sha1(f"blob {len(raw)}\0".encode() + raw).hexdigest()


def _parse_time(value: Any) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def validate_fingerprint_bank(value: Any) -> None:
    """Validate the fields consumed by the bundled scorer before activation."""
    if not isinstance(value, dict) or not isinstance(value.get("models"), list) or not value["models"]:
        raise ValueError("指纹库模型列表无效")
    if any(not isinstance(model, dict) or not isinstance(model.get("id"), str)
           or not isinstance(model.get("display_name"), str) for model in value["models"]):
        raise ValueError("指纹库模型项无效")
    robust = value.get("robust")
    if not isinstance(robust, dict) or not isinstance(robust.get("hellinger"), dict):
        raise ValueError("指纹库评分参数无效")
    hellinger = robust["hellinger"]
    if not all(isinstance(hellinger.get(key), list) for key in ("feature_mean", "feature_scale", "centroids")):
        raise ValueError("指纹库特征参数无效")
    means, scales, centroids = hellinger["feature_mean"], hellinger["feature_scale"], hellinger["centroids"]
    if not means or len(means) != len(scales) or any(not isinstance(value, (int, float)) for value in means + scales):
        raise ValueError("指纹库特征维度无效")
    if len(centroids) != len(value["models"]) or any(value == 0 for value in scales) or not centroids or any(
            not isinstance(row, list) or len(row) != len(means)
            or any(not isinstance(value, (int, float)) for value in row) for row in centroids):
        raise ValueError("指纹库中心参数无效")
    ordered = robust.get("ordered_blocks")
    if ordered is not None:
        if not isinstance(ordered, dict) or not isinstance(ordered.get("weight"), (int, float)):
            raise ValueError("指纹库顺序特征参数无效")
        ordered_means, ordered_scales = ordered.get("feature_mean"), ordered.get("feature_scale")
        if not isinstance(ordered_means, list) or not isinstance(ordered_scales, list) \
                or len(ordered_means) != len(ordered_scales) or not ordered_means \
                or any(not isinstance(item, (int, float)) for item in ordered_means) \
                or any(not isinstance(item, (int, float)) or item == 0 for item in ordered_scales):
            raise ValueError("指纹库顺序特征维度无效")
        environments = ordered.get("environment_centroids")
        if not isinstance(environments, list) or not environments or any(
                not isinstance(row, list) or len(row) != len(centroids)
                or any(not isinstance(item, list) or len(item) != len(ordered_means)
                       or any(not isinstance(value, (int, float)) for value in item) for item in row)
                for row in environments):
            raise ValueError("指纹库环境中心参数无效")
    calibration = value.get("calibration")
    if not isinstance(calibration, dict) or any(not isinstance(calibration.get(str(i)), dict) for i in (1, 2, 3)):
        raise ValueError("指纹库校准参数无效")
    if any(not isinstance(calibration[str(i)].get("beta"), (int, float)) for i in (1, 2, 3)):
        raise ValueError("指纹库校准系数无效")
    built_at = _parse_time(value.get("built_at"))
    if built_at is None:
        raise ValueError("指纹库构建时间无效")


class FingerprintBankService:
    """Hourly, deduplicated, last-known-good bank synchronization."""

    def __init__(self, state_path: Path | str, audit_path: str | Path | None = None,
                 *, now: Callable[[], float] | None = None,
                 fetcher: Callable[[str], tuple[int, dict[str, str], bytes]] | None = None) -> None:
        self.path = Path(state_path)
        self.audit_path = str(audit_path) if audit_path else ""
        self.now = now or time.time
        self.fetcher = fetcher
        self._lock = threading.RLock()
        self._sync_lock = threading.Lock()
        self._initialized = False
        self._active: tuple[dict[str, Any], dict[str, Any]] | None = None
        self._state: dict[str, Any] = {}

    @staticmethod
    def _manifest() -> dict[str, Any]:
        from .modeltrace import DATA
        value = json.loads((DATA / "manifest.json").read_text(encoding="utf-8"))
        if not isinstance(value, dict):
            raise ValueError("指纹库 manifest 无效")
        return value

    @staticmethod
    def _bundled() -> tuple[dict[str, Any], dict[str, Any]]:
        from .modeltrace import bundled_bank
        return bundled_bank()

    def _audit(self, action: str, **fields: Any) -> None:
        if self.audit_path:
            write_audit(self.audit_path, "fingerprint_bank_" + action, fields)

    @contextmanager
    def _state_lock(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        lock_path = self.path.with_suffix(".lock")
        with lock_path.open("a+") as handle:
            lock_path.chmod(0o600)
            fcntl.flock(handle, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(handle, fcntl.LOCK_UN)

    def _read_state(self) -> dict[str, Any] | None:
        try:
            value = json.loads(self.path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return None
        except (OSError, UnicodeError, json.JSONDecodeError) as exc:
            raise OSError("指纹库状态无法读取") from exc
        if not isinstance(value, dict) or value.get("version") != 1:
            raise ValueError("指纹库状态版本无效")
        active = value.get("active")
        if not isinstance(active, dict) or not isinstance(active.get("raw_bank"), str):
            raise ValueError("指纹库缓存无效")
        if len(active["raw_bank"].encode()) > MAX_BYTES:
            raise ValueError("指纹库缓存过大")
        version = active.get("version")
        if not isinstance(version, dict) or not SHA1.fullmatch(str(version.get("revision", ""))) \
                or not SHA256.fullmatch(str(version.get("sha256", ""))) \
                or not isinstance(version.get("built_at"), str) \
                or version.get("analyzer_version") != self._manifest().get("analyzerVersion"):
            raise ValueError("指纹库缓存版本无效")
        for field in ("core_blob", "challenge_blob"):
            if not SHA1.fullmatch(str(version.get(field, ""))):
                raise ValueError("指纹库兼容性标识无效")
        for field in ("checked_at", "synced_at"):
            if not isinstance(value.get(field), (int, float)) or value[field] < 0:
                raise ValueError("指纹库状态时间无效")
        return value

    def _validate_cached(self, state: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
        manifest = self._manifest()
        active = state["active"]
        version = active["version"]
        if version["core_blob"] != manifest["files"]["core"]["gitBlob"] \
                or version["challenge_blob"] != manifest["files"]["challenge"]["gitBlob"]:
            raise IncompatibleFingerprintBankError("指纹库分析器版本不兼容")
        raw = active["raw_bank"].encode("utf-8")
        if fingerprint_digest(raw) != version["sha256"]:
            raise ValueError("指纹库缓存摘要不一致")
        bank = json.loads(raw)
        validate_fingerprint_bank(bank)
        bundled, _ = self._bundled()
        built_at = _parse_time(bank.get("built_at"))
        bundled_at = _parse_time(bundled.get("built_at"))
        if not built_at or not bundled_at or version["built_at"] != bank.get("built_at") or built_at < bundled_at:
            raise IncompatibleFingerprintBankError("指纹库版本过旧")
        return bank, {"revision": version["revision"], "sha256": version["sha256"],
                      "built_at": version["built_at"], "analyzer_version": version["analyzer_version"]}

    def initialize(self) -> None:
        with self._lock:
            if self._initialized:
                return
            bundled, version = self._bundled()
            self._active = (bundled, version)
            from .modeltrace import DATA
            bundled_raw = (DATA / "unified_bank.json").read_bytes()
            self._state = {"version": 1, "active": {"raw_bank": bundled_raw.decode("utf-8"),
                                                       "version": {"revision": version["revision"], "sha256": version["sha256"],
                                                                    "built_at": bundled.get("built_at", ""),
                                                                    "analyzer_version": self._manifest().get("analyzerVersion"),
                                                                    "core_blob": self._manifest()["files"]["core"]["gitBlob"],
                                                                    "challenge_blob": self._manifest()["files"]["challenge"]["gitBlob"]}},
                           "checked_at": 0.0, "synced_at": 0.0, "source": "bundled", "last_error": None}
            try:
                cached = self._read_state()
                if cached is not None:
                    bank, cached_version = self._validate_cached(cached)
                    self._active = (bank, cached_version)
                    self._state = cached
                    self._state["source"] = "cache"
                    self._state["last_error"] = None
            except (OSError, ValueError, IncompatibleFingerprintBankError) as exc:
                self._audit("cache_error", error=type(exc).__name__)
            self._initialized = True

    def snapshot(self) -> tuple[dict[str, Any], dict[str, Any]]:
        self.initialize()
        with self._lock:
            assert self._active is not None
            return self._active

    def capture(self) -> tuple[dict[str, Any], dict[str, Any]]:
        bank, version = self.snapshot()
        return copy.deepcopy(bank), dict(version)

    def _read_remote(self, url: str) -> bytes:
        if self.fetcher is not None:
            status, _headers, body = self.fetcher(url)
            if not 200 <= status < 300:
                raise OSError(f"HTTP {status}")
            if len(body) > MAX_BYTES:
                raise ValueError("远程指纹库响应过大")
            return body
        with httpx.Client(timeout=httpx.Timeout(10, connect=5), follow_redirects=False, trust_env=False,
                          headers={"Accept": "application/vnd.github+json", "User-Agent": "sub2ops-modeltrace"}) as client:
            with client.stream("GET", url) as response:
                if not 200 <= response.status_code < 300:
                    raise OSError(f"HTTP {response.status_code}")
                length = int(response.headers.get("content-length", "0") or 0)
                if length > MAX_BYTES:
                    raise ValueError("远程指纹库响应过大")
                chunks: list[bytes] = []
                total = 0
                for chunk in response.iter_bytes():
                    total += len(chunk)
                    if total > MAX_BYTES:
                        raise ValueError("远程指纹库响应过大")
                    chunks.append(chunk)
                return b"".join(chunks)

    def _persist(self, state: dict[str, Any]) -> None:
        with self._state_lock():
            write_json(self.path, state)

    def sync(self, force: bool = False) -> None:
        self.initialize()
        if not self._sync_lock.acquire(blocking=False):
            return
        try:
            now = self.now()
            with self._lock:
                checked_at = float(self._state.get("checked_at", 0) or 0)
            if not force and checked_at > 0 and now >= checked_at and now - checked_at < FINGERPRINT_SYNC_INTERVAL_SECONDS:
                return
            manifest = self._manifest()
            stage = "network"
            try:
                base = f"https://api.github.com/repos/{manifest['repository']}"
                head = json.loads(self._read_remote(f"{base}/commits/{manifest['branch']}"))
                revision = head.get("sha") if isinstance(head, dict) else None
                if not isinstance(revision, str) or not SHA1.fullmatch(revision):
                    raise ValueError("远程提交版本无效")
                tree = json.loads(self._read_remote(f"{base}/git/trees/{revision}?recursive=1"))
                stage = "invalid-data"
                if not isinstance(tree, dict) or tree.get("truncated") or not isinstance(tree.get("tree"), list):
                    raise ValueError("远程 Git tree 不完整")
                entries = {item.get("path"): item for item in tree["tree"] if isinstance(item, dict)}
                for name in ("core", "challenge"):
                    expected = manifest["files"][name]["gitBlob"]
                    item = entries.get(manifest["files"][name]["path"])
                    if not item or item.get("type") != "blob" or item.get("mode") != "100644" or item.get("sha") != expected:
                        raise IncompatibleFingerprintBankError("远程分析器版本不兼容")
                bank_item = entries.get(manifest["files"]["bank"]["path"])
                expected_blob = bank_item.get("sha") if isinstance(bank_item, dict) else ""
                if not isinstance(expected_blob, str) or not SHA1.fullmatch(expected_blob):
                    raise ValueError("远程指纹库 blob 无效")
                stage = "network"
                raw = self._read_remote(f"https://raw.githubusercontent.com/{manifest['repository']}/{revision}/{manifest['files']['bank']['path']}")
                stage = "invalid-data"
                if git_blob_digest(raw) != expected_blob:
                    raise ValueError("远程指纹库 Git 摘要不一致")
                text = raw.decode("utf-8")
                bank = json.loads(text)
                validate_fingerprint_bank(bank)
                current = self.snapshot()[0]
                if (_parse_time(bank.get("built_at")) or datetime.min.replace(tzinfo=timezone.utc)) < \
                        (_parse_time(current.get("built_at")) or datetime.min.replace(tzinfo=timezone.utc)):
                    raise IncompatibleFingerprintBankError("远程指纹库版本过旧")
                version = {"revision": revision, "sha256": fingerprint_digest(raw),
                           "built_at": bank["built_at"], "analyzer_version": manifest["analyzerVersion"]}
                next_state = {"version": 1, "active": {"raw_bank": text, "version": {**version,
                    "core_blob": manifest["files"]["core"]["gitBlob"],
                    "challenge_blob": manifest["files"]["challenge"]["gitBlob"]}},
                              "checked_at": now, "synced_at": self.now(), "source": "remote", "last_error": None}
                self._persist(next_state)
                with self._lock:
                    self._state = next_state
                    self._active = (bank, version)
                self._audit("updated", revision=revision, sha256=version["sha256"])
            except Exception as exc:
                with self._lock:
                    next_state = dict(self._state)
                    next_state["checked_at"] = now
                    next_state["last_error"] = {"stage": stage, "kind": type(exc).__name__}
                    next_state["source"] = next_state.get("source") or "bundled"
                try:
                    self._persist(next_state)
                    with self._lock:
                        self._state = next_state
                except Exception:
                    pass
                self._audit("sync_error", stage=stage, kind=type(exc).__name__)
        finally:
            self._sync_lock.release()

    def start(self) -> None:
        """Run one synchronous check; the async owner schedules hourly calls."""
        self.sync()
