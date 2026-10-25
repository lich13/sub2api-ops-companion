#!/usr/bin/env python3
"""Audit both lockfiles, verifying the exact webpki-roots compatibility re-export."""
from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
FORWARDER_SHA = "521bc38abb08001b01866da9f51eb7c5d647a19260e00054a8c7fd5f9e57f7a9"
PATCHED_ROOTS_SHA = "f5f280bbb2b287bc8e7de7e3852945a0ff50d1bbcb5ed42b6c7a568c546df171"


def verified_forwarder(finding: dict, packages: list[dict]) -> bool:
    """0.26.11 contains only `pub use parent::TLS_SERVER_ROOTS`, not root data.

    Source: https://docs.rs/webpki-roots/0.26.11/src/webpki_roots/lib.rs.html
    Fix: https://github.com/rustls/webpki-roots/pull/133
    Match exact registry archives and the locked forwarding edge. Other versions,
    sources, advisories or dependency changes require a new review; fail closed.
    """
    if finding["advisory"]["id"] != "RUSTSEC-2026-0381":
        return False
    package = finding["package"]
    if (package["name"], package["version"], package.get("source"), package.get("checksum")) != (
        "webpki-roots", "0.26.11", REGISTRY, FORWARDER_SHA
    ):
        return False
    forwarders = [p for p in packages if p["name"] == "webpki-roots" and p["version"] == "0.26.11"]
    roots = [p for p in packages if p["name"] == "webpki-roots" and p["version"] == "1.0.11"]
    return (
        len(forwarders) == len(roots) == 1
        and forwarders[0].get("source") == REGISTRY
        and forwarders[0].get("checksum") == FORWARDER_SHA
        and forwarders[0].get("dependencies") == ["webpki-roots 1.0.11"]
        and roots[0].get("source") == REGISTRY
        and roots[0].get("checksum") == PATCHED_ROOTS_SHA
        and package.get("dependencies") == [{"name": "webpki-roots", "version": "1.0.11", "source": REGISTRY}]
    )


def review(report: dict, packages: list[dict], exit_code: int) -> tuple[list[dict], int]:
    settings = report["settings"]
    if any(settings.get(key) for key in ("ignore", "target_arch", "target_os", "severity")):
        raise ValueError("audit must not ignore advisories or filter findings")
    vulnerabilities = report["vulnerabilities"]
    findings = vulnerabilities["list"]
    if not isinstance(findings, list) or vulnerabilities["count"] != len(findings):
        raise ValueError("invalid audit finding count")
    if vulnerabilities["found"] != bool(findings) or exit_code != int(bool(findings)):
        raise ValueError("audit execution did not complete normally")
    blocked = [finding for finding in findings if not verified_forwarder(finding, packages)]
    return blocked, len(findings) - len(blocked)


def main() -> int:
    failed = False
    for relative in ("server/Cargo.lock", "desktop/src-tauri/Cargo.lock"):
        try:
            lockfile = ROOT / relative
            packages = tomllib.loads(lockfile.read_text())["package"]
            result = subprocess.run(
                ["cargo", "audit", "--file", str(lockfile), "--json"],
                cwd=ROOT, capture_output=True, text=True, check=False,
            )
            report = json.loads(result.stdout)
            blocked, forwarders = review(report, packages, result.returncode)
            for finding in blocked:
                print(f"FAIL {relative}: {finding['advisory']['id']} {finding['package']['name']} {finding['package']['version']}")
            for warnings in report.get("warnings", {}).values():
                for warning in warnings:
                    print(f"WARN {relative}: {warning.get('kind', 'advisory')} {warning['package']['name']} {warning['package']['version']}")
            if forwarders:
                print(f"VERIFIED {relative}: RUSTSEC-2026-0381 reports the 0.26.11 re-export; its exact locked root data is patched 1.0.11")
            if not blocked:
                print(f"PASS {relative}: no unresolved vulnerable dependencies")
            failed |= bool(blocked)
        except (OSError, ValueError, KeyError, TypeError):
            print(f"FAIL {relative}: dependency audit could not be verified")
            failed = True
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
