#!/usr/bin/env bash
set -euo pipefail
umask 077

# Invoked only while devhost's resources lock is held. Never pipe yes, copy
# license hashes, or accept a new license on the user's behalf.
SDK=/workspace/devhost/toolchains/android-sdk
STATE=/workspace/devhost/state
BUILD=15859902
ARCHIVE_SHA=4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583
URL="https://dl.google.com/android/repository/commandlinetools-linux-${BUILD}_latest.zip"
SOURCE_DIR=$(cd -- "$(dirname -- "$0")" && pwd)

test "$(uname -m)" = x86_64
test "$(uname -s)" = Linux
mkdir -p -- "$SDK" "$STATE"
export ANDROID_HOME="$SDK" ANDROID_SDK_ROOT="$SDK"
export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64
export PATH="$JAVA_HOME/bin:$PATH"

if ! test -x "$JAVA_HOME/bin/javac"; then
    sudo -n apt-get update
    sudo -n apt-get install --no-install-recommends -y openjdk-21-jdk-headless unzip ca-certificates curl
fi
"$JAVA_HOME/bin/java" -version
"$JAVA_HOME/bin/javac" -version

if ! test -x "$SDK/cmdline-tools/latest/bin/sdkmanager"; then
    STAGE=$(mktemp -d /workspace/devhost/toolchains/.android-bootstrap-XXXXXX)
    cleanup() {
        if test -n "${STAGE:-}" && test -d "$STAGE" && ! test -L "$STAGE"; then
            python3 -B - "$STAGE" <<'PY'
from pathlib import Path
import shutil, sys
p = Path(sys.argv[1])
assert p.parent == Path('/workspace/devhost/toolchains')
assert p.name.startswith('.android-bootstrap-') and not p.is_symlink()
shutil.rmtree(p)
PY
        fi
    }
    trap cleanup EXIT
    curl --fail --location --proto '=https' --tlsv1.2 --retry 2 "$URL" -o "$STAGE/tools.zip"
    printf '%s  %s\n' "$ARCHIVE_SHA" "$STAGE/tools.zip" | sha256sum --check --status
    unzip -q "$STAGE/tools.zip" -d "$STAGE/unpacked"
    test -x "$STAGE/unpacked/cmdline-tools/bin/sdkmanager"
    mkdir -p -- "$SDK/cmdline-tools"
    if test -e "$SDK/cmdline-tools/latest"; then
        printf '%s\n' 'Existing incomplete cmdline-tools/latest is protected; inspect it before retry.' >&2
        exit 74
    fi
    mv -- "$STAGE/unpacked/cmdline-tools" "$SDK/cmdline-tools/latest"
    python3 -B - "$STATE/android-bootstrap.json" "$BUILD" "$ARCHIVE_SHA" <<'PY'
import json, sys
from pathlib import Path
p = Path(sys.argv[1])
p.write_text(json.dumps({'build':sys.argv[2], 'sha256':sys.argv[3]})+'\n')
PY
fi

SDKMANAGER="$SDK/cmdline-tools/latest/bin/sdkmanager"
# EOF declines new terms. A successful exit alone does not prove acceptance.
LICENSE_OUTPUT=$("$SDKMANAGER" --sdk_root="$SDK" --licenses </dev/null 2>&1) || true
if ! printf '%s' "$LICENSE_OUTPUT" | grep -q 'All SDK package licenses accepted'; then
    printf '%s\n' 'ANDROID_LICENSE_HANDOFF_REQUIRED' >&2
    printf '%s\n' 'Run this yourself in the cloud terminal, read the terms and answer interactively:' >&2
    printf '%s\n' 'JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64 /workspace/devhost/toolchains/android-sdk/cmdline-tools/latest/bin/sdkmanager --sdk_root=/workspace/devhost/toolchains/android-sdk --licenses' >&2
    exit 78
fi
unset LICENSE_OUTPUT

# Bootstrap is pinned to an official archive; never silently update latest.
PACKAGES=()
while IFS= read -r package; do
    case "$package" in ''|'cmdline-tools;latest') continue ;; esac
    PACKAGES+=("$package")
done < "$SOURCE_DIR/android-packages.txt"
"$SDKMANAGER" --sdk_root="$SDK" --install "${PACKAGES[@]}" </dev/null
rustup toolchain install 1.94.1 --profile minimal --target aarch64-linux-android --no-self-update

python3 -B - "$SDK" "$STATE" <<'PY'
from pathlib import Path
import hashlib, json, os, sys, tempfile
sdk, state = map(Path, sys.argv[1:])
def props(p):
    return dict(line.split('=',1) for line in p.read_text().splitlines()
                if '=' in line and not line.startswith('#'))
def values(p):
    return {k.strip():v.strip() for k,v in props(p).items()}
expected = {'platforms/android-36':None, 'build-tools/36.0.0':'36.0.0',
            'ndk/28.2.13676358':'28.2.13676358', 'platform-tools':None,
            'cmdline-tools/latest':None}
result = {}
for package, version in expected.items():
    p = sdk/package/'source.properties'
    d = values(p)
    if version and d.get('Pkg.Revision') != version:
        raise SystemExit('SDK version mismatch: '+package)
    if package == 'platforms/android-36' and d.get('AndroidVersion.ApiLevel') != '36':
        raise SystemExit('Android API level mismatch')
    result[package] = {'revision':d.get('Pkg.Revision'), 'source_sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
bootstrap = state/'android-bootstrap.json'
if not bootstrap.is_file():
    raise SystemExit('Pinned command-line tools receipt missing; do not relabel an unknown install.')
result['bootstrap'] = json.loads(bootstrap.read_text())
result['java_major'] = 21
result['rust'] = '1.94.1/aarch64-linux-android'
fd, tmp = tempfile.mkstemp(prefix='.android-receipt-',dir=state)
with os.fdopen(fd,'w') as f:
    json.dump(result,f,sort_keys=True); f.write('\n')
os.replace(tmp,state/'android-install.json')
print(json.dumps(result,sort_keys=True))
PY
"$SDK/platform-tools/adb" version
rustup target list --installed --toolchain 1.94.1
