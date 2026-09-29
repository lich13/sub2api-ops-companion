"""Verify the installable ARM64 release, including ELF and APK 16 KB alignment."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import zipfile


def run(*args):
    return subprocess.check_output([str(a) for a in args], text=True, stderr=subprocess.STDOUT)


def verify(apk):
    sdk = Path(os.environ["ANDROID_HOME"])
    build = sdk / "build-tools/36.0.0"
    host = "darwin-x86_64" if sys.platform == "darwin" else "linux-x86_64"
    elf = sdk / f"ndk/28.2.13676358/toolchains/llvm/prebuilt/{host}/bin/llvm-readelf"
    run(build / "apksigner", "verify", "--verbose", apk)
    run(build / "zipalign", "-c", "-P", "16", "4", apk)
    manifest = run(build / "aapt2", "dump", "badging", apk)
    assert re.search(r"^(?:minSdkVersion|sdkVersion):'31'$", manifest, re.MULTILINE), "Android 12 minimum changed"
    assert "targetSdkVersion:'36'" in manifest, "Unexpected target SDK"
    assert "native-code: 'arm64-v8a'" in manifest, "APK must contain only ARM64"
    with zipfile.ZipFile(apk) as package, tempfile.TemporaryDirectory(prefix="sub2ops-elf-") as temporary:
        libraries = [n for n in package.namelist() if n.startswith("lib/") and n.endswith(".so")]
        assert libraries and all(n.startswith("lib/arm64-v8a/") for n in libraries)
        for name in libraries:
            library = Path(temporary) / Path(name).name
            library.write_bytes(package.read(name))
            headers = run(elf, "-lW", library)
            alignments = [int(line.split()[-1], 16) for line in headers.splitlines() if re.match(r"\s*LOAD\s", line)]
            assert alignments and min(alignments) >= 0x4000, f"4 KB ELF: {name}"
    print(f"PASS: signed ARM64 APK, Android 12+, {len(libraries)} native libraries, ELF/APK 16 KB alignment")


if __name__ == "__main__":
    verify(Path(sys.argv[1]).resolve(strict=True))
