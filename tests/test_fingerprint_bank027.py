from __future__ import annotations

import hashlib
import json
import tempfile
import threading
import unittest
from pathlib import Path
from typing import Any

from app.fingerprint_bank import FingerprintBankService, git_blob_digest
from app.modeltrace import DATA


def _pkt_line(payload: bytes) -> bytes:
    return f"{len(payload) + 4:04x}".encode("ascii") + payload


class RemoteFixture:
    """Small deterministic stand-in for the GitHub API and raw file host."""

    def __init__(self, manifest: dict[str, Any], raw: bytes, head: str) -> None:
        self.manifest = manifest
        self.raw = raw
        self.head = head
        self.calls: list[str] = []
        self.tree_blob: str | None = None
        self.scripts: list[tuple[Any, list[Any]]] = []
        self.core = b"fixture-core"
        self.challenge = b"fixture-challenge"

    def script(self, predicate: Any, *responses: Any) -> None:
        self.scripts.append((predicate, list(responses)))

    @staticmethod
    def _response(value: Any) -> tuple[int, dict[str, str], bytes]:
        if isinstance(value, BaseException):
            raise value
        return value

    def __call__(self, url: str) -> tuple[int, dict[str, str], bytes]:
        self.calls.append(url)
        for predicate, responses in self.scripts:
            if responses and predicate(url):
                return self._response(responses.pop(0))

        if "/commits/" in url:
            return 200, {}, json.dumps({"sha": self.head}).encode()
        if ".git/info/refs" in url:
            branch = self.manifest["branch"]
            body = (
                _pkt_line(b"# service=git-upload-pack\n")
                + b"0000"
                + _pkt_line(f"{self.head} refs/heads/{branch}\0report-status\n".encode())
                + b"0000"
            )
            return 200, {}, body
        if "/git/trees/" in url:
            entries = [
                {
                    "path": self.manifest["files"]["core"]["path"],
                    "type": "blob",
                    "mode": "100644",
                    "sha": self.manifest["files"]["core"]["gitBlob"],
                },
                {
                    "path": self.manifest["files"]["challenge"]["path"],
                    "type": "blob",
                    "mode": "100644",
                    "sha": self.manifest["files"]["challenge"]["gitBlob"],
                },
                {
                    "path": self.manifest["files"]["bank"]["path"],
                    "type": "blob",
                    "mode": "100644",
                    "sha": self.tree_blob or git_blob_digest(self.raw),
                },
            ]
            return 200, {}, json.dumps({"truncated": False, "tree": entries}).encode()
        if "fingerprint-core.js" in url:
            return 200, {}, self.core
        if "challenge-browser.js" in url:
            return 200, {}, self.challenge
        if "unified_bank.json" in url:
            return 200, {}, self.raw
        raise AssertionError(f"unexpected URL: {url}")


class FingerprintBank027Tests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "modeltrace-bank-state.json"
        self.clock = [1000.0]
        self.rejection_index = 0
        self.manifest = json.loads((DATA / "manifest.json").read_text(encoding="utf-8"))
        self.raw = (DATA / "unified_bank.json").read_bytes()

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def service(self, remote: RemoteFixture) -> FingerprintBankService:
        return FingerprintBankService(self.path, now=lambda: self.clock[0], fetcher=remote)

    def remote(self, *, head: str = "a" * 40, raw: bytes | None = None) -> RemoteFixture:
        return RemoteFixture(self.manifest, raw or self.raw, head)

    @staticmethod
    def error_code(public: dict[str, Any]) -> str | None:
        error = public.get("error")
        if isinstance(error, dict):
            return error.get("code") or error.get("kind")
        return error

    def bank_variant(self, mutate: Any) -> bytes:
        bank = json.loads(self.raw.decode("utf-8"))
        mutate(bank)
        return json.dumps(bank, allow_nan=True, separators=(",", ":")).encode("utf-8")

    def assert_public_contract(self, public: dict[str, Any]) -> None:
        self.assertTrue(
            {
                "source",
                "status",
                "result",
                "error",
                "checked_at",
                "synced_at",
                "cooldown_until",
                "version",
            }.issubset(public)
        )

    def test_public_contract_and_cache_startup(self) -> None:
        remote = self.remote()
        service = self.service(remote)
        service.sync()

        public = service.public()
        self.assert_public_contract(public)
        self.assertEqual(public["source"], "remote")
        self.assertEqual(public["status"], "idle")
        self.assertEqual(public["result"], "updated")
        self.assertIsNone(public["error"])
        self.assertEqual(public["checked_at"], 1000.0)
        self.assertGreaterEqual(public["synced_at"], 1000.0)
        self.assertIsNone(public["cooldown_until"])
        self.assertEqual(public["version"]["revision"], remote.head)

        restarted = FingerprintBankService(
            self.path,
            now=lambda: self.clock[0],
            fetcher=lambda url: (_ for _ in ()).throw(AssertionError(url)),
        )
        cached = restarted.public()
        self.assert_public_contract(cached)
        self.assertEqual(cached["source"], "cache")
        self.assertEqual(cached["status"], "idle")
        self.assertIsNone(cached["error"])
        self.assertEqual(cached["version"]["revision"], remote.head)

    def test_hourly_check_same_revision_skips_tree_and_payload(self) -> None:
        remote = self.remote()
        service = self.service(remote)
        service.sync()
        self.assertEqual(len(remote.calls), 3)

        self.clock[0] += 3599
        service.sync()
        self.assertEqual(len(remote.calls), 3)

        self.clock[0] += 1
        service.sync()
        self.assertEqual(len(remote.calls), 4)
        self.assertEqual(sum("/git/trees/" in url for url in remote.calls), 1)
        self.assertEqual(sum("raw.githubusercontent.com" in url for url in remote.calls), 1)
        self.assertEqual(service.public()["result"], "up-to-date")

    def test_network_and_timeout_retry_then_five_minute_cooldown(self) -> None:
        for raised, expected in ((OSError("offline"), "network"), (TimeoutError("slow"), "timeout")):
            with self.subTest(expected=expected):
                self.clock[0] = 1000.0
                remote = self.remote()
                remote.script(lambda url: "/commits/" in url, raised, raised)
                remote.script(lambda url: ".git/info/refs" in url, raised, raised)
                service = FingerprintBankService(
                    self.path.with_name(f"network-{expected}.json"),
                    now=lambda: self.clock[0],
                    fetcher=remote,
                )
                service.sync()

                public = service.public()
                self.assertEqual(self.error_code(public), expected)
                self.assertEqual(public["status"], "error")
                self.assertEqual(public["cooldown_until"], 1300.0)
                self.assertEqual(sum("/commits/" in url for url in remote.calls), 2)

                calls = len(remote.calls)
                self.clock[0] += 299
                service.sync()
                self.assertEqual(len(remote.calls), calls)
                self.clock[0] += 1
                service.sync()
                self.assertGreater(len(remote.calls), calls)

    def test_rate_limit_retry_after_and_rate_reset_are_respected(self) -> None:
        retry_after = self.remote()
        retry_after.script(
            lambda url: "/commits/" in url,
            (429, {"Retry-After": "600"}, b""),
        )
        retry_after.script(
            lambda url: ".git/info/refs" in url,
            (429, {"Retry-After": "600"}, b""),
        )
        service = self.service(retry_after)
        service.sync()
        public = service.public()
        self.assertEqual(self.error_code(public), "rate-limit")
        self.assertEqual(public["cooldown_until"], 1600.0)
        calls = len(retry_after.calls)
        self.clock[0] = 1599
        service.sync(force=True)
        self.assertEqual(len(retry_after.calls), calls)
        self.clock[0] = 1600
        service.sync(force=True)
        self.assertEqual(service.public()["result"], "updated")

        reset = self.remote()
        reset.script(
            lambda url: "/commits/" in url,
            (403, {"X-RateLimit-Reset": "2200"}, b""),
        )
        reset.script(
            lambda url: ".git/info/refs" in url,
            (403, {"X-RateLimit-Reset": "2200"}, b""),
        )
        reset_service = FingerprintBankService(
            self.path.with_name("reset-state.json"),
            now=lambda: self.clock[0],
            fetcher=reset,
        )
        self.clock[0] = 1000
        reset_service.sync()
        self.assertEqual(reset_service.public()["cooldown_until"], 2200.0)

    def test_official_git_refs_fallback_uses_pinned_raw_files(self) -> None:
        remote = self.remote(head="b" * 40)
        remote.script(
            lambda url: "/commits/" in url,
            (503, {}, b""),
            (503, {}, b""),
        )
        fixture_manifest = json.loads(json.dumps(self.manifest))
        for name, payload in (("core", remote.core), ("challenge", remote.challenge)):
            fixture_manifest["files"][name]["gitBlob"] = git_blob_digest(payload)
            fixture_manifest["files"][name]["sha256"] = hashlib.sha256(payload).hexdigest()
        service = self.service(remote)
        service._manifest = lambda: fixture_manifest  # type: ignore[method-assign]
        service.sync()
        public = service.public()
        self.assertEqual(public["source"], "remote")
        self.assertEqual(public["status"], "idle")
        self.assertEqual(public["version"]["revision"], remote.head)
        self.assertEqual(public.get("retrieval"), "git_refs")
        self.assertTrue(any(".git/info/refs" in url for url in remote.calls))
        self.assertFalse(any("/git/trees/" in url for url in remote.calls))
        self.assertEqual(sum("raw.githubusercontent.com" in url for url in remote.calls), 3)

    def test_successful_fallback_keeps_explicit_api_rate_limit_for_manual_checks(self) -> None:
        remote = self.remote(head=self.manifest["revision"])
        remote.script(lambda url: "/commits/" in url, (429, {"Retry-After": "600"}, b""))
        service = self.service(remote)
        service.sync()
        self.assertEqual(service.public()["result"], "up-to-date")
        self.assertEqual(service.public()["cooldown_until"], 1600)
        calls = len(remote.calls)
        self.clock[0] = 1599
        service.sync(force=True)
        self.assertEqual(len(remote.calls), calls)
        self.clock[0] = 1600
        service.sync(force=True)
        self.assertGreater(len(remote.calls), calls)

    def assert_rejected_bank(self, raw: bytes, expected: str, *, tree_blob: str | None = None) -> None:
        self.rejection_index += 1
        path = self.path.with_name(f"rejected-{self.rejection_index}.json")
        remote = self.remote(head="c" * 40, raw=raw)
        remote.tree_blob = tree_blob
        service = FingerprintBankService(path, now=lambda: self.clock[0], fetcher=remote)
        bundled_revision = service.snapshot()[1]["revision"]
        service.sync()
        public = service.public()
        self.assertEqual(self.error_code(public), expected)
        expected_status = "upgrade-required" if expected == "incompatible" else "error"
        self.assertEqual(public["status"], expected_status)
        self.assertEqual(public["source"], "bundled")
        self.assertEqual(service.snapshot()[1]["revision"], bundled_revision)

    def test_invalid_json_nan_infinity_dimension_and_model_id_are_rejected(self) -> None:
        self.assert_rejected_bank(b"{", "invalid-data")
        self.assert_rejected_bank(
            self.bank_variant(lambda bank: bank["robust"]["hellinger"]["feature_mean"].__setitem__(0, float("nan"))),
            "invalid-data",
        )
        self.assert_rejected_bank(
            self.bank_variant(lambda bank: bank["robust"]["hellinger"]["feature_mean"].__setitem__(0, float("inf"))),
            "invalid-data",
        )
        self.assert_rejected_bank(
            self.bank_variant(lambda bank: bank["robust"]["hellinger"]["feature_mean"].append(0)),
            "invalid-data",
        )
        self.assert_rejected_bank(
            self.bank_variant(lambda bank: bank["models"][0].__setitem__("id", bank["models"][1]["id"])),
            "invalid-data",
        )
        self.assert_rejected_bank(
            self.bank_variant(lambda bank: bank.__setitem__("built_at", "2026-09-01T00:00:00+00:00")),
            "incompatible",
        )

    def test_hash_mismatch_is_invalid_data(self) -> None:
        self.assert_rejected_bank(self.raw, "invalid-data", tree_blob="0" * 40)

    def test_cache_write_failure_keeps_active_snapshot(self) -> None:
        remote = self.remote()
        service = self.service(remote)
        service.sync()
        before = service.snapshot()
        remote.head = "d" * 40
        self.clock[0] += 3600
        service._persist = lambda state: (_ for _ in ()).throw(OSError("read-only"))  # type: ignore[method-assign]
        service.sync()
        public = service.public()
        self.assertEqual(self.error_code(public), "cache")
        self.assertEqual(public["status"], "error")
        self.assertEqual(public["cooldown_until"], 4900.0)
        self.assertEqual(service.snapshot(), before)

    def test_capture_and_snapshot_are_isolated(self) -> None:
        remote = self.remote()
        service = self.service(remote)
        service.sync()
        captured = service.capture()
        captured[0]["models"][0]["id"] = "changed"
        captured[1]["revision"] = "changed"
        self.assertNotEqual(service.snapshot()[0]["models"][0]["id"], "changed")
        self.assertNotEqual(service.snapshot()[1]["revision"], "changed")

        snapshot = service.snapshot()
        snapshot[0]["models"][0]["id"] = "changed-again"
        self.assertNotEqual(service.capture()[0]["models"][0]["id"], "changed-again")

    def test_concurrent_sync_calls_merge_into_one_check(self) -> None:
        remote = self.remote()
        entered = threading.Event()
        release = threading.Event()
        original = remote

        def blocked(url: str) -> tuple[int, dict[str, str], bytes]:
            if "/commits/" in url and not entered.is_set():
                entered.set()
                self.assertTrue(release.wait(timeout=2))
            return original(url)

        service = FingerprintBankService(self.path, now=lambda: self.clock[0], fetcher=blocked)
        owner = threading.Thread(target=service.sync)
        owner.start()
        self.assertTrue(entered.wait(timeout=2))
        self.assertEqual(service.public()["status"], "checking")
        followers = [threading.Thread(target=service.sync) for _ in range(4)]
        for thread in followers:
            thread.start()
        for thread in followers:
            thread.join(timeout=2)
        self.assertEqual(sum("/commits/" in url for url in remote.calls), 0)
        release.set()
        owner.join(timeout=2)
        self.assertFalse(owner.is_alive())
        self.assertEqual(sum("/commits/" in url for url in remote.calls), 1)
        self.assertEqual(service.public()["status"], "idle")


if __name__ == "__main__":
    unittest.main()
