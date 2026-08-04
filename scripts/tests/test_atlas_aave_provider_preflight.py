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


if __name__ == "__main__":
    unittest.main()
