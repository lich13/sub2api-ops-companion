from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import server_mutation_contract as mutation


class StartupIntercepted(RuntimeError):
    """Stop the owned runner after its service-start boundary was inspected."""


class MutationContractIsolationTest(unittest.TestCase):
    def test_both_modes_seed_the_verified_reference_fingerprint_before_start(self) -> None:
        http = mutation.http
        real_archive = http.archive_reference_app
        real_seed = http.seed_fingerprint_cache
        fixture_apps: list[Path] = []
        seeded: list[Path] = []
        observed: list[tuple[str, dict[str, str]]] = []
        suite_runs: list[str] = []
        postgres_instances: list[object] = []
        seed_timestamp = 1_800_000_000.25

        def archive_reference(destination: Path, sha: str) -> Path:
            self.assertEqual(sha, mutation.REFERENCE_SHA)
            app_dir = real_archive(destination, sha)
            fixture_apps.append(app_dir)
            return app_dir

        def seed_fingerprint(app_dir: Path, state_dir: Path) -> None:
            with patch.object(http.time, "time", return_value=seed_timestamp):
                real_seed(app_dir, state_dir)
            seeded.append(app_dir)

        class FakePostgres:
            def __init__(self, _root: Path, _bin_dir: str | None = None) -> None:
                self.url = "postgresql://127.0.0.1:55432/fixture"
                self.postgres_pid = 12345
                self.closed = False
                postgres_instances.append(self)

            def start(self) -> None:
                pass

            def close(self) -> None:
                self.closed = True

        class FakeUpstream:
            def __init__(self, database_url: str) -> None:
                self.database_url = database_url

            def __enter__(self):
                return self

            def __exit__(self, *_args) -> None:
                self.release_all()

            def release_all(self) -> None:
                pass

            def snapshot(self) -> dict[str, object]:
                return {}

        class InterceptService:
            def __init__(self, _root: Path, state_dir: Path, _port: int,
                         _mocks: object, _database_url: str, *,
                         command: list[str] | None, reference_app: Path | None,
                         target: str) -> None:
                self.state_dir = state_dir
                self.command = command
                self.reference_app = reference_app
                self.target = target
                self.process = SimpleNamespace(pid=23456)
                self.exit_code = 0
                self.closed = False

            def start(self) -> None:
                app_dir = fixture_apps[-1]
                manifest_path = app_dir / "modeltrace_data" / "manifest.json"
                bank_path = app_dir / "modeltrace_data" / "unified_bank.json"
                manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
                raw_bank = bank_path.read_bytes()
                bank_sha = hashlib.sha256(raw_bank).hexdigest()
                self.assert_fixture(
                    manifest=manifest,
                    raw_bank=raw_bank,
                    bank_sha=bank_sha,
                    state_path=self.state_dir / "modeltrace-bank-state.json",
                )
                if self.target == "python-reference":
                    if self.command is not None or self.reference_app != app_dir:
                        raise AssertionError("Python mode did not receive its fixed reference app")
                elif self.target == "command":
                    if self.command is None or self.reference_app is not None:
                        raise AssertionError("command mode unexpectedly selected the Python app")
                else:
                    raise AssertionError("unexpected runner target")

            def assert_fixture(self, *, manifest: dict[str, object], raw_bank: bytes,
                               bank_sha: str, state_path: Path) -> None:
                self_owner = test_case
                expected_version = {
                    "revision": manifest["revision"],
                    "sha256": manifest["files"]["bank"]["sha256"],
                    "built_at": json.loads(raw_bank)["built_at"],
                    "analyzer_version": manifest["analyzerVersion"],
                    "core_blob": manifest["files"]["core"]["gitBlob"],
                    "challenge_blob": manifest["files"]["challenge"]["gitBlob"],
                }
                self_owner.assertEqual(bank_sha, expected_version["sha256"])
                state = json.loads(state_path.read_text(encoding="utf-8"))
                self_owner.assertEqual(state["source"], "bundled")
                self_owner.assertEqual(state["active"]["raw_bank"].encode("utf-8"), raw_bank)
                self_owner.assertEqual(state["active"]["version"], expected_version)
                self_owner.assertGreater(state["checked_at"], 0)
                self_owner.assertGreater(state["synced_at"], 0)
                self_owner.assertEqual(state["checked_at"], seed_timestamp)
                self_owner.assertEqual(state["synced_at"], seed_timestamp)
                observed.append((bank_sha, expected_version))

            def close(self) -> None:
                self.closed = True

        class StopBeforeRequestsSuite:
            def __init__(self, _service: object, _mocks: object, _database_url: str,
                         _command: list[str] | None, _temporary: Path) -> None:
                self.restart_service = None
                self.cases: list[dict[str, object]] = []

            def run(self) -> None:
                suite_runs.append("reached")
                raise StartupIntercepted("service-start boundary inspected")

        test_case = self

        with tempfile.TemporaryDirectory(prefix="mutation-contract-command-fixture-") as temp:
            command_binary = Path(temp) / "fixture-command"
            command_binary.write_text("runner command marker\n", encoding="utf-8")
            command_binary.chmod(0o700)

            for command in (None, [str(command_binary)]):
                with self.subTest(target="python-reference" if command is None else "command"), \
                     patch.object(http, "archive_reference_app", side_effect=archive_reference), \
                     patch.object(http, "seed_fingerprint_cache", side_effect=seed_fingerprint), \
                     patch.object(http, "TemporaryPostgres", FakePostgres), \
                     patch.object(mutation, "seed_accounts", lambda _url: None), \
                     patch.object(mutation, "MutationUpstream", FakeUpstream), \
                     patch.object(http, "ServiceProcess", InterceptService), \
                     patch.object(mutation, "Suite", StopBeforeRequestsSuite):
                    result = mutation.run_owned(command, None)
                    self.assertEqual(result["error"], "StartupIntercepted")
                    self.assertEqual(result["exit_code"], 1)
                    self.assertTrue(result["temporary_directory_removed"])
                    self.assertEqual(result["cleanup"]["temporary_files"], "removed")

        self.assertEqual(len(fixture_apps), 2)
        self.assertEqual(seeded, fixture_apps)
        self.assertEqual(len(observed), 2)
        self.assertEqual(observed[0], observed[1])
        self.assertEqual(len(suite_runs), 2)
        self.assertEqual(len(postgres_instances), 2)
        self.assertTrue(all(getattr(instance, "closed") for instance in postgres_instances))


if __name__ == "__main__":
    unittest.main()
