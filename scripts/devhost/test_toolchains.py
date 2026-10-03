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

    def test_digest_mismatch_is_rejected(self):
        class Response(io.BytesIO):
            url = 'https://example.invalid/fixture'
        with tempfile.TemporaryDirectory() as name, \
             patch.object(t.urllib.request, 'urlopen', return_value=Response(b'fixture')):
            with self.assertRaises(t.d.Refused):
                t.fetch('https://example.invalid/fixture', Path(name) / 'archive', '0' * 64)


if __name__ == '__main__': unittest.main()
