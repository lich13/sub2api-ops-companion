from __future__ import annotations

import copy
import json
import tempfile
import threading
import unittest
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from fastapi import HTTPException

from app.account_templates import AccountTemplates, TemplateApplication, account_version
from app.capacity_alerts import CapacityAlertStore
from app.model_detection import DEFAULT_MODEL, TARGET, ModelDetection


NOW = datetime(2026, 10, 6, 3, 0, tzinfo=timezone.utc)
ADMIN_TOKEN = "fixture-admin-token"


class _Clock:
    def __call__(self):
        return NOW


class _AdminState:
    def admin_token(self):
        return ADMIN_TOKEN


class _FixtureDB:
    def __init__(self):
        self.row = {
            "id": 1,
            "name": "fixture-account",
            "platform": "openai",
            "type": "apikey",
            "status": "active",
            "schedulable": True,
            "deleted_at": None,
            "parent_account_id": None,
            "group_ids": [],
            "model_catalog_version": "fixture-catalog",
            "model_mapping": {"fixture-old": "fixture-old"},
            "model_mapping_version": "fixture-model-mapping",
            "credential_version": "fixture-credential",
            "passthrough": False,
            "extra": {"fixture": True},
            "proxy_id": None,
            "credentials": {"api_key": "fixture-key", "base_url": "https://example.invalid"},
        }
        self.lock = threading.RLock()

    def fetch_one(self, _sql, _params):
        with self.lock:
            return copy.deepcopy(self.row)

    def fetch_all(self, _sql, _params=None):
        with self.lock:
            return [copy.deepcopy(self.row)]


class _LoopbackHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format, *_args):
        return

    def _reply(self, status, body):
        raw = json.dumps(body, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(raw)

    def do_POST(self):
        state = self.server.state
        length = int(self.headers.get("Content-Length", "0"))
        try:
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            self._reply(400, {"code": 1})
            return
        state.requests.append({
            "path": self.path,
            "token": self.headers.get("x-api-key"),
            "body": copy.deepcopy(payload),
        })
        if self.headers.get("x-api-key") != ADMIN_TOKEN:
            self._reply(403, {"code": 1})
            return
        if self.path == "/api/v1/admin/accounts/1/schedulable":
            requested = payload.get("schedulable")
            if type(requested) is not bool:
                self._reply(422, {"code": 1})
                return
            with state.db.lock:
                state.db.row["schedulable"] = requested
            self._reply(200, {"code": 0, "data": {"id": 1, "schedulable": requested}})
            return
        if self.path == "/api/v1/admin/accounts/bulk-update":
            account_ids = payload.get("account_ids")
            credentials = payload.get("credentials")
            mapping = credentials.get("model_mapping") if isinstance(credentials, dict) else None
            if account_ids != [1] or not isinstance(mapping, dict):
                self._reply(422, {"code": 1})
                return
            with state.db.lock:
                state.db.row["model_mapping"] = copy.deepcopy(mapping)
            self._reply(200, {"code": 0, "data": {"updated": [1]}})
            return
        self._reply(404, {"code": 1})


class _LoopbackHTTP:
    def __init__(self, db):
        self.db = db
        self.requests = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), _LoopbackHandler)
        self.server.state = self
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class DetectionDispositionTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.clock = _Clock()
        self.db = _FixtureDB()
        self.http = _LoopbackHTTP(self.db)
        self.addCleanup(self.http.close)
        self.capacity_path = root / "capacity-alert-state.json"
        self.capacity = SimpleNamespace(store=CapacityAlertStore(self.capacity_path))
        self.actions = SimpleNamespace(
            account=AsyncMock(side_effect=lambda _aid: self.db.fetch_one(None, None)),
            model_allowed=AsyncMock(return_value=True),
        )
        self.settings = SimpleNamespace(
            usage_query_state_path=str(root / "usage.json"),
            audit_path=str(root / "audit.jsonl"),
        )
        self.service = SimpleNamespace(
            invalidate=Mock(),
            actions=self.actions,
            r=SimpleNamespace(
                db=self.db,
                settings=self.settings,
                capacity_alerts=self.capacity,
                oauth_state_store=lambda: _AdminState(),
                oauth_base_url=lambda: self.http.base_url,
                fingerprint_bank=SimpleNamespace(capture=Mock(return_value=({"models": [{"id": TARGET}]}, "fixture-bank"))),
            ),
        )
        self.detection = ModelDetection(self.service, clock=self.clock)

    def _job(self, job_id="fixture-detection-job"):
        return {
            "id": job_id,
            "account_id": 1,
            "requested_model": DEFAULT_MODEL,
            "detection_generation": self.detection.control(1)["generation"],
            "detection_mark_version": self.detection.mark(1)["version"],
        }

    async def test_first_group_verdict_persists_mark_and_confirms_schedulable_http(self):
        result = await self.detection.verdict(self._job(), {"prediction": TARGET, "used_outputs": 1})

        self.assertEqual(result["status"], "completed")
        self.assertTrue(result["marked"])
        self.assertTrue(result["schedule_verified"])
        self.assertFalse(self.db.row["schedulable"])
        self.assertEqual(len(self.http.requests), 1)
        request = self.http.requests[0]
        self.assertEqual(request["path"], "/api/v1/admin/accounts/1/schedulable")
        self.assertEqual(request["token"], ADMIN_TOKEN)
        self.assertEqual(request["body"], {"schedulable": False})

        persisted = ModelDetection(self.service, clock=self.clock).control(1)
        self.assertEqual(persisted["disposition"]["status"], "completed")
        self.assertTrue(persisted["hold"])
        mark = CapacityAlertStore(self.capacity_path).snapshot()["marks"]["1"]
        self.assertTrue(mark["marked"])
        self.assertEqual(mark["detection_job_id"], "fixture-detection-job")

    async def test_restart_with_writing_disposition_marks_unknown_without_replay(self):
        initial_mark = self.detection.mark(1)
        self.capacity.store.set_mark(1, True, initial_mark["version"], self.clock(), detection_job_id="fixture-writing-job")
        mark_version = self.detection.mark(1)["version"]
        self.detection.update(
            1,
            hold=True,
            status="paused",
            disposition={
                "status": "writing",
                "job_id": "fixture-writing-job",
                "generation": 0,
                "mark_version": mark_version,
                "marked": True,
                "schedule_verified": False,
                "created_at": NOW.isoformat(),
            },
        )

        restarted = ModelDetection(self.service, clock=self.clock)
        outcome = restarted.finish_disposition(1)

        self.assertEqual(outcome["status"], "needs_confirmation")
        self.assertIn("未重复写入", outcome["reason"])
        self.assertEqual(self.http.requests, [])
        self.assertTrue(restarted.control(1)["hold"])

    async def test_manual_generation_change_rejects_stale_verdict_without_disposition(self):
        job = self._job()
        self.detection.human_control(1, release_hold=True)

        with self.assertRaises(HTTPException) as raised:
            await self.detection.verdict(job, {"prediction": TARGET, "used_outputs": 1})

        self.assertEqual(raised.exception.status_code, 409)
        self.assertIn("新的人工操作", str(raised.exception.detail))
        self.assertNotIn("disposition", self.detection.control(1))
        self.assertEqual(self.capacity.store.snapshot()["marks"], {})
        self.assertEqual(self.http.requests, [])

    async def test_policy_save_failure_leaves_mark_and_schedule_untouched(self):
        with patch("app.policy_store.write_json", side_effect=OSError("fixture save failure")):
            with self.assertRaisesRegex(OSError, "fixture save failure"):
                await self.detection.verdict(self._job(), {"prediction": TARGET, "used_outputs": 1})

        self.assertFalse(self.detection.store.path.exists())
        self.assertEqual(self.capacity.store.snapshot()["marks"], {})
        self.assertTrue(self.db.row["schedulable"])
        self.assertEqual(self.http.requests, [])

    async def test_template_apply_merges_only_model_mapping_over_loopback_http(self):
        templates = AccountTemplates(self.service)
        with templates.store.transaction() as data:
            data.update(
                configured=True,
                templates={
                    "full": {"whitelist": ["fixture-full"], "mappings": []},
                    "degraded": {
                        "whitelist": ["fixture-model"],
                        "mappings": [{"source": "fixture-input-*", "target": "fixture-model"}],
                    },
                    "takeover": {"whitelist": [], "mappings": []},
                },
            )
        view = templates.view()
        payload = TemplateApplication(
            expected_version=account_version(self.db.row),
            template_id="degraded",
            template_version=view["version"],
        )
        before = copy.deepcopy(self.db.row)

        result = templates.apply(1, payload, ADMIN_TOKEN)

        self.assertTrue(result["verified"])
        self.assertEqual(self.db.row["model_mapping"], {"fixture-model": "fixture-model", "fixture-input-*": "fixture-model"})
        self.assertEqual(len(self.http.requests), 1)
        self.assertEqual(self.http.requests[0]["path"], "/api/v1/admin/accounts/bulk-update")
        for key, value in before.items():
            if key != "model_mapping":
                self.assertEqual(self.db.row[key], value)


if __name__ == "__main__":
    unittest.main()
