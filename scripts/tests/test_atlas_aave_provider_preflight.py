import unittest

from scripts import atlas_aave_provider_preflight as module


class SampleProvider:
    def __init__(self, implementation: str = module.POOL_IMPLEMENTATION):
        self.implementation = implementation

    def call(self, method, _params, attempts=1):
        self.attempts = attempts
        if method == "eth_getCode":
            return "0x60016000"
        if method == "eth_getStorageAt":
            return "0x" + "0" * 24 + self.implementation[2:]
        if method == "eth_call":
            return "0x" + "0" * 64
        raise AssertionError(method)


class AtlasAaveProviderPreflightTests(unittest.TestCase):
    def test_protocol_sample_is_exact_block_hash_only_evidence(self):
        sample = module._state_sample(SampleProvider(), 123)
        self.assertEqual(
            sample["implementation_storage_word"][-40:],
            module.POOL_IMPLEMENTATION[2:],
        )
        self.assertRegex(sample["code_sha256"], r"^[0-9a-f]{64}$")
        self.assertRegex(sample["reserves_call_sha256"], r"^[0-9a-f]{64}$")
        self.assertNotIn("60016000", sample["code_sha256"])

    def test_unreviewed_pool_implementation_fails_closed(self):
        with self.assertRaisesRegex(module.ExportError, "protocol state"):
            module._state_sample(SampleProvider("0x" + "1" * 40), 123)

    def test_bridge_failure_artifact_is_sanitized_and_stage_bound(self):
        error = module.BridgeRequestError(
            failure_class="bridge_invalid_json",
            provider_id="production-nownodes-arbitrum",
            method="eth_getCode",
            request_id=9,
            stage="nownodes_stability_round_2",
            stability_round=2,
            process_returncode=None,
            stderr_class="unavailable",
            json_rpc_item_count=9,
            transport_request_count=8,
            retry_count=0,
        )
        artifact = module.failure_artifact(error)
        self.assertEqual(artifact["status"], "failed_closed")
        self.assertEqual(artifact["failure_class"], "bridge_invalid_json")
        self.assertEqual(artifact["stage"], "nownodes_stability_round_2")
        self.assertEqual(artifact["stability_round"], 2)
        self.assertFalse(artifact["execution_authority"])
        self.assertNotIn("endpoint", artifact)
        self.assertNotIn("stderr", artifact)

    def test_non_bridge_failure_artifact_remains_fail_closed(self):
        artifact = module.failure_artifact(module.ExportError("sensitive detail"))
        self.assertEqual(artifact["failure_class"], "preflight_invariant_failed")
        self.assertNotIn("sensitive detail", str(artifact))

    def test_valid_rpc_failure_artifact_retains_sanitized_operation_identity(self):
        error = module.ProviderDiagnosticError(
            failure_class="rpc_error:-32000",
            provider_id="production-slot-0",
            method="eth_getProof",
            request_id=7,
            stage="proof_policy",
            stability_round=None,
            process_returncode=None,
            stderr_class="unavailable",
            json_rpc_item_count=7,
            transport_request_count=7,
            retry_count=0,
        )
        artifact = module.failure_artifact(error)
        self.assertEqual(artifact["failure_class"], "rpc_error:-32000")
        self.assertEqual(artifact["provider_id"], "production-slot-0")
        self.assertEqual(artifact["method"], "eth_getProof")
        self.assertEqual(artifact["stage"], "proof_policy")


if __name__ == "__main__":
    unittest.main()
