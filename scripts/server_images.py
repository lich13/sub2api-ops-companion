#!/usr/bin/env python3
"""Build, inspect and archive the exact Rust image plus a fixed Python control."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

from server_http_contract import REFERENCE_SHA, ROOT, require


def test_fixture_path(name: str) -> bool:
    parts = PurePosixPath(name.lstrip("./")).parts
    return any("supervision_fixture" in part for part in parts)


def require_no_fixture_layers(bundle: Path) -> None:
    with tarfile.open(bundle, mode="r:gz") as archive:
        manifest_file = archive.extractfile("manifest.json")
        require(manifest_file is not None, "runtime image archive manifest missing")
        manifest = json.load(manifest_file)
        for image in manifest:
            for layer_name in image.get("Layers", []):
                layer = archive.extractfile(layer_name)
                require(layer is not None, "runtime image archive layer missing")
                with tarfile.open(fileobj=layer, mode="r|") as contents:
                    for entry in contents:
                        require(not test_fixture_path(entry.name), "runtime image archive contains a CI supervision fixture")


def run(arguments, **kwargs):
    return subprocess.run(arguments, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--rust-image", default="sub2ops-companion:ci")
    parser.add_argument("--python-image", default="sub2ops-companion:python-reference")
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    release_version = tomllib.loads((ROOT / "server/Cargo.toml").read_text())["package"]["version"]
    head = run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    require(head == args.revision, "image build revision must match the checkout")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sub2ops-image-") as directory:
        scratch = Path(directory)
        reference = scratch / "reference"
        reference.mkdir()
        archive = run(["git", "archive", REFERENCE_SHA], cwd=ROOT, capture_output=True).stdout
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            source.extractall(reference, filter="data")
        run(["docker", "build", "--platform", "linux/amd64", "--label", "org.opencontainers.image.revision=" + REFERENCE_SHA,
             "-t", args.python_image, str(reference)])
        run(["docker", "build", "--platform", "linux/amd64", "--build-arg", "BUILD_REVISION=" + args.revision,
             "-t", args.rust_image, str(ROOT)])
        info = json.loads(run(["docker", "image", "inspect", args.rust_image], capture_output=True, text=True).stdout)[0]
        require(info["Config"]["Labels"]["org.opencontainers.image.revision"] == args.revision, "image revision label differs")
        require(info["Config"].get("User") == "10001:10001", "runtime image must use the non-root companion user")
        curl = run(["docker", "run", "--rm", "--entrypoint", "/usr/bin/curl", args.rust_image, "--version"],
                   capture_output=True, text=True).stdout.strip()
        require(curl.startswith("curl "), "runtime image curl healthcheck client missing")
        version = run(["docker", "run", "--rm", args.rust_image, "--version"], capture_output=True, text=True).stdout.strip()
        require(version == "Sub2Ops " + release_version + " rust " + args.revision, "executable build revision differs")
        container = run(["docker", "create", args.rust_image], capture_output=True, text=True).stdout.strip()
        exported = scratch / "runtime.tar"
        try:
            with exported.open("wb") as output:
                run(["docker", "export", container], stdout=output)
            with tarfile.open(exported) as filesystem:
                names = [entry.name.lstrip("./") for entry in filesystem]
                require(not any(test_fixture_path(name) for name in names), "runtime image contains a CI supervision fixture")
                forbidden = [name for name in names if name == "workspace" or name.startswith("workspace/")
                    or name.startswith(("usr/bin/python", "usr/local/bin/python", "usr/lib/python", "usr/local/lib/python"))]
                require(not forbidden, "runtime image contains Python or a workspace source mount")
                binary = filesystem.extractfile("usr/local/bin/sub2ops-server")
                require(binary is not None, "runtime executable missing")
                raw = binary.read()
                require(b"/Users/" not in raw and b"/home/runner/" not in raw, "runtime executable contains an unremapped build path")
                binary_hash = hashlib.sha256(raw).hexdigest()
        finally:
            run(["docker", "rm", container], stdout=subprocess.DEVNULL)
        bundle = args.output_dir / "companion-linux-amd64.tar.gz"
        with bundle.open("wb") as destination, gzip.GzipFile(fileobj=destination, mode="wb", filename="", mtime=0) as compressed:
            process = subprocess.Popen(["docker", "save", args.rust_image], stdout=subprocess.PIPE)
            try:
                assert process.stdout is not None
                shutil.copyfileobj(process.stdout, compressed)
            finally:
                if process.stdout:
                    process.stdout.close()
            require(process.wait() == 0, "image archive failed")
        digest = hashlib.sha256()
        with bundle.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        require_no_fixture_layers(bundle)
        (args.output_dir / "SHA256SUMS").write_text(digest.hexdigest() + "  " + bundle.name + "\n")
        (args.output_dir / "manifest.json").write_text(json.dumps({"version": release_version, "revision": args.revision,
            "image_id": info["Id"], "image_tag": args.rust_image, "image_bytes": info["Size"],
            "image_size_method": "docker image inspect Size; storage-backend metadata, not host disk allocation",
            "binary_sha256": binary_hash, "binary_bytes": len(raw), "python_reference_sha": REFERENCE_SHA,
            "runtime_python": False, "workspace_sources": False}, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
