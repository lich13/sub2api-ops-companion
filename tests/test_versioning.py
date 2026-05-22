from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from app.versioning import UpdateError, perform_update


def run_git(workdir: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(workdir), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


class VersioningUpdateTests(unittest.TestCase):
    def make_repo_pair(self) -> tuple[tempfile.TemporaryDirectory[str], Path, Path]:
        tmp = tempfile.TemporaryDirectory()
        root = Path(tmp.name)
        origin = root / "origin"
        workdir = root / "workdir"
        origin.mkdir()
        run_git(origin, "init", "-b", "main")
        run_git(origin, "config", "user.email", "ops@example.test")
        run_git(origin, "config", "user.name", "Ops Test")
        write(origin / "requirements.txt", "fastapi==0.115.12\n")
        write(origin / "app" / "main.py", "VERSION = 'one'\n")
        run_git(origin, "add", ".")
        run_git(origin, "commit", "-m", "initial")
        subprocess.run(["git", "clone", str(origin), str(workdir)], check=True, capture_output=True, text=True)
        return tmp, origin, workdir

    def settings_for(self, workdir: Path) -> SimpleNamespace:
        return SimpleNamespace(update_enabled=True, update_workdir=str(workdir), update_branch="main")

    def test_perform_update_rejects_dependency_changes_before_resetting(self) -> None:
        tmp, origin, workdir = self.make_repo_pair()
        self.addCleanup(tmp.cleanup)
        before = run_git(workdir, "rev-parse", "HEAD")
        write(origin / "requirements.txt", "fastapi==0.115.12\nquickjs==1.19.4\n")
        run_git(origin, "add", "requirements.txt")
        run_git(origin, "commit", "-m", "add quickjs")

        with self.assertRaises(UpdateError) as raised:
            perform_update(self.settings_for(workdir))

        self.assertIn("docker compose up -d --build", str(raised.exception))
        self.assertEqual(run_git(workdir, "rev-parse", "HEAD"), before)

    def test_perform_update_allows_source_only_changes(self) -> None:
        tmp, origin, workdir = self.make_repo_pair()
        self.addCleanup(tmp.cleanup)
        write(origin / "app" / "main.py", "VERSION = 'two'\n")
        run_git(origin, "add", "app/main.py")
        run_git(origin, "commit", "-m", "update app")

        result = perform_update(self.settings_for(workdir))

        self.assertTrue(result["need_restart"])
        self.assertEqual(result["after_commit"], run_git(origin, "rev-parse", "HEAD"))
