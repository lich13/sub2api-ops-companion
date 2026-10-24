#!/usr/bin/env python3
"""Compare reviewed configuration documentation with an immutable upstream commit.

The catalogue contains reviewed Chinese explanations and validation rules. This
tool never imports deployment values or silently accepts new upstream semantics.
Run --revision SHA to inspect a new release, review the changes in the catalogue,
then --revision SHA --accept-reviewed to record its source fingerprints.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "server/resources/sub2api-config.json"
SOURCES = ("backend/internal/config/config.go", "backend/internal/config/validate_dingtalk.go", "deploy/config.example.yaml")


def inventory(source: str) -> dict[str, str]:
    structs = {}
    for match in re.finditer(r"type\s+(\w+)\s+struct\s*\{(.*?)^\}", source, re.S | re.M):
        fields = re.findall(r'^\s*(\w+)\s+([^`\n]+)\s+`[^`]*mapstructure:"([^"-]+)"[^`]*`', match[2], re.M)
        structs[match[1]] = [(tag, kind.strip()) for _, kind, tag in fields]
    result = {}

    def walk(name: str, prefix: str = "", parents: tuple[str, ...] = ()) -> None:
        if name in parents:
            raise ValueError("recursive configuration structure")
        for tag, kind in structs.get(name, []):
            path = f"{prefix}.{tag}".lstrip(".")
            if kind in structs:
                walk(kind, path, (*parents, name))
            else:
                result[path] = kind
                inner = kind.removeprefix("[]").removeprefix("map[string]")
                if inner in structs:
                    walk(inner, path + ("[]" if kind.startswith("[]") else ".*"), (*parents, name))

    walk("Config")
    return result


def normalize(path: str) -> str:
    return re.sub(r"<[^>]+>", "*", path)


def defaults(source: str) -> dict[str, str]:
    """Keep source expressions, including lists and named Go constants, for review."""
    result = {}
    for match in re.finditer(r'viper\.SetDefault\("([^"]+)",\s*', source):
        start, depth, quote, escaped = match.end(), 1, None, False
        for index in range(start, len(source)):
            char = source[index]
            if quote:
                if escaped:
                    escaped = False
                elif char == "\\" and quote != "`":
                    escaped = True
                elif char == quote:
                    quote = None
            elif char in ('"', "'", "`"):
                quote = char
            elif char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    result[match[1]] = source[start:index].strip()
                    break
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision")
    parser.add_argument("--version", help="Reviewed upstream release version; required when accepting a new commit")
    parser.add_argument("--source-dir", type=Path, help="Use an already downloaded upstream tree")
    parser.add_argument("--accept-reviewed", action="store_true")
    args = parser.parse_args()
    catalog = json.loads(CATALOG.read_text())
    revision = args.revision or catalog["revision"]
    if not re.fullmatch(r"[a-f0-9]{40}", revision):
        parser.error("revision must be a full immutable commit SHA")
    sources = {}
    for path in SOURCES:
        if args.source_dir:
            content = (args.source_dir / path).read_bytes()
        else:
            with urllib.request.urlopen(f"https://raw.githubusercontent.com/Wei-Shaw/sub2api/{revision}/{path}", timeout=30) as response:
                content = response.read()
        sources[path] = content
    discovered = inventory(sources[SOURCES[0]].decode())
    reviewed = {normalize(f["path"]): f.get("go_type") for f in catalog["fields"]}
    added = sorted(discovered.keys() - reviewed.keys())
    removed = sorted(reviewed.keys() - discovered.keys())
    changed = sorted(k for k in discovered.keys() & reviewed.keys() if reviewed[k] != discovered[k])
    fingerprints = {k: hashlib.sha256(v).hexdigest() for k, v in sources.items()}
    source_changes = [k for k, v in fingerprints.items() if catalog.get("sources", {}).get(k) != v]
    source_defaults = defaults(sources[SOURCES[0]].decode())
    previous_defaults = catalog.get("source_defaults", {})
    default_changes = {k: {"before": previous_defaults.get(k), "after": source_defaults.get(k)}
                       for k in sorted(source_defaults.keys() | previous_defaults.keys())
                       if source_defaults.get(k) != previous_defaults.get(k)}
    print(json.dumps({"revision": revision, "added": added, "removed": removed, "changed_types": changed,
                      "changed_defaults": default_changes, "changed_sources": source_changes}, ensure_ascii=False, indent=2))
    if added or removed or changed:
        print("Review field types, explanations, defaults and constraints before accepting.")
        return 1
    if args.accept_reviewed:
        if revision != catalog["revision"] and not args.version:
            parser.error("--version is required when accepting a new upstream commit")
        if args.version and not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", args.version):
            parser.error("version must identify a specific release")
        catalog["revision"] = revision
        catalog["version"] = args.version or catalog["version"]
        catalog["sources"] = fingerprints
        catalog["source_defaults"] = source_defaults
        for field in catalog["fields"]:
            field["source_url"] = re.sub(r"/blob/[a-f0-9]{40}/", f"/blob/{revision}/", field["source_url"])
        CATALOG.write_text(json.dumps(catalog, ensure_ascii=False, indent=2) + "\n")
        return 0
    return int(bool(source_changes or default_changes))


if __name__ == "__main__":
    raise SystemExit(main())
