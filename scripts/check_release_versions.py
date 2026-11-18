#!/usr/bin/env python3
"""Read-only release and verified-upstream version consistency checks."""
from __future__ import annotations

import json
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def check(root: Path = ROOT) -> list[str]:
    errors: list[str] = []
    version = tomllib.loads((root / "server/Cargo.toml").read_text())["package"]["version"]

    def same(source: str, actual: object) -> None:
        if actual != version:
            errors.append(f"{source}: release version differs from server/Cargo.toml")

    for file in ("desktop/package.json", "desktop/src-tauri/tauri.conf.json"):
        same(file, json.loads((root / file).read_text())["version"])
    same("desktop/src-tauri/Cargo.toml", tomllib.loads((root / "desktop/src-tauri/Cargo.toml").read_text())["package"]["version"])
    for file, name in (("server/Cargo.lock", "sub2ops-server"), ("desktop/src-tauri/Cargo.lock", "sub2ops")):
        packages = tomllib.loads((root / file).read_text())["package"]
        own = [package["version"] for package in packages if package["name"] == name and "source" not in package]
        same(file, own[0] if len(own) == 1 else None)
    for file, pattern in (
        ("Dockerfile", r'org\.opencontainers\.image\.version="([^"]+)"'),
        ("desktop/RELEASE.md", r'^# Sub2Ops ([0-9]+\.[0-9]+\.[0-9]+)$'),
        ("desktop/CHANGELOG.md", r'^## ([0-9]+\.[0-9]+\.[0-9]+) ·'),
    ):
        match = re.search(pattern, (root / file).read_text(), re.MULTILINE)
        same(file, match.group(1) if match else None)

    return errors


def main() -> int:
    try:
        errors = check()
    except (OSError, ValueError, KeyError, TypeError):
        print("release metadata cannot be read", file=sys.stderr)
        return 1
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print("release and verified upstream versions are consistent")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
