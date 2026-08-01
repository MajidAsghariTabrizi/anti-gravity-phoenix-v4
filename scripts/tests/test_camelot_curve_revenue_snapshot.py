import importlib.util
from pathlib import Path
import unittest


MODULE_PATH = Path(__file__).resolve().parents[1] / "camelot_curve_revenue_snapshot.py"
SPEC = importlib.util.spec_from_file_location("camelot_curve_revenue_snapshot", MODULE_PATH)
assert SPEC and SPEC.loader
snapshot = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(snapshot)


class RevenueSnapshotTest(unittest.TestCase):
    def test_seven_size_ladder_preserves_reviewed_bounds(self):
        self.assertEqual(len(snapshot.SIZES), 7)
        self.assertEqual(snapshot.SIZES[0], 100_000_000_000_000)
        self.assertEqual(snapshot.SIZES[-1], 10_000_000_000_000_000)
        self.assertEqual(tuple(sorted(set(snapshot.SIZES))), snapshot.SIZES)

    def test_abi_helpers_are_exact_and_reject_malformed_values(self):
        self.assertEqual(snapshot.word(1), "0" * 63 + "1")
        self.assertEqual(
            snapshot.address_word(snapshot.ADDRESSES["weth"]),
            "0" * 24 + snapshot.ADDRESSES["weth"][2:],
        )
        self.assertEqual(snapshot.decoded_address("0x" + "00" * 12 + "11" * 20), "0x" + "11" * 20)
        self.assertEqual(snapshot.signed(2**256 - 2), -2)
        encoded_addresses = "0x" + snapshot.word(32) + snapshot.word(2) + snapshot.word(1) + snapshot.word(2)
        self.assertEqual(
            snapshot.dynamic_addresses(encoded_addresses),
            ["0x" + "0" * 39 + "1", "0x" + "0" * 39 + "2"],
        )
        with self.assertRaises(snapshot.SnapshotError):
            snapshot.quantity("123")
        with self.assertRaises(snapshot.SnapshotError):
            snapshot.word(-1)

    def test_bound_identity_accepts_only_exact_route(self):
        document = {
            "camelot": {
                "factory_pool": snapshot.ADDRESSES["camelot_pool"],
                "pool_factory": snapshot.ADDRESSES["camelot_factory"],
                "token0": snapshot.ADDRESSES["weth"],
                "token1": snapshot.ADDRESSES["usdc"],
            },
            "curve": {
                "factory_token0": snapshot.ADDRESSES["weth"],
                "factory_token1": snapshot.ADDRESSES["usdc"],
                "implementation": snapshot.ADDRESSES["curve_implementation"],
                "token0": snapshot.ADDRESSES["weth"],
                "token1": snapshot.ADDRESSES["usdc"],
            },
            "aave": {"flash_premium_total_bps": 5},
        }
        snapshot.validate_bound_identity(document)
        document["curve"]["token1"] = snapshot.ADDRESSES["weth"]
        with self.assertRaisesRegex(snapshot.SnapshotError, "bound on-chain identity"):
            snapshot.validate_bound_identity(document)

    def test_provider_projection_excludes_only_provider_local_metadata(self):
        base = {
            "provider_index": 0,
            "head_at_start": 100,
            "block_timestamp": "2026-08-01T00:00:00+00:00",
            "max_priority_fee_per_gas_wei": "1",
            "block_number": 99,
            "block_hash": "0xabc",
            "base_fee_per_gas_wei": "2",
            "camelot": {"state": 1},
            "curve": {"state": 2},
            "aave": {"state": 3},
            "code_identities": {"state": 4},
            "seven_size_ladder": [{"state": 5}],
        }
        peer = dict(base)
        peer["provider_index"] = 1
        peer["head_at_start"] = 101
        self.assertEqual(snapshot.invariant_projection(base), snapshot.invariant_projection(peer))
        peer["curve"] = {"state": 99}
        self.assertNotEqual(snapshot.invariant_projection(base), snapshot.invariant_projection(peer))


if __name__ == "__main__":
    unittest.main()
