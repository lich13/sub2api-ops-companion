#!/usr/bin/env python3
"""Private two-generation transport backups. Never prints identities or key data."""
import argparse
import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import pwd
import re
import stat
import sys
import tarfile
import tempfile
import time
import uuid

MAX_FILE = 4 * 1024 * 1024
MAX_TOTAL = 16 * 1024 * 1024
NAMES = frozenset(('transport.json', 'tailscaled.state', 'ssh_host_ed25519_key',
                   'authorized_keys', 'toolchains.lock.json'))
REQUIRED = NAMES - {'toolchains.lock.json'}


def checked(path):
    path = Path(path).absolute()
    if '..' in path.parts or any(p.is_symlink() for p in (path, *path.parents)):
        raise RuntimeError('Symlink or parent traversal refused')
    return path


def read_file(path):
    path = checked(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_FILE:
            raise RuntimeError('Unexpected backup file type or size')
        data = handle.read(MAX_FILE + 1)
    if len(data) > MAX_FILE:
        raise RuntimeError('Backup file is too large')
    return data


def atomic(path, data, mode=0o600, owner=None):
    path = checked(path)
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.new-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
            os.fchmod(handle.fileno(), mode)
            if owner is not None:
                os.fchown(handle.fileno(), *owner)
        os.replace(name, path)
        directory = os.open(path.parent, os.O_DIRECTORY)
        try: os.fsync(directory)
        finally: os.close(directory)
    finally:
        if os.path.exists(name): os.unlink(name)


def encode(value):
    return (json.dumps(value, sort_keys=True) + '\n').encode()


def validate(manifest, files):
    if not isinstance(manifest, dict) or manifest.get('schema') != 1 or not isinstance(manifest.get('files'), dict):
        raise RuntimeError('Invalid backup manifest')
    expected = manifest['files']
    if not REQUIRED <= set(expected) <= NAMES or set(files) != set(expected):
        raise RuntimeError('Backup allowlist mismatch')
    if sum(len(data) for data in files.values()) > MAX_TOTAL:
        raise RuntimeError('Backup size limit exceeded')
    for name, data in files.items():
        item = expected[name]
        if not isinstance(item, dict):
            raise RuntimeError('Invalid backup file metadata')
        if len(data) != item.get('size') or hashlib.sha256(data).hexdigest() != item.get('sha256'):
            raise RuntimeError('Backup checksum mismatch')
    config = json.loads(files['transport.json'])
    if not isinstance(config, dict) or not config.get('persistent_identity'):
        raise RuntimeError('Backup is not a persistent transport identity')
    if not isinstance(json.loads(files['tailscaled.state']), dict):
        raise RuntimeError('Backup state is invalid')
    if not files['authorized_keys'].strip() or not files['ssh_host_ed25519_key'].strip():
        raise RuntimeError('Backup identity is incomplete')


def read_archive(stream):
    files = {}
    total = 0
    with tarfile.open(fileobj=stream, mode='r:gz') as archive:
        for member in archive:
            if member.name not in NAMES | {'manifest.json'} or member.name in files:
                raise RuntimeError('Archive contains an unexpected or duplicate member')
            if not member.isfile() or member.size > MAX_FILE or member.size < 0:
                raise RuntimeError('Archive member type or size refused')
            total += member.size
            if total > MAX_TOTAL:
                raise RuntimeError('Archive size limit exceeded')
            files[member.name] = archive.extractfile(member).read(MAX_FILE + 1)
    if 'manifest.json' not in files:
        raise RuntimeError('Archive manifest is missing')
    manifest = json.loads(files.pop('manifest.json'))
    validate(manifest, files)
    return manifest, files


class Store:
    def __init__(self, user, home=None, system=None):
        self.user = user
        account = pwd.getpwnam(user)
        if account.pw_uid == 0: raise RuntimeError('A regular development user is required')
        self.owner = (account.pw_uid, account.pw_gid)
        self.home = checked(home or account.pw_dir)
        self.identity = self.home / '.local/state/devhost/transport'
        self.root = self.home / '.local/state/devhost/transport-backups'
        self.system = Path(system or '/etc/devhost')
        self.sources = {name: self.identity / name for name in REQUIRED - {'authorized_keys'}}
        self.sources['authorized_keys'] = self.home / '.ssh/authorized_keys'
        self.sources['toolchains.lock.json'] = self.system / 'toolchains.lock.json'

    @contextlib.contextmanager
    def locked(self):
        checked(self.root)
        self.root.mkdir(parents=True, mode=0o700, exist_ok=True)
        self.root.chmod(0o700)
        fd = os.open(self.root / 'backup.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield
        finally: os.close(fd)

    def generation(self, name):
        if not isinstance(name, str) or not re.fullmatch(r'g-[0-9a-f]{32}', name):
            raise RuntimeError('Invalid backup generation')
        return checked(self.root / name)

    def load(self, previous=False):
        index = json.loads(read_file(self.root / 'index.json'))
        name = index.get('previous' if previous else 'current')
        directory = self.generation(name)
        manifest = json.loads(read_file(directory / 'manifest.json'))
        if not isinstance(manifest.get('files'), dict) or not set(manifest['files']) <= NAMES:
            raise RuntimeError('Invalid generation allowlist')
        files = {name: read_file(directory / name) for name in manifest['files']}
        validate(manifest, files)
        return manifest, files

    def publish(self, manifest, files, repair=False):
        validate(manifest, files)
        try:
            old = json.loads(read_file(self.root / 'index.json')) if (self.root / 'index.json').exists() else {}
            if not isinstance(old, dict): raise ValueError('Invalid index')
        except (OSError, ValueError):
            if not repair: raise
            old = {}
        repaired = False
        if old:
            try:
                current_manifest, _ = self.load()
            except (OSError, ValueError, RuntimeError, KeyError, TypeError):
                if not repair: raise
                repaired = True
                try:
                    current_manifest, _ = self.load(previous=True)
                    old = {'current': old['previous'], 'previous': None}
                except (OSError, ValueError, RuntimeError, KeyError, TypeError):
                    old = {}
            if old and current_manifest['files'] == manifest['files']:
                if repaired: atomic(self.root / 'index.json', encode(old))
                return {'changed': False, 'created_at': current_manifest['created_at']}
        name = 'g-' + uuid.uuid4().hex
        directory = self.generation(name)
        directory.mkdir(mode=0o700)
        for filename, data in files.items(): atomic(directory / filename, data)
        atomic(directory / 'manifest.json', encode(manifest))
        atomic(self.root / 'index.json', encode({'current': name, 'previous': old.get('current')}))
        # Delete only the previous generation previously named by our index.
        expired = old.get('previous')
        if expired and expired not in (name, old.get('current')):
            target = self.generation(expired)
            contents = list(target.iterdir())
            if any(p.name not in NAMES | {'manifest.json'} or not p.is_file() or p.is_symlink()
                   for p in contents):
                raise RuntimeError('Unexpected old backup contents; retention stopped')
            for item in contents: item.unlink()
            target.rmdir()
        return {'changed': True, 'created_at': manifest['created_at']}

    def snapshot(self):
        files = {}
        for name, path in self.sources.items():
            checked(path)
            if name not in REQUIRED and not path.exists(): continue
            files[name] = read_file(path)
        config = json.loads(files['transport.json'])
        if config.get('user') != self.user:
            raise RuntimeError('Backup development user differs')
        manifest = {'schema': 1, 'created_at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
                    'files': {name: {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
                              for name, data in files.items()}}
        return manifest, files

    def backup(self):
        with self.locked():
            return self.publish(*self.snapshot())

    def restore(self, previous=False, archive=None, if_available=False):
        if if_available and not (self.root / 'index.json').exists() and archive is None:
            return {'available': False, 'restored': 0}
        with self.locked():
            if archive:
                with checked(archive).open('rb') as stream: manifest, files = read_archive(stream)
            else:
                manifest, files = self.load(previous)
            if json.loads(files['transport.json']).get('user') != self.user:
                raise RuntimeError('Restore development user differs')
            destinations = dict(self.sources)
            destinations['system-config'] = self.system / 'transport.json'
            files = {**files, 'system-config': files['transport.json']}
            missing = []
            # Validate every destination before the first write. Live files are never rolled back.
            for name, data in files.items():
                target = checked(destinations[name])
                if target.exists():
                    if not target.is_file(): raise RuntimeError('Restore destination is not a file')
                    continue
                missing.append((name, target, data))
            for name, target, data in missing:
                owner = self.owner if name == 'authorized_keys' else None
                atomic(target, data, owner=owner)
                if name == 'authorized_keys':
                    target.parent.chmod(0o700)
                    os.chown(target.parent, *self.owner)
            if self.identity.exists(): self.identity.chmod(0o700)
            if previous or archive:
                # Explicit recovery may repair a damaged current index; preserve
                # still-existing live state, and never silently do this in backup().
                self.publish(*self.snapshot(), repair=True)
            return {'available': True, 'restored': len(missing)}

    def export(self, output):
        with self.locked():
            manifest, files = self.load()
            with tarfile.open(fileobj=output, mode='w|gz') as archive:
                for name, data in {'manifest.json': encode(manifest), **files}.items():
                    info = tarfile.TarInfo(name)
                    info.size = len(data); info.mode = 0o600
                    archive.addfile(info, io.BytesIO(data))

    def status(self):
        if not (self.root / 'index.json').exists(): return {'available': False}
        manifest, files = self.load()
        index = json.loads(read_file(self.root / 'index.json'))
        return {'available': True, 'created_at': manifest['created_at'],
                'files': len(files), 'bytes': sum(map(len, files.values())),
                'previous_available': bool(index.get('previous'))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('backup', 'restore', 'export', 'status'))
    parser.add_argument('--user', required=True)
    parser.add_argument('--previous', action='store_true')
    parser.add_argument('--archive')
    parser.add_argument('--if-available', action='store_true')
    args = parser.parse_args()
    if os.geteuid() != 0: raise RuntimeError('Private transport backup requires sudo')
    store = Store(args.user)
    if args.action == 'backup': result = store.backup()
    elif args.action == 'restore':
        result = store.restore(args.previous, args.archive, args.if_available)
    elif args.action == 'status': result = store.status()
    else:
        store.export(sys.stdout.buffer)
        return
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    try: main()
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, tarfile.TarError):
        # Do not print parsed data, addresses, credentials, or arbitrary paths.
        print('Transport backup operation refused; inspect private state.', file=sys.stderr)
        raise SystemExit(74)
