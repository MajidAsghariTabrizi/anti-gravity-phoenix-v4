import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DISCOVERY_PATH = ROOT / "scripts" / "export_aave_borrow_discovery.py"
DISCOVERY_SPEC = importlib.util.spec_from_file_location(
    "export_aave_borrow_discovery", DISCOVERY_PATH
)
DISCOVERY = importlib.util.module_from_spec(DISCOVERY_SPEC)
assert DISCOVERY_SPEC.loader is not None
DISCOVERY_SPEC.loader.exec_module(DISCOVERY)

VERIFY_PATH = ROOT / "scripts" / "verify_aave_borrow_archive.py"
VERIFY_SPEC = importlib.util.spec_from_file_location(
    "verify_aave_borrow_archive", VERIFY_PATH
)
VERIFY = importlib.util.module_from_spec(VERIFY_SPEC)
assert VERIFY_SPEC.loader is not None
VERIFY_SPEC.loader.exec_module(VERIFY)


def hash_value(byte):
    return "0x" + byte * 64


def log(block, tx_byte, log_index, borrower_byte):
    return {
        "block_number": block,
        "block_hash": hash_value("a"),
        "transaction_hash": hash_value(tx_byte),
        "transaction_index": 0,
        "log_index": log_index,
        "reserve": "0x" + "1" * 40,
        "borrower": "0x" + borrower_byte * 40,
        "referral_code": 0,
        "data_sha256": "f" * 64,
    }


class ArchiveVerifierTests(unittest.TestCase):
    def test_hash_bound_state_requires_exact_contiguous_ranges(self):
        chunks = [
            {
                "start_block": 1,
                "end_block": 512,
                "content_sha256": "a" * 64,
                "log_count": 1,
            },
            {
                "start_block": 513,
                "end_block": 1024,
                "content_sha256": "b" * 64,
                "log_count": 0,
            },
        ]
        value = DISCOVERY.archive_state(
            1,
            1024,
            512,
            [{"provider_id": "primary"}],
            chunks,
            True,
            "c" * 64,
        )
        self.assertEqual(VERIFY.validate_state(value, True), [(1, 512), (513, 1024)])
        value["chunks"][1]["start_block"] = 514
        with self.assertRaisesRegex(VERIFY.VerificationError, "non-contiguous"):
            VERIFY.validate_state(value, True)

    def test_minimum_unique_key_binds_contract_and_topic(self):
        value = log(10, "b", 3, "2")
        identity = VERIFY.log_identity(value)
        self.assertEqual(identity[0], DISCOVERY.CHAIN_ID)
        self.assertEqual(identity[-2], DISCOVERY.POOL)
        self.assertEqual(identity[-1], DISCOVERY.BORROW_TOPIC)

    def test_chunk_hashes_and_cross_chunk_duplicates_are_deterministic(self):
        first = [log(10, "b", 3, "2"), log(11, "c", 4, "3")]
        second = list(reversed(first))
        second.sort(
            key=lambda item: (
                item["block_number"],
                item["transaction_index"],
                item["log_index"],
            )
        )
        self.assertEqual(VERIFY.log_set_hash(first), VERIFY.log_set_hash(second))
        self.assertEqual(VERIFY.borrower_set_hash(first), VERIFY.borrower_set_hash(second))

    def test_hash_bound_loader_rejects_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            value = DISCOVERY.archive_state(
                1, 512, 512, [{"provider_id": "primary"}], [], False
            )
            DISCOVERY.write_json_atomic(path, value)
            self.assertEqual(
                VERIFY.load_hash_bound(path, VERIFY.STATE_SCHEMA)["start_block"], 1
            )
            mutated = json.loads(path.read_text(encoding="utf-8"))
            mutated["start_block"] = 2
            path.write_text(json.dumps(mutated), encoding="utf-8")
            with self.assertRaisesRegex(VERIFY.VerificationError, "hash mismatch"):
                VERIFY.load_hash_bound(path, VERIFY.STATE_SCHEMA)


if __name__ == "__main__":
    unittest.main()
