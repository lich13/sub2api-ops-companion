import importlib.util
import json
from types import SimpleNamespace
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('transport', Path(__file__).with_name('transport.py'))
t = importlib.util.module_from_spec(spec)
spec.loader.exec_module(t)


class TransportTests(unittest.TestCase):
    def test_recovery_waits_for_sustained_failure_and_backs_off(self):
        gate = t.RecoveryGate()
        self.assertFalse(gate.should_recover(False, True, 0))
        self.assertFalse(gate.should_recover(False, True, 30))
        self.assertFalse(gate.should_recover(False, True, 60))
        self.assertTrue(gate.should_recover(False, True, 90))
        gate.attempted(90)
        for now in range(120, 690, 30):
            self.assertFalse(gate.should_recover(False, True, now))
        self.assertTrue(gate.should_recover(False, True, 690))

    def test_recovery_never_restarts_logged_out_or_healthy_clients(self):
        gate = t.RecoveryGate()
        for now in range(0, 1000, 30):
            self.assertFalse(gate.should_recover(False, False, now))
        for now in range(0, 1000, 30):
            self.assertFalse(gate.should_recover(True, True, now))
        self.assertFalse(gate.should_recover(False, True, 1000))

    def test_probe_errors_do_not_erase_known_authenticated_failures(self):
        gate = t.RecoveryGate()
        for now in (0, 30, 60):
            self.assertFalse(gate.probe_unavailable(True, now))
        self.assertTrue(gate.probe_unavailable(True, 90))
        gate.attempted(90)
        self.assertFalse(gate.probe_unavailable(True, 120))
        self.assertFalse(gate.probe_unavailable(False, 150))

    def test_running_process_is_not_online_evidence(self):
        result = subprocess.CompletedProcess([], 0, json.dumps({
            'BackendState': 'Running', 'Self': {'Online': False},
            'TailscaleIPs': ['127.0.0.1']}), '')
        with patch.object(t, 'ts', return_value=result):
            self.assertEqual(t.network(), (False, '127.0.0.1'))

    def test_invalid_identity_cannot_inject_sshd_settings(self):
        for user in ('root', 'fixture\nPermitRootLogin yes', 'fixture;command'):
            with self.assertRaises(RuntimeError):
                t.ssh_config(user, '127.0.0.1')
        with self.assertRaises(ValueError):
            t.ssh_config('fixture', '127.0.0.1\nPort 2222')

    def test_status_requires_private_listener_and_running_backend(self):
        listener = subprocess.CompletedProcess([], 0, 'LISTEN 0 128 127.0.0.1:22 *:* users:(("sshd"))', '')
        with patch.object(t, 'network', return_value=(False, '127.0.0.1')), \
             patch.object(t, 'run', return_value=listener), patch.object(t, 'processes', return_value=[]), \
             patch.object(t, 'ssh_banner_ready', return_value=False):
            self.assertFalse(t.status()['ssh_ready'])
        with patch.object(t, 'network', return_value=(True, '127.0.0.1')), \
             patch.object(t, 'run', return_value=listener), patch.object(t, 'processes', return_value=[(1, [])]), \
             patch.object(t, 'ssh_banner_ready', return_value=True):
            self.assertTrue(t.status()['ssh_ready'])
        with patch.object(t, 'network', return_value=(True, '127.0.0.2')), \
             patch.object(t, 'run', return_value=listener), patch.object(t, 'processes', return_value=[]), \
             patch.object(t, 'ssh_banner_ready', return_value=False):
            self.assertFalse(t.status()['ssh_ready'])

    def test_ssh_banner_probe_requires_ssh_prefix(self):
        class Connection:
            def __init__(self, payload): self.payload = payload
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def settimeout(self, value): pass
            def recv(self, size): return self.payload
        with patch.object(t.socket, 'create_connection', return_value=Connection(b'SSH-2.0-test\r\n')):
            self.assertTrue(t.ssh_banner_ready('127.0.0.1'))
        with patch.object(t.socket, 'create_connection', return_value=Connection(b'HTTP/1.1 200')):
            self.assertFalse(t.ssh_banner_ready('127.0.0.1'))

    def test_runtime_write_refuses_symlink_and_keeps_private_mode(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name).resolve()
            outside = root / 'fixture-original'
            outside.write_text('keep')
            link = root / 'fixture-link'
            link.symlink_to(outside)
            with self.assertRaises(RuntimeError): t.write(link, 'replace')
            self.assertEqual(outside.read_text(), 'keep')
            target = root / 'fixture-config'
            t.write(target, json.dumps({'fixture': True}))
            self.assertEqual(target.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(target.read_text()), {'fixture': True})


class PersistentIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.config = self.root / 'system-config.json'
        self.legacy = self.root / 'legacy.state'
        self.key = self.root / 'system-key'
        self.key.write_text('fixture-host-key')
        self.account = SimpleNamespace(pw_uid=1000, pw_dir=str(self.root / 'home'))
        for target, value in [('CONFIG', self.config), ('STATE', self.legacy),
                              ('SYSTEM_HOST_KEY', self.key)]:
            patcher = patch.object(t, target, value); patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch.object(t.pwd, 'getpwnam', return_value=self.account)
        patcher.start(); self.addCleanup(patcher.stop)

    def test_system_loss_recovers_config_state_and_original_host_key(self):
        config = {'user': 'fixture', 'control_url': 'https://example.invalid'}
        self.legacy.write_text('{"fixture": "registered"}')
        target = t.prepare_identity(config)
        with patch.object(t, 'processes', return_value=[]):
            self.assertTrue(t.migrate_daemon(target))
        self.assertFalse(self.legacy.exists())
        self.key.write_text('replacement-system-key')
        restored = t.load_config('fixture')
        self.assertEqual(restored, config)
        t.prepare_identity(restored)
        t.write(self.config, json.dumps(restored))
        self.assertEqual(t.state_path().read_text(), '{"fixture": "registered"}')
        self.assertEqual(t.host_key_path().read_text(), 'fixture-host-key')
        self.assertEqual(target.parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)
        self.assertEqual(t.host_key_path().stat().st_mode & 0o777, 0o600)

    def test_running_state_migration_stops_owner_before_copying(self):
        self.legacy.write_text('before-stop')
        target = t.prepare_identity({'user': 'fixture'})
        def flush_on_stop(pid):
            self.assertEqual(pid, 42)
            self.legacy.write_text('flushed-state')
        with patch.object(t, 'processes', return_value=[(42, ['--state=' + str(self.legacy)])]), \
             patch.object(t, 'stop_managed_tail', side_effect=flush_on_stop) as stopped:
            self.assertTrue(t.migrate_daemon(target))
            stopped.assert_called_once_with(42)
        self.assertEqual(target.read_text(), 'flushed-state')

    def test_conflicting_and_symlink_state_never_stops_or_overwrites(self):
        self.legacy.write_text('old-state')
        target = t.prepare_identity({'user': 'fixture'})
        target.write_text('different-identity')
        with patch.object(t, 'processes', return_value=[(42, ['--state=' + str(self.legacy)])]), \
             patch.object(t, 'stop_managed_tail') as stopped:
            with self.assertRaises(RuntimeError): t.migrate_daemon(target)
            stopped.assert_not_called()
        self.assertEqual(target.read_text(), 'different-identity')
        target.unlink(); target.symlink_to(self.legacy)
        with self.assertRaises(RuntimeError): t.migrate_daemon(target)
        self.assertEqual(self.legacy.read_text(), 'old-state')

    def test_persistent_identity_parent_symlink_is_rejected(self):
        home = Path(self.account.pw_dir); home.mkdir()
        outside = self.root / 'outside'; outside.mkdir()
        (home / '.local').symlink_to(outside)
        with self.assertRaises(RuntimeError): t.prepare_identity({'user': 'fixture'})
        self.assertEqual(list(outside.iterdir()), [])


if __name__ == '__main__':
    unittest.main()


class HealthMonitorTests(unittest.TestCase):
    def monitor(self):
        monitor = t.HealthMonitor()
        monitor.next_backup = float('inf')
        return monitor

    def prefs(self, authenticated=True):
        return subprocess.CompletedProcess([], 0, json.dumps({
            'WantRunning': authenticated, 'LoggedOut': not authenticated}), '')

    def test_ssh_probe_exception_never_restarts_healthy_tailscale(self):
        monitor = self.monitor()
        with patch.object(t, 'network', return_value=(True, '127.0.0.1')), \
             patch.object(t, 'ts', return_value=self.prefs()), \
             patch.object(t, 'ssh_listener_status', side_effect=subprocess.TimeoutExpired('ss', 5)), \
             patch.object(t, 'health_event'), patch.object(t, 'restart_tailscale') as network, \
             patch.object(monitor, 'recover_ssh') as ssh:
            for now in (0, 30, 60, 90):
                monitor.step(now)
        network.assert_not_called()
        ssh.assert_called_once_with()

    def test_stuck_client_recovers_with_recent_authentication(self):
        monitor = self.monitor()
        with patch.object(t, 'network', return_value=(False, None)), \
             patch.object(t, 'ts', side_effect=[self.prefs(), OSError(), OSError(), OSError()]), \
             patch.object(t, 'health_event'), patch.object(t, 'restart_tailscale') as restart:
            for now in (0, 30, 60, 90):
                monitor.step(now)
        restart.assert_called_once_with()

    def test_old_or_logged_out_authentication_never_causes_restart(self):
        monitor = self.monitor()
        with patch.object(t, 'ts', return_value=self.prefs()):
            self.assertTrue(monitor.authentication(0))
        with patch.object(t, 'ts', side_effect=OSError()):
            self.assertTrue(monitor.authentication(90))
            self.assertFalse(monitor.authentication(301))
        with patch.object(t, 'ts', return_value=self.prefs(False)):
            self.assertFalse(monitor.authentication(400))
        with patch.object(t, 'ts', side_effect=OSError()):
            self.assertFalse(monitor.authentication(401))

    def test_restart_exception_is_contained_and_backoff_kept(self):
        monitor = self.monitor()
        with patch.object(t, 'network', return_value=(False, None)), \
             patch.object(t, 'ts', return_value=self.prefs()), \
             patch.object(t, 'health_event') as events, \
             patch.object(t, 'restart_tailscale', side_effect=OSError()) as restart:
            for now in (0, 30, 60, 90, 120):
                monitor.step(now)
        restart.assert_called_once_with()
        self.assertIn(unittest.mock.call('tailscale-recovery-failed'), events.call_args_list)
        self.assertEqual(monitor.network_gate.next_attempt, 690)

    def test_fragmented_ssh_banner_is_accepted(self):
        class Connection:
            def __init__(self): self.parts = iter([b'S', b'SH-2.0-test\r', b'\n'])
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def settimeout(self, value): pass
            def recv(self, size): return next(self.parts)
        with patch.object(t.socket, 'create_connection', return_value=Connection()):
            self.assertTrue(t.ssh_banner_ready('127.0.0.1'))
