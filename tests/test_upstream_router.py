import json
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from app.upstream_router import ProbeResult, UpstreamProbeError, UpstreamRouter


class Server:
    def __init__(self, status=200, body=None):
        self.status = status
        self.body = body if body is not None else {"code": 0, "data": []}
        self.count = 0
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                outer.count += 1
                outer.path = self.path
                outer.key = self.headers.get("x-api-key")
                encoded = json.dumps(outer.body).encode()
                self.send_response(outer.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def log_message(self, *_args):
                pass

        self.http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.http.server_port}"

    def close(self):
        self.http.shutdown()
        self.thread.join(timeout=2)
        self.http.server_close()


class UpstreamRouterTests(unittest.TestCase):
    def settings(self, verify="", public=""):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        config = Path(temp.name) / "connection.json"
        config.write_text(json.dumps({"verify_base_url": verify, "base_url": public}))
        return SimpleNamespace(sub2api_config_path=str(config), sub2api_verify_base_url="", sub2api_base_url="")

    def test_v0214_envelope_uses_admin_api_key_and_sets_runtime_route(self):
        server = Server()
        self.addCleanup(server.close)
        router = UpstreamRouter(self.settings(verify=server.url))
        result = router.authenticate("fixture-admin-key", fresh=True)
        self.assertEqual(result.state, "ok")
        self.assertEqual(router.active_url(), server.url)
        self.assertEqual(server.path, "/api/v1/admin/groups/all")
        self.assertEqual(server.key, "fixture-admin-key")
        self.assertEqual(router.status()["endpoint"], "verify")

    def test_404_falls_back_to_public_route_but_auth_failure_does_not(self):
        verify, public = Server(404, {"message": "not found"}), Server()
        self.addCleanup(verify.close)
        self.addCleanup(public.close)
        router = UpstreamRouter(self.settings(verify.url, public.url))
        router.authenticate("fixture-admin-key", fresh=True)
        self.assertEqual(router.active_url(), public.url)
        self.assertEqual(router.status()["endpoint"], "public")
        router.authenticate("fixture-admin-key")
        self.assertEqual((verify.count, public.count), (1, 1))

        verify2, public2 = Server(403, {"message": "forbidden"}), Server()
        self.addCleanup(verify2.close)
        self.addCleanup(public2.close)
        router2 = UpstreamRouter(self.settings(verify2.url, public2.url))
        with self.assertRaises(UpstreamProbeError) as caught:
            router2.authenticate("fixture-admin-key", fresh=True)
        self.assertEqual(caught.exception.result.state, "auth_rejected")
        self.assertIsNone(getattr(public2, "key", None))

    def test_protocol_mismatch_is_not_hidden_by_a_second_endpoint(self):
        verify, public = Server(200, {"code": 0, "data": {}}), Server()
        self.addCleanup(verify.close)
        self.addCleanup(public.close)
        router = UpstreamRouter(self.settings(verify.url, public.url))
        with self.assertRaises(UpstreamProbeError) as caught:
            router.authenticate("fixture-admin-key", fresh=True)
        self.assertEqual(caught.exception.result.state, "protocol_mismatch")
        self.assertIsNone(getattr(public, "key", None))

    def test_explicit_upstream_error_does_not_try_public_endpoint(self):
        verify, public = Server(200, {"code": 1, "message": "fixture error"}), Server()
        self.addCleanup(verify.close)
        self.addCleanup(public.close)
        router = UpstreamRouter(self.settings(verify.url, public.url))
        with self.assertRaises(UpstreamProbeError) as caught:
            router.authenticate("fixture-key", fresh=True)
        self.assertEqual(caught.exception.result.state, "upstream_error")
        self.assertEqual(public.count, 0)

    def test_connection_status_has_no_address_or_key(self):
        router = UpstreamRouter(self.settings())
        status = router.status()
        self.assertEqual(status["state"], "network_unreachable")
        self.assertNotIn("key", json.dumps(status).lower())
        self.assertNotIn("address", json.dumps(status).lower())

    def test_retryable_failures_back_off_but_manual_check_can_bypass(self):
        router = UpstreamRouter(self.settings(verify="http://127.0.0.1:1"))
        failure = ProbeResult("network_unreachable", "verify", message="无法连接 Sub2API 上游", retryable=True)
        with patch.object(UpstreamRouter, "_probe", return_value=failure) as probe:
            with self.assertRaises(UpstreamProbeError):
                router.authenticate("fixture-admin-key")
            with self.assertRaises(UpstreamProbeError):
                router.authenticate("fixture-admin-key")
            self.assertEqual(probe.call_count, 1)
            with self.assertRaises(UpstreamProbeError):
                router.authenticate("fixture-admin-key", fresh=True)
            self.assertEqual(probe.call_count, 2)

    def test_invalid_key_clears_cached_success_and_does_not_report_ok(self):
        router = UpstreamRouter(self.settings(verify="http://127.0.0.1:1"))
        with patch.object(UpstreamRouter, "_probe", return_value=ProbeResult("ok", "verify", 200)):
            router.authenticate("fixture-key")
        with self.assertRaises(UpstreamProbeError):
            router.authenticate("")
        self.assertEqual(router.status()["state"], "auth_rejected")
        self.assertEqual(router._auth, {})

    def test_network_fallback_and_legacy_array_are_supported(self):
        public = Server(body=[])
        self.addCleanup(public.close)
        router = UpstreamRouter(self.settings("http://127.0.0.1:1", public.url))
        self.assertEqual(router.authenticate("fixture-key").state, "ok")
        self.assertEqual(router.active_url(), public.url)
        self.assertEqual(public.count, 1)

    def test_concurrent_reads_share_probe_and_rejected_auth_drops_cache(self):
        from concurrent.futures import ThreadPoolExecutor
        router = UpstreamRouter(self.settings(verify="http://127.0.0.1:1"))
        with patch.object(UpstreamRouter, "_probe", return_value=ProbeResult("ok", "verify", 200)) as probe:
            with ThreadPoolExecutor(max_workers=4) as pool:
                results = list(pool.map(lambda _: router.authenticate("fixture-key"), range(8)))
            self.assertEqual(probe.call_count, 1)
            self.assertTrue(all(r.state == "ok" for r in results))
        with patch.object(UpstreamRouter, "_probe", return_value=ProbeResult("auth_rejected", "verify", 401)):
            with self.assertRaises(UpstreamProbeError):
                router.authenticate("fixture-key", fresh=True)
        self.assertEqual(router._auth, {})

    def test_envelope_code_requires_integer_zero(self):
        self.assertFalse(UpstreamRouter._valid_response({"code": False, "data": []}))
        self.assertFalse(UpstreamRouter._valid_response({"code": "0", "data": []}))


if __name__ == "__main__":
    unittest.main()
