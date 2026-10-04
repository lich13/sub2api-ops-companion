import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import devhost
import editor
import repositories


class RepositoryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='devhost-profile-fixture-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.host = devhost.Host(self.root, process_probe=lambda: [])

    def test_nested_npm_project_clears_previous_python_and_pnpm_environment(self):
        inherited = {'VIRTUAL_ENV': str(self.host.home / 'venvs/pylon'),
                     'npm_config_store_dir': 'fixture', 'DEVHOST_PNPM_VERSION': 'fixture',
                     'PATH': str(self.host.home / 'venvs/pylon/bin') + ':/usr/bin'}
        with patch.dict(os.environ, inherited):
            env = self.host.environment('cloudtune')
        self.assertNotIn('npm_config_store_dir', env)
        self.assertNotIn('DEVHOST_PNPM_VERSION', env)
        self.assertNotIn('VIRTUAL_ENV', env)
        self.assertNotIn('/venvs/pylon/bin', env['PATH'])
        self.assertEqual(env['DEVHOST_PACKAGE_MANAGER'], 'npm')

    def test_directory_selection_changes_repo_after_cd(self):
        cwd = self.root / 'repos/sub2api-settlement/frontend'
        exports = self.host.shell_environment('NexusHub', cwd)
        self.assertIn('DEVHOST_REPO=sub2api-settlement', exports)
        self.assertIn('DEVHOST_PNPM_VERSION=10.27.0', exports)
        self.assertEqual(repositories.from_directory(self.root / 'repos/nexushub/webui', self.root), 'NexusHub')

    def test_profile_rejects_escaping_task_directory(self):
        data = json.loads(Path(repositories.__file__).with_name('repositories.json').read_text())
        data['repositories']['cloudtune']['check'][0]['cwd'] = '../fixture'
        manifest = self.root / 'fixture.json'
        manifest.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, 'escapes'):
            repositories.load(manifest)

    def test_failed_recipe_stops_before_next_command(self):
        base = self.root / 'repos/cloudtune'
        base.mkdir(parents=True)
        with patch.object(repositories.subprocess, 'run', return_value=subprocess.CompletedProcess([], 17)) as run:
            result = repositories.run_profile('cloudtune', 'check', base, self.host.home)
        self.assertEqual(result, 17)
        self.assertEqual(run.call_count, 1)

    def test_recipe_refuses_symlink_outside_repo(self):
        base = self.root / 'repos/sub2api-settlement'
        base.mkdir(parents=True)
        outside = self.root / 'fixture'
        outside.mkdir()
        (base / 'frontend').symlink_to(outside, target_is_directory=True)
        with patch.object(repositories.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)):
            with self.assertRaisesRegex(ValueError, 'escapes'):
                repositories.run_profile('sub2api-settlement', 'check', base, self.host.home)

    def test_new_frontend_dependencies_count_toward_budget(self):
        node = self.root / 'repos/sub2api-settlement/frontend/node_modules/fixture'
        node.parent.mkdir(parents=True)
        node.write_bytes(b'x' * 8192)
        self.assertGreaterEqual(self.host.cache_bytes(), 8192)

    def test_generated_editor_builds_use_disk_gate_and_repo_target(self):
        editor.workspaces(self.host)
        document = json.loads((self.host.home / 'workspaces/lich13-switch.code-workspace').read_text())
        settings = document['settings']
        self.assertFalse(settings['rust-analyzer.checkOnSave'])
        command = settings['rust-analyzer.cargo.buildScripts.overrideCommand']
        self.assertEqual(command[:3], [str(self.host.home / 'bin/devhost-rust-analyzer'), '--repo', 'lich13-switch'])
        self.assertEqual(settings['rust-analyzer.cargo.extraEnv']['CARGO_TARGET_DIR'],
                         str(self.root / 'cache/cargo-target/lich13-switch'))

    def test_folder_opening_also_guards_editor_builds(self):
        settings = editor.machine_settings(self.host)
        self.assertFalse(settings['rust-analyzer.checkOnSave'])
        for key in ('rust-analyzer.cargo.buildScripts.overrideCommand',
                    'rust-analyzer.check.overrideCommand'):
            self.assertEqual(settings[key], [str(self.host.home / 'bin/devhost-rust-analyzer')])

    def test_editor_build_infers_nested_repo_and_uses_guard(self):
        with patch.object(self.host, 'run', return_value=75) as run:
            self.assertEqual(self.host.rust_analyzer(cwd=self.root / 'repos/nexushub/crates'), 75)
        repo, command = run.call_args.args
        self.assertEqual(repo, 'NexusHub')
        self.assertIn('--locked', command)
        self.assertEqual(command[-1], str(self.root / 'repos/nexushub/Cargo.toml'))

    def test_unknown_editor_project_cannot_start_build(self):
        with patch.object(self.host, 'run') as run:
            with self.assertRaises(devhost.Refused):
                self.host.rust_analyzer(cwd=self.root / 'fixture')
        run.assert_not_called()

    def test_all_package_versions_match_toolchain_lock(self):
        lock = json.loads(Path(devhost.__file__).with_name('toolchains.lock.json').read_text())
        for name, profile in repositories.PROFILES.items():
            if profile['package_manager'] == 'pnpm':
                self.assertEqual(profile['package_manager_version'], lock['pnpm'][name])
            elif profile['package_manager'] == 'npm':
                self.assertEqual(profile['package_manager_version'], lock['npm']['version'])
        self.assertEqual(len(repositories.PROFILES), 13)


if __name__ == '__main__':
    unittest.main()
