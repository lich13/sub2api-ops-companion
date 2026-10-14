#!/usr/bin/env python3
"""Run the retired service's regression suite from its immutable Git revision."""
from __future__ import annotations

import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlsplit

import psycopg

from server_http_contract import REFERENCE_SHA, ROOT, TemporaryPostgres


def reference_database(postgres: TemporaryPostgres) -> str:
    # The unchanged historical integration tests require this database name
    # and reject URL query overrides. It still belongs to our disposable cluster.
    with psycopg.connect(postgres.url, autocommit=True) as connection:
        connection.execute("CREATE DATABASE sub2ops_qa")
    return urlsplit(postgres.url)._replace(path="/sub2ops_qa", query="", fragment="").geturl()


def main() -> int:
    archive = subprocess.run(
        ["git", "archive", REFERENCE_SHA], cwd=ROOT, capture_output=True, check=True
    ).stdout
    with tempfile.TemporaryDirectory(prefix="sub2ops-reference-") as directory:
        reference = Path(directory)
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as bundle:
            bundle.extractall(reference, filter="data")
        env = os.environ.copy()
        for key in tuple(env):
            if any(word in key.upper() for word in ("TOKEN", "SECRET", "PASSWORD", "COOKIE", "API_KEY", "PRIVATE_KEY")):
                env.pop(key, None)
            elif key.startswith(("DATABASE_", "SUB2API_", "OAUTH_", "BARK_", "KEY_FALLBACK_", "USAGE_QUERY_", "AUDIT_")):
                env.pop(key, None)
        env.update(PYTHONDONTWRITEBYTECODE="1", PYTHONPATH=str(reference))
        postgres = TemporaryPostgres(reference)
        try:
            postgres.start()
            env["SUB2OPS_TEST_DATABASE_URL"] = reference_database(postgres)
            result = subprocess.run(
                [sys.executable, "-m", "unittest", "discover", "-s", "tests", "-q"],
                cwd=reference, env=env, capture_output=True, text=True,
            )
        finally:
            postgres.close()
        output = (result.stdout + result.stderr).replace(str(reference), "<reference>").replace(str(ROOT), "<repository>")
        output = re.sub(r"/(?:Users|home)/[^/\s]+", "/build", output)
        print(output, end="")
        print(f"Reference {REFERENCE_SHA}; exit {result.returncode}")
        return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
