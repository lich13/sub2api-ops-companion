import fcntl
import importlib.util
import io
import json
import multiprocessing
import os
from pathlib import Path
import shutil
import stat
import tarfile
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    'transport_backup_under_test', Path(__file__).with_name('transport_backup.py'))
tb = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tb)


def hold_lock(store, ready, release):
    with store.locked():
        ready.send(True)
        release.recv()


class TransportBackupTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='transport-backup-fixture-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.home = self.root / 'home'
        self.system = self.root / 'system'
        self.home.mkdir()
        self.system.mkdir()
        self.account = SimpleNamespace(pw_uid=23001, pw_gid=23002,
                                       pw_dir=str(self.home))
        self.fchown_calls = []
        self.chown_calls = []
        patchers = [
            patch.object(tb.pwd, 'getpwnam', return_value=self.account),
            patch.object(tb.os, 'fchown',
                         side_effect=lambda fd, uid, gid: self.fchown_calls.append((uid, gid))),
            patch.object(tb.os, 'chown',
                         side_effect=lambda path, uid, gid: self.chown_calls.append(
                             (Path(path), uid, gid))),
        ]
        for patcher in patchers:
            patcher.start()
            self.addCleanup(patcher.stop)
        self.values = {
            'transport.json': json.dumps({
                'persistent_identity': True,
                'user': 'fixture',
                'control_url': 'https://example.invalid/transport',
            }, sort_keys=True).encode() + b'\n',
            'tailscaled.state': b'{"fixture":"state-v1"}\n',
            'ssh_host_ed25519_key': b'fixture-host-key-material\n',
            'authorized_keys': b'fixture-authorized-entry@example.invalid\n',
            'toolchains.lock.json': b'{"fixture":"toolchain-v1"}\n',
        }
        self._write_live_fixture()
        self.store = tb.Store('fixture', home=self.home, system=self.system)

    def _write_live_fixture(self):
        identity = self.home / '.local/state/devhost/transport'
        identity.mkdir(parents=True)
        ssh = self.home / '.ssh'
        ssh.mkdir()
        for name in ('transport.json', 'tailscaled.state', 'ssh_host_ed25519_key'):
            (identity / name).write_bytes(self.values[name])
        (ssh / 'authorized_keys').write_bytes(self.values['authorized_keys'])
        (self.system / 'toolchains.lock.json').write_bytes(self.values['toolchains.lock.json'])
        (self.system / 'transport.json').write_bytes(self.values['transport.json'])

    @staticmethod
    def _mode(path):
        return stat.S_IMODE(path.stat().st_mode)

    def _archive_bytes(self, members):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode='w:gz') as archive:
            for name, data, *size_override in members:
                info = tarfile.TarInfo(name)
                info.mode = 0o600
                info.size = size_override[0] if size_override else len(data or b'')
                archive.addfile(info, None if data is None else io.BytesIO(data))
        return output.getvalue()

    def test_two_generation_rotation_and_identical_content_does_not_rotate(self):
        first = self.store.backup()
        self.assertTrue(first['changed'])
        first_index = json.loads((self.store.root / 'index.json').read_text())
        first_name = first_index['current']

        unchanged = self.store.backup()
        self.assertFalse(unchanged['changed'])
        self.assertEqual(json.loads((self.store.root / 'index.json').read_text()), first_index)
        self.assertEqual(len(list(self.store.root.glob('g-*'))), 1)

        (self.store.identity / 'tailscaled.state').write_bytes(b'{"fixture":"state-v2"}\n')
        second = self.store.backup()
        self.assertTrue(second['changed'])
        second_index = json.loads((self.store.root / 'index.json').read_text())
        second_name = second_index['current']
        self.assertEqual(second_index['previous'], first_name)
        self.assertNotEqual(second_name, first_name)

        (self.store.identity / 'tailscaled.state').write_bytes(b'{"fixture":"state-v3"}\n')
        third = self.store.backup()
        self.assertTrue(third['changed'])
        third_index = json.loads((self.store.root / 'index.json').read_text())
        self.assertEqual(third_index['previous'], second_name)
        self.assertFalse((self.store.root / first_name).exists())
        self.assertEqual(len(list(self.store.root.glob('g-*'))), 2)
        _, current_files = self.store.load()
        _, previous_files = self.store.load(previous=True)
        self.assertEqual(current_files['tailscaled.state'], b'{"fixture":"state-v3"}\n')
        self.assertEqual(previous_files['tailscaled.state'], b'{"fixture":"state-v2"}\n')

    def test_missing_source_keeps_the_last_valid_backup(self):
        self.store.backup()
        index_path = self.store.root / 'index.json'
        before_index = index_path.read_bytes()
        current = json.loads(before_index)['current']
        before_payload = (self.store.root / current / 'manifest.json').read_bytes()
        (self.store.identity / 'tailscaled.state').unlink()

        with self.assertRaises(FileNotFoundError):
            self.store.backup()

        self.assertEqual(index_path.read_bytes(), before_index)
        self.assertEqual((self.store.root / current / 'manifest.json').read_bytes(), before_payload)
        _, files = self.store.load()
        self.assertEqual(files['tailscaled.state'], self.values['tailscaled.state'])

    def test_restore_recreates_missing_values_but_preserves_existing_different_value(self):
        self.store.backup()
        shutil.rmtree(self.store.identity)
        (self.system / 'transport.json').unlink()
        (self.system / 'toolchains.lock.json').unlink()
        different = b'fixture-existing-authorized-entry@example.invalid\n'
        (self.home / '.ssh/authorized_keys').write_bytes(different)

        result = self.store.restore()

        self.assertTrue(result['available'])
        self.assertEqual(result['restored'], 5)
        for name in ('transport.json', 'tailscaled.state', 'ssh_host_ed25519_key'):
            self.assertEqual((self.store.identity / name).read_bytes(), self.values[name])
        self.assertEqual((self.system / 'toolchains.lock.json').read_bytes(),
                         self.values['toolchains.lock.json'])
        self.assertEqual((self.system / 'transport.json').read_bytes(),
                         self.values['transport.json'])
        self.assertEqual((self.home / '.ssh/authorized_keys').read_bytes(), different)

    def test_corrupt_generation_is_rejected_before_any_restore_write(self):
        self.store.backup()
        current = json.loads((self.store.root / 'index.json').read_text())['current']
        (self.store.root / current / 'tailscaled.state').write_bytes(b'{"fixture":"corrupt"}\n')
        shutil.rmtree(self.store.identity)
        (self.home / '.ssh/authorized_keys').unlink()
        (self.system / 'transport.json').unlink()
        (self.system / 'toolchains.lock.json').unlink()

        with self.assertRaisesRegex(RuntimeError, 'checksum'):
            self.store.restore()

        self.assertFalse(self.store.identity.exists())
        self.assertFalse((self.home / '.ssh/authorized_keys').exists())
        self.assertFalse((self.system / 'transport.json').exists())
        self.assertFalse((self.system / 'toolchains.lock.json').exists())

    def test_destination_symlink_is_rejected_before_any_restore_write(self):
        self.store.backup()
        shutil.rmtree(self.store.identity)
        (self.home / '.ssh/authorized_keys').unlink()
        (self.system / 'transport.json').unlink()
        (self.system / 'toolchains.lock.json').unlink()
        outside = self.root / 'outside'
        outside.write_bytes(b'fixture-outside-content')
        self.store.identity.mkdir(parents=True)
        (self.store.identity / 'transport.json').symlink_to(outside)

        with self.assertRaisesRegex(RuntimeError, 'Symlink'):
            self.store.restore()

        self.assertEqual(outside.read_bytes(), b'fixture-outside-content')
        self.assertFalse((self.store.identity / 'tailscaled.state').exists())
        self.assertFalse((self.store.identity / 'ssh_host_ed25519_key').exists())
        self.assertFalse((self.home / '.ssh/authorized_keys').exists())
        self.assertFalse((self.system / 'transport.json').exists())
        self.assertFalse((self.system / 'toolchains.lock.json').exists())

    def _prepare_corrupt_current(self):
        self.store.backup()
        first_name = json.loads((self.store.root / 'index.json').read_text())['current']
        (self.store.identity / 'tailscaled.state').write_bytes(b'{"fixture":"state-v2"}\n')
        self.store.backup()
        index = json.loads((self.store.root / 'index.json').read_text())
        current_name = index['current']
        self.assertEqual(index['previous'], first_name)
        (self.store.root / current_name / 'tailscaled.state').write_bytes(
            b'{"fixture":"corrupt-current"}\n')
        return first_name

    def _remove_live_transport(self):
        shutil.rmtree(self.store.identity)
        (self.home / '.ssh/authorized_keys').unlink()
        (self.system / 'transport.json').unlink()
        (self.system / 'toolchains.lock.json').unlink()

    def _assert_repaired_backup_is_usable(self, expected_state):
        index = json.loads((self.store.root / 'index.json').read_text())
        self.assertIsNone(index.get('previous'))
        _, files = self.store.load()
        self.assertEqual(files['tailscaled.state'], expected_state)
        status = self.store.status()
        self.assertTrue(status['available'])
        self.assertFalse(status['previous_available'])
        self.assertIn('changed', self.store.backup())
        self.assertTrue(self.store.status()['available'])

    def test_previous_restore_repairs_corrupt_current_for_followup_backup_and_status(self):
        first_name = self._prepare_corrupt_current()
        self._remove_live_transport()

        result = self.store.restore(previous=True)

        self.assertTrue(result['available'])
        self.assertEqual(result['restored'], 6)
        self.assertEqual(json.loads((self.store.root / 'index.json').read_text())['current'], first_name)
        self._assert_repaired_backup_is_usable(self.values['tailscaled.state'])

    def test_archive_restore_repairs_corrupt_current_for_followup_backup_and_status(self):
        self.store.backup()
        first_name = json.loads((self.store.root / 'index.json').read_text())['current']
        archive_path = self.root / 'fixture-previous.tar.gz'
        with archive_path.open('wb') as output:
            self.store.export(output)
        (self.store.identity / 'tailscaled.state').write_bytes(b'{"fixture":"state-v2"}\n')
        self.store.backup()
        index = json.loads((self.store.root / 'index.json').read_text())
        self.assertEqual(index['previous'], first_name)
        (self.store.root / index['current'] / 'tailscaled.state').write_bytes(
            b'{"fixture":"corrupt-current"}\n')
        self._remove_live_transport()

        result = self.store.restore(archive=archive_path)

        self.assertTrue(result['available'])
        self.assertEqual(result['restored'], 6)
        self.assertEqual(json.loads((self.store.root / 'index.json').read_text())['current'], first_name)
        self._assert_repaired_backup_is_usable(self.values['tailscaled.state'])

    def test_archive_rejects_traversal_duplicate_and_oversized_members(self):
        traversal = self._archive_bytes([('../escape', b'')])
        with self.assertRaisesRegex(RuntimeError, 'unexpected'):
            tb.read_archive(io.BytesIO(traversal))

        duplicate = self._archive_bytes([('manifest.json', b'{}'), ('manifest.json', b'{}')])
        with self.assertRaisesRegex(RuntimeError, 'duplicate'):
            tb.read_archive(io.BytesIO(duplicate))

        oversized = self._archive_bytes([('manifest.json', None, tb.MAX_FILE + 1)])
        with self.assertRaisesRegex(RuntimeError, 'size refused'):
            tb.read_archive(io.BytesIO(oversized))

    def test_export_import_restores_another_fixture_with_secure_modes_and_owner(self):
        self.store.backup()
        archive_path = self.root / 'fixture-export.tar.gz'
        with archive_path.open('wb') as output:
            self.store.export(output)

        other_root = self.root / 'other'
        other_home = other_root / 'home'
        other_system = other_root / 'system'
        other_home.mkdir(parents=True)
        other_system.mkdir()
        other = tb.Store('fixture', home=other_home, system=other_system)
        self.fchown_calls.clear()
        self.chown_calls.clear()

        result = other.restore(archive=archive_path)

        self.assertTrue(result['available'])
        self.assertEqual(result['restored'], 6)
        for name, data in self.values.items():
            target = other.sources[name]
            self.assertEqual(target.read_bytes(), data)
            self.assertEqual(self._mode(target), 0o600)
        self.assertEqual((other.system / 'transport.json').read_bytes(), self.values['transport.json'])
        self.assertEqual(self._mode(other.system / 'transport.json'), 0o600)
        self.assertEqual(self._mode(other.identity), 0o700)
        self.assertEqual(self._mode(other.home / '.ssh'), 0o700)
        self.assertEqual(self._mode(other.home / '.ssh/authorized_keys'), 0o600)
        self.assertEqual(self._mode(other.root), 0o700)
        self.assertEqual(self._mode(other.root / 'backup.lock'), 0o600)
        self.assertEqual(self.fchown_calls, [(self.account.pw_uid, self.account.pw_gid)])
        self.assertEqual(self.chown_calls,
                         [(other.home / '.ssh', self.account.pw_uid, self.account.pw_gid)])

    @unittest.skipUnless(hasattr(os, 'fork'), 'requires fork-based process isolation')
    def test_backup_lock_is_mutually_exclusive_across_processes(self):
        context = multiprocessing.get_context('fork')
        ready_recv, ready_send = context.Pipe(False)
        release_recv, release_send = context.Pipe(False)
        process = context.Process(target=hold_lock,
                                  args=(self.store, ready_send, release_recv))
        process.start()
        try:
            self.assertTrue(ready_recv.poll(5), 'lock holder did not start')
            self.assertTrue(ready_recv.recv())
            fd = os.open(self.store.root / 'backup.lock', os.O_RDWR | os.O_NOFOLLOW)
            try:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            finally:
                os.close(fd)
        finally:
            try:
                release_send.send(True)
            except (BrokenPipeError, OSError):
                pass
            process.join(5)
            if process.is_alive():
                process.terminate()
                process.join(5)
        self.assertEqual(process.exitcode, 0)


if __name__ == '__main__':
    unittest.main()
