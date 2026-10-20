from __future__ import annotations

import copy
import json
import subprocess
import tomllib
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from unittest.mock import patch

import server_dependency_audit as audit


SERVER_LOCK = audit.ROOT / "server/Cargo.lock"
DESKTOP_LOCK = audit.ROOT / "desktop/src-tauri/Cargo.lock"


def load_packages(path: Path) -> list[dict]:
    return tomllib.loads(path.read_text(encoding="utf-8"))["package"]


def forwarder_finding(*, advisory_id: str = "RUSTSEC-2026-0381", **package_overrides: object) -> dict:
    package = {
        "name": "webpki-roots",
        "version": "0.26.11",
        "source": audit.REGISTRY,
        "checksum": audit.FORWARDER_SHA,
        "dependencies": [
            {
                "name": "webpki-roots",
                "version": "1.0.10",
                "source": audit.REGISTRY,
            }
        ],
    }
    package.update(package_overrides)
    return {
        "advisory": {"id": advisory_id},
        "package": package,
    }


def unknown_finding(**package_overrides: object) -> dict:
    package = {
        "name": "openssl",
        "version": "0.10.60",
        "source": audit.REGISTRY,
        "checksum": "fixture-unknown-checksum",
        "dependencies": [],
    }
    package.update(package_overrides)
    return {
        "advisory": {"id": "RUSTSEC-2099-9999"},
        "package": package,
    }


def report_for(findings: list[dict], *, settings: dict | None = None) -> dict:
    return {
        "settings": {
            "ignore": [],
            "target_arch": None,
            "target_os": None,
            "severity": None,
            **(settings or {}),
        },
        "vulnerabilities": {
            "found": bool(findings),
            "count": len(findings),
            "list": findings,
        },
        "warnings": {},
    }


class DependencyAuditTests(unittest.TestCase):
    def test_real_lockfiles_pin_the_expected_root_packages(self) -> None:
        server = load_packages(SERVER_LOCK)
        desktop = load_packages(DESKTOP_LOCK)

        server_forwarders = [
            package
            for package in server
            if package.get("name") == "webpki-roots" and package.get("version") == "0.26.11"
        ]
        server_roots = [
            package
            for package in server
            if package.get("name") == "webpki-roots" and package.get("version") == "1.0.10"
        ]
        desktop_roots = [
            package
            for package in desktop
            if package.get("name") == "webpki-roots" and package.get("version") == "1.0.10"
        ]

        self.assertEqual(len(server_forwarders), 1)
        self.assertEqual(len(server_roots), 1)
        self.assertEqual(len(desktop_roots), 1)
        self.assertEqual(server_forwarders[0]["source"], audit.REGISTRY)
        self.assertEqual(server_forwarders[0]["checksum"], audit.FORWARDER_SHA)
        self.assertEqual(server_forwarders[0]["dependencies"], ["webpki-roots 1.0.10"])
        self.assertEqual(server_roots[0]["source"], audit.REGISTRY)
        self.assertEqual(server_roots[0]["checksum"], audit.PATCHED_ROOTS_SHA)
        self.assertEqual(desktop_roots[0]["source"], audit.REGISTRY)
        self.assertEqual(desktop_roots[0]["checksum"], audit.PATCHED_ROOTS_SHA)

    def test_verified_forwarder_accepts_exact_registry_archive_and_edge(self) -> None:
        packages = load_packages(SERVER_LOCK)

        self.assertTrue(audit.verified_forwarder(forwarder_finding(), packages))

    def test_verified_forwarder_rejects_any_unreviewed_change(self) -> None:
        base_packages = load_packages(SERVER_LOCK)
        cases: list[tuple[str, dict, list[dict]]] = []

        cases.extend(
            [
                (
                    "old root version",
                    forwarder_finding(
                        dependencies=[
                            {
                                "name": "webpki-roots",
                                "version": "1.0.9",
                                "source": audit.REGISTRY,
                            }
                        ]
                    ),
                    self._with_root(base_packages, version="1.0.9", checksum="7dcd9d09a39985f5344844e66b0c530a33843579125f23e21e9f0f220850f22a"),
                ),
                ("wrong finding checksum", forwarder_finding(checksum="bad-checksum"), copy.deepcopy(base_packages)),
                ("wrong forwarder checksum", forwarder_finding(), self._with_forwarder(base_packages, checksum="bad-checksum")),
                ("wrong root checksum", forwarder_finding(), self._with_root(base_packages, checksum="bad-checksum")),
                ("wrong finding source", forwarder_finding(source="git+https://example.invalid/webpki-roots"), copy.deepcopy(base_packages)),
                ("wrong forwarder source", forwarder_finding(), self._with_forwarder(base_packages, source="git+https://example.invalid/webpki-roots")),
                ("wrong root source", forwarder_finding(), self._with_root(base_packages, source="git+https://example.invalid/webpki-roots")),
                ("missing parent edge", forwarder_finding(), self._with_forwarder(base_packages, dependencies=[])),
                ("extra parent edge", forwarder_finding(), self._with_forwarder(base_packages, dependencies=["webpki-roots 1.0.10", "webpki-roots 0.26.11"])),
                ("other advisory", forwarder_finding(advisory_id="RUSTSEC-2099-9999"), copy.deepcopy(base_packages)),
                ("other package", forwarder_finding(name="other-package"), copy.deepcopy(base_packages)),
                ("other version", forwarder_finding(version="0.26.12"), copy.deepcopy(base_packages)),
            ]
        )
        for label, finding, packages in cases:
            with self.subTest(label=label):
                self.assertFalse(audit.verified_forwarder(finding, packages))

    def test_verified_forwarder_rejects_missing_locked_parent_package(self) -> None:
        packages = load_packages(SERVER_LOCK)
        packages = [
            package
            for package in packages
            if not (
                package.get("name") == "webpki-roots"
                and package.get("version") == "1.0.10"
            )
        ]

        self.assertFalse(audit.verified_forwarder(forwarder_finding(), packages))

    def test_verified_forwarder_rejects_non_registry_finding_package(self) -> None:
        packages = load_packages(SERVER_LOCK)
        finding = forwarder_finding(source=None)

        self.assertFalse(audit.verified_forwarder(finding, packages))

    def test_review_accepts_clean_report(self) -> None:
        blocked, forwarders = audit.review(report_for([]), load_packages(DESKTOP_LOCK), 0)

        self.assertEqual(blocked, [])
        self.assertEqual(forwarders, 0)

    def test_review_accepts_only_verified_forwarder_and_keeps_other_vulnerability(self) -> None:
        findings = [forwarder_finding(), unknown_finding()]

        blocked, forwarders = audit.review(
            report_for(findings), load_packages(SERVER_LOCK), 1
        )

        self.assertEqual(forwarders, 1)
        self.assertEqual(blocked, [findings[1]])

    def test_review_rejects_incomplete_or_inconsistent_reports(self) -> None:
        valid = report_for([])
        malformed_reports = {
            "missing settings": {key: value for key, value in valid.items() if key != "settings"},
            "missing vulnerabilities": {key: value for key, value in valid.items() if key != "vulnerabilities"},
            "missing finding list": {
                **valid,
                "vulnerabilities": {key: value for key, value in valid["vulnerabilities"].items() if key != "list"},
            },
            "count mismatch": {
                **valid,
                "vulnerabilities": {**valid["vulnerabilities"], "count": 1},
            },
            "found mismatch": {
                **valid,
                "vulnerabilities": {**valid["vulnerabilities"], "found": True},
            },
        }

        for label, report in malformed_reports.items():
            with self.subTest(label=label):
                with self.assertRaises((KeyError, TypeError, ValueError)):
                    audit.review(report, load_packages(DESKTOP_LOCK), 0)

    def test_review_rejects_abnormal_exit_status(self) -> None:
        for findings, exit_code in (([], 1), ([unknown_finding()], 0)):
            with self.subTest(findings=findings, exit_code=exit_code):
                with self.assertRaises(ValueError):
                    audit.review(report_for(findings), load_packages(SERVER_LOCK), exit_code)

    def test_review_rejects_ignore_and_all_finding_filters(self) -> None:
        for key, value in (
            ("ignore", ["RUSTSEC-2026-0381"]),
            ("target_arch", "x86_64"),
            ("target_os", "linux"),
            ("severity", "low"),
        ):
            with self.subTest(setting=key):
                with self.assertRaises(ValueError):
                    audit.review(
                        report_for([], settings={key: value}),
                        load_packages(DESKTOP_LOCK),
                        0,
                    )

    def test_main_audits_both_lockfiles_and_accepts_exact_forwarder(self) -> None:
        exact = report_for([forwarder_finding()])
        clean = report_for([])
        responses = [
            subprocess.CompletedProcess([], 1, json.dumps(exact), ""),
            subprocess.CompletedProcess([], 0, json.dumps(clean), ""),
        ]
        output = StringIO()

        with patch.object(audit.subprocess, "run", side_effect=responses) as run:
            with redirect_stdout(output):
                result = audit.main()

        self.assertEqual(result, 0)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(run.call_args_list[0].args[0][:3], ["cargo", "audit", "--file"])
        self.assertEqual(run.call_args_list[1].args[0][:3], ["cargo", "audit", "--file"])
        self.assertIn("VERIFIED server/Cargo.lock", output.getvalue())
        self.assertIn("PASS desktop/src-tauri/Cargo.lock", output.getvalue())

    def test_main_reports_unknown_vulnerability_for_each_lockfile(self) -> None:
        report = report_for([unknown_finding()])
        responses = [
            subprocess.CompletedProcess([], 1, json.dumps(report), ""),
            subprocess.CompletedProcess([], 1, json.dumps(report), ""),
        ]
        output = StringIO()

        with patch.object(audit.subprocess, "run", side_effect=responses):
            with redirect_stdout(output):
                result = audit.main()

        rendered = output.getvalue()
        self.assertEqual(result, 1)
        self.assertIn("FAIL server/Cargo.lock: RUSTSEC-2099-9999 openssl 0.10.60", rendered)
        self.assertIn("FAIL desktop/src-tauri/Cargo.lock: RUSTSEC-2099-9999 openssl 0.10.60", rendered)
        self.assertNotIn("PASS server/Cargo.lock", rendered)
        self.assertNotIn("PASS desktop/src-tauri/Cargo.lock", rendered)

    @staticmethod
    def _with_forwarder(packages: list[dict], **updates: object) -> list[dict]:
        copied = copy.deepcopy(packages)
        for package in copied:
            if package.get("name") == "webpki-roots" and package.get("version") == "0.26.11":
                package.update(updates)
        return copied

    @staticmethod
    def _with_root(packages: list[dict], **updates: object) -> list[dict]:
        copied = copy.deepcopy(packages)
        for package in copied:
            if package.get("name") == "webpki-roots" and package.get("version") == "1.0.10":
                package.update(updates)
        return copied


if __name__ == "__main__":
    unittest.main()
