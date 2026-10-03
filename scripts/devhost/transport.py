#!/usr/bin/env python3
"""Private OpenSSH transport; identity and privileged runtime stay outside source."""
import argparse
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import signal
import subprocess
import tempfile
import tarfile
import time
import urllib.parse
import urllib.request

CONFIG = Path('/etc/devhost/transport.json')
RUN = Path('/run/devhost')
STATE = Path('/var/lib/devhost/tailscaled.state')
SOCKET = Path('/run/devhost/tailscaled.sock')
SELF = '/usr/local/libexec/devhost-transport.py'


def run(args, **kwargs):
    return subprocess.run(args, stdin=subprocess.DEVNULL, capture_output=True, text=True, **kwargs)


def write(path, text, mode=0o600):
    if any(p.is_symlink() for p in (path, *path.parents)):
        raise RuntimeError('Refusing symlink in transport path')
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix='.devhost-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as f:
            f.write(text); f.flush(); os.fsync(f.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp): os.unlink(tmp)


def ts(*args):
    return run(['/usr/bin/tailscale', '--socket=' + str(SOCKET), *args], timeout=35)


def network():
    try:
        result = ts('status', '--json')
        data = json.loads(result.stdout) if result.returncode == 0 else {}
        address = next((x for x in data.get('TailscaleIPs', [])
                        if ipaddress.ip_address(x).version == 4), None)
        return (data.get('BackendState') == 'Running'
                and data.get('Self', {}).get('Online') is True), address
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return False, None


class RecoveryGate:
    """Recover a stuck authenticated client without a restart storm."""
    def __init__(self):
        self.failures = 0
        self.next_attempt = 0

    def should_recover(self, online, authenticated, now):
        if online or not authenticated:
            self.failures = 0
            return False
        self.failures += 1
        return self.failures >= 4 and now >= self.next_attempt

    def attempted(self, now):
        self.failures = 0
        self.next_attempt = now + 600


def health_event(category):
    # Deliberately record no addresses, daemon output, keys or authentication URLs.
    path = RUN / 'transport-events.log'
    previous = path.read_text() if path.exists() else ''
    entries = previous.splitlines()[-99:]
    entries.append(time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()) + ' ' + category)
    write(path, '\n'.join(entries) + '\n')


def watch_health():
    gate = RecoveryGate()
    time.sleep(120)
    while True:
        try:
            online, _ = network()
            prefs = ts('debug', 'prefs')
            data = json.loads(prefs.stdout) if prefs.returncode == 0 else {}
            authenticated = data.get('WantRunning') is True and data.get('LoggedOut') is False
            now = time.monotonic()
            if gate.should_recover(online, authenticated, now):
                gate.attempted(now)
                config = json.loads(CONFIG.read_text())
                request = urllib.request.Request(config['control_url'].rstrip('/') + '/health')
                with urllib.request.urlopen(request, timeout=15) as response:
                    reachable = response.status == 200
                daemons = [(pid, argv) for pid, argv in processes('tailscaled')
                           if '--state=' + str(STATE) in argv]
                if reachable and len(daemons) == 1:
                    health_event('authenticated-client-offline-restart')
                    stop(daemons[0][0])  # Its existing manager starts the same identity again.
                else:
                    health_event('recovery-deferred')
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
            # Connectivity and authentication failures must never cause rapid restarts.
            gate.attempted(time.monotonic())
            health_event('health-probe-unavailable')
        time.sleep(30)


def processes(name):
    result = []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit(): continue
        try:
            if (proc / 'exe').resolve().name == name:
                argv = [x.decode() for x in (proc / 'cmdline').read_bytes().split(b'\0') if x]
                result.append((int(proc.name), argv))
        except (OSError, UnicodeError): pass
    return result


def status():
    running, address = network()
    result = run(['ss', '-H', '-ltnp']) if address else None
    listening = bool(result and any(address + ':22' in row and 'sshd' in row
                                    for row in result.stdout.splitlines()))
    return {'tailscale_running': running, 'ssh_listening': listening,
            'ssh_ready': running and listening, 'tailscaled_count': len(processes('tailscaled'))}


def stop(pid):
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 10
    while Path('/proc', str(pid)).exists() and time.monotonic() < deadline:
        try:
            if ') Z ' in Path('/proc', str(pid), 'stat').read_text(): return
        except FileNotFoundError:
            return
        time.sleep(.1)
    if Path('/proc', str(pid)).exists():
        raise RuntimeError('Existing transport did not stop; inspect before retry')


def migrate_daemon():
    daemons = processes('tailscaled')
    if len(daemons) > 1: raise RuntimeError('Multiple Tailscale daemons; inspect before migration')
    for pid, args in daemons:
        current = next((a.split('=', 1)[1] for a in args if a.startswith('--state=')), None)
        if current == str(STATE): return
        if not current: raise RuntimeError('Cannot identify current Tailscale state')
        source = Path(current)
        if source.is_symlink() or not source.is_file() or STATE.exists():
            raise RuntimeError('Conflicting or unsafe Tailscale state; nothing replaced')
        stop(pid)
        STATE.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        shutil.move(str(source), STATE)
        STATE.chmod(0o600)


def ssh_config(user, address):
    if not re.fullmatch(r'[a-z_][a-z0-9_-]*', user) or user == 'root':
        raise RuntimeError('A regular development user is required')
    ipaddress.ip_address(address)
    return f'''Port 22
ListenAddress {address}
HostKey /etc/ssh/ssh_host_ed25519_key
AuthorizedKeysFile .ssh/authorized_keys
PubkeyAuthentication yes
AuthenticationMethods publickey
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
PermitEmptyPasswords no
UsePAM yes
AllowUsers {user}
AllowTcpForwarding local
AllowStreamLocalForwarding local
GatewayPorts no
AllowAgentForwarding no
X11Forwarding no
UseDNS no
Subsystem sftp internal-sftp
PidFile /run/devhost/sshd.pid
LogLevel ERROR
'''


def serve_ssh():
    config = json.loads(CONFIG.read_text())
    while True:
        running, address = network()
        if running and address: break
        time.sleep(2)
    Path('/run/sshd').mkdir(mode=0o755, exist_ok=True)
    path = RUN / 'sshd.conf'
    write(path, ssh_config(config['user'], address))
    if run(['/usr/sbin/sshd', '-t', '-f', str(path)]).returncode:
        raise RuntimeError('SSH configuration failed validation')
    os.execv('/usr/sbin/sshd', ['/usr/sbin/sshd', '-D', '-e', '-f', str(path)])


def uses_systemd():
    return Path('/run/systemd/system').is_dir() and run(['systemctl', 'is-system-running']).returncode in (0, 1)


def configure_manager():
    tail = '/usr/sbin/tailscaled --state=' + str(STATE) + ' --socket=' + str(SOCKET) + ' --tun=tailscale0 --port=0'
    ssh = '/usr/bin/python3 -B ' + SELF + ' serve-sshd'
    health = '/usr/bin/python3 -B ' + SELF + ' watch-health'
    if uses_systemd():
        for service in ('ssh.socket', 'ssh.service', 'tailscaled.service'):
            run(['systemctl', 'disable', '--now', service])
        for name, command in (('tailscale', tail), ('sshd', ssh), ('network-health', health)):
            unit = ('[Unit]\nAfter=network-online.target\n[Service]\nType=simple\n'
                    f'ExecStart={command}\nRestart=always\nRestartSec=3\n'
                    'Environment=TS_DEBUG_FIREWALL_MODE=nftables\n'
                    'StandardOutput=null\nStandardError=null\n[Install]\nWantedBy=multi-user.target\n')
            write(Path('/etc/systemd/system/devhost-' + name + '.service'), unit, 0o644)
        run(['systemctl', 'daemon-reload'], check=True)
        run(['systemctl', 'enable', '--now', 'devhost-tailscale', 'devhost-sshd',
             'devhost-network-health'], check=True)
    else:
        config = '''[unix_http_server]
file=/run/devhost/supervisor.sock
chmod=0700
[supervisord]
pidfile=/run/devhost/supervisord.pid
logfile=/dev/null
logfile_maxbytes=0
childlogdir=/run/devhost
[rpcinterface:supervisor]
supervisor.rpcinterface_factory=supervisor.rpcinterface:make_main_rpcinterface
[supervisorctl]
serverurl=unix:///run/devhost/supervisor.sock
'''
        for name, command in (('tailscale', tail), ('sshd', ssh), ('network-health', health)):
            config += f'''[program:{name}]
command={command}
environment=TS_DEBUG_FIREWALL_MODE="nftables"
autostart=true
autorestart=true
startsecs=2
startretries=5
stopasgroup=true
killasgroup=true
stdout_logfile=/dev/null
stdout_logfile_maxbytes=0
stderr_logfile=/dev/null
stderr_logfile_maxbytes=0
'''
        path = Path('/etc/devhost/supervisor.conf')
        write(path, config)
        probe = run(['supervisorctl', '-c', str(path), 'pid'])
        if probe.returncode or not probe.stdout.strip().isdigit():
            run(['supervisord', '-c', str(path)], check=True)
        else:
            run(['supervisorctl', '-c', str(path), 'reread'], check=True)
            run(['supervisorctl', '-c', str(path), 'update'], check=True)
            run(['supervisorctl', '-c', str(path), 'start', 'all'])


def configure(args):
    if os.uname().machine != 'x86_64' or not Path('/dev/net/tun').exists():
        raise RuntimeError('Linux x86_64 and a TUN device are required')
    account = pwd.getpwnam(args.user)
    if account.pw_uid == 0: raise RuntimeError('Root SSH login is not permitted')
    if not Path('/usr/sbin/tailscaled').exists():
        item = json.loads(Path('/etc/devhost/toolchains.lock.json').read_text())['tailscale']
        with tempfile.TemporaryDirectory(prefix='devhost-tailscale-') as temp:
            archive = Path(temp) / 'package.tgz'
            with urllib.request.urlopen(item['url'], timeout=90) as response:
                archive.write_bytes(response.read())
            if hashlib.sha256(archive.read_bytes()).hexdigest() != item['sha256']:
                raise RuntimeError('Tailscale archive SHA-256 mismatch')
            with tarfile.open(archive) as package:
                for name, dest in [('tailscale', '/usr/bin/tailscale'), ('tailscaled', '/usr/sbin/tailscaled')]:
                    member = package.getmember('tailscale_' + item['version'] + '_amd64/' + name)
                    if not member.isfile(): raise RuntimeError('Invalid Tailscale archive member')
                    Path(dest).write_bytes(package.extractfile(member).read())
                    Path(dest).chmod(0o755)
    config = json.loads(CONFIG.read_text()) if CONFIG.exists() else {'user': args.user}
    if config['user'] != args.user: raise RuntimeError('Existing SSH user differs')
    if args.control_url_file:
        url = Path(args.control_url_file).read_text().strip()
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password:
            raise RuntimeError('Control endpoint must use HTTPS without credentials')
        config['control_url'] = url
    elif 'control_url' not in config:
        for socket in (SOCKET, Path('/workspace/devhost/transport/tailscaled.sock'),
                       Path('/workspace/devhost/state/tailscaled.sock')):
            prefs = run(['/usr/bin/tailscale', '--socket=' + str(socket), 'debug', 'prefs'])
            if prefs.returncode == 0:
                config['control_url'] = json.loads(prefs.stdout)['ControlURL']; break
    if 'control_url' not in config: raise RuntimeError('A protected control URL file is required')
    auth = Path(account.pw_dir) / '.ssh/authorized_keys'
    if args.public_key_file:
        key = Path(args.public_key_file).read_text().strip()
        if '\n' in key or not re.fullmatch(r'ssh-ed25519 [A-Za-z0-9+/=]+(?: [^\n]+)?', key):
            raise RuntimeError('An ed25519 public key is required')
        old = auth.read_text() if auth.exists() else ''
        if key not in old.splitlines(): write(auth, old.rstrip() + '\n' + key + '\n')
        auth.parent.chmod(0o700)
        os.chown(auth.parent, account.pw_uid, account.pw_gid)
        os.chown(auth, account.pw_uid, account.pw_gid)
    if not auth.is_file() or auth.is_symlink() or not auth.read_text().strip():
        raise RuntimeError('An authorized public key must be installed first')
    missing = [p for p, binary in [('openssh-server', '/usr/sbin/sshd'),
                                 ('supervisor', '/usr/bin/supervisord')] if not Path(binary).exists()]
    if missing:
        run(['apt-get', 'update'], check=True)
        # Package post-install must not start a wildcard SSH listener.
        policy = Path('/usr/sbin/policy-rc.d')
        prior = (policy.read_text(), policy.stat()) if policy.exists() else None
        write(policy, '#!/bin/sh\nexit 101\n', 0o755)
        try:
            run(['apt-get', 'install', '-y', '--no-install-recommends', *missing], check=True)
        finally:
            if prior:
                write(policy, prior[0], prior[1].st_mode & 0o777)
                os.chown(policy, prior[1].st_uid, prior[1].st_gid)
            else:
                policy.unlink()
    if not Path('/usr/sbin/tailscaled').exists():
        raise RuntimeError('Install the official Tailscale package before enrollment')
    write(CONFIG, json.dumps(config) + '\n')
    migrate_daemon()
    for pid, argv in processes('sshd'):
        if re.search(r'\s-D(?:\s|$)', ' '.join(argv)) and any('/workspace/devhost/' in a for a in argv): stop(pid)
    STATE.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    SOCKET.parent.mkdir(parents=True, exist_ok=True)
    run(['ssh-keygen', '-A'], check=True)
    configure_manager()
    for _ in range(30):
        if SOCKET.exists(): break
        time.sleep(.2)
    prefs = ts('debug', 'prefs')
    previous = json.loads(prefs.stdout).get('ControlURL') if prefs.returncode == 0 else None
    extra = ['--force-reauth'] if previous and previous != config['control_url'] else []
    result = ts('up', '--reset', '--login-server=' + config['control_url'], '--hostname=grok-cloud',
                '--ssh=false', '--accept-dns=false', '--timeout=20s', *extra)
    if result.returncode:
        links = re.findall(r'https://[^\s]+', result.stdout + result.stderr)
        if links: write(RUN / 'enrollment-url', links[-1] + '\n')
        print('Registration required; Headscale administrator uses the protected runtime enrollment file.')
        return 78
    return up()


def up():
    if not CONFIG.exists(): raise RuntimeError('Transport bootstrap has not been completed')
    if uses_systemd():
        run(['systemctl', 'start', 'devhost-tailscale', 'devhost-sshd', 'devhost-network-health'], check=True)
    else:
        path = '/etc/devhost/supervisor.conf'
        probe = run(['supervisorctl', '-c', path, 'pid'])
        if probe.returncode: run(['supervisord', '-c', path], check=True)
        else: run(['supervisorctl', '-c', path, 'start', 'all'])
    for _ in range(30):
        info = status()
        if info['ssh_ready']:
            print(json.dumps(info)); return 0
        time.sleep(.5)
    print(json.dumps(info)); return 69


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('configure', 'up', 'status', 'serve-sshd', 'watch-health'))
    parser.add_argument('--user')
    parser.add_argument('--control-url-file')
    parser.add_argument('--public-key-file')
    args = parser.parse_args()
    if args.action == 'status': print(json.dumps(status())); return 0
    if os.geteuid() != 0: raise RuntimeError('Transport operations require sudo')
    if args.action == 'serve-sshd': serve_ssh()
    if args.action == 'watch-health': watch_health()
    RUN.mkdir(mode=0o700, exist_ok=True)
    with (RUN / 'setup.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return configure(args) if args.action == 'configure' else up()


if __name__ == '__main__':
    try: raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(str(exc) if isinstance(exc, RuntimeError) else 'Transport operation failed; inspect protected service state.')
        raise SystemExit(74)
