#!/usr/bin/env python3
"""Install or inspect pinned cloud toolchains and native build dependencies."""
import argparse
import base64
import ctypes
import ctypes.util
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


def restore_missing_tree(source, destination):
    """Repair a partially retained pinned toolchain, without overwriting changes."""
    for entry in sorted(source.rglob('*'), key=lambda p: len(p.parts)):
        target = destination / entry.relative_to(source)
        if any(p.is_symlink() for p in target.parents):
            raise d.Refused('Symlink parent in existing toolchain')
        if entry.is_symlink():
            link = os.readlink(entry)
            if not entry.resolve().is_relative_to(source.resolve()):
                raise d.Refused('Toolchain link escapes archive')
            if target.is_symlink() and os.readlink(target) == link:
                continue
            if target.exists() or target.is_symlink():
                raise d.Refused('Existing toolchain link differs from pinned archive')
            target.symlink_to(link)
        elif target.is_symlink():
            raise d.Refused('Symlink replacing a regular toolchain entry')
        elif entry.is_dir():
            target.mkdir(exist_ok=True)
        elif entry.is_file():
            if target.exists():
                if (not target.is_file() or
                        hashlib.sha256(target.read_bytes()).digest() != hashlib.sha256(entry.read_bytes()).digest()):
                    raise d.Refused('Existing toolchain file differs from pinned archive')
            else:
                with target.open('xb') as output:
                    output.write(entry.read_bytes())
                target.chmod(entry.stat().st_mode & 0o777)


def install_archive(host, name, item, stage):
    dest = host.home / 'toolchains' / name
    receipt = host.state / (name + '-install.json')
    host.safe(dest, [host.home / 'toolchains'])
    binary = dest / {'node': 'bin/node', 'go': 'bin/go', 'uv': 'uv'}[name]
    required = [binary]
    if name == 'node':
        required += [dest / 'bin/npm', dest / 'bin/npx']
    if all(p.is_file() for p in required) and receipt.exists():
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
        restore_missing_tree(source, dest)
    else:
        shutil.move(source, dest)
    d.atomic_json(receipt, {**item, 'executable_sha256': digest})


def system_status(lock, env=None):
    """Read actual native build prerequisites, not just installation receipts."""
    packages, modules, libraries = {}, {}, {}
    for package, expected in lock['apt_packages'].items():
        result = subprocess.run(
            ['dpkg-query', '-W', '-f=${db:Status-Status}\t${Version}', package],
            capture_output=True, text=True, env=env)
        fields = result.stdout.strip().split('\t', 1)
        installed = result.returncode == 0 and len(fields) == 2 and fields[0] == 'installed'
        actual = fields[1] if installed else None
        packages[package] = {'installed': installed, 'expected': expected,
                             'version': actual, 'matches': actual == expected}
    for module in lock.get('native_checks', {}).get('pkg_config', []):
        try:
            result = subprocess.run(['pkg-config', '--modversion', module],
                                    capture_output=True, text=True, env=env)
            modules[module] = {'available': result.returncode == 0,
                               'version': result.stdout.strip() if result.returncode == 0 else None}
        except OSError:
            modules[module] = {'available': False, 'version': None}
    for library in lock.get('native_checks', {}).get('shared_libraries', []):
        try:
            name = ctypes.util.find_library(library)
            libraries[library] = bool(name and ctypes.CDLL(name))
        except OSError:
            libraries[library] = False
    return {'ready': (all(item['matches'] for item in packages.values())
                      and all(item['available'] for item in modules.values())
                      and all(libraries.values())),
            'packages': packages, 'pkg_config': modules, 'shared_libraries': libraries}


def install_system_packages(lock, env=None):
    """Install the complete pinned manifest; never upgrade or downgrade drift."""
    before = system_status(lock, env)
    drift = [name for name, item in before['packages'].items()
             if item['installed'] and not item['matches']]
    if drift:
        raise d.Refused('System packages differ from pinned manifest: ' + ', '.join(drift))
    missing = [name + '=' + item['expected'] for name, item in before['packages'].items()
               if not item['installed']]
    if missing:
        subprocess.run(['sudo', '-n', 'apt-get', 'update'], check=True)
        subprocess.run(['sudo', '-n', 'env', 'DEBIAN_FRONTEND=noninteractive',
                        'apt-get', 'install', '-y', '--no-install-recommends', '--no-remove',
                        *missing], check=True)
    after = system_status(lock, env)
    if not after['ready']:
        failed = ([name for name, item in after['packages'].items() if not item['matches']]
                  + [name for name, item in after['pkg_config'].items() if not item['available']]
                  + [name for name, available in after['shared_libraries'].items() if not available])
        raise d.Refused('Native build dependencies unavailable after setup: ' + ', '.join(failed))
    return after


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
            install_system_packages(lock, env)
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
                cached = host.cache / 'corepack/v1/pnpm' / version
                if cached.exists():
                    host.safe(cached, [host.cache])
                    unpack = stage / ('pnpm-' + version)
                    unpack.mkdir()
                    with tarfile.open(stage / ('pnpm-' + version + '.tgz')) as package:
                        package.extractall(unpack, filter='data')
                    restore_missing_tree(unpack / 'package', cached)
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--system-only', action='store_true')
    mode.add_argument('--check-system', action='store_true')
    args = parser.parse_args()
    if not (args.system_only or args.check_system):
        install()
        return 0
    host = d.Host()
    lock = json.loads((SOURCE / 'toolchains.lock.json').read_text())
    if args.check_system:
        result = system_status(lock, host.environment())
    else:
        if os.uname().sysname != 'Linux' or os.uname().machine != 'x86_64':
            raise d.Refused('System packages belong on the Linux cloud host')
        with host.lock():
            host.preflight_locked()
            result = install_system_packages(lock, host.environment())
    print(json.dumps(result, sort_keys=True))
    return 0 if result['ready'] else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except d.Refused as error:
        import sys
        print(str(error), file=sys.stderr)
        raise SystemExit(error.code)
