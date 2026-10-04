#!/usr/bin/env python3
"""Install pinned public toolchains; run explicitly during bootstrap only."""
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.parse
import urllib.request

SOURCE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('devhost', SOURCE / 'devhost.py')
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)


def fetch(url, path, digest=None, integrity=None):
    if urllib.parse.urlparse(url).scheme != 'https':
        raise d.Refused('Toolchain downloads require HTTPS')
    h = hashlib.sha256()
    h512 = hashlib.sha512()
    request = urllib.request.Request(url, headers={'User-Agent': 'devhost-toolchain/1'})
    with urllib.request.urlopen(request, timeout=90) as response, path.open('wb') as out:
        if urllib.parse.urlparse(response.url).scheme != 'https':
            raise d.Refused('Insecure download redirect')
        while chunk := response.read(1024 * 1024):
            h.update(chunk); h512.update(chunk); out.write(chunk)
    if digest and h.hexdigest() != digest:
        raise d.Refused('Official toolchain SHA-256 mismatch')
    if integrity and 'sha512-' + base64.b64encode(h512.digest()).decode() != integrity:
        raise d.Refused('Official package integrity mismatch')
    return h.hexdigest()


def install_archive(host, name, item, stage):
    dest = host.home / 'toolchains' / name
    receipt = host.state / (name + '-install.json')
    host.safe(dest, [host.home / 'toolchains'])
    binary = dest / {'node': 'bin/node', 'go': 'bin/go', 'uv': 'uv'}[name]
    if binary.is_file() and receipt.exists():
        installed = json.loads(receipt.read_text())
        if (installed.get('sha256') == item['sha256'] and
                installed.get('executable_sha256') == hashlib.sha256(binary.read_bytes()).hexdigest()):
            return
    archive = stage / (name + '.archive')
    fetch(item['url'], archive, item['sha256'])
    unpack = stage / (name + '-unpacked')
    unpack.mkdir()
    with tarfile.open(archive) as tf:
        tf.extractall(unpack, filter='data')
    source = unpack / item['prefix']
    if not source.is_dir(): raise d.Refused('Unexpected archive layout')
    staged_binary = source / binary.relative_to(dest)
    digest = hashlib.sha256(staged_binary.read_bytes()).hexdigest()
    if dest.exists():
        if not binary.is_file() or hashlib.sha256(binary.read_bytes()).hexdigest() != digest:
            raise d.Refused('Existing toolchain differs from pinned archive; inspect before replacement')
    else:
        shutil.move(source, dest)
    d.atomic_json(receipt, {**item, 'executable_sha256': digest})


def install():
    host = d.Host()
    if os.uname().sysname != 'Linux' or os.uname().machine != 'x86_64':
        raise d.Refused('Toolchains must be installed on the Linux cloud host')
    manifest_path = SOURCE / 'toolchains.lock.json'
    lock = json.loads(manifest_path.read_text())
    with host.lock():
        host.preflight_locked()
        tools = host.home / 'toolchains'
        tools.mkdir(exist_ok=True)
        legacy_rust = host.cache / 'rustup'
        rust = tools / 'rustup'
        if legacy_rust.exists() and not rust.exists():
            host.safe(legacy_rust, [host.cache]); host.safe(rust, [tools])
            shutil.move(legacy_rust, rust)
        with tempfile.TemporaryDirectory(prefix='.tools-', dir=tools) as temp:
            stage = Path(temp)
            for name in ('node', 'go', 'uv'):
                install_archive(host, name, lock[name], stage)
            env = host.environment()
            # The runtime managers also verify their own packages. Pin and
            # independently verify the selected upstream inputs before using them.
            fetch(lock['rust']['manifest_url'], stage / 'rust-manifest.toml',
                  lock['rust']['manifest_sha256'])
            fetch(lock['python']['url'], stage / 'python.tar.gz', lock['python']['sha256'])
            for version, item in lock['pnpm_packages'].items():
                fetch(item['url'], stage / ('pnpm-' + version + '.tgz'),
                      item['sha256'], item['integrity'])
            corepack = tools / 'corepack/bin/corepack'
            if not corepack.exists():
                package = stage / 'corepack.tgz'
                fetch(lock['corepack']['url'], package, lock['corepack']['sha256'],
                      lock['corepack']['integrity'])
                subprocess.run([str(tools / 'node/bin/npm'), 'install', '-g', '--prefix',
                                str(tools / 'corepack'), str(package), '--ignore-scripts'], env=env, check=True)
            uv = str(tools / 'uv/uv')
            subprocess.run([uv, 'python', 'install', lock['python']['version']], env=env, check=True)
            python = subprocess.check_output([uv, 'python', 'find', lock['python']['version']], env=env, text=True).strip()
            version = subprocess.check_output([python, '-c', 'import platform;print(platform.python_version())'], text=True).strip()
            python_digest = hashlib.sha256(Path(python).read_bytes()).hexdigest()
            if (not lock['python'].get('resolve_once', False) and
                    lock['python'].get('executable_sha256', python_digest) != python_digest):
                raise d.Refused('Pinned Python executable SHA-256 mismatch')
            lock['python'].update(version=version, resolve_once=False,
                                  executable_sha256=python_digest)
            for name in ('python', 'python3'):
                shim = host.home / 'bin' / name
                if shim.is_symlink():
                    if not shim.resolve().is_relative_to(tools / 'python'):
                        raise d.Refused('Existing Python entrypoint is not managed')
                    shim.unlink()
                elif shim.exists():
                    raise d.Refused('Existing Python entrypoint requires inspection')
                shim.symlink_to(python)
            for repo in (name for name in d.REPOS if d.PROFILES[name]['python']):
                venv = host.home / 'venvs' / repo
                existing = venv / 'bin/python'
                existing_version = subprocess.check_output(
                    [str(existing), '-c', 'import platform;print(platform.python_version())'],
                    text=True).strip() if existing.exists() else None
                if existing_version != version:
                    # Preserve contents; dependency installation fills the new Python site-packages.
                    subprocess.run([uv, 'venv', '--allow-existing', '--python', python, str(venv)], env=env, check=True)
            rustup = host.cache / 'cargo-home/bin/rustup'
            if not rustup.exists():
                installer = stage / 'rustup-init'
                fetch(lock['rustup']['url'], installer, lock['rustup']['sha256'])
                installer.chmod(0o700)
                subprocess.run([str(installer), '-y', '--no-modify-path', '--profile', 'minimal',
                                '--default-toolchain', lock['rust']['version']], env=env, check=True)
            rustup_version = subprocess.check_output([str(rustup), '--version'], env=env,
                                                     text=True, stderr=subprocess.DEVNULL)
            if not rustup_version.startswith('rustup ' + lock['rustup']['version'] + ' '):
                raise d.Refused('Installed rustup differs from the pinned version')
            subprocess.run([str(rustup), 'toolchain', 'install', lock['rust']['version'], '--profile', 'minimal',
                            '--target', lock['rust']['target'], '--component', ','.join(lock['rust']['components']),
                            '--no-self-update'], env=env, check=True)
            packages = ['build-essential', 'pkg-config', 'libwebkit2gtk-4.1-dev', 'libssl-dev',
                        'libxdo-dev', 'libayatana-appindicator3-dev', 'librsvg2-dev', 'patchelf',
                        'openjdk-21-jdk-headless', 'unzip', 'ca-certificates']
            missing = []
            for package in packages:
                p = subprocess.run(['dpkg-query', '-W', '-f=${db:Status-Status}\t${Version}', package], capture_output=True, text=True)
                expected = lock['apt_packages'][package]
                if p.returncode or not p.stdout.startswith('installed\t'):
                    missing.append(package + '=' + expected)
                elif p.stdout.split('\t', 1)[1] != expected:
                    raise d.Refused('System package differs from pinned manifest: ' + package)
            if missing:
                subprocess.run(['sudo', '-n', 'apt-get', 'update'], check=True)
                subprocess.run(['sudo', '-n', 'apt-get', 'install', '-y', '--no-install-recommends', *missing], check=True)
            cargo = str(host.cache / 'cargo-home/bin/cargo')
            cli = host.cache / 'cargo-home/bin/cargo-tauri'
            current_cli = subprocess.run([str(cli), '--version'], capture_output=True, text=True) if cli.exists() else None
            if current_cli is None or current_cli.returncode or lock['cargo_tauri']['version'] not in current_cli.stdout:
                fetch(lock['cargo_tauri']['crate_url'], stage / 'tauri-cli.crate',
                      lock['cargo_tauri']['crate_sha256'])
                env['CARGO_TARGET_DIR'] = str(host.cache / 'cargo-target/tools')
                subprocess.run([cargo, 'install', 'tauri-cli', '--version', lock['cargo_tauri']['version'], '--locked'],
                               env=env, check=True)
            lock['cargo_tauri']['executable_sha256'] = hashlib.sha256(cli.read_bytes()).hexdigest()
            for version in sorted(set(d.PNPM.values())):
                subprocess.run([str(corepack), 'pnpm@' + version, '--version'], env=env, check=True)
            d.atomic_json(manifest_path, lock)
            d.atomic_json(host.state / 'toolchains-installed.json', lock)
            # Bash reads this before its usual noninteractive early return.
            hook = '[ ! -r /workspace/devhost/env ] || . /workspace/devhost/env\n'
            for name in ('.bashrc', '.profile'):
                profile = Path.home() / name
                if profile.is_symlink():
                    raise d.Refused('Shell profile symlink requires inspection')
                previous = profile.read_text() if profile.exists() else ''
                if hook not in previous:
                    profile.write_text(hook + previous)
            print('Pinned toolchains installed; runtime lock manifest finalized.')


if __name__ == '__main__':
    install()
