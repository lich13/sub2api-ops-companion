#!/usr/bin/env python3
"""Build, inspect and archive the exact Rust image plus a fixed Python control."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

from server_http_contract import REFERENCE_SHA, ROOT, require


def run(arguments, **kwargs):
    return subprocess.run(arguments, check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--rust-image", default="sub2ops-companion:ci")
    parser.add_argument("--python-image", default="sub2ops-companion:python-reference")
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
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
        version = run(["docker", "run", "--rm", args.rust_image, "--version"], capture_output=True, text=True).stdout.strip()
        require(version == "Sub2Ops 0.1.40 rust " + args.revision, "executable build revision differs")
        container = run(["docker", "create", args.rust_image], capture_output=True, text=True).stdout.strip()
        exported = scratch / "runtime.tar"
        try:
            with exported.open("wb") as output:
                run(["docker", "export", container], stdout=output)
            with tarfile.open(exported) as filesystem:
                names = [entry.name.lstrip("./") for entry in filesystem]
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
        (args.output_dir / "SHA256SUMS").write_text(digest.hexdigest() + "  " + bundle.name + "\n")
        (args.output_dir / "manifest.json").write_text(json.dumps({"version": "0.1.40", "revision": args.revision,
            "image_id": info["Id"], "image_tag": args.rust_image, "image_bytes": info["Size"],
            "image_size_method": "docker image inspect Size; storage-backend metadata, not host disk allocation",
            "binary_sha256": binary_hash, "binary_bytes": len(raw), "python_reference_sha": REFERENCE_SHA,
            "runtime_python": False, "workspace_sources": False}, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
