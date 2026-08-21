"""Tests for the bounded, credential-free Atlas RPC transcript exporter.

Covers the reviewed provider contract (authenticated production contract,
legacy RPC_PROVIDER_URLS, deterministic precedence), secret handling and
redaction, a deterministic local HTTP server fixture (no external Internet),
and the bounded block-span behavior.
"""

import json
import subprocess
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
EXPORTER_DIR = REPO_ROOT / "atlas-observer" / "scripts"
sys.path.insert(0, str(EXPORTER_DIR))

import export_rpc_transcript as exporter  # noqa: E402

SENTINEL = "PHOENIX_TEST_SECRET_DO_NOT_LEAK_123"
AUTH_ENV = {
    "RPC_AUTHORITY_MODE": "single_primary",
    "RPC_AUTH_PROVIDER_ID": "production-nownodes-arbitrum",
    "RPC_AUTH_PROVIDER_URL": "https://reviewed.example.invalid/rpc",
    "RPC_AUTH_PROVIDER_PRIORITY": "100",
    "RPC_AUTH_PROVIDER_HEADER_NAME": "X-Reviewed-Api-Key",
    "RPC_AUTH_PROVIDER_HEADER_FILE": "/run/secrets/test-api-key",
}


class _CannedRun:
    def __init__(self, returncode: int = 0, stdout: bytes = b"", stderr: bytes = b""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class _SilentHandler(BaseHTTPRequestHandler):
    requests: list[dict] = []
    mode = "ok"

    def log_message(self, *args):  # Keep the server quiet.
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        self.__class__.requests.append(
            {"path": self.path, "headers": dict(self.headers), "body": body}
        )
        if self.__class__.mode == "timeout":
            import time

            time.sleep(5)
            return
        if self.__class__.mode == "status_401":
            self.send_response(401)
            self.end_headers()
            return
        if self.__class__.mode == "status_403":
            self.send_response(403)
            self.end_headers()
            return
        if self.__class__.mode == "status_429":
            self.send_response(429)
            self.end_headers()
            return
        if self.__class__.mode == "status_500":
            self.send_response(500)
            self.end_headers()
            return
        if self.__class__.mode == "invalid_json":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"this is not json")
            return
        if self.__class__.mode == "wrong_id":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(
                b'{"jsonrpc":"2.0","id":999,"result":"0x1"}'
            )
            return
        if self.__class__.mode == "missing_result":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"jsonrpc":"2.0","id":1}')
            return
        if self.__class__.mode == "rpc_error":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(
                b'{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"boom"}}'
            )
            return
        request = json.loads(body)
        method = request.get("method")
        result = "0xa4b1"
        if method == "eth_blockNumber":
            result = "0x64"
        elif method == "eth_getLogs":
            result = []
        elif method in ("eth_getTransactionByHash", "eth_getTransactionReceipt"):
            result = {}
        payload = json.dumps({"jsonrpc": "2.0", "id": request.get("id"), "result": result})
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(payload.encode())


class _FakeServer:
    def __enter__(self):
        _SilentHandler.requests = []
        _SilentHandler.mode = "ok"
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), _SilentHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        host, port = self.server.server_address
        self.url = f"http://{host}:{port}/rpc"
        return self

    def __exit__(self, *args):
        self.server.shutdown()
        self.server.server_close()


class ProviderContractTests(unittest.TestCase):
    def test_legacy_provider_urls_still_work(self):
        providers = exporter._legacy_providers(
            {"RPC_PROVIDER_URLS": "https://one.example/rpc,https://two.example/rpc"}
        )
        self.assertEqual(
            [(p.label, p.url) for p in providers],
            [
                ("provider_0", "https://one.example/rpc"),
                ("provider_1", "https://two.example/rpc"),
            ],
        )
        self.assertIsNone(providers[0].header_name)
        self.assertIsNone(providers[0].header_value)

    def test_legacy_json_list_still_works(self):
        providers = exporter._legacy_providers(
            {"RPC_PROVIDER_URLS": json.dumps(["http://a.example", "http://b.example"])}
        )
        self.assertEqual(len(providers), 2)

    def test_legacy_invalid_shape_fails_closed(self):
        with self.assertRaises(RuntimeError):
            exporter._legacy_providers({"RPC_PROVIDER_URLS": "ftp://bad.example"})

    def test_authenticated_provider_contract_loads(self):
        seen = {}

        def fake_secret(container, path):
            seen["path"] = path
            return SENTINEL

        environment = dict(AUTH_ENV)
        environment["RPC_AUTH_PROVIDER_URL"] = "https://reviewed.example/rpc"
        original = exporter.read_container_secret
        exporter.read_container_secret = fake_secret
        try:
            provider = exporter._authenticated_provider("gateway", environment)
        finally:
            exporter.read_container_secret = original
        self.assertEqual(provider.label, "production-nownodes-arbitrum")
        self.assertEqual(provider.header_name, "X-Reviewed-Api-Key")
        self.assertEqual(provider.header_value, SENTINEL)
        self.assertEqual(seen["path"], "/run/secrets/test-api-key")

    def test_precedence_auth_wins_over_legacy(self):
        environment = dict(AUTH_ENV)
        environment["RPC_PROVIDER_URLS"] = "https://legacy.example/rpc"
        original_env = exporter.container_environment
        original_secret = exporter.read_container_secret
        exporter.container_environment = lambda container: environment
        exporter.read_container_secret = lambda container, path: SENTINEL
        try:
            providers = exporter.load_reviewed_providers("gateway")
        finally:
            exporter.container_environment = original_env
            exporter.read_container_secret = original_secret
        self.assertEqual(len(providers), 1)
        self.assertIsNotNone(providers[0].header_value)

    def test_no_configuration_fails_closed(self):
        original_env = exporter.container_environment
        exporter.container_environment = lambda container: {}
        try:
            with self.assertRaisesRegex(RuntimeError, "no reviewed RPC provider"):
                exporter.load_reviewed_providers("gateway")
        finally:
            exporter.container_environment = original_env

    def test_identity_mismatch_fails_closed(self):
        environment = dict(AUTH_ENV)
        environment["RPC_AUTH_PROVIDER_ID"] = "other-identity"
        with self.assertRaisesRegex(RuntimeError, "identity"):
            exporter._authenticated_provider("gateway", environment)

    def test_invalid_url_schemes_fail_closed(self):
        for url in (
            "file:///etc/passwd",
            "ftp://example/rpc",
            "javascript:alert(1)",
            "unix:///var/run/rpc.sock",
            "data:text/plain,x",
            "not-a-url",
            "http://",
            "https:///nohost",
            "http://@/",
            "//example.com",
        ):
            environment = dict(AUTH_ENV)
            environment["RPC_AUTH_PROVIDER_URL"] = url
            with self.assertRaises(RuntimeError, msg=f"url accepted: {url}"):
                exporter._authenticated_provider("gateway", environment)

    def test_invalid_header_name_fails_closed(self):
        for name in ("", "has space", "x" * 201, "bad\nname", "ümlaut"):
            environment = dict(AUTH_ENV)
            environment["RPC_AUTH_PROVIDER_HEADER_NAME"] = name
            with self.assertRaises(RuntimeError, msg=f"header accepted: {name!r}"):
                exporter._authenticated_provider("gateway", environment)

    def test_invalid_priority_fails_closed(self):
        for priority in ("0", "-1", "not-a-number", str(2**32)):
            environment = dict(AUTH_ENV)
            environment["RPC_AUTH_PROVIDER_PRIORITY"] = priority
            with self.assertRaises(RuntimeError, msg=f"priority accepted: {priority}"):
                exporter._authenticated_provider("gateway", environment)

    def test_relative_secret_path_fails_closed(self):
        environment = dict(AUTH_ENV)
        environment["RPC_AUTH_PROVIDER_HEADER_FILE"] = "run/secrets/api-key"
        with self.assertRaisesRegex(RuntimeError, "unsafe"):
            exporter._authenticated_provider("gateway", environment)

    def test_secret_reader_variants(self):
        original = subprocess.run
        try:
            subprocess.run = lambda *a, **k: _CannedRun(1)
            with self.assertRaisesRegex(RuntimeError, "unavailable"):
                exporter.read_container_secret("gateway", "/run/secrets/k")
            subprocess.run = lambda *a, **k: _CannedRun(0, b"")
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                exporter.read_container_secret("gateway", "/run/secrets/k")
            subprocess.run = lambda *a, **k: _CannedRun(0, b"x" * 4097)
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                exporter.read_container_secret("gateway", "/run/secrets/k")
            subprocess.run = lambda *a, **k: _CannedRun(0, b"a\rb")
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                exporter.read_container_secret("gateway", "/run/secrets/k")
            subprocess.run = lambda *a, **k: _CannedRun(0, b"a\nb")
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                exporter.read_container_secret("gateway", "/run/secrets/k")
            subprocess.run = lambda *a, **k: _CannedRun(0, b"\xff\xfe")
            with self.assertRaisesRegex(RuntimeError, "invalid"):
                exporter.read_container_secret("gateway", "/run/secrets/k")
            subprocess.run = lambda *a, **k: _CannedRun(0, b"ok-secret")
            self.assertEqual(
                exporter.read_container_secret("gateway", "/run/secrets/k"), "ok-secret"
            )
        finally:
            subprocess.run = original

    def test_block_span_enforcement(self):
        self.assertEqual(exporter.plan_block_bounds(10, 30, 100), 30)
        self.assertEqual(exporter.plan_block_bounds(10, "latest", 100), 100)
        with self.assertRaises(RuntimeError):
            exporter.plan_block_bounds(0, 20_001, 100_000)
        with self.assertRaises(RuntimeError):
            exporter.plan_block_bounds(100, 50, 100)
        with self.assertRaises(RuntimeError):
            exporter.plan_block_bounds(100, 200, 150)


class FakeServerTests(unittest.TestCase):
    def provider_for(self, url):
        return exporter.ReviewedProvider(
            label="production-nownodes-arbitrum",
            url=url,
            header_name="X-Reviewed-Api-Key",
            header_value=SENTINEL,
        )

    def test_authenticated_request_reaches_server(self):
        with _FakeServer() as server:
            rpc = exporter.BoundedRPC([self.provider_for(server.url)])
            self.assertEqual(rpc.call("eth_chainId", []), "0xa4b1")
            request = _SilentHandler.requests[0]
            self.assertEqual(request["headers"].get("X-Reviewed-Api-Key"), SENTINEL)
            body = json.loads(request["body"])
            self.assertEqual(body["jsonrpc"], "2.0")
            self.assertEqual(body["method"], "eth_chainId")
            self.assertEqual(body["params"], [])

    def test_rpc_error_and_missing_result_are_rpc_errors(self):
        with _FakeServer() as server:
            _SilentHandler.mode = "rpc_error"
            rpc = exporter.BoundedRPC([self.provider_for(server.url)])
            with self.assertRaisesRegex(RuntimeError, "=rpc_error"):
                rpc.call("eth_chainId", [])
        with _FakeServer() as server:
            _SilentHandler.mode = "missing_result"
            rpc = exporter.BoundedRPC([self.provider_for(server.url)])
            with self.assertRaisesRegex(RuntimeError, "=rpc_error"):
                rpc.call("eth_chainId", [])

    def test_http_error_statuses_are_transport_errors(self):
        for mode in ("status_401", "status_403", "status_429", "status_500"):
            with _FakeServer() as server:
                _SilentHandler.mode = mode
                rpc = exporter.BoundedRPC([self.provider_for(server.url)])
                with self.assertRaisesRegex(RuntimeError, "=transport_error"):
                    rpc.call("eth_chainId", [])

    def test_invalid_json_and_timeout_are_transport_errors(self):
        with _FakeServer() as server:
            _SilentHandler.mode = "invalid_json"
            rpc = exporter.BoundedRPC([self.provider_for(server.url)])
            with self.assertRaisesRegex(RuntimeError, "=transport_error"):
                rpc.call("eth_chainId", [])
        original_timeout = exporter.RPC_TIMEOUT_SECONDS
        exporter.RPC_TIMEOUT_SECONDS = 0.2
        try:
            with _FakeServer() as server:
                _SilentHandler.mode = "timeout"
                rpc = exporter.BoundedRPC([self.provider_for(server.url)])
                with self.assertRaisesRegex(RuntimeError, "=transport_error"):
                    rpc.call("eth_chainId", [])
        finally:
            exporter.RPC_TIMEOUT_SECONDS = original_timeout

    def test_mismatched_response_id_keeps_existing_semantics(self):
        # The reviewed exporter does not validate JSON-RPC response ids; it
        # accepts the result. This pins the existing semantic so future
        # changes are deliberate.
        with _FakeServer() as server:
            _SilentHandler.mode = "wrong_id"
            rpc = exporter.BoundedRPC([self.provider_for(server.url)])
            self.assertEqual(rpc.call("eth_chainId", []), "0x1")


class RedactionTests(unittest.TestCase):
    def assert_sentinel_absent(self, captured: str):
        self.assertNotIn(SENTINEL, captured)
        self.assertNotIn("reviewed.example.invalid", captured)

    def test_sentinel_absent_from_transport_failures(self):
        with _FakeServer() as server:
            for mode in ("status_401", "status_500", "invalid_json", "rpc_error"):
                _SilentHandler.mode = mode
                rpc = exporter.BoundedRPC(
                    [
                        exporter.ReviewedProvider(
                            label="production-nownodes-arbitrum",
                            url=server.url,
                            header_name="X-Reviewed-Api-Key",
                            header_value=SENTINEL,
                        )
                    ]
                )
                with self.assertRaises(RuntimeError) as caught:
                    rpc.call("eth_chainId", [])
                self.assert_sentinel_absent(str(caught.exception))

    def test_sentinel_absent_from_config_errors(self):
        environment = dict(AUTH_ENV)
        environment["RPC_AUTH_PROVIDER_URL"] = "https://reviewed.example.invalid/rpc"
        for mutate in (
            lambda env: env.update({"RPC_AUTH_PROVIDER_HEADER_NAME": ""}),
            lambda env: env.update({"RPC_AUTH_PROVIDER_PRIORITY": "0"}),
            lambda env: env.update({"RPC_AUTH_PROVIDER_ID": "bad-identity"}),
            lambda env: env.update({"RPC_AUTH_PROVIDER_HEADER_FILE": "relative"}),
        ):
            broken = dict(environment)
            mutate(broken)
            with self.assertRaises(RuntimeError) as caught:
                exporter._authenticated_provider("gateway", broken)
            self.assert_sentinel_absent(str(caught.exception))

    def test_sentinel_absent_from_secret_reader_errors(self):
        original = subprocess.run
        try:
            subprocess.run = lambda *a, **k: _CannedRun(0, b"x" * 4097)
            with self.assertRaises(RuntimeError) as caught:
                exporter.read_container_secret("gateway", "/run/secrets/k")
            self.assert_sentinel_absent(str(caught.exception))
            subprocess.run = lambda *a, **k: _CannedRun(1, b"", SENTINEL.encode())
            with self.assertRaises(RuntimeError) as caught:
                exporter.read_container_secret("gateway", "/run/secrets/k")
            self.assert_sentinel_absent(str(caught.exception))
        finally:
            subprocess.run = original


class EndToEndTranscriptTests(unittest.TestCase):
    def test_transcript_generation_against_fake_server(self):
        with _FakeServer() as server:
            environment = dict(AUTH_ENV)
            environment["RPC_AUTH_PROVIDER_URL"] = server.url
            original_env = exporter.container_environment
            original_secret = exporter.read_container_secret
            exporter.container_environment = lambda container: environment
            exporter.read_container_secret = lambda container, path: SENTINEL
            try:
                rpc = exporter.BoundedRPC(exporter.load_reviewed_providers("gateway"))
                self.assertEqual(rpc.call("eth_chainId", []), "0xa4b1")
                self.assertEqual(rpc.call("eth_blockNumber", []), "0x64")
                logs = rpc.call(
                    "eth_getLogs",
                    [
                        {
                            "address": exporter.ATLAS,
                            "fromBlock": "0x1",
                            "toBlock": "0x64",
                            "topics": [[exporter.SOLVER_RESULT_TOPIC]],
                        }
                    ],
                )
                self.assertEqual(logs, [])
            finally:
                exporter.container_environment = original_env
                exporter.read_container_secret = original_secret


if __name__ == "__main__":
    unittest.main()
