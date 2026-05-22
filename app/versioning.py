from __future__ import annotations

import os
import re
import subprocess
import threading
import time
from fnmatch import fnmatch
from pathlib import Path
from typing import Any

APP_VERSION = "0.1.0"
REPO_SLUG = "lich13/sub2api-ops-companion"
REPO_WEB_URL = f"https://github.com/{REPO_SLUG}"
REBUILD_REQUIRED_PATTERNS = (
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
    "requirements*.txt",
    "pyproject.toml",
    "poetry.lock",
    "Pipfile",
    "Pipfile.lock",
    "setup.py",
    "setup.cfg",
    "uv.lock",
)


class UpdateError(RuntimeError):
    pass


def _run_git(args: list[str], workdir: Path, settings: Any, timeout: int = 20) -> str:
    env = os.environ.copy()
    result = subprocess.run(
        ["git", "-C", str(workdir), *args],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=timeout,
    )
    if result.returncode != 0:
        stderr = (result.stderr or result.stdout or "").strip()
        raise UpdateError(stderr or f"git {' '.join(args)} failed")
    return result.stdout.strip()


def _is_git_worktree(workdir: Path, settings: Any) -> bool:
    try:
        return _run_git(["rev-parse", "--is-inside-work-tree"], workdir, settings) == "true"
    except Exception:
        return False


def _short_commit(value: str) -> str:
    return value[:12] if value else ""


def _version_key(value: str) -> tuple[int, int, int, str]:
    match = re.match(r"^v?(\d+)\.(\d+)\.(\d+)(.*)$", value.strip())
    if not match:
        return (0, 0, 0, value)
    return (int(match.group(1)), int(match.group(2)), int(match.group(3)), match.group(4) or "")


def _clean_tag(value: str) -> str:
    return value.strip().removeprefix("refs/tags/").removeprefix("v")


def _latest_remote_tag(workdir: Path, settings: Any) -> str:
    try:
        output = _run_git(["ls-remote", "--tags", "origin", "refs/tags/v*"], workdir, settings, timeout=30)
    except UpdateError:
        return ""
    versions: list[str] = []
    for line in output.splitlines():
        parts = line.split()
        if len(parts) != 2 or parts[1].endswith("^{}"):
            continue
        version = _clean_tag(parts[1])
        if re.match(r"^\d+\.\d+\.\d+", version):
            versions.append(version)
    if not versions:
        return ""
    return sorted(versions, key=_version_key)[-1]


def _changed_files(workdir: Path, settings: Any, before: str, target: str) -> list[str]:
    output = _run_git(["diff", "--name-only", before, target], workdir, settings, timeout=30)
    return [line.strip() for line in output.splitlines() if line.strip()]


def _requires_rebuild(path: str) -> bool:
    normalized = path.lstrip("./")
    return any(fnmatch(normalized, pattern) for pattern in REBUILD_REQUIRED_PATTERNS)


def _reject_rebuild_required_update(workdir: Path, settings: Any, before: str, target: str) -> None:
    changed = _changed_files(workdir, settings, before, target)
    rebuild_files = [path for path in changed if _requires_rebuild(path)]
    if not rebuild_files:
        return
    preview = ", ".join(rebuild_files[:5])
    if len(rebuild_files) > 5:
        preview += f" 等 {len(rebuild_files)} 个文件"
    raise UpdateError(
        "本次更新包含依赖或容器构建文件变更"
        f"（{preview}）。为避免 Docker 旧镜像加载新源码导致 502，已取消面板内热更新；"
        f"请在云机 {workdir} 执行：git fetch --prune origin "
        f"{getattr(settings, 'update_branch', 'main') or 'main'} && "
        f"git reset --hard origin/{getattr(settings, 'update_branch', 'main') or 'main'} && "
        "docker compose up -d --build"
    )


def version_info(settings: Any, force: bool = False) -> dict[str, Any]:
    del force
    workdir = Path(getattr(settings, "update_workdir", "/workspace") or "/workspace")
    branch = str(getattr(settings, "update_branch", "main") or "main")
    info: dict[str, Any] = {
        "current_version": APP_VERSION,
        "latest_version": APP_VERSION,
        "has_update": False,
        "cached": False,
        "build_type": "source",
        "repo_url": REPO_WEB_URL,
        "branch": branch,
        "workdir": str(workdir),
        "current_commit": "",
        "current_commit_short": "",
        "latest_commit": "",
        "latest_commit_short": "",
        "update_supported": False,
        "release_info": {
            "name": f"Sub2API Ops Companion v{APP_VERSION}",
            "body": "",
            "published_at": "",
            "html_url": f"{REPO_WEB_URL}/releases",
        },
    }
    if not getattr(settings, "update_enabled", True):
        info["warning"] = "面板更新已关闭"
        return info
    if not workdir.exists():
        info["warning"] = f"更新工作目录不存在：{workdir}"
        return info
    if not _is_git_worktree(workdir, settings):
        info["warning"] = f"更新工作目录不是 Git 仓库：{workdir}"
        return info

    info["update_supported"] = True
    try:
        current_commit = _run_git(["rev-parse", "HEAD"], workdir, settings)
        latest_commit = _run_git(["ls-remote", "origin", f"refs/heads/{branch}"], workdir, settings, timeout=30)
        latest_commit = latest_commit.split()[0] if latest_commit else ""
        latest_tag = _latest_remote_tag(workdir, settings) or APP_VERSION
        has_version_update = _version_key(latest_tag) > _version_key(APP_VERSION)
        has_commit_update = bool(current_commit and latest_commit and current_commit != latest_commit)
        info.update(
            {
                "latest_version": latest_tag,
                "has_update": has_version_update or has_commit_update,
                "current_commit": current_commit,
                "current_commit_short": _short_commit(current_commit),
                "latest_commit": latest_commit,
                "latest_commit_short": _short_commit(latest_commit),
                "release_info": {
                    "name": f"Sub2API Ops Companion v{latest_tag}",
                    "body": "",
                    "published_at": "",
                    "html_url": f"{REPO_WEB_URL}/releases",
                },
            }
        )
    except Exception as exc:
        info["warning"] = str(exc)
    return info


def perform_update(settings: Any) -> dict[str, Any]:
    if not getattr(settings, "update_enabled", True):
        raise UpdateError("面板更新已关闭")
    workdir = Path(getattr(settings, "update_workdir", "/workspace") or "/workspace")
    branch = str(getattr(settings, "update_branch", "main") or "main")
    if not _is_git_worktree(workdir, settings):
        raise UpdateError(f"更新工作目录不是 Git 仓库：{workdir}")

    before = _run_git(["rev-parse", "HEAD"], workdir, settings)
    _run_git(["fetch", "--prune", "origin", branch], workdir, settings, timeout=90)
    target = _run_git(["rev-parse", f"origin/{branch}"], workdir, settings)
    if before == target:
        return {
            "message": "已经是最新版本",
            "need_restart": False,
            "before_commit": before,
            "after_commit": before,
            "after_commit_short": _short_commit(before),
        }
    _reject_rebuild_required_update(workdir, settings, before, target)
    _run_git(["reset", "--hard", f"origin/{branch}"], workdir, settings, timeout=60)
    after = _run_git(["rev-parse", "HEAD"], workdir, settings)
    return {
        "message": f"已更新到 {APP_VERSION} / {_short_commit(after)}，服务正在重启",
        "need_restart": True,
        "before_commit": before,
        "before_commit_short": _short_commit(before),
        "after_commit": after,
        "after_commit_short": _short_commit(after),
    }


def restart_process_soon(delay_seconds: float = 0.8) -> None:
    def _restart() -> None:
        time.sleep(delay_seconds)
        os._exit(0)

    threading.Thread(target=_restart, name="sub2ops-restart", daemon=True).start()
