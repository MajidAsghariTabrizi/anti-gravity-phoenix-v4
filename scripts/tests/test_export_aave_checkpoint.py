import importlib.util
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "export_aave_checkpoint.py"
SPEC = importlib.util.spec_from_file_location("export_aave_checkpoint", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def abi_word(value):
    return f"{value:064x}"


class StaticProvider:
    def __init__(self, label, result):
        self.label = label
        self.result = result

    def eth_calls(self, calls, block):
        return list(self.result[: len(calls)])


class EmptyLogProvider:
    label = "logs"

    def __init__(self):
        self.ranges = []

    def call(self, method, params):
        assert method == "eth_getLogs"
        query = params[0]
        self.ranges.append((int(query["fromBlock"], 16), int(query["toBlock"], 16)))
        return []


class HeaderProvider:
    def __init__(self, label, reference):
        self.label = label
        self.provider_reference_sha256 = reference
        self.endpoint_identity = label
        self.header_name = None
        self.authenticated = False

    def call(self, method, params):
        assert method == "eth_getBlockByNumber"
        block = 100 if params[0] == "finalized" else int(params[0], 16)
        return {
            "number": hex(block),
            "hash": "0x" + "a" * 64,
            "parentHash": "0x" + "b" * 64,
            "timestamp": hex(1_700_000_000),
            "stateRoot": "0x" + "c" * 64,
        }


class ExactBlockProvider:
    label = "exact-block"

    def __init__(self, logs):
        self.logs = logs

    def call(self, method, params):
        assert method == "eth_getLogs"
        assert params[0]["blockHash"] == self.logs[0]["blockHash"]
        return self.logs


class AaveCheckpointTests(unittest.TestCase):
    def test_finalized_checkpoint_preserves_redacted_provider_references(self):
        providers = [
            HeaderProvider("one", "8" * 64),
            HeaderProvider("two", "9" * 64),
        ]
        block, heads, headers = MODULE.finalized_checkpoint(providers)
        self.assertEqual(block, 100)
        self.assertEqual(
            [item["provider_reference_sha256"] for item in heads],
            ["8" * 64, "9" * 64],
        )
        self.assertEqual(
            [item["provider_reference_sha256"] for item in headers],
            ["8" * 64, "9" * 64],
        )

    def test_discovery_only_manifest_has_zero_historical_authority(self):
        discovery = {
            "schema": "phoenix.atlas.aave-borrow-discovery.v1",
            "chain_id": MODULE.CHAIN_ID,
            "pool": MODULE.POOL,
            "archive_complete": True,
            "start_block": 1,
            "checkpoint_block": 100,
            "log_count": 1,
            "borrower_count": 1,
            "borrowers": ["0x" + "1" * 40],
        }
        discovery["content_sha256"] = MODULE.canonical_hash(discovery)
        manifest = {
            "schema": MODULE.ARCHIVE_MANIFEST_SCHEMA,
            "chain_id": MODULE.CHAIN_ID,
            "contract_address": MODULE.POOL,
            "event_topic0": MODULE.BORROW_TOPIC,
            "archive_complete": True,
            "independent_validation": False,
            "coverage_gaps": [],
            "deployment_boundary": None,
            "final_archive_sha256": discovery["content_sha256"],
        }
        manifest["content_sha256"] = MODULE.canonical_hash(manifest)
        validated_discovery = MODULE.validate_discovery(
            discovery, MODULE.AUTHORITY_CURRENT_STATE
        )
        validated_manifest = MODULE.validate_archive_manifest(
            manifest, validated_discovery, MODULE.AUTHORITY_CURRENT_STATE
        )
        state = MODULE.initial_screen_state(validated_discovery, validated_manifest)
        self.assertEqual(state["next_address_index"], 0)
        self.assertEqual(state["addresses"], discovery["borrowers"])
        self.assertEqual(
            MODULE.validate_screen_state(
                state, validated_discovery, validated_manifest
            )["content_sha256"],
            state["content_sha256"],
        )
        with self.assertRaisesRegex(MODULE.ExportError, "independent"):
            MODULE.validate_archive_manifest(manifest, validated_discovery)

    def test_checkpoint_requires_complete_independently_validated_archive_manifest(self):
        discovery = {
            "schema": "phoenix.atlas.aave-borrow-discovery.v1",
            "chain_id": MODULE.CHAIN_ID,
            "pool": MODULE.POOL,
            "archive_complete": True,
            "start_block": 1,
            "checkpoint_block": 100,
            "log_count": 1,
            "borrower_count": 1,
            "borrowers": ["0x" + "1" * 40],
        }
        discovery["content_sha256"] = MODULE.canonical_hash(discovery)
        manifest = {
            "schema": MODULE.ARCHIVE_MANIFEST_SCHEMA,
            "chain_id": MODULE.CHAIN_ID,
            "contract_address": MODULE.POOL,
            "event_topic0": MODULE.BORROW_TOPIC,
            "archive_complete": True,
            "independent_validation": True,
            "coverage_gaps": [],
            "deployment_boundary": {
                "status": "verified_exact_creation",
                "prior_block": {"number": 0, "hash": "0x" + "a" * 64},
                "deployment_block": {"number": 1, "hash": "0x" + "b" * 64},
                "prior_code": "0x",
                "deployment_code_sha256": "c" * 64,
            },
            "final_archive_sha256": discovery["content_sha256"],
        }
        manifest["content_sha256"] = MODULE.canonical_hash(manifest)
        self.assertEqual(
            MODULE.validate_archive_manifest(manifest, discovery)[
                "final_archive_sha256"
            ],
            discovery["content_sha256"],
        )
        without_boundary = {
            key: value
            for key, value in manifest.items()
            if key not in {"content_sha256", "deployment_boundary"}
        }
        without_boundary["content_sha256"] = MODULE.canonical_hash(without_boundary)
        with self.assertRaisesRegex(MODULE.ExportError, "deployment boundary"):
            MODULE.validate_archive_manifest(without_boundary, discovery)
        manifest["independent_validation"] = False
        manifest["content_sha256"] = MODULE.canonical_hash(
            {
                key: value
                for key, value in manifest.items()
                if key != "content_sha256"
            }
        )
        with self.assertRaisesRegex(MODULE.ExportError, "independent"):
            MODULE.validate_archive_manifest(manifest, discovery)

    def test_discovery_hash_and_canonical_borrower_set_are_required(self):
        value = {
            "schema": "phoenix.atlas.aave-borrow-discovery.v1",
            "chain_id": MODULE.CHAIN_ID,
            "pool": MODULE.POOL,
            "archive_complete": True,
            "start_block": 1,
            "checkpoint_block": 100,
            "log_count": 1,
            "borrower_count": 1,
            "borrowers": ["0x" + "1" * 40],
        }
        value["content_sha256"] = MODULE.canonical_hash(value)
        self.assertEqual(MODULE.validate_discovery(value)["checkpoint_block"], 100)
        value["borrowers"] = ["0x" + "2" * 40]
        with self.assertRaisesRegex(MODULE.ExportError, "hash mismatch"):
            MODULE.validate_discovery(value)

    def test_address_array_and_symbol_decoding(self):
        address = "0x" + "a" * 40
        encoded = "0x" + abi_word(32) + abi_word(1) + "0" * 24 + address[2:]
        self.assertEqual(MODULE.decode_address_array(encoded), [address])
        symbol = b"WETH"
        dynamic = "0x" + abi_word(32) + abi_word(len(symbol)) + symbol.hex().ljust(64, "0")
        self.assertEqual(MODULE.decode_symbol(dynamic), "WETH")
        fixed = "0x" + symbol.hex().ljust(64, "0")
        self.assertEqual(MODULE.decode_symbol(fixed), "WETH")

    def test_independent_state_disagreement_fails_closed(self):
        calls = [(MODULE.POOL, "0x" + MODULE.SELECTORS["get_reserves_list"])]
        providers = [StaticProvider("one", ["0x01"]), StaticProvider("two", ["0x02"])]
        with self.assertRaisesRegex(MODULE.ExportError, "provider disagreement"):
            MODULE.independently_agreed_calls(providers, calls, 100, "test")

    def test_independent_state_binding_is_hash_bound(self):
        calls = [(MODULE.POOL, "0x" + MODULE.SELECTORS["get_reserves_list"])]
        providers = [StaticProvider("one", ["0x01"]), StaticProvider("two", ["0x01"])]
        result, bindings = MODULE.independently_agreed_calls(
            providers, calls, 100, "test"
        )
        self.assertEqual(result, ["0x01"])
        self.assertEqual(bindings[0]["result_sha256"], bindings[1]["result_sha256"])
        self.assertEqual(bindings[0]["call_count"], 1)

    def test_complete_activity_screen_uses_current_debt_bits(self):
        borrowers = ["0x" + "1" * 40, "0x" + "2" * 40]
        reserves = [{"reserve_id": 0}, {"reserve_id": 1}]
        debt_on_reserve_one = "0x" + abi_word(1 << 2)
        collateral_only = "0x" + abi_word(1 << 1)
        providers = [
            StaticProvider("one", [debt_on_reserve_one, collateral_only]),
            StaticProvider("two", [debt_on_reserve_one, collateral_only]),
        ]
        active, configurations, bindings = MODULE.active_borrower_state(
            providers, 100, borrowers, reserves
        )
        self.assertEqual(active, [borrowers[0]])
        self.assertEqual(configurations[borrowers[0]], 1 << 2)
        self.assertEqual(configurations[borrowers[1]], 1 << 1)
        primary = [item for item in bindings if item["context"] == "borrower_activity_primary"]
        retained = [
            item for item in bindings if item["context"] == "borrower_activity_retained"
        ]
        self.assertEqual(len(primary), 1)
        self.assertEqual(primary[0]["call_count"], 2)
        self.assertEqual(len(retained), 2)
        self.assertEqual({item["call_count"] for item in retained}, {1})

    def test_abi_encoding_rejects_unbounded_values(self):
        with self.assertRaisesRegex(MODULE.ExportError, "out of bounds"):
            MODULE.uint_word(-1)
        with self.assertRaisesRegex(MODULE.ExportError, "out of bounds"):
            MODULE.uint_word(2**256)
        with self.assertRaisesRegex(MODULE.ExportError, "canonical address"):
            MODULE.encode_address("0x1234")
        self.assertEqual(MODULE.word_int("f" * 64), -1)

    def test_tail_log_export_is_contiguous_and_bounded(self):
        provider = EmptyLogProvider()
        with mock.patch.object(MODULE.time, "sleep"):
            logs = MODULE.sanitized_tail_borrow_logs(provider, 1, 4_501)
        self.assertEqual(logs, [])
        self.assertEqual(provider.ranges, [(1, 2_000), (2_001, 4_000), (4_001, 4_501)])

    def test_tail_log_set_requires_independent_agreement(self):
        providers = [StaticProvider("one", []), StaticProvider("two", [])]
        with mock.patch.object(
            MODULE, "sanitized_tail_borrow_logs", side_effect=[[], []]
        ):
            logs, bindings = MODULE.independently_agreed_tail_logs(
                providers, 100, 101
            )
        self.assertEqual(logs, [])
        self.assertEqual([item["provider_id"] for item in bindings], ["one", "two"])
        self.assertEqual(
            bindings[0]["logs_content_sha256"],
            bindings[1]["logs_content_sha256"],
        )
        with mock.patch.object(
            MODULE, "sanitized_tail_borrow_logs", side_effect=[[], [{"x": 1}]]
        ):
            with self.assertRaisesRegex(MODULE.ExportError, "Borrow tail"):
                MODULE.independently_agreed_tail_logs(providers, 100, 101)

    def test_discovery_only_tail_is_reproduced_from_exact_block(self):
        transaction_hash = "0x" + "c" * 64
        log = {
            "address": MODULE.POOL,
            "blockNumber": hex(100),
            "blockHash": "0x" + "a" * 64,
            "transactionHash": transaction_hash,
            "transactionIndex": hex(2),
            "logIndex": hex(3),
            "topics": [
                MODULE.BORROW_TOPIC,
                "0x" + "0" * 24 + "1" * 40,
                "0x" + "0" * 24 + "2" * 40,
                "0x" + "0" * 63 + "4",
            ],
            "data": "0x1234",
        }
        expected = [
            {
                "block_number": 100,
                "block_hash": log["blockHash"],
                "transaction_hash": transaction_hash,
                "transaction_index": 2,
                "log_index": 3,
                "reserve": "0x" + "1" * 40,
                "borrower": "0x" + "2" * 40,
                "referral_code": 4,
                "data_sha256": MODULE.hashlib.sha256(b"0x1234").hexdigest(),
            }
        ]
        provider = ExactBlockProvider([log])
        self.assertEqual(
            MODULE.exact_block_verified_tail_borrow_logs(
                provider, expected, 100, 100
            ),
            expected,
        )
        provider.logs[0]["data"] = "0xabcd"
        with self.assertRaisesRegex(MODULE.ExportError, "exact-block Borrow logs"):
            MODULE.exact_block_verified_tail_borrow_logs(
                provider, expected, 100, 100
            )


if __name__ == "__main__":
    unittest.main()
