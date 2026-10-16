"""Reject build-machine paths in packaged native executables without echoing them."""
from __future__ import annotations

from pathlib import Path
import re
import sys


HOME_PATH = re.compile(rb"/(?:Users|home)/[^/\x00\r\n]{1,128}/")
MAC_TEMP_PATH = re.compile(rb"/(?:private/)?var/folders/[A-Za-z0-9_-]+/[A-Za-z0-9_-]+/[CT]/")


def verify_native_binary(data: bytes) -> None:
    if HOME_PATH.search(data):
        raise ValueError("native release contains a build-time home path")
    if MAC_TEMP_PATH.search(data):
        raise ValueError("native release contains a build-time temporary path")


def main() -> int:
    if len(sys.argv) != 2:
        print("expected one native release executable", file=sys.stderr)
        return 1
    try:
        verify_native_binary(Path(sys.argv[1]).read_bytes())
    except OSError:
        print("native release executable cannot be read", file=sys.stderr)
        return 1
    except ValueError as error:
        print(str(error), file=sys.stderr)
        return 1
    print("PASS: native release contains no build-machine paths")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
