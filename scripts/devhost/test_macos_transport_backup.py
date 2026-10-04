import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
transport_module = sys.modules.get('transport_backup')
if transport_module is None:
    transport_spec = importlib.util.spec_from_file_location(
        'transport_backup', HERE / 'transport_backup.py')
    transport_module = importlib.util.module_from_spec(transport_spec)
    sys.modules['transport_backup'] = transport_module
    transport_spec.loader.exec_module(transport_module)
tb = transport_module

mac_spec = importlib.util.spec_from_file_location(
    'macos_transport_backup_under_test', HERE / 'macos_transport_backup.py')
mac = importlib.util.module_from_spec(mac_spec)
mac_spec.loader.exec_module(mac)


class FakeSshRunner:
    def __init__(self, payload=None, returncode=0):
        self.payload = payload
        self.returncode = returncode
        self.calls = []

    def __call__(self, command, stdout=None, **kwargs):
        self.calls.append((command, kwargs))
        if stdout is not None and self.payload is not None:
            stdout.write(self.payload)
        return subprocess.CompletedProcess(command, self.returncode)


class MacTransportBackupTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='mac-transport-backup-fixture-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.base = self.root / 'recovery'
        self.base_patcher = patch.object(mac, 'BASE', self.base)
        self.ssh_patcher = patch.object(mac, 'SSH', ['fixture-ssh'])
        self.base_patcher.start()
        self.ssh_patcher.start()
        self.addCleanup(self.base_patcher.stop)
        self.addCleanup(self.ssh_patcher.stop)

    @staticmethod
    def archive_bytes(state='state-v1', corrupt_manifest=False):
        files = {
            'transport.json': json.dumps({
                'persistent_identity': True,
                'user': 'fixture',
                'control_url': 'https://example.invalid/transport',
            }, sort_keys=True).encode() + b'\n',
            'tailscaled.state': json.dumps({'fixture': state}, sort_keys=True).encode() + b'\n',
            'ssh_host_ed25519_key': b'fixture-host-key-material\n',
            'authorized_keys': b'fixture-authorized-entry@example.invalid\n',
            'toolchains.lock.json': b'{"fixture":"toolchain-v1"}\n',
        }
        manifest = {
            'schema': 1,
            'created_at': '2026-10-05T00:00:00Z',
            'files': {name: {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
                      for name, data in files.items()},
        }
        if corrupt_manifest:
            manifest['files']['tailscaled.state']['sha256'] = '0' * 64
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w:gz') as archive:
            for name, data in [('manifest.json', tb.encode(manifest)), *files.items()]:
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o600
                archive.addfile(info, io.BytesIO(data))
        return output.getvalue()

    def run_snapshot(self, payload, returncode=0):
        runner = FakeSshRunner(payload=payload, returncode=returncode)
        with patch.object(mac.subprocess, 'run', side_effect=runner):
            result = mac.snapshot()
        return result, runner

    def test_offline_snapshot_does_not_replace_existing_local_backup(self):
        payload = self.archive_bytes()
        result, _ = self.run_snapshot(payload)
        self.assertEqual(result, 0)
        index_path = self.base / 'index.json'
        old_index = index_path.read_bytes()
        current = json.loads(old_index)['current']
        old_archive = (self.base / 'backups' / current).read_bytes()

        result, runner = self.run_snapshot(None, returncode=1)

        self.assertEqual(result, 1)
        self.assertEqual(index_path.read_bytes(), old_index)
        self.assertEqual((self.base / 'backups' / current).read_bytes(), old_archive)
        self.assertEqual(len(list((self.base / 'backups').glob('*.tar.gz'))), 1)
        self.assertEqual(json.loads((self.base / 'last-result.json').read_text())['category'],
                         'ssh-or-remote-backup-unavailable')
        self.assertEqual(runner.calls[0][0][-1], '/workspace/devhost/bin/devhost-backup --export')

    def test_identical_snapshot_does_not_rotate_local_generation(self):
        payload = self.archive_bytes()
        self.assertEqual(self.run_snapshot(payload)[0], 0)
        index_path = self.base / 'index.json'
        old_index = index_path.read_bytes()
        result, _ = self.run_snapshot(payload)

        self.assertEqual(result, 0)
        self.assertEqual(index_path.read_bytes(), old_index)
        self.assertEqual(len(list((self.base / 'backups').glob('*.tar.gz'))), 1)
        self.assertEqual(json.loads((self.base / 'last-result.json').read_text())['category'],
                         'verified-unchanged')

    def test_only_two_recent_different_valid_archives_are_retained(self):
        indexes = []
        for state in ('state-v1', 'state-v2', 'state-v3'):
            result, _ = self.run_snapshot(self.archive_bytes(state=state))
            self.assertEqual(result, 0)
            indexes.append(json.loads((self.base / 'index.json').read_text()))

        self.assertEqual(indexes[1]['previous'], indexes[0]['current'])
        self.assertEqual(indexes[2]['previous'], indexes[1]['current'])
        self.assertFalse((self.base / 'backups' / indexes[0]['current']).exists())
        self.assertTrue((self.base / 'backups' / indexes[1]['current']).exists())
        self.assertTrue((self.base / 'backups' / indexes[2]['current']).exists())
        self.assertEqual(len(list((self.base / 'backups').glob('*.tar.gz'))), 2)

    def test_corrupt_download_is_rejected_without_replacing_existing_archive(self):
        self.assertEqual(self.run_snapshot(self.archive_bytes(state='state-v1'))[0], 0)
        index_path = self.base / 'index.json'
        old_index = index_path.read_bytes()
        current = json.loads(old_index)['current']
        old_archive = (self.base / 'backups' / current).read_bytes()

        runner = FakeSshRunner(payload=self.archive_bytes(state='state-v2', corrupt_manifest=True))
        with patch.object(mac.subprocess, 'run', side_effect=runner):
            with self.assertRaisesRegex(RuntimeError, 'checksum'):
                mac.snapshot()

        self.assertEqual(index_path.read_bytes(), old_index)
        self.assertEqual((self.base / 'backups' / current).read_bytes(), old_archive)
        self.assertEqual(list((self.base / 'backups').glob('*.tar.gz')),
                         [self.base / 'backups' / current])
        self.assertEqual(list((self.base / 'backups').glob('.download-*')), [])


if __name__ == '__main__':
    unittest.main()
