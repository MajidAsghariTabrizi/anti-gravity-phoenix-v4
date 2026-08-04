import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "export_aave_borrow_discovery.py"
SPEC = importlib.util.spec_from_file_location("export_aave_borrow_discovery", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def hash_value(byte):
    return "0x" + byte * 64


def address_topic(byte):
    return "0x" + "0" * 24 + byte * 40


class SplitProvider:
    label = "reviewed-provider-1"

    def __init__(self):
        self.calls = []

    def call(self, method, params, attempts=3):
        self.calls.append((method, params, attempts))
        start = int(params[0]["fromBlock"], 16)
        end = int(params[0]["toBlock"], 16)
        if end - start + 1 > 512:
            raise MODULE.ExportError(
                "reviewed-provider-1:eth_getLogs:rpc_error:-32005"
            )
        return []


class AaveBorrowDiscoveryTests(unittest.TestCase):
    def test_current_state_bridge_allows_calls_and_disables_redirects(self):
        provider = object.__new__(MODULE.SSHContainerProvider)
        provider.label = "production-nownodes-arbitrum"
        provider._request_id = 0
        provider.retry_count = 0
        provider._process = mock.Mock()
        provider._process.poll.return_value = None
        provider._request = mock.Mock(return_value={"result": "0x1234"})
        self.assertEqual(
            provider.call("eth_call", [{"to": "0x" + "1" * 40}, "0x1"]),
            "0x1234",
        )
        module_source = MODULE_PATH.read_text(encoding="utf-8")
        source = module_source.split("class SSHContainerProvider", 1)[1].split(
            "\ndef header", 1
        )[0]
        self.assertIn("class NoRedirect", source)
        self.assertIn("opener.open(request", source)
        self.assertNotIn("urllib.request.urlopen(request", source)

    def test_provider_environment_reference_is_protected_and_single_provider_safe(self):
        with mock.patch.dict(
            MODULE.os.environ,
            {"PHOENIX_ATLAS_ARCHIVE_PRIMARY_RPC_URL": "https://provider.invalid"},
            clear=False,
        ):
            self.assertEqual(
                MODULE.provider_urls(
                    None, ["PHOENIX_ATLAS_ARCHIVE_PRIMARY_RPC_URL"]
                ),
                ["https://provider.invalid"],
            )
        with self.assertRaisesRegex(MODULE.ExportError, "unset"):
            MODULE.provider_urls(None, ["PHOENIX_ATLAS_ARCHIVE_MISSING_RPC_URL"])

    def test_resumable_state_is_hash_bound_and_exactly_counts_ranges(self):
        bindings = [
            {
                "provider_id": "primary",
                "chain_id": MODULE.CHAIN_ID,
                "start_block": {"number": 1, "hash": hash_value("1")},
                "checkpoint_block": {"number": 1024, "hash": hash_value("2")},
            }
        ]
        chunks = [
            {
                "start_block": 1,
                "end_block": 512,
                "content_sha256": "a" * 64,
                "log_count": 0,
            }
        ]
        value = MODULE.archive_state(1, 1024, 512, bindings, chunks, False)
        self.assertEqual(value["expected_chunk_count"], 2)
        self.assertEqual(value["completed_chunk_count"], 1)
        self.assertEqual(value["next_start_block"], 513)
        observed = value["content_sha256"]
        body = {key: item for key, item in value.items() if key != "content_sha256"}
        self.assertEqual(observed, MODULE.canonical_hash(body))

    def test_chunk_cache_is_hash_bound_and_range_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            logs = [{"block_number": 10, "transaction_hash": hash_value("a")}]
            MODULE.write_cached_chunk(root, 1, 20, logs)
            path = root / "1-20.json"
            self.assertEqual(MODULE.load_cached_chunk(path, 1, 20), logs)
            value = json.loads(path.read_text(encoding="utf-8"))
            value["logs"][0]["block_number"] = 21
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(MODULE.ExportError, "content hash mismatch"):
                MODULE.load_cached_chunk(path, 1, 20)

    def test_borrow_log_is_sanitized_and_borrower_bound(self):
        log = {
            "address": MODULE.POOL,
            "blockNumber": "0x64",
            "blockHash": hash_value("a"),
            "transactionHash": hash_value("b"),
            "transactionIndex": "0x2",
            "logIndex": "0x3",
            "topics": [
                MODULE.BORROW_TOPIC,
                address_topic("1"),
                address_topic("2"),
                "0x" + "0" * 63 + "7",
            ],
            "data": "0x1234",
            "removed": False,
        }
        result = MODULE.sanitize_log(log)
        self.assertEqual(result["borrower"], "0x" + "2" * 40)
        self.assertEqual(result["reserve"], "0x" + "1" * 40)
        self.assertEqual(result["referral_code"], 7)
        self.assertNotIn("data", result)
        self.assertRegex(result["data_sha256"], r"^[0-9a-f]{64}$")

    def test_removed_or_wrong_signature_log_fails_closed(self):
        base = {
            "blockNumber": "0x1",
            "blockHash": hash_value("a"),
            "transactionHash": hash_value("b"),
            "transactionIndex": "0x0",
            "logIndex": "0x0",
            "topics": [MODULE.BORROW_TOPIC, address_topic("1"), address_topic("2"), "0x0"],
            "data": "0x",
            "removed": True,
        }
        with self.assertRaisesRegex(MODULE.ExportError, "removed"):
            MODULE.sanitize_log(base)
        base["removed"] = False
        base["topics"][0] = hash_value("f")
        with self.assertRaisesRegex(MODULE.ExportError, "signature"):
            MODULE.sanitize_log(base)

    def test_large_log_range_splits_until_provider_bound(self):
        provider = SplitProvider()
        self.assertEqual(MODULE.get_logs(provider, 1, 1024), [])
        ranges = [
            (int(params[0]["fromBlock"], 16), int(params[0]["toBlock"], 16))
            for method, params, _attempts in provider.calls
            if method == "eth_getLogs"
        ]
        self.assertEqual(ranges, [(1, 1024), (1, 512), (513, 1024)])

    def test_transport_throttle_is_not_misclassified_as_range_limit(self):
        class ThrottledProvider:
            label = "reviewed-provider-1"

            def call(self, _method, _params, attempts=3):
                raise MODULE.ExportError(
                    "reviewed-provider-1:eth_getLogs:http_error:429"
                )

        with self.assertRaisesRegex(MODULE.ExportError, "http_error:429"):
            MODULE.get_logs(ThrottledProvider(), 1, 1024)

        class TransportFailedProvider:
            label = "reviewed-provider-1"

            def call(self, _method, _params, attempts=3):
                raise MODULE.ExportError(
                    "reviewed-provider-1:eth_getLogs:transport_error"
                )

        with self.assertRaisesRegex(MODULE.ExportError, "transport_error"):
            MODULE.get_logs(TransportFailedProvider(), 1, 1024)


if __name__ == "__main__":
    unittest.main()
