#!/usr/bin/env python3
"""Repository profiles shared by shell, package managers and guarded tasks."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

SOURCE = Path(__file__).resolve().parent


def load(path=SOURCE / 'repositories.json'):
    document = json.loads(path.read_text())
    if document.get('schema') != 1:
        raise ValueError('Unsupported repository profile schema')
    profiles = document['repositories']
    directories = set()
    for name, profile in profiles.items():
        directory = profile['directory']
        if not name or '/' in name or directory in ('', '.', '..') or '/' in directory:
            raise ValueError('Invalid repository directory')
        if directory in directories:
            raise ValueError('Duplicate repository directory')
        directories.add(directory)
        if profile['package_manager'] not in (None, 'npm', 'pnpm'):
            raise ValueError('Unsupported package manager')
        for action in ('prepare', 'check'):
            for step in profile[action]:
                cwd = Path(step['cwd'])
                if cwd.is_absolute() or '..' in cwd.parts:
                    raise ValueError('Task working directory escapes the repository')
                if not step['argv'] or not all(isinstance(x, str) and x for x in step['argv']):
                    raise ValueError('Task requires a nonempty argument vector')
    return profiles


PROFILES = load()


def canonical(name):
    if name in PROFILES:
        return name
    for repo, profile in PROFILES.items():
        if name == profile['directory']:
            return repo
    raise ValueError('Unknown devhost repository')


def from_directory(cwd, root):
    try:
        directory = Path(cwd).absolute().relative_to(Path(root) / 'repos').parts[0]
    except (ValueError, IndexError):
        return None
    return canonical(directory)


def run_profile(repo, action, base, home):
    profile = PROFILES[canonical(repo)]
    venv = home / 'venvs' / canonical(repo)
    if action == 'prepare' and profile.get('python'):
        subprocess.run(['uv', 'venv', '--allow-existing', '--python', profile['python'], str(venv)], check=True)
    steps = profile[action]
    if action == 'check' and not steps:
        print('Native checks run in CI: ' + ', '.join(profile['ci_platforms']), file=sys.stderr)
        return 78
    for step in steps:
        cwd = base / step['cwd']
        if not cwd.resolve().is_relative_to(base.resolve()):
            raise ValueError('Task working directory escapes the repository')
        argv = [x.replace('{venv}', str(venv)) for x in step['argv']]
        print(f"{repo}: {action} / {step['name']}", flush=True)
        result = subprocess.run(argv, cwd=cwd)
        if result.returncode:
            return result.returncode
    return 0


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('prepare', 'check'))
    parser.add_argument('--repo', required=True)
    parser.add_argument('--base', type=Path, required=True)
    args = parser.parse_args()
    try:
        raise SystemExit(run_profile(args.repo, args.action, args.base,
                                     Path(os.environ.get('DEVHOST_HOME', '/workspace/devhost'))))
    except (ValueError, subprocess.CalledProcessError) as exc:
        print(str(exc), file=sys.stderr)
        raise SystemExit(64)
