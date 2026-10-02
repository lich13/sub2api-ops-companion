#!/usr/bin/env python3
"""Disk admission, conservative cache collection and managed cloud builds.

Python's flock(2) is the same advisory lock used by the flock CLI. All entry
points share it; no tool credentials, command arguments or build output are
written to the small operational event log.
"""
import argparse
import contextlib
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

GIB = 1024 ** 3
DAY = 86400
REPOS = ('sub2api-ops-companion', 'lich13studio', 'lich13-switch', 'loon-mihomo-rule-sync')
ENTRYPOINTS = ('devhost-clean', 'devhost-run', 'devhost-up', 'devhost-status',
               'android-sdk-ensure', 'devhost-sdk-prune')
MIN_FREE = 40 * GIB
EMERGENCY_FREE = 30 * GIB
STOP_FREE = 15 * GIB
CACHE_BUDGET = 50 * GIB
MIN_INODES = .10
STOP_INODES = .02
VSCODE_CLI_URL = 'https://update.code.visualstudio.com/latest/cli-linux-x64/stable'
VSCODE_CLI_API = 'https://update.code.visualstudio.com/api/update/cli-linux-x64/stable/latest'
VSCODE_CLI_HOSTS = frozenset({
    'update.code.visualstudio.com',
    'vscode.download.prss.microsoft.com',
})
VSCODE_CLI_MAX_ARCHIVE = 80 * 1024 * 1024


class Refused(Exception):
    def __init__(self, message, code=75):
        super().__init__(message)
        self.code = code


def atomic_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.new-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as handle:
            json.dump(data, handle, sort_keys=True)
            handle.write('\n')
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def tree_info(path):
    """Allocated size and newest mtime; never traverse a symlink or a mount."""
    if not path.exists() and not path.is_symlink():
        return 0, 0, False
    first = path.lstat()
    device = first.st_dev
    total, newest, unsafe = 0, 0, False
    stack = [path]
    while stack:
        item = stack.pop()
        st = item.lstat()
        total += st.st_blocks * 512
        newest = max(newest, st.st_mtime)
        if stat.S_ISLNK(st.st_mode) or st.st_dev != device:
            unsafe = True
            continue
        if stat.S_ISDIR(st.st_mode):
            stack.extend(item.iterdir())
    return total, newest, unsafe


class Host:
    def __init__(self, root=Path('/workspace'), probe=None, now=None, process_probe=None):
        self.root = Path(root).absolute()
        if self.root == Path('/') or self.root.is_symlink():
            raise Refused('Workspace must be a real dedicated directory.')
        self.home = self.root / 'devhost'
        self.state = self.home / 'state'
        self.cache = self.root / 'cache'
        self.now = time.time() if now is None else now
        self.probe = probe or self.space
        self.process_probe = process_probe or self.unmanaged_processes

    def safe(self, path, roots):
        path = Path(path).absolute()
        if '..' in path.parts:
            raise Refused('Parent-directory traversal refused.')
        if path == self.root or path == Path('/'):
            raise Refused('Refusing a workspace or filesystem root.')
        if not any(path == r or r in path.parents for r in roots):
            raise Refused('Path is outside the deletion allowlist.')
        for part in (path, *path.parents):
            if part.is_symlink():
                raise Refused('Symlink traversal refused.')
            if part == self.root:
                break
        if path.exists() and path.stat().st_dev != self.root.stat().st_dev:
            raise Refused('Mount traversal refused.')
        return path

    def space(self):
        usage = os.statvfs(self.root)
        return {'free_bytes': usage.f_bavail * usage.f_frsize,
                'inodes_free': usage.f_favail, 'inodes_total': usage.f_files,
                'readonly': bool(usage.f_flag & getattr(os, 'ST_RDONLY', 1))}

    def healthy(self, space, minimum=MIN_FREE, inode_min=MIN_INODES):
        if space.get('readonly'):
            raise Refused('Filesystem is read-only; no build or cleanup attempted.', 74)
        if space['inodes_total'] <= 0:
            raise Refused('Cannot establish inode capacity.', 74)
        return (space['free_bytes'] >= minimum and
                space['inodes_free'] / space['inodes_total'] >= inode_min)

    @contextlib.contextmanager
    def lock(self, name='resources', blocking=False):
        self.safe(self.state, [self.home])
        self.state.mkdir(parents=True, exist_ok=True)
        path = self.state / (name + '.lock')
        self.safe(path, [self.state])
        fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
            except BlockingIOError:
                raise Refused('Another managed build or cleanup holds the lock.')
            yield fd
        finally:
            os.close(fd)

    def unmanaged_processes(self):
        """Detect builds that bypassed the wrapper; report only PID/tool names."""
        if not Path('/proc').exists():
            raise Refused('Linux /proc is required for production cleanup.')
        active = []
        for pid in Path('/proc').iterdir():
            if not pid.name.isdigit() or int(pid.name) == os.getpid():
                continue
            try:
                exe = (pid / 'exe').resolve().name
                args = (pid / 'cmdline').read_bytes().split(b'\0')
                cwd = (pid / 'cwd').resolve()
                relevant = (self.root == cwd or self.root in cwd.parents)
                compiler = exe in ('cargo', 'rustc', 'rustup', 'go', 'javac', 'sdkmanager')
                gradle = exe == 'java' and any(b'gradle' in a.lower() for a in args)
                package = any(re.search(rb'(^|/)(pnpm|npm|uv)([./]|$)', a) for a in args[:3])
                if (compiler or gradle or package) and (relevant or gradle):
                    active.append({'pid': int(pid.name), 'tool': exe})
            except (FileNotFoundError, ProcessLookupError):
                continue
            except PermissionError:
                if pid.exists() and pid.stat().st_uid == os.geteuid():
                    raise Refused('Cannot inspect an owned process; cleanup refused.')
                continue
        return active

    def cache_bytes(self):
        paths = [self.cache]
        for repo in REPOS:
            base = self.root / 'repos' / repo
            paths.extend([base / 'node_modules', base / 'desktop/node_modules'])
        return sum(tree_info(p)[0] for p in paths if p.exists())

    def event(self, event, **fields):
        log = self.home / 'logs/maintenance.jsonl'
        self.safe(log, [self.home / 'logs'])
        log.parent.mkdir(parents=True, exist_ok=True)
        if log.exists() and log.stat().st_size > 1024 * 1024:
            old = log.with_suffix('.jsonl.1')
            self.safe(old, [log.parent])
            os.replace(log, old)
        with log.open('a') as handle:
            handle.write(json.dumps({'time': int(time.time()), 'event': event, **fields}) + '\n')

    def candidates(self):
        # Age uses the newest child mtime, not just directory mtime or atime.
        items = []
        logs = self.home / 'logs'
        if logs.exists():
            for p in logs.iterdir():
                if p.name not in ('maintenance.jsonl', 'tunnel.log') and p.suffix in ('.log', '.1'):
                    items.append((p, 7, 'log', None))
        targets = self.cache / 'cargo-target'
        for repo in REPOS:
            items.append((targets / repo, 14, 'target', repo))
        # Never delete CARGO_HOME, credentials.toml, config.toml or bin.
        registry = self.cache / 'cargo-home/registry'
        for kind in ('cache', 'src', 'index'):
            parent = registry / kind
            if parent.is_dir() and not parent.is_symlink():
                items.extend((p, 30, 'download-cache', None) for p in parent.iterdir())
        for rel in ('npm/_cacache', 'npm/_logs', 'pip', 'uv', 'go-build'):
            items.append((self.cache / rel, 30, 'download-cache', None))
        gradle = self.cache / 'gradle'
        caches = gradle / 'caches'
        if caches.is_dir() and not caches.is_symlink():
            for p in caches.iterdir():
                if p.name.startswith('build-cache-'):
                    items.append((p, 5, 'gradle', None))
                elif re.fullmatch(r'\d+(?:\.\d+)+(?:-.*)?', p.name):
                    items.append((p, 10 if 'SNAPSHOT' in p.name else 45, 'gradle', None))
        dists = gradle / 'wrapper/dists'
        if dists.is_dir() and not dists.is_symlink():
            items.extend((p, 10 if 'SNAPSHOT' in p.name else 45, 'gradle', None)
                         for p in dists.iterdir() if p.is_dir())
        daemons = gradle / 'daemon'
        if daemons.is_dir() and not daemons.is_symlink():
            for version in daemons.iterdir():
                if version.is_dir() and not version.is_symlink():
                    items.extend((p, 14, 'gradle-log', None) for p in version.iterdir()
                                 if re.fullmatch(r'daemon-\d+\.out\.log', p.name))
        # Only the tool-owned tmp namespace can be collected. Unknown artifacts
        # and releases are protected regardless of tag, age or disk pressure.
        tmp = self.root / 'artifacts/tmp'
        if tmp.is_dir() and not tmp.is_symlink():
            for p in tmp.iterdir():
                if p.is_dir() and (p / '.devhost-temporary').is_file():
                    items.append((p, 7, 'temporary-artifact', None))
        return items

    def clean_locked(self, mode, apply=False):
        before = self.probe()
        # Collection is allowed while below the build admission threshold;
        # only an unreadable filesystem or an unusable inode table blocks it.
        if before.get('readonly'):
            raise Refused('Filesystem is read-only; no build or cleanup attempted.', 74)
        if before.get('inodes_total', 0) <= 0 or before['inodes_free'] / before['inodes_total'] < STOP_INODES:
            raise Refused('Filesystem inode reserve is below 2%; cleanup stopped.', 74)
        active = self.process_probe()
        if active:
            raise Refused('Unmanaged build processes found; cleanup skipped: ' +
                          ', '.join(str(p['pid']) + ':' + p['tool'] for p in active))
        allowed = [self.cache, self.home / 'logs', self.root / 'artifacts/tmp']
        removed, skipped = [], []
        for path, days, kind, repo in self.candidates():
            if not path.exists() and not path.is_symlink():
                continue
            try:
                self.safe(path, allowed)
                size, newest, unsafe = tree_info(path)
                if unsafe or self.now - newest <= days * DAY:
                    continue
                cm = self.lock('repo-' + repo) if repo else contextlib.nullcontext()
                with cm:
                    # Recheck after acquiring the lock. Never remove active logs.
                    if tree_info(path)[1] != newest:
                        continue
                    record = {'path': str(path.relative_to(self.root)), 'bytes': size, 'kind': kind}
                    if apply:
                        self.safe(path, allowed)
                        if path.is_dir():
                            if not shutil.rmtree.avoids_symlink_attacks:
                                raise Refused('Platform lacks symlink-safe rmtree.', 74)
                            shutil.rmtree(path)
                        else:
                            path.unlink()
                    removed.append(record)
            except Refused as exc:
                skipped.append({'path': str(path.relative_to(self.root)), 'reason': str(exc)})
        # Use pnpm's own reference-aware collector; never remove its live store
        # by recursively guessing the on-disk format.
        store = self.cache / 'pnpm-store'
        prune_at = self.state / 'pnpm-prune.json'
        pressure = before['free_bytes'] < MIN_FREE or self.cache_bytes() > CACHE_BUDGET
        due = not prune_at.exists() or self.now - prune_at.stat().st_mtime > 7 * DAY
        if store.exists() and (due or pressure):
            self.safe(store, [self.cache])
            if tree_info(store)[2]:
                skipped.append({'path': 'cache/pnpm-store', 'reason': 'symlink found'})
            elif apply:
                pnpm = shutil.which('pnpm', path=self.environment().get('PATH'))
                if pnpm:
                    result = subprocess.run([pnpm, '--store-dir', str(store), 'store', 'prune'],
                                            env=self.environment(), stdin=subprocess.DEVNULL,
                                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    if result.returncode == 0:
                        atomic_json(prune_at, {'time': int(time.time())})
                    else:
                        skipped.append({'path': 'cache/pnpm-store', 'reason': 'pnpm prune failed'})
            else:
                skipped.append({'path': 'cache/pnpm-store', 'reason': 'native prune eligible (dry-run)'})
        after = self.probe()
        result = {'mode': mode, 'applied': apply, 'candidates': removed, 'skipped': skipped,
                  'free_bytes': after['free_bytes'], 'inodes_free': after['inodes_free'],
                  'released_bytes': max(0, after['free_bytes'] - before['free_bytes']) if apply else 0,
                  'cache_bytes': self.cache_bytes()}
        if apply:
            self.event('cleanup', mode=mode, count=len(removed), released_bytes=result['released_bytes'])
        return result

    def clean(self, mode, apply=False):
        with self.lock():
            return self.clean_locked(mode, apply)

    def preflight_locked(self):
        # Cleanup must run before the 40 GiB admission gate so emergency
        # collection is not rejected by the healthy() check while space is low.
        space = self.probe()
        if space.get('readonly'):
            raise Refused('Filesystem is read-only; no build or cleanup attempted.', 74)
        if space.get('inodes_total', 0) <= 0 or space['inodes_free'] / space['inodes_total'] < STOP_INODES:
            raise Refused('Filesystem inode reserve is below 2%; cleanup stopped.', 74)
        mode = 'emergency' if space['free_bytes'] < EMERGENCY_FREE else 'preflight'
        result = self.clean_locked(mode, True)
        if not self.healthy(self.probe()) or result['cache_bytes'] > CACHE_BUDGET:
            raise Refused('Build blocked: need 40 GiB free, 10% free inodes and caches <= 50 GiB.')
        # Actual writable/fsync check catches EROFS/EIO before starting a build.
        fd, name = tempfile.mkstemp(prefix='.probe-', dir=self.state)
        try:
            os.write(fd, b'devhost\n')
            os.fsync(fd)
        finally:
            os.close(fd)
            os.unlink(name)
        return result

    def environment(self, repo=None):
        env = os.environ.copy()
        sdk = self.home / 'toolchains/android-sdk'
        values = {'ANDROID_HOME': sdk, 'ANDROID_SDK_ROOT': sdk,
                  'NDK_HOME': sdk / 'ndk/28.2.13676358', 'GRADLE_USER_HOME': self.cache / 'gradle',
                  'CARGO_HOME': self.cache / 'cargo-home', 'PIP_CACHE_DIR': self.cache / 'pip',
                  'RUSTUP_HOME': self.cache / 'rustup',
                  'UV_CACHE_DIR': self.cache / 'uv', 'GOCACHE': self.cache / 'go-build',
                  'npm_config_cache': self.cache / 'npm',
                  'npm_config_store_dir': self.cache / 'pnpm-store'}
        env.update({k: str(v) for k, v in values.items()})
        java = Path('/usr/lib/jvm/java-21-openjdk-amd64')
        if java.exists():
            env['JAVA_HOME'] = str(java)
        paths = [self.home / 'bin', self.home / 'node/bin', self.home / 'npm/bin',
                 self.cache / 'cargo-home/bin', Path.home() / '.cargo/bin',
                 sdk / 'cmdline-tools/latest/bin', sdk / 'platform-tools']
        if java.exists():
            paths.insert(0, java / 'bin')
        env['PATH'] = os.pathsep.join(str(p) for p in paths) + os.pathsep + env.get('PATH', '')
        env['GRADLE_OPTS'] = (env.get('GRADLE_OPTS', '') +
                              ' -Dorg.gradle.daemon=false -Dorg.gradle.workers.max=2').strip()
        env['CARGO_BUILD_JOBS'] = '2'
        env['RUSTUP_TOOLCHAIN'] = '1.94.1'
        env['VSCODE_CLI_DATA_DIR'] = str(self.home / 'vscode-cli/data')
        if repo:
            env['CARGO_TARGET_DIR'] = str(self.cache / 'cargo-target' / repo)
        return env

    def run(self, repo, command, subdir='.'):
        if repo not in REPOS:
            raise Refused('Repository is not enrolled (NexusHub is excluded).', 64)
        base = self.root / 'repos' / repo
        cwd = self.safe(base / subdir, [base])
        if not cwd.is_dir() or not (base / '.git').exists():
            raise Refused('Repository or working directory is missing.', 64)
        if not command:
            raise Refused('A command is required after --.', 64)
        with self.lock():
            self.preflight_locked()
            with self.lock('repo-' + repo):
                self.event('build-start', repo=repo)
                process = subprocess.Popen(command, cwd=cwd, env=self.environment(repo), start_new_session=True)
                interrupted = False
                def cancel(signum, _frame):
                    nonlocal interrupted
                    interrupted = True
                    try:
                        os.killpg(process.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                previous = {sig: signal.signal(sig, cancel) for sig in (signal.SIGINT, signal.SIGTERM)}
                stopped_at = None
                code = 74
                try:
                    while process.poll() is None:
                        try:
                            ok = self.healthy(self.probe(), STOP_FREE, STOP_INODES)
                        except (OSError, Refused):
                            ok = False
                        if (not ok or interrupted) and stopped_at is None:
                            cancel(signal.SIGTERM, None)
                            stopped_at = time.monotonic()
                        if stopped_at is not None and time.monotonic() - stopped_at > 20:
                            with contextlib.suppress(ProcessLookupError):
                                os.killpg(process.pid, signal.SIGKILL)
                        time.sleep(2)
                    code = process.wait()
                    if interrupted:
                        code = 74
                finally:
                    for sig, handler in previous.items():
                        signal.signal(sig, handler)
                    if process.poll() is None:
                        cancel(signal.SIGTERM, None)
                        try:
                            process.wait(timeout=20)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                    target = self.cache / 'cargo-target' / repo
                    if target.exists() and not target.is_symlink():
                        os.utime(target, None)
                    self.event('build-end', repo=repo, exit_code=code)
            try:
                result = self.clean_locked('postbuild', True)
                print(json.dumps(result, sort_keys=True))
            except (Refused, OSError) as exc:
                print('Postbuild cleanup skipped: ' + str(exc), file=sys.stderr)
            return code if code >= 0 else 128 - code

    def install(self, source):
        if sys.platform != 'linux' or os.uname().machine != 'x86_64':
            raise Refused('Install requires Linux x86_64; local Mac is not a build host.', 64)
        with self.lock():
            for path in (self.home / 'bin', self.home / 'lib', self.home / 'logs',
                         self.home / 'toolchains', self.home / 'vscode-cli',
                         self.cache / 'gradle/init.d', self.root / 'artifacts/tmp'):
                self.safe(path, [self.home, self.cache, self.root / 'artifacts'])
                path.mkdir(parents=True, exist_ok=True)
            for name in ('devhost.py', 'android-setup.sh', 'android-packages.txt'):
                dest = self.home / 'lib' / name
                self.safe(dest, [self.home / 'lib'])
                data = (source / name).read_bytes()
                fd, name_tmp = tempfile.mkstemp(prefix='.install-', dir=dest.parent)
                with os.fdopen(fd, 'wb') as out:
                    out.write(data)
                os.chmod(name_tmp, 0o755 if name.endswith(('.py', '.sh')) else 0o644)
                os.replace(name_tmp, dest)
            for entry in ENTRYPOINTS:
                dest = self.home / 'bin' / entry
                # Replace the entry atomically; never follow an existing symlink.
                if dest.is_symlink():
                    raise Refused('Existing entrypoint is a symlink: ' + entry)
                text = '#!/bin/sh\nexec python3 -B "$(dirname "$0")/../lib/devhost.py" ' + entry + ' "$@"\n'
                fd, temp = tempfile.mkstemp(prefix='.entry-', dir=dest.parent)
                with os.fdopen(fd, 'w') as out:
                    out.write(text)
                os.chmod(temp, 0o755)
                os.replace(temp, dest)
            dest = self.cache / 'gradle/init.d/devhost-cache.gradle'
            self.safe(dest, [self.cache / 'gradle/init.d'])
            shutil.copyfile(source / 'cache-settings.init.gradle', dest)
            env_path = self.home / 'env'
            self.safe(env_path, [self.home])
            env = self.environment()
            exported = ('ANDROID_HOME', 'ANDROID_SDK_ROOT', 'NDK_HOME', 'GRADLE_USER_HOME',
                        'CARGO_HOME', 'RUSTUP_HOME', 'PIP_CACHE_DIR', 'UV_CACHE_DIR', 'GOCACHE',
                        'npm_config_cache', 'npm_config_store_dir', 'VSCODE_CLI_DATA_DIR',
                        'RUSTUP_TOOLCHAIN', 'CARGO_BUILD_JOBS')
            contents = '# Managed devhost paths; no credentials. Run builds via devhost-run.\n'
            contents += ''.join('export '+key+'='+shlex.quote(env[key])+'\n' for key in exported)
            contents += 'export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64\n'
            contents += ('export PATH="/usr/lib/jvm/java-21-openjdk-amd64/bin:/workspace/devhost/bin:'
                         '/workspace/cache/cargo-home/bin:/workspace/devhost/node/bin:'
                         '/workspace/devhost/npm/bin:$PATH"\n')
            fd, temp = tempfile.mkstemp(prefix='.env-', dir=env_path.parent)
            with os.fdopen(fd, 'w') as out:
                out.write(contents)
            os.replace(temp, env_path)
            print('Installed devhost entrypoints. No services, credentials or repository files modified.')

    def ensure_android_locked(self):
        self.preflight_locked()
        return subprocess.run(['bash', str(self.home / 'lib/android-setup.sh')],
                              env=self.environment()).returncode

    def _cli_https_url(self, url, label):
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != 'https' or not parsed.hostname:
            raise Refused(label + ' must use HTTPS with an explicit host.')
        if parsed.hostname not in VSCODE_CLI_HOSTS:
            raise Refused(label + ' host is not on the VS Code CLI allowlist.')
        return parsed

    def _cli_open(self, url, label):
        self._cli_https_url(url, label)

        class GuardedRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(inner_self, req, fp, code, msg, headers, newurl):
                parsed = urllib.parse.urlparse(newurl)
                if parsed.scheme != 'https' or parsed.hostname not in VSCODE_CLI_HOSTS:
                    raise Refused('VS Code CLI download redirected to an unexpected host.')
                return super().redirect_request(req, fp, code, msg, headers, newurl)

        opener = urllib.request.build_opener(GuardedRedirect())
        request = urllib.request.Request(url, method='GET', headers={'User-Agent': 'devhost-vscode-cli'})
        try:
            return opener.open(request, timeout=120)
        except Refused:
            raise
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise Refused('VS Code CLI download failed: network or TLS error.', 69) from exc

    def ensure_vscode_cli(self, fetcher=None):
        """Install the official Linux x64 CLI when missing; preserve auth data."""
        cli_dir = self.home / 'vscode-cli'
        binary = cli_dir / 'code'
        link = self.home / 'bin/code'
        data_dir = cli_dir / 'data'
        self.safe(cli_dir, [self.home])
        cli_dir.mkdir(parents=True, exist_ok=True)
        # Preserve any existing auth directory contents; never delete or rewrite it.
        data_before = sorted(p.name for p in data_dir.iterdir()) if data_dir.is_dir() else []
        data_dir.mkdir(parents=True, exist_ok=True)

        def link_ready():
            return (binary.is_file() and not binary.is_symlink() and
                    link.is_symlink() and link.resolve() == binary.resolve())

        if link_ready():
            return {'path': str(binary.relative_to(self.root)), 'downloaded': False}

        fetch = fetcher or self._cli_open
        stage = Path(tempfile.mkdtemp(prefix='.vscode-cli-stage-', dir=cli_dir))
        try:
            self.safe(stage, [cli_dir])
            with fetch(VSCODE_CLI_API, 'VS Code CLI metadata') as meta_resp:
                final_meta = meta_resp.geturl()
                self._cli_https_url(final_meta, 'VS Code CLI metadata')
                metadata = json.loads(meta_resp.read().decode())
            expected = metadata.get('sha256hash')
            if not isinstance(expected, str) or not re.fullmatch(r'[0-9a-f]{64}', expected):
                raise Refused('VS Code CLI metadata lacks a SHA-256 digest.', 69)
            source = metadata.get('url') or VSCODE_CLI_URL
            if not isinstance(source, str):
                source = VSCODE_CLI_URL
            self._cli_https_url(source, 'VS Code CLI archive')
            archive = stage / 'vscode_cli_linux_x64_cli.tar.gz'
            digest = hashlib.sha256()
            total = 0
            with fetch(source, 'VS Code CLI archive') as resp:
                final_url = resp.geturl()
                self._cli_https_url(final_url, 'VS Code CLI archive')
                length = resp.headers.get('Content-Length')
                if length is not None:
                    try:
                        if int(length) > VSCODE_CLI_MAX_ARCHIVE:
                            raise Refused('VS Code CLI archive exceeds size limit.', 69)
                    except ValueError as exc:
                        raise Refused('VS Code CLI archive size is invalid.', 69) from exc
                with archive.open('wb') as out:
                    while True:
                        chunk = resp.read(1024 * 1024)
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > VSCODE_CLI_MAX_ARCHIVE:
                            raise Refused('VS Code CLI archive exceeds size limit.', 69)
                        digest.update(chunk)
                        out.write(chunk)
            if digest.hexdigest() != expected:
                raise Refused('VS Code CLI archive SHA-256 mismatch.', 69)
            unpacked = stage / 'unpacked'
            unpacked.mkdir()
            with tarfile.open(archive, 'r:gz') as tar:
                members = [m for m in tar.getmembers() if m.name not in ('', '.')]
                if len(members) != 1 or members[0].name != 'code' or not members[0].isfile():
                    raise Refused('VS Code CLI archive must contain a single code member.', 69)
                if members[0].size > VSCODE_CLI_MAX_ARCHIVE:
                    raise Refused('VS Code CLI binary exceeds size limit.', 69)
                extract_kwargs = {'path': unpacked, 'set_attrs': False}
                if hasattr(tarfile, 'data_filter'):
                    extract_kwargs['filter'] = 'data'
                tar.extract(members[0], **extract_kwargs)
            extracted = unpacked / 'code'
            if not extracted.is_file() or extracted.is_symlink():
                raise Refused('VS Code CLI extraction did not produce a regular code file.', 69)
            os.chmod(extracted, 0o755)
            # Atomic replace of the binary only; auth data directory stays put.
            fd, tmp_name = tempfile.mkstemp(prefix='.code-new-', dir=cli_dir)
            os.close(fd)
            tmp_path = Path(tmp_name)
            try:
                shutil.copyfile(extracted, tmp_path)
                os.chmod(tmp_path, 0o755)
                os.replace(tmp_path, binary)
            finally:
                if tmp_path.exists():
                    tmp_path.unlink()
            self.safe(link.parent, [self.home])
            link.parent.mkdir(parents=True, exist_ok=True)
            if link.exists() or link.is_symlink():
                if not (link.is_symlink() and link.resolve() == binary.resolve()):
                    if link.is_symlink() or link.is_file():
                        link.unlink()
                    else:
                        raise Refused('bin/code exists and is not a replaceable link.', 69)
            link_tmp = link.parent / ('.code-link-' + str(os.getpid()))
            if link_tmp.exists() or link_tmp.is_symlink():
                link_tmp.unlink()
            os.symlink(binary, link_tmp)
            os.replace(link_tmp, link)
            data_after = sorted(p.name for p in data_dir.iterdir())
            if data_before != data_after and set(data_before) - set(data_after):
                raise Refused('VS Code CLI auth data directory was altered during install.', 69)
            return {'path': str(binary.relative_to(self.root)), 'downloaded': True,
                    'sha256': expected, 'bytes': total}
        finally:
            if stage.exists() and not stage.is_symlink():
                shutil.rmtree(stage)

    def tunnels(self):
        code = self.home / 'bin/code'
        result = []
        if not Path('/proc').exists():
            return result
        for p in Path('/proc').iterdir():
            if not p.name.isdigit():
                continue
            try:
                exe = (p / 'exe').resolve()
                args = (p / 'cmdline').read_bytes().split(b'\0')
                if exe == code.resolve() and b'tunnel' in args and b'grok-box' in args:
                    result.append(int(p.name))
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                continue
        return result

    def up(self):
        # A license handoff must not prevent accessing the existing tunnel.
        with self.lock('startup'):
            with self.lock():
                android_code = self.ensure_android_locked()
                self.ensure_vscode_cli()
            code = self.home / 'bin/code'
            if not code.is_file():
                raise Refused('VS Code CLI missing after guarded install attempt.', 69)
            running = self.tunnels()
            if len(running) > 1:
                raise Refused('Multiple tunnel processes detected; manual inspection required.')
            if not running:
                log = self.home / 'logs/tunnel.log'
                self.safe(log, [self.home / 'logs'])
                env = self.environment()
                env['VSCODE_CLI_DATA_DIR'] = str(self.home / 'vscode-cli/data')
                with log.open('ab') as output:
                    subprocess.Popen([str(code), 'tunnel', '--name', 'grok-box',
                                      '--accept-server-license-terms', '--no-sleep'],
                                     env=env, stdin=subprocess.DEVNULL, stdout=output,
                                     stderr=output, start_new_session=True)
            print(json.dumps({'tunnel_pids': self.tunnels(), 'android_exit_code': android_code}))
            return android_code

    def status(self):
        # Read-only: no locks, mkdir, installers, cleanup or cache-generating tools.
        if not self.root.exists():
            return {'workspace': str(self.root), 'available': False, 'reason': 'workspace missing',
                    'protected_repositories': ['NexusHub']}
        return {'workspace': str(self.root), **self.space(), 'cache_bytes': self.cache_bytes(),
                'tunnel_pids': self.tunnels(),
                'android_receipt_present': (self.state / 'android-install.json').exists(),
                'protected_repositories': ['NexusHub']}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    clean = commands.add_parser('devhost-clean')
    clean.add_argument('--mode', choices=('preflight', 'postbuild', 'emergency'), default='preflight')
    behavior = clean.add_mutually_exclusive_group()
    behavior.add_argument('--apply', action='store_true')
    behavior.add_argument('--dry-run', action='store_true')
    run = commands.add_parser('devhost-run')
    run.add_argument('--repo', choices=REPOS, required=True)
    run.add_argument('--cwd', default='.')
    run.add_argument('command', nargs=argparse.REMAINDER)
    commands.add_parser('devhost-status')
    commands.add_parser('devhost-up')
    commands.add_parser('android-sdk-ensure')
    commands.add_parser('install')
    prune = commands.add_parser('devhost-sdk-prune')
    prune.add_argument('--package', required=True)
    prune.add_argument('--apply', action='store_true')
    args = parser.parse_args(argv)
    host = Host()
    try:
        if args.action == 'install':
            host.install(Path(__file__).resolve().parent)
        elif args.action == 'devhost-status':
            print(json.dumps(host.status(), sort_keys=True))
        elif args.action == 'devhost-clean':
            print(json.dumps(host.clean(args.mode, args.apply), sort_keys=True))
        elif args.action == 'devhost-run':
            command = args.command[1:] if args.command[:1] == ['--'] else args.command
            return host.run(args.repo, command, args.cwd)
        elif args.action == 'android-sdk-ensure':
            with host.lock():
                return host.ensure_android_locked()
        elif args.action == 'devhost-up':
            return host.up()
        elif args.action == 'devhost-sdk-prune':
            package = args.package
            protected = (Path(__file__).parent / 'android-packages.txt').read_text().splitlines()
            if package in protected or not re.fullmatch(r'(platforms;android-\d+|build-tools;[\d.]+|ndk;[\d.]+)', package):
                raise Refused('Current or unknown SDK package is protected.', 64)
            if not args.apply:
                print('Would uninstall ' + package + '; rerun with --apply for explicit removal.')
                return 0
            with host.lock():
                if host.process_probe():
                    raise Refused('SDK prune refused while build processes are active.')
                sdk = host.home / 'toolchains/android-sdk'
                return subprocess.run([str(sdk / 'cmdline-tools/latest/bin/sdkmanager'),
                                       '--sdk_root=' + str(sdk), '--uninstall', package],
                                      env=host.environment(), stdin=subprocess.DEVNULL).returncode
        return 0
    except Refused as exc:
        print(str(exc), file=sys.stderr)
        return exc.code
    except OSError as exc:
        label = errno.errorcode.get(exc.errno, 'IO_ERROR')
        print('Filesystem/tool error: ' + label + '. No automatic retry.', file=sys.stderr)
        return 74


if __name__ == '__main__':
    sys.exit(main())
