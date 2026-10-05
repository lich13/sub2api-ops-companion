import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('toolchains', Path(__file__).with_name('toolchains.py'))
t = importlib.util.module_from_spec(spec)
spec.loader.exec_module(t)


class ToolchainTests(unittest.TestCase):
    def test_archive_cannot_overwrite_existing_toolchain(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            host = t.d.Host(root)
            dest = host.home / 'toolchains/node/bin'
            dest.mkdir(parents=True)
            (dest / 'node').write_text('existing fixture')
            archive = root / 'fixture.tgz'
            with tarfile.open(archive, 'w:gz') as package:
                data = b'replacement fixture'
                member = tarfile.TarInfo('fixture/bin/node')
                member.size = len(data)
                package.addfile(member, io.BytesIO(data))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            item = {'url': 'https://example.invalid/fixture.tgz', 'sha256': digest, 'prefix': 'fixture'}
            stage = root / 'stage'
            stage.mkdir()
            def download(_url, target, *_args):
                target.write_bytes(archive.read_bytes())
            with patch.object(t, 'fetch', side_effect=download):
                with self.assertRaises(t.d.Refused): t.install_archive(host, 'node', item, stage)
            self.assertEqual((dest / 'node').read_text(), 'existing fixture')

    def test_missing_npm_tree_is_restored_without_changing_node(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name).resolve()
            source = root / 'archive'; dest = root / 'retained'
            for folder in (source, dest): (folder / 'bin').mkdir(parents=True)
            (source / 'bin/node').write_text('same-node')
            (dest / 'bin/node').write_text('same-node')
            npm = source / 'lib/node_modules/npm/bin/npm-cli.js'
            npm.parent.mkdir(parents=True); npm.write_text('fixture-npm')
            (source / 'bin/npm').symlink_to('../lib/node_modules/npm/bin/npm-cli.js')
            (dest / 'bin/npm').symlink_to('../lib/node_modules/npm/bin/npm-cli.js')
            t.restore_missing_tree(source, dest)
            self.assertEqual((dest / 'bin/npm').read_text(), 'fixture-npm')
            self.assertEqual((dest / 'bin/node').read_text(), 'same-node')
            t.restore_missing_tree(source, dest)

    def test_repair_refuses_changed_files_and_symlink_escape(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name).resolve()
            source = root / 'archive'; dest = root / 'retained'
            source.mkdir(); dest.mkdir()
            (source / 'tool').write_text('official')
            (dest / 'tool').write_text('keep')
            with self.assertRaises(t.d.Refused): t.restore_missing_tree(source, dest)
            self.assertEqual((dest / 'tool').read_text(), 'keep')
            (dest / 'tool').unlink(); outside = root / 'outside'; outside.write_text('outside')
            (dest / 'tool').symlink_to(outside)
            with self.assertRaises(t.d.Refused): t.restore_missing_tree(source, dest)
            self.assertEqual(outside.read_text(), 'outside')

    def test_digest_mismatch_is_rejected(self):
        class Response(io.BytesIO):
            url = 'https://example.invalid/fixture'
        with tempfile.TemporaryDirectory() as name, \
             patch.object(t.urllib.request, 'urlopen', return_value=Response(b'fixture')):
            with self.assertRaises(t.d.Refused):
                t.fetch('https://example.invalid/fixture', Path(name) / 'archive', '0' * 64)



class NativeDependencyTests(unittest.TestCase):
    def test_installed_package_does_not_hide_missing_pc_file(self):
        lock = {'apt_packages': {'fixture-native-dev': '1.2'},
                'native_checks': {'pkg_config': ['fixture-native']}}
        def probe(argv, **kwargs):
            if argv[0] == 'dpkg-query':
                return t.subprocess.CompletedProcess(argv, 0, 'installed\t1.2', '')
            self.assertEqual(argv, ['pkg-config', '--modversion', 'fixture-native'])
            return t.subprocess.CompletedProcess(argv, 1, '', 'fixture unavailable')
        with patch.object(t.subprocess, 'run', side_effect=probe) as run:
            result = t.system_status(lock)
        self.assertFalse(result['ready'])
        self.assertTrue(result['packages']['fixture-native-dev']['matches'])
        self.assertFalse(result['pkg_config']['fixture-native']['available'])
        self.assertEqual(run.call_count, 2)

    def test_manifest_drives_missing_package_installation(self):
        lock = {'apt_packages': {'fixture-new-dev': '2.3'}}
        before = {'ready': False, 'packages': {'fixture-new-dev':
                  {'installed': False, 'matches': False, 'expected': '2.3'}},
                  'pkg_config': {}, 'shared_libraries': {}}
        after = {'ready': True}
        with patch.object(t, 'system_status', side_effect=[before, after]), \
             patch.object(t.subprocess, 'run') as run:
            self.assertEqual(t.install_system_packages(lock), after)
        self.assertEqual(run.call_args_list[1].args[0][-1], 'fixture-new-dev=2.3')
        self.assertIn('--no-remove', run.call_args_list[1].args[0])
        self.assertNotIn('upgrade', str(run.call_args_list))

    def test_version_drift_never_downgrades_or_upgrades_automatically(self):
        observed = {'packages': {'fixture-dev':
                    {'installed': True, 'matches': False, 'expected': '1.2', 'version': '1.3'}}}
        with patch.object(t, 'system_status', return_value=observed), \
             patch.object(t.subprocess, 'run') as run:
            with self.assertRaises(t.d.Refused):
                t.install_system_packages({'apt_packages': {'fixture-dev': '1.2'}})
        run.assert_not_called()

    def test_healthy_system_install_is_idempotent(self):
        observed = {'ready': True, 'packages': {'fixture-dev':
                    {'installed': True, 'matches': True, 'expected': '1.2'}},
                    'pkg_config': {}, 'shared_libraries': {}}
        with patch.object(t, 'system_status', return_value=observed), \
             patch.object(t.subprocess, 'run') as run:
            t.install_system_packages({'apt_packages': {'fixture-dev': '1.2'}})
        run.assert_not_called()

    def test_shared_library_must_actually_load(self):
        lock = {'apt_packages': {}, 'native_checks': {'shared_libraries': ['fixture']}}
        with patch.object(t.ctypes.util, 'find_library', return_value='libfixture.so'), \
             patch.object(t.ctypes, 'CDLL', side_effect=OSError('fixture missing')):
            result = t.system_status(lock)
        self.assertFalse(result['ready'])
        self.assertFalse(result['shared_libraries']['fixture'])

    def test_unresolved_native_check_is_an_install_failure(self):
        observed = {'ready': False, 'packages': {}, 'pkg_config':
                    {'fixture-native': {'available': False}}, 'shared_libraries': {}}
        with patch.object(t, 'system_status', return_value=observed), \
             patch.object(t.subprocess, 'run') as run:
            with self.assertRaisesRegex(t.d.Refused, 'fixture-native'):
                t.install_system_packages({'apt_packages': {}})
        run.assert_not_called()


if __name__ == '__main__': unittest.main()
