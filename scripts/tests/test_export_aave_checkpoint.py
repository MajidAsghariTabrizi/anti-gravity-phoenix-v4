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


class AaveCheckpointTests(unittest.TestCase):
    def test_discovery_hash_and_canonical_borrower_set_are_required(self):
        value = {
            "schema": "phoenix.atlas.aave-borrow-discovery.v1",
            "chain_id": MODULE.CHAIN_ID,
            "pool": MODULE.POOL,
            "archive_complete": True,
            "checkpoint_block": 100,
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

    def test_tail_log_export_is_contiguous_and_bounded(self):
        provider = EmptyLogProvider()
        with mock.patch.object(MODULE.time, "sleep"):
            logs = MODULE.sanitized_tail_borrow_logs(provider, 1, 4_501)
        self.assertEqual(logs, [])
        self.assertEqual(provider.ranges, [(1, 2_000), (2_001, 4_000), (4_001, 4_501)])


if __name__ == "__main__":
    unittest.main()
