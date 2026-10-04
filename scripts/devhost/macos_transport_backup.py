#!/usr/bin/env python3
"""Small private off-host transport backups; source/build caches are excluded."""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shlex
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

from transport_backup import atomic, checked, encode, read_archive

BASE = Path.home() / 'Library/Application Support/grok-cloud-recovery'
LABEL = 'com.devhost.grok-cloud-backup'
SSH = ['/usr/bin/ssh', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
       '-o', 'ConnectTimeout=12', '-o', 'ConnectionAttempts=1', 'grok-cloud']
RECOVERY = '/workspace/devhost/bin/devhost-restore --full'


def private_directory(path):
    checked(path)
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.chmod(0o700)


def archive_path(name):
    if not isinstance(name, str) or len(name) != 41 or not name.startswith('g-') or not name.endswith('.tar.gz'):
        raise RuntimeError('Invalid local generation')
    uuid.UUID(hex=name[2:-7])
    return checked(BASE / 'backups' / name)


@contextlib.contextmanager
def locked():
    private_directory(BASE)
    fd = os.open(BASE / 'backup.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally: os.close(fd)


def result(ok, category):
    atomic(BASE / 'last-result.json', encode({'success': ok, 'category': category,
           'at': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}))


def snapshot():
    with locked():
        directory = BASE / 'backups'
        private_directory(directory)
        fd, temporary = tempfile.mkstemp(prefix='.download-', dir=directory)
        try:
            with os.fdopen(fd, 'wb') as output:
                process = subprocess.run(SSH + ['/workspace/devhost/bin/devhost-backup --export'],
                    stdout=output, stderr=subprocess.DEVNULL, timeout=90)
                output.flush(); os.fsync(output.fileno())
            if process.returncode:
                result(False, 'ssh-or-remote-backup-unavailable')
                return 1
            with open(temporary, 'rb') as stream: manifest, files = read_archive(stream)
            index_file = BASE / 'index.json'
            old = json.loads(index_file.read_text()) if index_file.exists() else {}
            if old:
                old_archive = archive_path(old['current'])
                with old_archive.open('rb') as stream: previous, _ = read_archive(stream)
                if previous['files'] == manifest['files']:
                    result(True, 'verified-unchanged')
                    return 0
            name = 'g-' + uuid.uuid4().hex + '.tar.gz'
            os.replace(temporary, archive_path(name))
            digest = hashlib.sha256(archive_path(name).read_bytes()).hexdigest()
            atomic(index_file, encode({'current': name, 'previous': old.get('current'),
                                      'sha256': digest}))
            if old.get('previous'):
                stale = archive_path(old['previous'])
                if not stale.is_file(): raise RuntimeError('Unexpected old local backup type')
                stale.unlink()
            result(True, 'verified-new-generation')
            return 0
        finally:
            if os.path.exists(temporary): os.unlink(temporary)


def status():
    index = BASE / 'index.json'
    if not index.exists(): return {'available': False}
    data = json.loads(index.read_text())
    archive = archive_path(data['current'])
    payload = archive.read_bytes()
    if hashlib.sha256(payload).hexdigest() != data['sha256']:
        raise RuntimeError('Local archive checksum mismatch')
    with archive.open('rb') as stream: manifest, files = read_archive(stream)
    last = json.loads((BASE / 'last-result.json').read_text())
    return {'available': True, 'bytes': len(payload), 'files': len(files),
            'created_at': manifest['created_at'], 'previous_available': bool(data.get('previous')),
            'last_attempt': last}


def restore(from_local=False):
    ready = subprocess.run(SSH + ['true'], stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
    if ready.returncode:
        subprocess.run(['/usr/bin/pbcopy'], input=RECOVERY.encode(), check=True)
        print('SSH 暂不可达。恢复命令已复制。打开 Grok 云电脑终端，粘贴并执行即可恢复。')
        print('命令：' + RECOVERY)
        print('云电脑已停止时须先唤醒；用户目录也丢失时需使用本机备份导入。')
        return 2
    if not from_local:
        return subprocess.run(SSH + [RECOVERY]).returncode
    with locked():
        status()
        data = json.loads((BASE / 'index.json').read_text())
        command = """umask 077
stage=$(mktemp "$HOME/.devhost-import.XXXXXXXX")
trap 'test ! -f "$stage" || unlink "$stage"' EXIT
cat > "$stage"
/workspace/devhost/bin/devhost-restore --full --archive "$stage"
"""
        with archive_path(data['current']).open('rb') as stream:
            return subprocess.run(SSH + ['sh -c ' + shlex.quote(command)], stdin=stream).returncode


def install():
    if sys.platform != 'darwin': raise RuntimeError('Install this client on macOS')
    private_directory(BASE)
    source = Path(__file__).resolve().parent
    for filename in ('macos_transport_backup.py', 'transport_backup.py'):
        atomic(BASE / filename, (source / filename).read_bytes(), mode=0o700)
    agents = checked(Path.home() / 'Library/LaunchAgents')
    agents.mkdir(parents=True, exist_ok=True)
    plist = agents / (LABEL + '.plist')
    program = ['/usr/bin/python3', '-B', str(BASE / 'macos_transport_backup.py'), 'snapshot', '--quiet']
    if plist.exists():
        prior = plistlib.loads(plist.read_bytes())
        if prior.get('ProgramArguments') != program:
            raise RuntimeError('Existing launch agent has a different owner')
        subprocess.run(['/bin/launchctl', 'bootout', 'gui/' + str(os.getuid()), str(plist)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    atomic(plist, plistlib.dumps({'Label': LABEL, 'ProgramArguments': program,
        'StartInterval': 21600, 'RunAtLoad': True, 'ProcessType': 'Background'}))
    entry = Path.home() / 'Downloads/恢复grok-cloud.command'
    script = '#!/bin/zsh\n/usr/bin/python3 -B ' + shlex.quote(str(BASE / 'macos_transport_backup.py')) + ' restore\n'
    prompt = 'printf "\\n按回车关闭窗口。\\n"\nread -r reply\n'
    previous_script = script + prompt
    script += 'recovery_status=$?\n' + prompt + 'exit "$recovery_status"\n'
    if entry.exists() and entry.read_text() not in (script, previous_script):
        raise RuntimeError('Recovery entry already has different contents')
    atomic(entry, script.encode(), mode=0o700)
    subprocess.run(['/bin/launchctl', 'bootstrap', 'gui/' + str(os.getuid()), str(plist)], check=True)
    print('自动备份已安装：登录后和每 6 小时执行；保留最近两代，离线不覆盖旧备份。')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('install', 'snapshot', 'status', 'restore'))
    parser.add_argument('--quiet', action='store_true')
    parser.add_argument('--from-local', action='store_true')
    args = parser.parse_args()
    try:
        if args.action == 'install': install(); return 0
        if args.action == 'snapshot':
            code = snapshot()
            if not args.quiet: print('备份已验证。' if code == 0 else '未连接云端，已有备份保持不变。')
            return code
        if args.action == 'status': print(json.dumps(status(), sort_keys=True)); return 0
        return restore(args.from_local)
    except (OSError, ValueError, KeyError, RuntimeError, tarfile.TarError, subprocess.SubprocessError):
        if args.action == 'snapshot':
            try: result(False, 'backup-failed')
            except OSError: pass
        if not args.quiet: print('备份或恢复未完成；原文件和已有备份保持保留。', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
