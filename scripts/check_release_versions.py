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
        ("scripts/catalog_qa.compose.yml", r'image: sub2ops-catalog-qa:([^\s]+)'),
        ("desktop/RELEASE.md", r'^# Sub2Ops ([0-9]+\.[0-9]+\.[0-9]+)$'),
        ("desktop/CHANGELOG.md", r'^## ([0-9]+\.[0-9]+\.[0-9]+) ·'),
    ):
        match = re.search(pattern, (root / file).read_text(), re.MULTILINE)
        same(file, match.group(1) if match else None)
    source = (root / "server/src/model_reasoning.rs").read_text()
    declaration = re.search(r'pub const VERIFIED_SUB2API_VERSIONS: &\[&str\] = &\[(.*?)\];', source, re.DOTALL)
    verified = re.findall(r'"([0-9]+\.[0-9]+\.[0-9]+)"', declaration.group(1)) if declaration else []
    document = (root / "docs/model-catalog.md").read_text()
    declared = re.search(r'已核对原版 Sub2API ([0-9. /]+) 的 Responses', document)
    versions = re.findall(r'[0-9]+\.[0-9]+\.[0-9]+', declared.group(1)) if declared else []
    if not verified or versions != verified:
        errors.append("docs/model-catalog.md: verified upstream versions differ from server constant")

    workflow = (root / ".github/workflows/desktop.yml").read_text()
    desktop_doc = (root / "desktop/README.md").read_text()
    package = json.loads((root / "desktop/package.json").read_text())
    rust_image = re.search(r'^FROM rust:([0-9.]+)-', (root / "Dockerfile").read_text(), re.MULTILINE)
    nodes = re.findall(r"node-version:\s*'([0-9.]+)'", workflow)
    rust = rust_image.group(1) if rust_image else ""
    pnpm = package.get("packageManager", "").removeprefix("pnpm@")

    def toolchain(source: str, values: list[str], expected: str) -> None:
        if not expected or not values or any(value != expected for value in values):
            errors.append(f"{source}: build toolchain versions differ")

    toolchain("CI Node", nodes, nodes[0] if nodes else "")
    toolchain("desktop/README.md Node", re.findall(r'Node ([0-9.]+)', desktop_doc), nodes[0] if nodes else "")
    toolchain("desktop/README.md pnpm", re.findall(r'pnpm ([0-9.]+)', desktop_doc), pnpm)
    toolchain("CI pnpm", re.findall(r'uses: pnpm/action-setup@[^\n]+\n\s+with:\n\s+version:\s*([0-9.]+)', workflow), pnpm)
    toolchain("desktop/README.md Rust", re.findall(r'Rust ([0-9.]+)', desktop_doc), rust)
    toolchain("CI Rust", re.findall(r'rustup (?:toolchain install|override set) ([0-9.]+)', workflow), rust)
    desktop_rust = tomllib.loads((root / "desktop/src-tauri/Cargo.toml").read_text())["package"].get("rust-version", "")
    toolchain("desktop Rust minimum", [desktop_rust], ".".join(rust.split(".")[:2]))
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
