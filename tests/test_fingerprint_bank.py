from __future__ import annotations

import json
import tempfile
import threading
import unittest
from pathlib import Path

from app.fingerprint_bank import FingerprintBankService, git_blob_digest
from app.modeltrace import DATA


class FingerprintBankTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.clock = [1000.0]
        self.manifest = json.loads((DATA / "manifest.json").read_text())
        self.raw = (DATA / "unified_bank.json").read_bytes()
        self.head = "a" * 40
        self.calls: list[str] = []

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def fetcher(self, url: str):
        self.calls.append(url)
        if "/commits/" in url:
            return 200, {}, json.dumps({"sha": self.head}).encode()
        if "/git/trees/" in url:
            entries = [
                {"path": self.manifest["files"]["core"]["path"], "type": "blob", "mode": "100644", "sha": self.manifest["files"]["core"]["gitBlob"]},
                {"path": self.manifest["files"]["challenge"]["path"], "type": "blob", "mode": "100644", "sha": self.manifest["files"]["challenge"]["gitBlob"]},
                {"path": self.manifest["files"]["bank"]["path"], "type": "blob", "mode": "100644", "sha": git_blob_digest(self.raw)},
            ]
            return 200, {}, json.dumps({"truncated": False, "tree": entries}).encode()
        if "raw.githubusercontent.com" in url:
            return 200, {}, self.raw
        raise AssertionError(url)

    def service(self, fetcher=None):
        return FingerprintBankService(
            Path(self.tmp.name) / "modeltrace-bank-state.json",
            now=lambda: self.clock[0],
            fetcher=fetcher or self.fetcher,
        )

    def test_success_is_cached_and_hourly_check_is_gated(self):
        service = self.service()
        service.sync()
        self.assertEqual(service.snapshot()[1]["revision"], self.head)
        self.assertEqual(len(self.calls), 3)
        service.sync()
        self.assertEqual(len(self.calls), 3)
        self.clock[0] += 3600
        service.sync()
        # The same upstream revision only needs a head check; the tree and
        # payload are not downloaded again.
        self.assertEqual(len(self.calls), 4)

        restarted = self.service(fetcher=lambda url: (_ for _ in ()).throw(AssertionError(url)))
        self.assertEqual(restarted.snapshot()[1]["revision"], self.head)

    def test_failed_update_keeps_last_good_bank(self):
        service = self.service()
        service.sync()
        before = service.snapshot()[1].copy()
        self.clock[0] += 3600
        failed = self.service(fetcher=lambda url: (_ for _ in ()).throw(OSError("offline")))
        # Reuse the persisted last-good cache before the failed check.
        self.assertEqual(failed.snapshot()[1], before)
        failed.sync()
        self.assertEqual(failed.snapshot()[1], before)

    def test_concurrent_sync_is_deduplicated(self):
        service = self.service()
        threads = [threading.Thread(target=service.sync) for _ in range(5)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(self.calls.count(next(url for url in self.calls if "/commits/" in url)), 1)
        self.assertEqual(service.snapshot()[1]["revision"], self.head)


if __name__ == "__main__":
    unittest.main()
