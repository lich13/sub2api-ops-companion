#!/usr/bin/env python3
"""Prepare remote editor components and generated per-repository workspaces."""
import json
import os
from pathlib import Path
import subprocess
import tomllib

from devhost import Host, Refused, atomic_json
from repositories import PROFILES


def workspaces(host):
    directory = host.home / 'workspaces'
    host.safe(directory, [host.home])
    directory.mkdir(parents=True, exist_ok=True)
    for repo, profile in PROFILES.items():
        base = host.repo_path(repo)
        env = host.environment(repo)
        settings = {
            'rust-analyzer.checkOnSave': False,
            'typescript.tsdk': str(base / profile['frontend'] / 'node_modules/typescript/lib'),
            'python.defaultInterpreterPath': str(host.home / 'venvs' / repo / 'bin/python')
                if profile['python'] else str(host.home / 'bin/python3'),
            'python.terminal.activateEnvironment': False,
            'terminal.integrated.env.linux': {key: env[key] for key in (
                'DEVHOST_REPO', 'DEVHOST_ROOT', 'CARGO_TARGET_DIR', 'RUSTUP_HOME',
                'CARGO_HOME', 'RUSTUP_TOOLCHAIN')},
        }
        recommendations = []
        launches = []
        tasks = [{'label': action, 'type': 'process',
                  'command': str(host.home / 'bin' / ('devhost-' + action)),
                  'args': ['--repo', repo], 'problemMatcher': []}
                 for action in ('prepare', 'check')]
        if profile['rust_manifest']:
            recommendations.extend(['rust-lang.rust-analyzer', 'vadimcn.vscode-lldb'])
            manifest = base / profile['rust_manifest']
            settings['rust-analyzer.linkedProjects'] = [str(manifest)]
            settings['rust-analyzer.cargo.extraEnv'] = {key: env[key] for key in (
                'CARGO_HOME', 'CARGO_TARGET_DIR', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN',
                'CARGO_BUILD_JOBS')}
            guarded = [str(host.home / 'bin/devhost-rust-analyzer'), '--repo', repo]
            settings['rust-analyzer.cargo.buildScripts.overrideCommand'] = guarded
            settings['rust-analyzer.check.overrideCommand'] = guarded
            if manifest.is_file():
                package = tomllib.loads(manifest.read_text()).get('package', {})
                binary = package.get('name', 'nexushub-webd')
                tasks.append({'label': 'build-debug', 'type': 'process',
                              'command': str(host.home / 'bin/devhost-run'),
                              'args': ['--repo', repo, '--', 'cargo', 'build', '--locked',
                                       '--manifest-path', profile['rust_manifest']],
                              'problemMatcher': ['$rustc']})
                launches.append({'name': 'Rust', 'type': 'lldb', 'request': 'launch',
                                 'program': str(host.cache / 'cargo-target' / repo / 'debug' / binary),
                                 'cwd': str(base), 'preLaunchTask': 'build-debug'})
        if profile['python'] or repo == 'codex-design-director-plugin':
            recommendations.extend(['ms-python.python', 'ms-python.vscode-pylance', 'ms-python.debugpy'])
            launches.append({'name': 'Python current file', 'type': 'debugpy', 'request': 'launch',
                             'program': '${file}', 'cwd': str(base), 'console': 'integratedTerminal',
                             'justMyCode': True})
        package_path = base / profile['frontend'] / 'package.json'
        if package_path.is_file():
            package = json.loads(package_path.read_text())
            dependencies = {**package.get('dependencies', {}), **package.get('devDependencies', {})}
            if 'eslint' in dependencies:
                recommendations.append('dbaeumer.vscode-eslint')
                settings['eslint.workingDirectories'] = [{'directory': str(package_path.parent)}]
            if '@biomejs/biome' in dependencies:
                recommendations.append('biomejs.biome')
        output = directory / (repo + '.code-workspace')
        host.safe(output, [directory])
        atomic_json(output, {'folders': [{'path': str(base)}], 'settings': settings,
                            'extensions': {'recommendations': recommendations},
                            'tasks': {'version': '2.0.0', 'tasks': tasks},
                            'launch': {'version': '0.2.0', 'configurations': launches}})


def machine_settings(host):
    env = host.environment()
    guarded = [str(host.home / 'bin/devhost-rust-analyzer')]
    return {
        'rust-analyzer.checkOnSave': False,
        'rust-analyzer.cargo.buildScripts.overrideCommand': guarded,
        'rust-analyzer.check.overrideCommand': guarded,
        'rust-analyzer.cargo.buildScripts.invocationStrategy': 'per_workspace',
        'rust-analyzer.check.invocationStrategy': 'per_workspace',
        'rust-analyzer.server.extraEnv': {key: env[key] for key in (
            'PATH', 'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN', 'CARGO_BUILD_JOBS')},
    }


def install():
    host = Host(Path(os.environ.get('DEVHOST_ROOT', '/workspace')))
    lock = json.loads(Path(__file__).with_name('toolchains.lock.json').read_text())
    servers = sorted((Path.home() / '.vscode-server/cli/servers').glob('Stable-*/server/bin/code-server'),
                     key=lambda p: p.stat().st_mtime, reverse=True)
    if not servers:
        raise Refused('Connect VS Code Remote-SSH once before preparing its remote extensions.', 69)
    with host.lock():
        host.preflight_locked()
        env = host.environment()
        cli = str(servers[0])
        installed = subprocess.check_output([cli, '--list-extensions', '--show-versions'],
                                            text=True, env=env).splitlines()
        for extension, version in lock['vscode_extensions'].items():
            pinned = extension + '@' + version
            if pinned not in installed:
                subprocess.run([cli, '--install-extension', pinned], env=env, check=True)
        subprocess.run(['rustup', 'component', 'add', 'rust-src', '--toolchain', lock['rust']['version']],
                       env=env, check=True)
        machine = Path.home() / '.vscode-server/data/Machine/settings.json'
        if machine.is_symlink():
            raise Refused('Remote editor settings symlink requires inspection')
        settings = json.loads(machine.read_text()) if machine.is_file() else {}
        settings.update(machine_settings(host))
        atomic_json(machine, settings)
        workspaces(host)
        print('Remote extensions and repository workspaces are ready.')


if __name__ == '__main__':
    install()
