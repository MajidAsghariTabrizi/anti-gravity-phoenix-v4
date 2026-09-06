import copy
import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "probe_long_tail_trace_providers.py"
SPEC = importlib.util.spec_from_file_location("probe_long_tail_trace_providers", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)
ALLOWLIST = ROOT / "fixtures" / "long-tail" / "immutable_events_20260801.json"


class FakeProvider:
    label = "reviewed-provider-1"

    def __init__(self, event, *, debug=True, replay=False, arbtrace=False):
        self.event = event
        self.debug = debug
        self.replay = replay
        self.arbtrace = arbtrace

    def call(self, method, _params):
        if method == "eth_getBlockByNumber":
            return True, {"hash": self.event["block_hash"]}
        if method == "eth_getTransactionByHash":
            return True, {
                "hash": self.event["transaction_hash"],
                "transactionIndex": hex(int(self.event["transaction_index"])),
            }
        if method == "eth_getTransactionReceipt":
            return True, {"blockHash": self.event["block_hash"]}
        if method == "debug_traceTransaction":
            return (
                (True, {"pre": {"0x" + "1" * 40: {"balance": "0x1"}}, "post": {}})
                if self.debug
                else (False, "rpc_error:-32601")
            )
        if method == "trace_replayTransaction":
            return (
                (True, {"stateDiff": {"0x" + "2" * 40: {"balance": {"*": {"from": "0x1", "to": "0x2"}}}}})
                if self.replay
                else (False, "rpc_error:-32601")
            )
        if method == "arbtrace_replayTransaction":
            return (
                (True, {"stateDiff": {"0x" + "3" * 40: {"balance": {"+": "0x2"}}}})
                if self.arbtrace
                else (False, "rpc_error:-32601")
            )
        raise AssertionError(method)


class TraceProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.payload = json.loads(ALLOWLIST.read_text(encoding="utf-8"))
        cls.event = cls.payload["events"][0]

    def test_allowlist_is_exactly_bounded(self):
        events = MODULE.load_events(ALLOWLIST)
        self.assertEqual(len(events), 10)

        changed = copy.deepcopy(self.payload)
        changed["events"].pop()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "changed.json"
            path.write_text(json.dumps(changed), encoding="utf-8")
            with self.assertRaisesRegex(MODULE.ProbeError, "event count"):
                MODULE.load_events(path)

    def test_debug_trace_is_hashed_without_raw_payload(self):
        row = MODULE.trace_event(FakeProvider(self.event), self.event)
        self.assertEqual(row["trace_status"], "available")
        self.assertEqual(
            row["trace_method"], "debug_traceTransaction_prestate_diff"
        )
        self.assertRegex(row["trace_response_sha256"], r"^[0-9a-f]{64}$")
        self.assertNotIn("pre", row)
        self.assertNotIn("post", row)

    def test_parity_replay_is_a_reviewed_fallback(self):
        row = MODULE.trace_event(
            FakeProvider(self.event, debug=False, replay=True), self.event
        )
        self.assertEqual(row["trace_status"], "available")
        self.assertEqual(
            row["trace_method"], "trace_replayTransaction_stateDiff"
        )

    def test_unsupported_provider_stays_fail_closed(self):
        row = MODULE.trace_event(
            FakeProvider(self.event, debug=False, replay=False), self.event
        )
        self.assertEqual(row["trace_status"], "unsupported")
        self.assertNotIn("trace_response_sha256", row)
        self.assertEqual(row["debug_failure"], "rpc_error:-32601")
        self.assertEqual(row["replay_failure"], "rpc_error:-32601")
        self.assertEqual(row["arbtrace_failure"], "rpc_error:-32601")

    def test_arbitrum_replay_namespace_is_a_reviewed_fallback(self):
        row = MODULE.trace_event(
            FakeProvider(self.event, debug=False, replay=False, arbtrace=True), self.event
        )
        self.assertEqual(row["trace_status"], "available")
        self.assertEqual(
            row["trace_method"], "arbtrace_replayTransaction_stateDiff"
        )

    def test_main_retains_bounded_failures_and_continues(self):
        class UnavailableProvider:
            label = "reviewed-provider-1"

            def call(self, _method, _params):
                return False, "transport_error"

        original_urls = MODULE.load_provider_urls
        original_provider = MODULE.Provider
        original_argv = MODULE.sys.argv
        try:
            MODULE.load_provider_urls = lambda _container: ["https://one", "https://two"]
            MODULE.Provider = lambda label, _url: UnavailableProvider()
            MODULE.sys.argv = [
                "probe",
                "--container",
                "gateway",
                "--allowlist",
                str(ALLOWLIST),
            ]
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                self.assertEqual(MODULE.main(), 0)
            result = json.loads(stdout.getvalue())
        finally:
            MODULE.load_provider_urls = original_urls
            MODULE.Provider = original_provider
            MODULE.sys.argv = original_argv

        self.assertEqual(len(result["providers"]), 2)
        self.assertTrue(
            all(row["provider_status"] == "unavailable" for row in result["providers"])
        )
        self.assertTrue(all(row["events"] == [] for row in result["providers"]))


if __name__ == "__main__":
    unittest.main()
