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


if __name__ == '__main__': unittest.main()
