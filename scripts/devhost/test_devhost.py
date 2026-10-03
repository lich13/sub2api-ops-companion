#!/usr/bin/env python3
"""Only isolated temporary fixtures; no tool downloads or real caches."""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
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

    def repo(self, name='sub2api-ops-companion'):
        base = self.root / 'repos' / name
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
        nexus_old = self.file('cache/cargo-target/NexusHub/debug/fixture')
        go_mod_old = self.file('cache/go-mod/fixture')
        fresh = self.file('cache/cargo-target/lich13-switch/debug/fixture', 1)
        self.clean()
        self.assertFalse(old.exists())
        self.assertFalse(nexus_old.exists())
        self.assertFalse(go_mod_old.exists())
        self.assertTrue(fresh.exists())

    def test_protected_data_survives_pressure(self):
        files = [self.file(p) for p in (
            'repos/sub2api-ops-companion/.git/fixture', 'repos/NexusHub/target/fixture',
            'cache/cargo-home/credentials.toml',
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

    def test_nondumpable_ssh_session_is_not_an_unmanaged_build(self):
        proc = self.root / 'proc'
        pid = proc / '123456789'
        pid.mkdir(parents=True)
        (pid / 'comm').write_text('sshd-session\n')
        (pid / 'cmdline').write_bytes(b'sshd-session: fixture@notty\0')
        original_resolve = Path.resolve
        def resolve(path, *args, **kwargs):
            if path == pid / 'exe': raise PermissionError()
            return original_resolve(path, *args, **kwargs)
        with patch.object(d, 'Path', side_effect=lambda p: proc if p == '/proc' else Path(p)), \
             patch.object(Path, 'resolve', resolve):
            self.assertEqual(self.host.unmanaged_processes(), [])
            (pid / 'comm').write_text('cargo\n')
            (pid / 'cmdline').write_bytes(b'cargo\0build\0')
            with self.assertRaises(d.Refused):
                self.host.unmanaged_processes()

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
        self.assertEqual(env['RUSTUP_HOME'],str(self.root/'devhost/toolchains/rustup'))
        self.assertEqual(env['GOMODCACHE'],str(self.root/'cache/go-mod'))
        self.assertIn(str(self.root/'cache/cargo-home/bin'),env['PATH'])
        self.assertEqual(env['VSCODE_CLI_DATA_DIR'],str(Path.home()/'.local/share/devhost/vscode-cli'))

    def test_nexushub_is_enrolled(self):
        self.repo('NexusHub')
        with patch.object(self.host, 'preflight_locked', return_value={}), \
             patch.object(self.host, 'clean_locked', return_value={'cache_bytes': 0}), \
             patch.object(d.subprocess, 'Popen') as popen:
            popen.return_value.poll.return_value = 0
            popen.return_value.wait.return_value = 0
            with contextlib.redirect_stdout(io.StringIO()):
                code = self.host.run('NexusHub', ['fixture'])
        self.assertEqual(code, 0)
        popen.assert_called_once()

    def test_up_checks_transport_without_reinstalling_android(self):
        with patch.object(self.host, 'ensure_android_locked', side_effect=AssertionError('bootstrap only')), \
             patch.object(self.host, 'preflight_locked', return_value={}), \
             patch.object(d.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)):
            with contextlib.redirect_stdout(io.StringIO()) as output:
                code = self.host.up()
        self.assertEqual(code, 0)
        self.assertIn('"tunnel_started": false', output.getvalue())

    def test_up_propagates_transport_failure(self):
        with patch.object(self.host, 'preflight_locked') as preflight, \
             patch.object(d.subprocess, 'run', return_value=subprocess.CompletedProcess([], 69)):
            self.assertEqual(self.host.up(), 69)
        preflight.assert_not_called()

    def test_nexushub_lowercase_directory_is_reused(self):
        self.repo('nexushub')
        self.assertEqual(self.host.repo_path('NexusHub'), self.root / 'repos/nexushub')

    def test_tunnel_fallback_is_explicit(self):
        self.host.home.joinpath('bin').mkdir(parents=True)
        self.host.home.joinpath('logs').mkdir(parents=True)
        code_path = self.host.home / 'bin/code'
        code_path.write_text('#!/bin/sh\n')
        code_path.chmod(0o755)
        with patch.object(self.host, 'ensure_vscode_cli') as ensure, \
             patch.object(self.host, 'tunnels', side_effect=[[], [321]]), \
             patch.object(d.subprocess, 'Popen') as popen:
            with contextlib.redirect_stdout(io.StringIO()) as output:
                code = self.host.tunnel_fallback()
        self.assertEqual(code, 0)
        ensure.assert_called_once()
        popen.assert_called_once()
        self.assertIn('"fallback": true', output.getvalue())

    def test_sourceable_env_includes_go_module_cache(self):
        env_script = Path(__file__).with_name('devhost-env')
        result = subprocess.run(
            ['sh', '-c', '. "$1"; . "$1"; printf "%s\\n%s\\n%s\\n%s\\n" "$DEVHOST_ROOT" "$GOMODCACHE" "$DEVHOST_ENV_LOADED" "$GRADLE_OPTS"',
             'devhost-env-test', str(env_script)],
            env={**os.environ, 'DEVHOST_ROOT': str(self.root), 'GRADLE_OPTS': ''},
            check=True, capture_output=True, text=True)
        values = result.stdout.splitlines()
        self.assertEqual(values[:3], [str(self.root), str(self.root / 'cache/go-mod'), '1'])
        self.assertEqual(values[3].count('-Dorg.gradle.daemon=false'), 1)
        self.assertEqual(values[3].count('-Dorg.gradle.workers.max=2'), 1)

    def test_sourceable_env_selects_repo_target_dir(self):
        env_script = Path(__file__).with_name('devhost-env')
        result = subprocess.run(
            ['sh', '-c', '. "$1"; printf "%s\\n" "$CARGO_TARGET_DIR"',
             'devhost-env-test', str(env_script)],
            env={**os.environ, 'DEVHOST_ROOT': str(self.root),
                 'DEVHOST_REPO': 'NexusHub', 'CARGO_TARGET_DIR': ''},
            check=True, capture_output=True, text=True)
        self.assertEqual(result.stdout.strip(), str(self.root / 'cache/cargo-target/NexusHub'))

    def test_environment_separates_repo_targets_and_venvs(self):
        a = self.host.environment('NexusHub')
        b = self.host.environment('lich13studio')
        self.assertNotEqual(a['CARGO_TARGET_DIR'], b['CARGO_TARGET_DIR'])
        self.assertNotEqual(a['UV_PROJECT_ENVIRONMENT'], b['UV_PROJECT_ENVIRONMENT'])
        self.assertEqual(a['DEVHOST_PNPM_VERSION'], '11.0.8')
        self.assertEqual(b['DEVHOST_PNPM_VERSION'], '10.27.0')

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

    def test_preflight_emergency_runs_before_admission_gate(self):
        """Low space must enter cleanup first; gate is rechecked afterwards."""
        old = self.file('cache/cargo-target/lich13studio/debug/fixture')
        self.repo()
        sequence = {'n': 0}
        healthy = {'free_bytes': 90*d.GIB, 'inodes_free': 900000,
                   'inodes_total': 1000000, 'readonly': False}
        low = {**healthy, 'free_bytes': 25*d.GIB}

        def probe():
            sequence['n'] += 1
            # Only the initial preflight observation is low; after emergency cleanup, recover.
            if sequence['n'] == 1:
                return dict(low)
            return dict(healthy)

        self.host.probe = probe
        cleaned = []

        def clean_locked(mode, apply=False):
            cleaned.append(mode)
            return {'mode': mode, 'applied': apply, 'candidates': [], 'skipped': [],
                    'free_bytes': healthy['free_bytes'], 'inodes_free': healthy['inodes_free'],
                    'released_bytes': 10*d.GIB, 'cache_bytes': 0}

        with patch.object(self.host, 'clean_locked', side_effect=clean_locked):
            with patch.object(d.subprocess, 'Popen') as popen:
                popen.return_value.poll.return_value = 0
                popen.return_value.wait.return_value = 0
                with contextlib.redirect_stdout(io.StringIO()):
                    code = self.host.run('sub2api-ops-companion', ['true'])
        self.assertEqual(cleaned, ['emergency', 'postbuild'])
        self.assertEqual(code, 0)
        popen.assert_called_once()

    def test_preflight_still_blocks_when_cleanup_cannot_recover(self):
        self.repo()
        self.metrics['free_bytes'] = 25*d.GIB
        with patch.object(d.subprocess, 'Popen') as popen:
            with self.assertRaises(d.Refused) as ctx:
                self.host.run('sub2api-ops-companion', ['fixture'])
            popen.assert_not_called()
        self.assertIn('40 GiB', str(ctx.exception))

    def _fake_cli_archive(self, payload=b'#!/bin/sh\necho fixture\n', extra_members=None):
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode='w:gz') as tar:
            info = tarfile.TarInfo(name='code')
            info.size = len(payload)
            tar.addfile(info, io.BytesIO(payload))
            for name, data in (extra_members or []):
                info = tarfile.TarInfo(name=name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        return buffer.getvalue()

    def test_vscode_cli_download_is_guarded_and_preserves_auth_data(self):
        archive = self._fake_cli_archive()
        digest = hashlib.sha256(archive).hexdigest()
        auth = self.host.home / 'vscode-cli/data/token.json'
        auth.parent.mkdir(parents=True, exist_ok=True)
        auth.write_text('{"fixture":true}\n')
        os.chmod(auth, 0o600)
        meta = json.dumps({
            'url': 'https://update.code.visualstudio.com/latest/cli-linux-x64/stable',
            'sha256hash': digest,
        }).encode()

        class FakeResp:
            def __init__(self, body, url, length=None):
                self._body = body
                self._url = url
                self.headers = {'Content-Length': str(length if length is not None else len(body))}
                self._offset = 0
            def geturl(self):
                return self._url
            def read(self, n=-1):
                if n is None or n < 0:
                    data = self._body[self._offset:]
                    self._offset = len(self._body)
                    return data
                data = self._body[self._offset:self._offset+n]
                self._offset += len(data)
                return data
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return False

        def fetcher(url, label):
            if url == d.VSCODE_CLI_API:
                return FakeResp(meta, url)
            if 'cli-linux-x64' in url or url.endswith('.tar.gz') or 'latest' in url:
                return FakeResp(archive, 'https://vscode.download.prss.microsoft.com/dbazure/download/stable/fixture/vscode_cli_linux_x64_cli.tar.gz')
            raise AssertionError('unexpected url '+url)

        result = self.host.ensure_vscode_cli(fetcher=fetcher)
        binary = self.host.home / 'vscode-cli/code'
        link = self.host.home / 'bin/code'
        self.assertTrue(result['downloaded'])
        self.assertTrue(binary.is_file())
        self.assertFalse(binary.is_symlink())
        self.assertTrue(link.is_symlink())
        self.assertEqual(link.resolve(), binary.resolve())
        self.assertTrue(auth.exists())
        self.assertEqual(auth.read_text(), '{"fixture":true}\n')
        self.assertEqual(oct(auth.stat().st_mode & 0o777), '0o600')

    def test_vscode_cli_rejects_unexpected_host_and_multi_member_archive(self):
        with self.assertRaises(d.Refused):
            self.host._cli_https_url('https://evil.example/cli.tar.gz', 'VS Code CLI archive')
        with self.assertRaises(d.Refused):
            self.host._cli_https_url('http://update.code.visualstudio.com/latest/cli-linux-x64/stable', 'VS Code CLI archive')

        archive = self._fake_cli_archive(extra_members=[('extra', b'nope')])
        digest = hashlib.sha256(archive).hexdigest()
        meta = json.dumps({
            'url': 'https://update.code.visualstudio.com/latest/cli-linux-x64/stable',
            'sha256hash': digest,
        }).encode()

        class FakeResp:
            def __init__(self, body, url):
                self._body = body
                self._url = url
                self.headers = {'Content-Length': str(len(body))}
                self._offset = 0
            def geturl(self):
                return self._url
            def read(self, n=-1):
                if n is None or n < 0:
                    data = self._body[self._offset:]
                    self._offset = len(self._body)
                    return data
                data = self._body[self._offset:self._offset+n]
                self._offset += len(data)
                return data
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return False

        def fetcher(url, label):
            if url == d.VSCODE_CLI_API:
                return FakeResp(meta, url)
            return FakeResp(archive, 'https://vscode.download.prss.microsoft.com/artifact.tar.gz')

        with self.assertRaises(d.Refused) as ctx:
            self.host.ensure_vscode_cli(fetcher=fetcher)
        self.assertIn('single code member', str(ctx.exception))
        self.assertFalse((self.host.home / 'vscode-cli/code').exists())

    def test_vscode_cli_rejects_sha_mismatch_and_oversize(self):
        archive = self._fake_cli_archive()
        meta = json.dumps({
            'url': 'https://update.code.visualstudio.com/latest/cli-linux-x64/stable',
            'sha256hash': '0'*64,
        }).encode()

        class FakeResp:
            def __init__(self, body, url, length=None):
                self._body = body
                self._url = url
                self.headers = {}
                if length is not None:
                    self.headers['Content-Length'] = str(length)
                else:
                    self.headers['Content-Length'] = str(len(body))
                self._offset = 0
            def geturl(self):
                return self._url
            def read(self, n=-1):
                if n is None or n < 0:
                    data = self._body[self._offset:]
                    self._offset = len(self._body)
                    return data
                data = self._body[self._offset:self._offset+n]
                self._offset += len(data)
                return data
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return False

        def bad_sha(url, label):
            if url == d.VSCODE_CLI_API:
                return FakeResp(meta, url)
            return FakeResp(archive, 'https://vscode.download.prss.microsoft.com/artifact.tar.gz')

        with self.assertRaises(d.Refused) as ctx:
            self.host.ensure_vscode_cli(fetcher=bad_sha)
        self.assertIn('SHA-256', str(ctx.exception))

        good_meta = json.dumps({
            'url': 'https://update.code.visualstudio.com/latest/cli-linux-x64/stable',
            'sha256hash': hashlib.sha256(archive).hexdigest(),
        }).encode()

        def oversize(url, label):
            if url == d.VSCODE_CLI_API:
                return FakeResp(good_meta, url)
            return FakeResp(archive, 'https://vscode.download.prss.microsoft.com/artifact.tar.gz',
                            length=d.VSCODE_CLI_MAX_ARCHIVE + 1)

        with self.assertRaises(d.Refused) as ctx:
            self.host.ensure_vscode_cli(fetcher=oversize)
        self.assertIn('size limit', str(ctx.exception))


if __name__ == '__main__':
    unittest.main()
