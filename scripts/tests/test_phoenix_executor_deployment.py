import json
import unittest
from pathlib import Path


class PhoenixExecutorDeploymentSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.repo_root = Path(__file__).resolve().parents[2]
        cls.schema = json.loads(
            (
                cls.repo_root
                / "schemas"
                / "phoenix-executor-deployment.schema.json"
            ).read_text(encoding="utf-8")
        )

    def test_manifest_requires_bounded_public_deployment_evidence(self) -> None:
        self.assertEqual(
            set(self.schema["required"]),
            {
                "schema",
                "source_commit_sha",
                "chain_id",
                "deployer_address",
                "owner_address",
                "flash_provider_address",
                "contract_address",
                "deployment_transaction_hash",
                "bytecode_hash_algorithm",
                "creation_bytecode_hash",
                "runtime_bytecode_hash",
                "compiler",
            },
        )
        self.assertFalse(self.schema["additionalProperties"])
        self.assertEqual(
            self.schema["properties"]["source_commit_sha"]["pattern"],
            "^[0-9a-f]{40}$",
        )
        self.assertEqual(
            self.schema["properties"]["bytecode_hash_algorithm"]["const"],
            "keccak256",
        )

    def test_manifest_pins_arbitrum_owner_provider_and_compiler(self) -> None:
        properties = self.schema["properties"]
        self.assertEqual(properties["chain_id"]["const"], 42161)
        self.assertEqual(
            properties["owner_address"]["const"],
            "0x9F30c00B68F7C0eDb4b4117B9f04E0cA2EB2C17a",
        )
        self.assertEqual(
            properties["flash_provider_address"]["const"],
            "0x794a61358D6845594F94dc1DB02A252b5b4814aD",
        )
        self.assertEqual(
            properties["compiler"],
            {
                "type": "object",
                "additionalProperties": False,
                "required": [
                    "version",
                    "optimizer_enabled",
                    "optimizer_runs",
                ],
                "properties": {
                    "version": {"const": "0.8.24"},
                    "optimizer_enabled": {"const": True},
                    "optimizer_runs": {"const": 200},
                },
            },
        )

    def test_address_and_transaction_fields_allow_only_empty_placeholders_or_hashes(self) -> None:
        properties = self.schema["properties"]
        self.assertEqual(properties["contract_address"]["oneOf"][0], {"type": "null"})
        self.assertEqual(
            properties["deployment_transaction_hash"]["oneOf"][0],
            {"type": "null"},
        )
        self.assertEqual(
            properties["deployment_transaction_hash"]["oneOf"][1]["pattern"],
            "^0x[0-9a-f]{64}$",
        )

    def test_deployment_helper_pins_the_same_public_parameters(self) -> None:
        source = (
            self.repo_root / "contracts" / "script" / "DeployPhoenixExecutor.s.sol"
        ).read_text(encoding="utf-8")
        self.assertIn("ARBITRUM_ONE_CHAIN_ID = 42161", source)
        self.assertIn(
            "INITIAL_OWNER = 0x9F30c00B68F7C0eDb4b4117B9f04E0cA2EB2C17a",
            source,
        )
        self.assertIn(
            "FLASH_PROVIDER = 0x794a61358D6845594F94dc1DB02A252b5b4814aD",
            source,
        )
        self.assertNotIn("PRIVATE_KEY", source)
        self.assertNotIn("address(this)", source)
        self.assertIn("vm.startBroadcast()", source)
        self.assertIn("vm.stopBroadcast()", source)
        self.assertEqual(source.count("_assertNoApprovals(executor,"), 2)
        self.assertIn("_assertNoApprovals(executor, INITIAL_OWNER)", source)
        self.assertIn("_assertNoApprovals(executor, FLASH_PROVIDER)", source)
        for mutation in (
            ".setSearcher(",
            ".setAsset(",
            ".setRouter(",
            ".setFactory(",
            ".approvePool(",
            ".setMaximumInputAmount(",
            ".setPaused(false)",
        ):
            with self.subTest(mutation=mutation):
                self.assertNotIn(mutation, source)

    def test_ci_executes_the_real_deployment_entrypoint_without_broadcast(self) -> None:
        workflow = (self.repo_root / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        command = (
            "forge script script/DeployPhoenixExecutor.s.sol:"
            "DeployPhoenixExecutorScript --sig \"run()\" --chain 42161"
        )
        self.assertIn(command, workflow)
        solidity_job = workflow[
            workflow.index("  solidity:") : workflow.index("  python-dashboard:")
        ]
        self.assertNotIn("--broadcast", solidity_job)
        self.assertNotIn("--rpc-url", solidity_job)


if __name__ == "__main__":
    unittest.main()
