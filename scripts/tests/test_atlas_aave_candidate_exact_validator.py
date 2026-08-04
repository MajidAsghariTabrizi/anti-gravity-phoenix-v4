import argparse
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import atlas_aave_candidate_exact_validator as module


def encoded_words(*values):
    return "0x" + "".join(f"{value:064x}" for value in values)


def cohort_fixture(addresses=None):
    addresses = addresses or ["0x" + "1" * 40, "0x" + "2" * 40]
    value = {
        "schema": module.COHORT_SCHEMA,
        "chain_id": module.CHAIN_ID,
        "pool": module.POOL,
        "source_discovery_content_sha256": "a" * 64,
        "source_prefilter_content_sha256": "b" * 64,
        "finalized_prefilter_block": {
            "number": 90,
            "hash": "0x" + "c" * 64,
            "state_root": "0x" + "d" * 64,
        },
        "cohort_reason": "urgent_or_liquidatable",
        "address_count": len(addresses),
        "addresses": addresses,
        "candidate_authority": False,
        "execution_authority": False,
    }
    value["content_sha256"] = module.canonical_hash(value)
    return value


class RefreshProvider:
    instances = []

    def __init__(self, label, *_args, authenticated=False, **_kwargs):
        self.label = label
        self.provider_reference_sha256 = (
            "a" * 64 if label == "production-nownodes-arbitrum" else "b" * 64
        )
        self.endpoint_identity = (
            "https://arbitrum.nownodes.io/"
            if authenticated
            else "rpc-provider-slot-0"
        )
        self.header_name = "api-key" if authenticated else None
        self.authenticated = authenticated
        self._request_id = 0
        self.transport_request_count = 0
        self.retry_count = 0
        self.calls = []
        self.contexts = []
        self.__class__.instances.append(self)

    def set_diagnostic_context(self, stage):
        self.contexts.append(stage)

    def call(self, method, params, attempts=1):
        if attempts != 1:
            raise AssertionError("candidate validation must force attempts=1")
        self._request_id += 1
        self.transport_request_count += 1
        self.calls.append((method, params))
        if method == "eth_chainId":
            return hex(module.CHAIN_ID)
        if method == "eth_getBlockByNumber":
            return {
                "number": hex(100),
                "hash": "0x" + "c" * 64,
                "parentHash": "0x" + "e" * 64,
                "stateRoot": "0x" + "d" * 64,
                "timestamp": hex(1_700_000_000),
            }
        if method == "eth_getCode":
            return "0x6001600055"
        if method == "eth_getStorageAt":
            return "0x" + "0" * 24 + module.POOL_IMPLEMENTATION[2:]
        if method == "eth_call":
            return encoded_words(10, 5, 0, 8000, 7000, module.WAD + 1)
        raise AssertionError(method)

    def eth_calls(self, *_args, **_kwargs):
        raise AssertionError("JSON-RPC batch transport is forbidden")

    def close(self):
        pass


class CallProvider:
    def __init__(self, label, responder):
        self.label = label
        self.responder = responder
        self.provider_reference_sha256 = (
            "a" * 64 if label == "production-nownodes-arbitrum" else "b" * 64
        )
        self.endpoint_identity = label
        self.header_name = None
        self.authenticated = label == "production-nownodes-arbitrum"
        self._request_id = 0
        self.transport_request_count = 0
        self.retry_count = 0
        self.semantics = []

    def set_diagnostic_context(self, stage):
        self.semantics.append(stage)

    def call(self, method, params, attempts=1):
        if attempts != 1 or method != "eth_call":
            raise AssertionError((method, attempts))
        self._request_id += 1
        self.transport_request_count += 1
        return self.responder(self, params[0]["to"], params[0]["data"])


def bitmap_fixture():
    value = 8000
    value |= 8250 << 16
    value |= 10500 << 32
    value |= 18 << 48
    value |= 1 << 56
    value |= 1 << 58
    value |= 1000 << 64
    value |= 1000 << 152
    return value


def data_provider_result(selector):
    values = {
        module.SELECTORS["get_reserve_configuration"]: encoded_words(
            18, 8000, 8250, 10500, 1000, 1, 1, 0, 1, 0
        ),
        module.DATA_PROVIDER_SELECTORS["get_paused"]: encoded_words(0),
        module.DATA_PROVIDER_SELECTORS["get_liquidation_protocol_fee"]: encoded_words(
            1000
        ),
        module.DATA_PROVIDER_SELECTORS["get_siloed_borrowing"]: encoded_words(0),
        module.DATA_PROVIDER_SELECTORS["get_debt_ceiling"]: encoded_words(0),
    }
    return values[selector]


def oracle_record(
    provider_id,
    *,
    source="0x" + "4" * 40,
    code_hash="c" * 64,
    price=100_000_000,
    answer=100_000_000,
    round_supported=False,
    round_failure="rpc_error:3",
    round_id=10,
    round_answer=100_000_000,
    updated_at=1_700_000_000,
    answered_in_round=10,
):
    return {
        "asset": "0x" + "3" * 40,
        "source": source,
        "provider_id": provider_id,
        "provider_reference_sha256": (
            "a" * 64 if provider_id == "production-nownodes-arbitrum" else "b" * 64
        ),
        "source_code_sha256": code_hash,
        "source_code_length_bytes": 100,
        "aave_oracle_price": price,
        "source_latest_answer": answer,
        "round_metadata": (
            {
                "supported": True,
                "round_id": round_id,
                "answer": round_answer,
                "started_at": updated_at - 1,
                "updated_at": updated_at,
                "answered_in_round": answered_in_round,
                "response_sha256": "d" * 64,
            }
            if round_supported
            else {"supported": False, "failure_class": round_failure}
        ),
        "calls": {
            "get_source_of_asset": {"success": True},
            "source_code": {"success": True},
            "get_asset_price": {"success": True},
            "latest_answer": {"success": True},
            "latest_round_data": {
                "success": round_supported,
                "failure_class": None if round_supported else round_failure,
            },
            "decimals": {"success": True},
        },
    }


class CandidateExactValidatorTests(unittest.TestCase):
    def setUp(self):
        RefreshProvider.instances = []

    def test_stale_rows_stop_before_reserve_reconstruction(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cohort_path = root / "cohort.json"
            cohort_path.write_text(json.dumps(cohort_fixture()), encoding="utf-8")
            args = argparse.Namespace(
                cohort=cohort_path,
                output_dir=root / "output",
                ssh_executable="ssh.exe",
                ssh_provider_host="example",
                ssh_provider_port=9011,
                ssh_provider_identity=root / "identity",
                ssh_provider_known_hosts=root / "known_hosts",
                ssh_provider_container="app-rpc-gateway-1",
                max_relevant_reserves=20,
                max_runtime_seconds=300,
                max_items_per_provider=250,
            )
            with mock.patch.object(module, "SSHContainerProvider", RefreshProvider):
                result = module.run(args)
        self.assertEqual(result["status"], "stale_signals")
        self.assertTrue(result["reserve_reconstruction_skipped"])
        self.assertFalse(result["candidate_authority"])
        self.assertFalse(result["execution_authority"])
        self.assertEqual(len(RefreshProvider.instances), 2)
        for provider in RefreshProvider.instances:
            selectors = [
                params[0]["data"][:10]
                for method, params in provider.calls
                if method == "eth_call"
            ]
            self.assertEqual(
                selectors,
                ["0x" + module.SELECTORS["get_user_account_data"]] * 2,
            )
            self.assertLessEqual(provider._request_id, 10)

    def test_candidate_validation_uses_individual_calls_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cohort_path = root / "cohort.json"
            cohort_path.write_text(json.dumps(cohort_fixture()), encoding="utf-8")
            args = argparse.Namespace(
                cohort=cohort_path,
                output_dir=root / "output",
                ssh_executable="ssh.exe",
                ssh_provider_host="example",
                ssh_provider_port=9011,
                ssh_provider_identity=root / "identity",
                ssh_provider_known_hosts=None,
                ssh_provider_container="app-rpc-gateway-1",
                max_relevant_reserves=20,
                max_runtime_seconds=300,
                max_items_per_provider=250,
            )
            with mock.patch.object(module, "SSHContainerProvider", RefreshProvider):
                module.run(args)
        self.assertTrue(
            all(
                not isinstance(params, list) or method != "batch"
                for provider in RefreshProvider.instances
                for method, params in provider.calls
            )
        )

    def test_slot_zero_non_batch_shape_cannot_affect_individual_transport(self):
        provider = CallProvider(
            "production-slot-0", lambda _provider, _target, _data: encoded_words(7)
        )
        self.assertEqual(
            module._eth_call(provider, module.POOL, "0x12345678", 100, "sample"),
            encoded_words(7),
        )
        self.assertEqual(provider.transport_request_count, 1)

    def test_only_active_user_reserve_ids_are_queried(self):
        active_ids = {1, 5}
        configuration = sum(1 << (reserve_id * 2) for reserve_id in active_ids)

        def responder(_provider, _target, data):
            if data.startswith("0x" + module.SELECTORS["get_user_configuration"]):
                return encoded_words(configuration)
            if data.startswith("0x" + module.SELECTORS["get_reserve_address_by_id"]):
                reserve_id = int(data[-64:], 16)
                return encoded_words(reserve_id + 100)
            raise AssertionError(data)

        providers = [
            CallProvider("production-nownodes-arbitrum", responder),
            CallProvider("production-slot-0", responder),
        ]
        rows = [{"borrower": "0x" + "1" * 40}]
        assets, _ = module._relevant_reserves(providers, 100, rows, 20)
        self.assertEqual(set(assets.values()), active_ids)
        for provider in providers:
            mappings = [
                semantic
                for semantic in provider.semantics
                if semantic == "candidate_reserve_id_mapping"
            ]
            self.assertEqual(len(mappings), 2)

    def test_irrelevant_reserves_are_never_queried(self):
        configuration = 1 << (3 * 2 + 1)

        def responder(_provider, _target, data):
            if data.startswith("0x" + module.SELECTORS["get_user_configuration"]):
                return encoded_words(configuration)
            reserve_id = int(data[-64:], 16)
            self.assertEqual(reserve_id, 3)
            return encoded_words(103)

        providers = [
            CallProvider("production-nownodes-arbitrum", responder),
            CallProvider("production-slot-0", responder),
        ]
        module._relevant_reserves(
            providers, 100, [{"borrower": "0x" + "1" * 40}], 20
        )

    def test_direct_pool_configuration_agreement_succeeds(self):
        def responder(_provider, target, data):
            selector = data[2:10]
            if target == module.POOL:
                return encoded_words(bitmap_fixture())
            return data_provider_result(selector)

        providers = [
            CallProvider("production-nownodes-arbitrum", responder),
            CallProvider("production-slot-0", responder),
        ]
        evidence, configurations, source = module.configuration_matrix(
            providers, 100, ["0x" + "3" * 40]
        )
        self.assertEqual(source, "pool_configuration_bitmap")
        self.assertEqual(configurations["0x" + "3" * 40]["liquidation_protocol_fee_bps"], 1000)
        self.assertTrue(all("response" not in item for item in evidence))

    def test_nownodes_rpc_error_three_is_asset_method_block_bound(self):
        asset = "0x" + "3" * 40

        def responder(provider, target, data):
            selector = data[2:10]
            if target == module.POOL and provider.label == "production-nownodes-arbitrum":
                raise module.ProviderDiagnosticError(
                    "rpc_error:3", provider.label, "eth_call", 1, "matrix", None,
                    None, "unavailable", 1, 1, 0
                )
            if target == module.POOL:
                return encoded_words(bitmap_fixture())
            return data_provider_result(selector)

        providers = [
            CallProvider("production-nownodes-arbitrum", responder),
            CallProvider("production-slot-0", responder),
        ]
        evidence, _configurations, source = module.configuration_matrix(
            providers, 123, [asset]
        )
        failed = [item for item in evidence if not item["success"]]
        self.assertEqual(len(failed), 1)
        self.assertEqual(failed[0]["failure_class"], "rpc_error:3")
        self.assertEqual(failed[0]["asset"], asset)
        self.assertEqual(failed[0]["block_number"], 123)
        self.assertEqual(failed[0]["method_semantic_name"], "pool_configuration")
        self.assertEqual(source, "protocol_data_provider_field_set")

    def test_fallback_requires_field_complete_two_provider_agreement(self):
        def responder(provider, target, data):
            selector = data[2:10]
            if target == module.POOL and provider.label == "production-nownodes-arbitrum":
                raise module.ProviderDiagnosticError(
                    "rpc_error:3", provider.label, "eth_call", 1, "matrix", None,
                    None, "unavailable", 1, 1, 0
                )
            if target == module.POOL:
                return encoded_words(bitmap_fixture())
            if (
                provider.label == "production-slot-0"
                and selector == module.DATA_PROVIDER_SELECTORS["get_paused"]
            ):
                return encoded_words(1)
            return data_provider_result(selector)

        providers = [
            CallProvider("production-nownodes-arbitrum", responder),
            CallProvider("production-slot-0", responder),
        ]
        with self.assertRaisesRegex(module.ExportError, "provider disagreement"):
            module.configuration_matrix(providers, 100, ["0x" + "3" * 40])

    def test_missing_liquidation_protocol_fee_rejects(self):
        values = {
            "data_provider_configuration": encoded_words(
                18, 8000, 8250, 10500, 1000, 1, 1, 0, 1, 0
            ),
            "data_provider_paused": encoded_words(0),
            "data_provider_siloed": encoded_words(0),
            "data_provider_debt_ceiling": encoded_words(0),
        }
        with self.assertRaisesRegex(module.ExportError, "field-complete"):
            module._configuration_from_data_provider(values)

    def test_missing_paused_active_or_frozen_state_rejects(self):
        values = {
            "data_provider_configuration": encoded_words(
                18, 8000, 8250, 10500, 1000, 1, 1, 0, 1
            ),
            "data_provider_paused": encoded_words(0),
            "data_provider_liquidation_protocol_fee": encoded_words(1000),
            "data_provider_siloed": encoded_words(0),
            "data_provider_debt_ceiling": encoded_words(0),
        }
        with self.assertRaisesRegex(module.ExportError, "result"):
            module._configuration_from_data_provider(values)

    def test_oracle_disagreement_rejects(self):
        first = CallProvider(
            "production-nownodes-arbitrum",
            lambda *_args: encoded_words(1),
        )
        second = CallProvider(
            "production-slot-0",
            lambda *_args: encoded_words(2),
        )
        with self.assertRaisesRegex(module.ExportError, "provider disagreement"):
            module._agreed_eth_call(
                [first, second], module.ORACLE, "0x12345678", 100, "oracle"
            )

    def test_aave_v3_latest_answer_only_source_is_accepted(self):
        records = [
            oracle_record("production-nownodes-arbitrum"),
            oracle_record("production-slot-0"),
        ]
        result = module._select_oracle_policy("0x" + "3" * 40, records)
        self.assertEqual(result["oracle_semantics"], "aave_v3_latest_answer")
        self.assertFalse(result["round_metadata_supported"])
        self.assertEqual(result["source_latest_answer"], result["aave_oracle_price"])
        self.assertFalse(result["fallback_path_active"])

    def test_latest_round_data_revert_does_not_reject_latest_answer_adapter(self):
        records = [
            oracle_record("production-nownodes-arbitrum", round_failure="rpc_error:3"),
            oracle_record("production-slot-0", round_failure="rpc_error:3"),
        ]
        self.assertEqual(
            module._select_oracle_policy("0x" + "3" * 40, records)[
                "oracle_semantics"
            ],
            "aave_v3_latest_answer",
        )

    def test_aggregator_v3_round_data_receives_full_validation(self):
        records = [
            oracle_record("production-nownodes-arbitrum", round_supported=True),
            oracle_record("production-slot-0", round_supported=True),
        ]
        result = module._select_oracle_policy("0x" + "3" * 40, records)
        self.assertEqual(result["oracle_semantics"], "aggregator_v3_round_data")
        self.assertTrue(result["round_metadata_supported"])
        self.assertEqual(result["round_metadata"]["answer"], 100_000_000)

    def test_latest_answer_disagreement_rejects(self):
        records = [
            oracle_record("production-nownodes-arbitrum"),
            oracle_record("production-slot-0", answer=99_999_999),
        ]
        with self.assertRaisesRegex(module.CandidateEvidenceError, "latestAnswer"):
            module._select_oracle_policy("0x" + "3" * 40, records)

    def test_latest_answer_aave_price_mismatch_rejects(self):
        records = [
            oracle_record("production-nownodes-arbitrum", answer=99_999_999),
            oracle_record("production-slot-0", answer=99_999_999),
        ]
        with self.assertRaisesRegex(module.CandidateEvidenceError, "price mismatch"):
            module._select_oracle_policy("0x" + "3" * 40, records)

    def test_hidden_fallback_oracle_usage_rejects(self):
        records = [
            oracle_record("production-nownodes-arbitrum", source="0x" + "0" * 40),
            oracle_record("production-slot-0", source="0x" + "0" * 40),
        ]
        with self.assertRaisesRegex(module.CandidateEvidenceError, "fallback"):
            module._select_oracle_policy("0x" + "3" * 40, records)

    def test_oracle_source_address_disagreement_rejects(self):
        records = [
            oracle_record("production-nownodes-arbitrum"),
            oracle_record("production-slot-0", source="0x" + "5" * 40),
        ]
        with self.assertRaisesRegex(module.CandidateEvidenceError, "source disagreement"):
            module._select_oracle_policy("0x" + "3" * 40, records)

    def test_oracle_source_code_disagreement_rejects(self):
        records = [
            oracle_record("production-nownodes-arbitrum"),
            oracle_record("production-slot-0", code_hash="e" * 64),
        ]
        with self.assertRaisesRegex(module.CandidateEvidenceError, "code disagreement"):
            module._select_oracle_policy("0x" + "3" * 40, records)

    def test_unexpected_optional_provider_error_rejects(self):
        records = [
            oracle_record("production-nownodes-arbitrum", round_failure="transport_error"),
            oracle_record("production-slot-0", round_failure="rpc_error:3"),
        ]
        with self.assertRaisesRegex(module.CandidateEvidenceError, "unexpected"):
            module._select_oracle_policy("0x" + "3" * 40, records)

    def test_oracle_source_and_numeric_health_factors_survive_failure(self):
        records = [
            oracle_record("production-nownodes-arbitrum"),
            oracle_record("production-slot-0", answer=99_999_999),
        ]
        partial = {
            "signal_rows": [
                {
                    "borrower": "0x" + "1" * 40,
                    "classification": "exact_liquidatable_signal",
                    "agreed_account_data": {"health_factor_wad": module.WAD - 7},
                }
            ],
            "oracle_provider_evidence": records,
            "provider_request_usage": [
                {
                    "provider_id": "production-nownodes-arbitrum",
                    "json_rpc_item_count": 27,
                    "transport_request_count": 27,
                    "retry_count": 0,
                }
            ],
        }
        artifact = module.failure_artifact(
            module.CandidateEvidenceError(
                "oracle mismatch", partial, "candidate_oracle_semantics", "eth_call"
            )
        )
        summary = module._summary(artifact)
        self.assertEqual(summary["hf_classifications"][0]["health_factor_wad"], module.WAD - 7)
        self.assertEqual(
            summary["oracle_evidence"][0]["source"], "0x" + "4" * 40
        )
        self.assertEqual(summary["provider_request_usage"][0]["json_rpc_item_count"], 27)

    def test_health_factor_disagreement_rejects(self):
        protocol = {
            "total_collateral_base": 100,
            "total_debt_base": 50,
            "health_factor_wad": module.WAD,
        }
        derived = {
            "total_collateral_base": 100,
            "total_debt_base": 50,
            "health_factor_wad": module.WAD - 1,
        }
        with self.assertRaisesRegex(module.ExportError, "Health Factor"):
            module._require_protocol_derived_agreement(protocol, derived)

    def test_evidence_never_contains_raw_rpc_or_credentials(self):
        provider = CallProvider(
            "production-slot-0", lambda *_args: encoded_words(42)
        )
        _result, evidence = module._probe(
            provider,
            module.POOL,
            "0x12345678",
            100,
            "sample",
            "0x" + "3" * 40,
        )
        serialized = json.dumps(evidence)
        self.assertNotIn(encoded_words(42), serialized)
        self.assertNotIn("url", serialized.lower())
        self.assertNotIn("credential", serialized.lower())

    def test_provider_urls_are_absent_from_persisted_bindings_and_usage(self):
        provider = RefreshProvider("production-nownodes-arbitrum", authenticated=True)
        binding = module._provider_binding(provider)
        usage = module._sanitized_request_usage([provider])
        serialized = json.dumps({"binding": binding, "usage": usage})
        self.assertNotIn("https://", serialized)
        self.assertNotIn("endpoint_identity", serialized)
        self.assertNotIn("raw_rpc_response", serialized)

    def test_request_bound_fails_closed(self):
        cohort = cohort_fixture(["0x" + "1" * 40])
        providers = [
            RefreshProvider("production-nownodes-arbitrum", authenticated=True),
            RefreshProvider("production-slot-0"),
        ]
        providers[0]._request_id = 10
        providers[0].transport_request_count = 10
        with self.assertRaisesRegex(module.ExportError, "hard bound"):
            module.refresh_signals(providers, cohort, module.time.monotonic_ns())

    def test_candidate_authority_requires_complete_exact_contract(self):
        artifact = module.failure_artifact(module.ExportError("incomplete"))
        self.assertFalse(artifact["candidate_authority"])
        self.assertEqual(artifact["terminal"], "LIVE_PLATFORM_BLOCKER")

    def test_execution_authority_is_always_false(self):
        artifact = module.failure_artifact(module.ExportError("blocked"))
        summary = module._summary(artifact)
        self.assertFalse(artifact["execution_authority"])
        self.assertFalse(summary["execution_authority"])


if __name__ == "__main__":
    unittest.main()
