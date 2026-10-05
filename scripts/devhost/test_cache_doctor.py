import fcntl
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

import cache_doctor as c
from devhost import Host, Refused


class CacheDoctorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.host = Host(self.root, process_probe=lambda: [])
        self.registry = self.root / 'cache/cargo-home/registry'
        self.archive = self.registry / 'cache/fixture/cc-1.2.60.crate'
        self.source = self.registry / 'src/fixture/cc-1.2.60'
        self.index = self.registry / 'index/fixture/.cache/2/cc'
        self.archive.parent.mkdir(parents=True)
        self.source.mkdir(parents=True)
        self.index.parent.mkdir(parents=True)
        self.contents = {'src/lib.rs': b'mod target;\n',
                         'src/target/parser.rs': b'// target source\n',
                         'build/probe.rs': b'// build source\n',
                         'src/dist/mod.rs': b'// dist source\n'}
        with tarfile.open(self.archive, 'w:gz') as archive:
            for name, data in self.contents.items():
                entry = tarfile.TarInfo('cc-1.2.60/' + name)
                entry.size = len(data)
                entry.mode = 0o644
                archive.addfile(entry, io.BytesIO(data))
        self.update_index()
        for name, data in self.contents.items():
            dest = self.source / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)

    def update_index(self):
        record = {'name': 'cc', 'vers': '1.2.60',
                  'cksum': hashlib.sha256(self.archive.read_bytes()).hexdigest()}
        self.index.write_bytes(b'\x03\x02\0\0\0etag: fixture\0' + b'1.2.60\0' + json.dumps(record).encode() + b'\0')

    def test_missing_and_modified_sources_are_repaired_and_verified(self):
        (self.source / 'src/target/parser.rs').unlink()
        (self.source / 'build/probe.rs').write_bytes(b'broken')
        before = c.audit(self.host)
        self.assertFalse(before['ok'])
        self.assertEqual(before['files_repaired'], 0)
        self.assertFalse((self.source / 'src/target/parser.rs').exists())
        result = c.audit(self.host, repair=True)
        self.assertTrue(result['ok'])
        self.assertEqual(result['files_repaired'], 2)
        for name, data in self.contents.items():
            self.assertEqual((self.source / name).read_bytes(), data)
        after = c.audit(self.host)
        self.assertTrue(after['ok'])
        self.assertEqual(after['affected'], [])

    def test_bad_archive_checksum_refuses_repair(self):
        (self.source / 'src/lib.rs').unlink()
        with self.archive.open('ab') as stream:
            stream.write(b'corrupt')
        result = c.audit(self.host, True)
        self.assertFalse(result['ok'])
        self.assertEqual(result['files_repaired'], 0)
        self.assertFalse((self.source / 'src/lib.rs').exists())

    def test_missing_checksum_refuses_repair(self):
        self.index.unlink()
        self.assertFalse(c.audit(self.host, True)['ok'])

    def test_symlink_source_never_changes_external_file(self):
        external = self.root / 'external'
        external.write_bytes(b'keep')
        target = self.source / 'src/lib.rs'
        target.unlink()
        target.symlink_to(external)
        self.assertFalse(c.audit(self.host, True)['ok'])
        self.assertEqual(external.read_bytes(), b'keep')

    def test_parent_traversal_archive_is_refused_before_any_write(self):
        with tarfile.open(self.archive, 'w:gz') as archive:
            entry = tarfile.TarInfo('cc-1.2.60/../../external')
            entry.size = 4
            archive.addfile(entry, io.BytesIO(b'evil'))
        self.update_index()
        self.assertFalse(c.audit(self.host, True)['ok'])
        self.assertFalse((self.root / 'external').exists())

    def test_active_build_and_cargo_lock_refuse_mutation(self):
        self.host.process_probe = lambda: [{'pid': 42, 'tool': 'cargo'}]
        with self.assertRaises(Refused):
            c.audit(self.host, True)
        self.host.process_probe = lambda: []
        with (self.registry.parent / '.package-cache').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaises(Refused):
                c.audit(self.host, True)

    def test_sources_with_build_dist_target_names_are_healthy(self):
        result = c.audit(self.host)
        self.assertTrue(result['ok'])
        self.assertEqual(result['files_checked'], len(self.contents))

    def test_semver_build_metadata_is_split_without_corrupting_name(self):
        self.assertEqual(c.split_crate('toml-0.9.10+spec-1.1.0'), ('toml', '0.9.10+spec-1.1.0'))
        self.assertEqual(c.split_crate('proc-macro2-1.0.92'), ('proc-macro2', '1.0.92'))

    def test_unextracted_archives_and_extra_user_files_are_untouched(self):
        extra = self.source / 'extra.txt'
        extra.write_text('keep')
        self.assertTrue(c.audit(self.host, True)['ok'])
        self.assertEqual(extra.read_text(), 'keep')
