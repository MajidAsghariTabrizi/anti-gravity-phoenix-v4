import argparse
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import atlas_aave_economic_prefilter as module


def encoded_account_data(values):
    return "0x" + "".join(f"{value:064x}" for value in values)


def discovery_fixture(count=100):
    value = {
        "schema": "phoenix.atlas.aave-borrow-discovery.v1",
        "chain_id": module.CHAIN_ID,
        "pool": module.POOL,
        "archive_complete": True,
        "start_block": 1,
        "checkpoint_block": 10,
        "log_count": count,
        "borrower_count": count,
        "borrowers": [f"0x{index:040x}" for index in range(1, count + 1)],
    }
    value["content_sha256"] = module.canonical_hash(value)
    return value


def preflight_fixture():
    headers = []
    for provider_id, digest in (
        ("production-nownodes-arbitrum", "a" * 64),
        ("production-slot-0", "b" * 64),
    ):
        headers.append(
            {
                "provider_id": provider_id,
                "provider_reference_sha256": digest,
                "checkpoint": {
                    "number": 100,
                    "hash": "0x" + "c" * 64,
                    "state_root": "0x" + "d" * 64,
                },
            }
        )
    value = {
        "schema": module.PREFLIGHT_SCHEMA,
        "chain_id": module.CHAIN_ID,
        "provider_headers": headers,
        "direct_state_independent_agreement": True,
        "nownodes_stability_rounds_passed": 10,
        "proof_policy": {"status": "passed"},
        "execution_authority": False,
    }
    value["content_sha256"] = module.canonical_hash(value)
    return value


class FakePrimaryProvider:
    def __init__(self, *_args, **_kwargs):
        self.label = "production-nownodes-arbitrum"
        self.provider_reference_sha256 = "a" * 64
        self._request_id = 0
        self.transport_request_count = 0
        self.retry_count = 0

    def set_diagnostic_context(self, _stage):
        pass

    def call(self, method, params, attempts=1):
        assert attempts == 1
        self._request_id += 1
        self.transport_request_count += 1
        if method == "eth_chainId":
            return hex(module.CHAIN_ID)
        if method == "eth_getBlockByNumber":
            number = 100 if params[0] == "finalized" else int(params[0], 16)
            return {
                "number": hex(number),
                "hash": "0x" + "c" * 64,
                "parentHash": "0x" + "e" * 64,
                "stateRoot": "0x" + "d" * 64,
                "timestamp": hex(1_700_000_000),
            }
        raise AssertionError(method)

    def eth_calls(self, calls, _block, batch_size=200):
        self._request_id += len(calls)
        self.transport_request_count += (len(calls) + batch_size - 1) // batch_size
        return [encoded_account_data([0, 0, 0, 0, 0, 2**256 - 1])] * len(calls)

    def close(self):
        pass


class EconomicPrefilterTests(unittest.TestCase):
    def test_health_factor_bucket_boundaries_are_integer_exact(self):
        context = {
            "number": 100,
            "hash": "0x" + "c" * 64,
            "state_root": "0x" + "d" * 64,
        }
        cases = (
            ([0, 0, 0, 0, 0, 0], "no_debt"),
            ([1, 1, 0, 0, 0, module.WATCH_HF_WAD + 1], "debt_safe"),
            ([1, 1, 0, 0, 0, module.WATCH_HF_WAD], "watch"),
            ([1, 1, 0, 0, 0, module.URGENT_HF_WAD + 1], "watch"),
            ([1, 1, 0, 0, 0, module.URGENT_HF_WAD], "urgent"),
            ([1, 1, 0, 0, 0, module.WAD], "urgent"),
            ([1, 1, 0, 0, 0, module.WAD - 1], "liquidatable"),
        )
        for values, expected in cases:
            with self.subTest(expected=expected, health_factor=values[-1]):
                row = module.classify_account_data(
                    "0x" + "1" * 40,
                    encoded_account_data(values),
                    context,
                    "a" * 64,
                    0,
                )
                self.assertEqual(row["bucket"], expected)
        incomplete = module.classify_account_data(
            "0x" + "1" * 40,
            "0x1234",
            context,
            "a" * 64,
            0,
        )
        self.assertEqual(incomplete["bucket"], "incomplete")

    def test_exact_100_address_batch_commits_atomic_cursor_with_103_items(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            discovery_path = root / "discovery.json"
            preflight_path = root / "preflight.json"
            resume_dir = root / "ledger"
            discovery_path.write_text(json.dumps(discovery_fixture()), encoding="utf-8")
            preflight_path.write_text(json.dumps(preflight_fixture()), encoding="utf-8")
            args = argparse.Namespace(
                discovery=discovery_path,
                preflight_context=preflight_path,
                resume_dir=resume_dir,
                batch_size=100,
                rpc_batch_size=200,
                max_json_rpc_items=125,
                max_transport_requests=5,
                max_retries=0,
                max_runtime_seconds=180,
                ssh_executable="ssh.exe",
                ssh_provider_host="example",
                ssh_provider_port=9011,
                ssh_provider_identity=root / "identity",
                ssh_provider_known_hosts=root / "known_hosts",
                ssh_provider_container="app-rpc-gateway-1",
            )
            with mock.patch.object(module, "SSHContainerProvider", FakePrimaryProvider):
                result = module.run(args)
            self.assertEqual(result["prefilter_cursor_before"], 0)
            self.assertEqual(result["prefilter_cursor_after"], 100)
            self.assertEqual(result["bucket_counts"]["no_debt"], 100)
            self.assertEqual(result["exact_validation_cohort_size"], 0)
            self.assertEqual(result["request_usage"]["json_rpc_item_count"], 103)
            self.assertEqual(result["request_usage"]["transport_request_count"], 4)
            self.assertFalse(result["candidate_authority"])
            state = json.loads((resume_dir / "state.json").read_text(encoding="utf-8"))
            self.assertEqual(state["next_address_index"], 100)
            self.assertEqual(state["completed_batches"], 1)
            batch_file = resume_dir / state["batch_artifacts"][0]["prefilter_file"]
            batch = json.loads(batch_file.read_text(encoding="utf-8"))
            self.assertFalse(batch["raw_rpc_responses_persisted"])
            self.assertNotIn("result", batch["rows"][0])
            cohort_file = resume_dir / state["batch_artifacts"][0]["cohort_file"]
            cohort = json.loads(cohort_file.read_text(encoding="utf-8"))
            self.assertEqual(cohort["addresses"], [])
            self.assertFalse(cohort["execution_authority"])

    def test_hard_request_bound_does_not_advance_cursor(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            discovery_path = root / "discovery.json"
            preflight_path = root / "preflight.json"
            resume_dir = root / "ledger"
            discovery_path.write_text(json.dumps(discovery_fixture()), encoding="utf-8")
            preflight_path.write_text(json.dumps(preflight_fixture()), encoding="utf-8")
            args = argparse.Namespace(
                discovery=discovery_path,
                preflight_context=preflight_path,
                resume_dir=resume_dir,
                batch_size=100,
                rpc_batch_size=200,
                max_json_rpc_items=102,
                max_transport_requests=5,
                max_retries=0,
                max_runtime_seconds=180,
                ssh_executable="ssh.exe",
                ssh_provider_host="example",
                ssh_provider_port=9011,
                ssh_provider_identity=root / "identity",
                ssh_provider_known_hosts=root / "known_hosts",
                ssh_provider_container="app-rpc-gateway-1",
            )
            with mock.patch.object(module, "SSHContainerProvider", FakePrimaryProvider):
                with self.assertRaisesRegex(module.ExportError, "hard execution bound"):
                    module.run(args)
            self.assertFalse((resume_dir / "state.json").exists())


if __name__ == "__main__":
    unittest.main()
