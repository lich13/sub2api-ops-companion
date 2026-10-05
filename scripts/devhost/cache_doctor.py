"""Verify Cargo registry sources against checksum-verified cached archives.

Never prune directories by basename: build, dist and target may be crate source.
Repairs are opt-in, locked, atomic file replacements; no network or source reset.
"""
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import tarfile
import tempfile
import functools
import urllib.request


class MissingChecksum(ValueError):
    pass


def digest(stream):
    value = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b''):
        value.update(chunk)
    return value.hexdigest()


def index_key(name):
    name = name.lower()
    if len(name) < 3:
        return Path(str(len(name))) / name
    if len(name) == 3:
        return Path('3') / name[0] / name
    return Path(name[:2]) / name[2:4] / name


def split_crate(stem):
    # Versions may contain hyphens in prerelease/build metadata; choose the
    # earliest split whose suffix is a valid SemVer-shaped version.
    version = re.compile(r'\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$')
    for index, char in enumerate(stem):
        if char != '-':
            continue
        name, candidate = stem[:index], stem[index + 1:]
        if re.fullmatch(r'[A-Za-z0-9_-]+', name) and version.fullmatch(candidate):
            return name, candidate
    raise ValueError('Invalid crate archive name')


def expected_checksum(host, registry, namespace, name, version):
    index = registry / 'index' / namespace
    relative = index_key(name)
    for path in (index / '.cache' / relative, index / relative):
        host.safe(path, [registry])
        if not path.is_file():
            continue
        # Sparse cache records are NUL-delimited; the git index uses JSON lines.
        records = path.read_bytes().replace(b'\0', b'\n').splitlines()
        values = set()
        for raw in records:
            if not raw.startswith(b'{'):
                continue
            item = json.loads(raw)
            if item.get('name') == name and item.get('vers') == version:
                checksum = item.get('cksum', '')
                if re.fullmatch(r'[0-9a-f]{64}', checksum):
                    values.add(checksum)
        if len(values) == 1:
            return values.pop()
        if len(values) > 1:
            raise ValueError('Conflicting registry checksums')
    raise MissingChecksum('No trusted registry checksum')


@functools.lru_cache(maxsize=None)
def official_checksums(name):
    if not re.fullmatch(r'[A-Za-z0-9_-]+', name):
        raise ValueError('Invalid registry package name')
    url = 'https://index.crates.io/' + index_key(name).as_posix()
    with urllib.request.urlopen(url, timeout=30) as response:
        if response.geturl() != url:
            raise ValueError('Unexpected registry redirect')
        raw = response.read(16 * 1024 * 1024 + 1)
    if len(raw) > 16 * 1024 * 1024:
        raise ValueError('Oversized registry record')
    values = {}
    for line in raw.splitlines():
        item = json.loads(line)
        if item.get('name') == name and re.fullmatch(r'[0-9a-f]{64}', item.get('cksum', '')):
            values[item['vers']] = item['cksum']
    return values


def replace_file(path, stream, mode):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.cargo-repair-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                output.write(chunk)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, mode & 0o777)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def audit(host, repair=False, online=False):
    from devhost import Refused
    registry = host.safe(host.cache / 'cargo-home/registry', [host.cache])
    report = {'archives_checked': 0, 'sources_checked': 0, 'files_checked': 0,
              'affected': [], 'errors': [], 'files_repaired': 0, 'repair': repair}
    with host.lock(), contextlib.ExitStack() as locks:
        if host.process_probe():
            raise Refused('Build processes are active; Cargo cache audit deferred.')
        # Cargo mutations share this lock. Never unlink lock files.
        for name in ('.package-cache', '.package-cache-mutate'):
            path = host.safe(registry.parent / name, [host.cache])
            fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
            locks.callback(os.close, fd)
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise Refused('Cargo owns its cache lock; audit deferred.')
        namespaces = sorted((registry / 'cache').iterdir()) if (registry / 'cache').is_dir() else []
        for namespace in namespaces:
            host.safe(namespace, [registry])
            if not namespace.is_dir():
                continue
            for archive in sorted(namespace.glob('*.crate')):
                crate = archive.stem
                try:
                    host.safe(archive, [registry])
                    name, version = split_crate(crate)
                    try:
                        expected = expected_checksum(host, registry, namespace.name, name, version)
                    except MissingChecksum:
                        if not online or namespace.name != 'index.crates.io-1949cf8c6b5b557f':
                            raise
                        expected = official_checksums(name).get(version)
                        if not expected:
                            raise MissingChecksum('Version absent from official registry')
                    with archive.open('rb') as stream:
                        if digest(stream) != expected:
                            raise ValueError('Archive checksum mismatch; repair refused')
                    report['archives_checked'] += 1
                    source = host.safe(registry / 'src' / namespace.name / crate, [registry])
                    if not source.exists():
                        continue  # An unextracted archive is healthy; Cargo will extract it.
                    if not source.is_dir():
                        raise ValueError('Crate source is not a directory')
                    report['sources_checked'] += 1
                    changed = []
                    with tarfile.open(archive, 'r:gz') as bundle:
                        members = []
                        seen = set()
                        for member in bundle.getmembers():
                            rel = PurePosixPath(member.name)
                            if (rel.is_absolute() or '..' in rel.parts or not rel.parts
                                    or rel.parts[0] != crate or member.name in seen
                                    or not (member.isdir() or member.isfile())):
                                raise ValueError('Unsafe or duplicate archive member')
                            seen.add(member.name)
                            if member.isfile():
                                if len(rel.parts) < 2:
                                    raise ValueError('Invalid archive file path')
                                target = host.safe(source.joinpath(*rel.parts[1:]), [source])
                                if target.exists() and not target.is_file():
                                    raise ValueError('Source file type conflict')
                                members.append((member, target))
                        for member, target in members:
                            with bundle.extractfile(member) as stream:
                                expected_file = digest(stream)
                            present = target.is_file()
                            if present:
                                with target.open('rb') as stream:
                                    same = digest(stream) == expected_file
                            else:
                                same = False
                            report['files_checked'] += 1
                            if same:
                                continue
                            changed.append({'file': str(target.relative_to(source)),
                                            'kind': 'modified' if present else 'missing'})
                            if repair:
                                host.safe(target, [source])
                                with bundle.extractfile(member) as stream:
                                    replace_file(target, stream, member.mode)
                                with target.open('rb') as stream:
                                    if digest(stream) != expected_file:
                                        raise ValueError('Repaired file failed verification')
                                report['files_repaired'] += 1
                        if changed:
                            report['affected'].append({'crate': crate, 'files': changed})
                except (OSError, ValueError, tarfile.TarError, Refused) as error:
                    report['errors'].append({'crate': crate, 'error': type(error).__name__ + ': ' + str(error)})
    report['ok'] = not report['errors'] and (repair or not report['affected'])
    return report
