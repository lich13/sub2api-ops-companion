#!/usr/bin/env python3
"""Only isolated temporary fixtures; no tool downloads or real caches."""
import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import devhost as d


class GuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='devhost-fixture-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.now = time.time()
        self.metrics = {'free_bytes': 90*d.GIB, 'inodes_free': 900000,
                        'inodes_total': 1000000, 'readonly': False}
        self.host = d.Host(self.root, probe=lambda: dict(self.metrics), now=self.now,
                           process_probe=lambda: [])

    def file(self, rel, age=60):
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('fixture\n')
        os.utime(path, (self.now-age*d.DAY,)*2)
        for parent in path.parents:
            if parent == self.root:
                break
            os.utime(parent, (self.now-age*d.DAY,)*2)
        return path

    def clean(self, apply=True):
        return self.host.clean('preflight', apply)

    def repo(self):
        base = self.root / 'repos/sub2api-ops-companion'
        (base / '.git').mkdir(parents=True)
        return base

    def test_dry_run_does_not_delete(self):
        file = self.file('cache/cargo-target/lich13studio/debug/fixture')
        result = self.clean(False)
        self.assertTrue(file.exists())
        self.assertEqual(len(result['candidates']), 1)
        self.assertEqual(result['released_bytes'], 0)

    def test_old_targets_deleted_but_recent_children_preserved(self):
        old = self.file('cache/cargo-target/lich13studio/debug/fixture')
        fresh = self.file('cache/cargo-target/lich13-switch/debug/fixture', 1)
        self.clean()
        self.assertFalse(old.exists())
        self.assertTrue(fresh.exists())

    def test_protected_data_survives_pressure(self):
        files = [self.file(p) for p in (
            'repos/sub2api-ops-companion/.git/fixture', 'repos/NexusHub/target/fixture',
            'cache/cargo-target/NexusHub/debug/fixture', 'cache/cargo-home/credentials.toml',
            'cache/cargo-home/config.toml', 'cache/cargo-home/bin/fixture',
            'cache/rustup/toolchains/fixture', 'devhost/venvs/fixture',
            'devhost/toolchains/android-sdk/licenses/fixture', 'devhost/vscode-cli/data/fixture',
            'artifacts/release/fixture.apk', 'artifacts/unknown/fixture',
            'devhost/logs/tunnel.log')]
        self.metrics['free_bytes'] = 20*d.GIB
        self.host.clean('emergency', True)
        self.assertTrue(all(f.exists() for f in files))

    def test_only_marked_tmp_artifacts_deleted(self):
        marked = self.file('artifacts/tmp/build1/.devhost-temporary')
        unmarked = self.file('artifacts/tmp/build2/fixture.apk')
        self.clean()
        self.assertFalse(marked.exists())
        self.assertTrue(unmarked.exists())

    def test_symlink_root_refused(self):
        outside = self.root / 'outside'
        outside.mkdir()
        (self.root / 'cache').symlink_to(outside, target_is_directory=True)
        with self.assertRaises(d.Refused):
            self.host.safe(self.root/'cache/registry', [self.root/'cache'])

    def test_nested_symlink_skips_target(self):
        file = self.file('cache/cargo-target/lich13studio/debug/fixture')
        target = file.parents[1]
        (target / 'escape').symlink_to(self.root / 'outside')
        for p in (target/'escape', target):
            os.utime(p, (self.now-60*d.DAY,)*2, follow_symlinks=False)
        self.clean()
        self.assertTrue(file.exists())

    def test_parent_traversal_refused(self):
        base = self.root / 'repos/sub2api-ops-companion'
        with self.assertRaises(d.Refused):
            self.host.safe(base / '../../outside', [base])

    def test_root_deletion_refused(self):
        with self.assertRaises(d.Refused):
            self.host.safe(self.root, [self.root])

    def test_repo_lock_preserves_target(self):
        file = self.file('cache/cargo-target/lich13studio/debug/fixture')
        with self.host.lock('repo-lich13studio'):
            result = self.clean()
        self.assertTrue(file.exists())
        self.assertEqual(len(result['skipped']), 1)

    def test_global_lock_excludes_cleanup(self):
        file = self.file('cache/npm/_cacache/fixture')
        with self.host.lock():
            with self.assertRaises(d.Refused):
                self.clean()
        self.assertTrue(file.exists())

    def test_unmanaged_build_blocks_cleanup(self):
        file = self.file('cache/pip/fixture')
        self.host.process_probe = lambda: [{'pid': 123, 'tool':'cargo'}]
        with self.assertRaises(d.Refused):
            self.clean()
        self.assertTrue(file.exists())

    def test_low_space_does_not_start_child(self):
        self.repo()
        self.metrics['free_bytes'] = 39*d.GIB
        with patch.object(d.subprocess, 'Popen') as popen:
            with self.assertRaises(d.Refused):
                self.host.run('sub2api-ops-companion', ['fixture'])
            popen.assert_not_called()

    def test_low_inodes_does_not_start_child(self):
        self.repo()
        self.metrics['inodes_free'] = 1
        with patch.object(d.subprocess, 'Popen') as popen:
            with self.assertRaises(d.Refused):
                self.host.run('sub2api-ops-companion', ['fixture'])
            popen.assert_not_called()

    def test_readonly_stops_before_cleanup(self):
        file = self.file('cache/pip/fixture')
        self.metrics['readonly'] = True
        with self.assertRaises(d.Refused):
            self.clean()
        self.assertTrue(file.exists())

    def test_cache_budget_blocks_child(self):
        self.repo()
        with patch.object(self.host, 'cache_bytes', return_value=51*d.GIB):
            with patch.object(d.subprocess, 'Popen') as popen:
                with self.assertRaises(d.Refused):
                    self.host.run('sub2api-ops-companion', ['fixture'])
                popen.assert_not_called()

    def test_failed_command_returns_original_code_and_runs_cleanup(self):
        self.repo()
        with contextlib.redirect_stdout(io.StringIO()):
            code = self.host.run('sub2api-ops-companion', [sys.executable,'-c','raise SystemExit(17)'])
        self.assertEqual(code, 17)
        log = (self.host.home/'logs/maintenance.jsonl').read_text()
        self.assertIn('postbuild', log)
        self.assertNotIn('raise SystemExit', log)

    def test_space_monitor_terminates_only_owned_command(self):
        self.repo()
        child_marker = self.root/'child-ready'
        healthy = dict(self.metrics)
        def metrics():
            return {**healthy, 'free_bytes': 1*d.GIB} if child_marker.exists() else healthy
        self.host.probe = metrics
        command = [sys.executable, '-c',
                   'from pathlib import Path; import time; Path('+repr(str(child_marker))+').touch(); time.sleep(90)']
        with contextlib.redirect_stdout(io.StringIO()):
            code = self.host.run('sub2api-ops-companion', command)
        self.assertEqual(code, 74)

    def test_status_is_readonly(self):
        with patch.object(self.host, 'tunnels', return_value=[]):
            self.host.status()
        self.assertFalse(self.host.home.exists())

    def test_env_preserves_rustup_and_tunnel_auth_location(self):
        env = self.host.environment('lich13studio')
        self.assertEqual(env['CARGO_TARGET_DIR'],str(self.root/'cache/cargo-target/lich13studio'))
        self.assertEqual(env['RUSTUP_HOME'],str(self.root/'cache/rustup'))
        self.assertIn(str(self.root/'cache/cargo-home/bin'),env['PATH'])
        self.assertEqual(env['VSCODE_CLI_DATA_DIR'],str(self.host.home/'vscode-cli/data'))

    def test_nexushub_not_enrolled(self):
        with self.assertRaises(d.Refused):
            self.host.run('NexusHub',['fixture'])

    def test_fresh_log_retained_old_log_removed(self):
        old = self.file('devhost/logs/old.log')
        fresh = self.file('devhost/logs/fresh.log',1)
        self.clean()
        self.assertFalse(old.exists())
        self.assertTrue(fresh.exists())

    def test_pnpm_dry_run_never_invokes_native_tool(self):
        self.file('cache/pnpm-store/fixture')
        with patch.object(d.subprocess,'run') as run:
            self.clean(False)
            run.assert_not_called()

    def test_gradle_retention_categories(self):
        released = self.file('cache/gradle/wrapper/dists/gradle-8.0-bin/fixture',46)
        recent = self.file('cache/gradle/wrapper/dists/gradle-8.14.3-bin/fixture',44)
        snapshot = self.file('cache/gradle/caches/9.0-SNAPSHOT/fixture',11)
        build = self.file('cache/gradle/caches/build-cache-1/fixture',6)
        self.clean()
        self.assertFalse(released.exists())
        self.assertTrue(recent.exists())
        self.assertFalse(snapshot.exists())
        self.assertFalse(build.exists())


if __name__ == '__main__':
    unittest.main()
