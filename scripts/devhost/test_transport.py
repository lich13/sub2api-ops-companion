import importlib.util
import json
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
             patch.object(t, 'run', return_value=listener), patch.object(t, 'processes', return_value=[]):
            self.assertFalse(t.status()['ssh_ready'])
        with patch.object(t, 'network', return_value=(True, '127.0.0.1')), \
             patch.object(t, 'run', return_value=listener), patch.object(t, 'processes', return_value=[(1, [])]):
            self.assertTrue(t.status()['ssh_ready'])
        with patch.object(t, 'network', return_value=(True, '127.0.0.2')), \
             patch.object(t, 'run', return_value=listener), patch.object(t, 'processes', return_value=[]):
            self.assertFalse(t.status()['ssh_ready'])

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


if __name__ == '__main__':
    unittest.main()
