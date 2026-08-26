import json
import tempfile
import unittest
from pathlib import Path

from scripts import phoenix_executor_rotation_context as rotation


def plan() -> dict:
    return {
        "schema": "phoenix.executor-rotation.v1",
        "chain_id": 42161,
        "source_sha": "1" * 40,
        "base_release_sha": "2" * 40,
        "old_executor": rotation.OLD_EXECUTOR,
        "old_runtime_sha256": rotation.OLD_HASH,
        "expected_new_runtime_sha256": "3" * 64,
        "creation_bytecode_sha256": "4" * 64,
        "config_digest": "5" * 64,
        "owner": rotation.OWNER,
        "flash_provider": "0x794a61358d6845594f94dc1db02a252b5b4814ad",
        "atlas": "0x8ad1ae9d97c79aa68a0a151e83ff3942f68f86c1",
        "weth": rotation.WETH,
        "maximum_input_amount": rotation.MAX_INPUT,
        "searcher": rotation.OWNER,
        "assets": [rotation.WETH, rotation.NATIVE_USDC, "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8"],
        "routers": [
            "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45",
            "0xa51afafe0263b40edaef0df8781ea9aa03e381a3",
            "0xe592427a0aece92de3edee1f18e0157c05861564",
        ],
        "factory": rotation.FACTORY,
        "pools": [],
    }


def provenance() -> dict:
    return {
        "schema": "phoenix.executor-rotation.v1",
        "tooling_source_sha": "1" * 40,
        "base_release_sha": "2" * 40,
        "chain_id": 42161,
        "old_executor": rotation.OLD_EXECUTOR,
        "new_executor": "0x" + "6" * 40,
        "old_runtime_sha256": rotation.OLD_HASH,
        "new_runtime_sha256": "3" * 64,
        "creation_bytecode_sha256": "4" * 64,
        "config_digest": "5" * 64,
        "deployment_tx_hash": "0x" + "7" * 64,
        "deployment_block_number": 1,
        "cutover_tx_hashes": [],
        "rollback_tx_hash": None,
        "config_tx_hashes": ["0x" + "8" * 64],
        "config_verified": True,
        "pre_cutover_spl_absent": True,
        "old_bound_work_drained": True,
        "fenced_old_requests": 0,
        "cutover_started": False,
        "cutover_completed": False,
        "identity_consumers": [],
        "identity_consumers_verified": False,
        "rollback_used": False,
        "rollback_completed": False,
    }


class PhoenixExecutorRotationContextTests(unittest.TestCase):
    def test_materialization_changes_only_executor_identity(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            plan_path = root / "plan.json"
            state_path = root / "state.json"
            env_path = root / "phoenix.env"
            plan_path.write_text(json.dumps(plan()), encoding="utf-8")
            state_path.write_text(json.dumps(provenance()), encoding="utf-8")
            env_path.write_text(
                "PHOENIX_MODE=LIVE\nLIVE_EXECUTION=true\nAUTONOMOUS_EXECUTION=true\n"
                f"LIVE_EXECUTOR_EXECUTOR_ADDRESS={rotation.OLD_EXECUTOR}\n"
                f"LIVE_EXECUTOR_EXECUTOR_CODE_HASH={rotation.OLD_HASH}\n"
                "LIVE_EXECUTOR_MAX_INPUT_AMOUNT=10000000000000000\n"
                "LIVE_EXECUTOR_MIN_EXPECTED_PROFIT=1000000000000\n"
                "LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI=600000000000000\n",
                encoding="utf-8",
            )
            before = dict(line.split("=", 1) for line in env_path.read_text().splitlines())
            rotation.materialize(env_path, state_path, env_path, False, plan_path)
            after = dict(line.split("=", 1) for line in env_path.read_text().splitlines())
            self.assertEqual(after["LIVE_EXECUTOR_EXECUTOR_ADDRESS"], provenance()["new_executor"])
            self.assertEqual(after["LIVE_EXECUTOR_EXECUTOR_CODE_HASH"], provenance()["new_runtime_sha256"])
            self.assertEqual(
                {key: value for key, value in after.items() if "EXECUTOR_ADDRESS" not in key and "EXECUTOR_CODE_HASH" not in key},
                {key: value for key, value in before.items() if "EXECUTOR_ADDRESS" not in key and "EXECUTOR_CODE_HASH" not in key},
            )

    def test_rollback_can_be_claimed_exactly_once(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            state = Path(raw) / "state.json"
            value = provenance()
            value["identity_consumers"] = ["atlas-observer", "economic-supervisor", "live-executor", "phoenix-engine"]
            value["cutover_started"] = True
            value["cutover_completed"] = True
            state.write_text(json.dumps(value), encoding="utf-8")
            rotation.mark(state, plan(), "rollback_used")
            with self.assertRaisesRegex(ValueError, "ROTATION_ROLLBACK_REJECTED"):
                rotation.mark(state, plan(), "rollback_used")

    def test_rotation_spl_direct_simulation_uses_live_max_authority(self) -> None:
        repay = 194_262_222_175_986
        premium = 100_000_000_000
        expected_profit = 2_000_000_000_000
        output = repay + premium + expected_profit

        exact = {
            "schema_version": "phoenix.rpc.aave-exact-response.v5",
            "chain_id": 42161,
            "request_id": "exact",
            "block_number": 495_787_761,
            "block_hash": "0x" + "a" * 64,
            "state_root": "0x" + "b" * 64,
            "confirmation": None,
            "quorum": 1,
            "primary": {
                "provider_id": "production-nownodes-arbitrum",
                "liquidations": [{
                    "debt_asset": rotation.WETH,
                    "collateral_asset": rotation.NATIVE_USDC,
                    "debt_asset_price_base": "100000000",
                    "weth_price_base": "100000000",
                    "repay_amount": str(repay),
                    "flash_premium_amount": str(premium),
                    "liquidator_collateral": "200000000",
                    "unwind_quotes": [{
                        "pool": rotation.POOL,
                        "factory": rotation.FACTORY,
                        "fee": 100,
                        "zero_for_one": False,
                        "output_debt_asset": str(output),
                    }],
                }],
            },
        }

        request = rotation.simulation_request(plan(), provenance(), exact)
        simulation = request["simulations"][0]

        self.assertFalse(simulation["counterfactual"])
        self.assertEqual(simulation["repay_amount"], str(repay))
        self.assertEqual(
            simulation["maximum_input_amount"],
            str(rotation.MAX_INPUT),
        )
        self.assertEqual(
            simulation["live_maximum_input_amount"],
            str(rotation.MAX_INPUT),
        )
        self.assertEqual(
            simulation["maximum_input_weth_wei"],
            str(rotation.MAX_INPUT),
        )
        self.assertEqual(
            simulation["live_maximum_input_weth_wei"],
            str(rotation.MAX_INPUT),
        )

    def test_successful_single_primary_simulation_is_the_only_spl_proof(self) -> None:
        request = {
            "schema_version": "phoenix.rpc.aave-simulate-batch-request.v4",
            "chain_id": 42161,
            "request_id": "batch",
            "simulations": [{
                "request_id": "item", "block_number": 1,
                "block_hash": "0x" + "a" * 64, "state_root": "0x" + "b" * 64,
            }],
        }
        response = {
            "schema_version": "phoenix.rpc.aave-simulate-batch-response.v5",
            "chain_id": 42161, "request_id": "batch", "block_number": 1,
            "block_hash": "0x" + "a" * 64, "state_root": "0x" + "b" * 64,
            "primary_provider_id": "production-nownodes-arbitrum",
            "confirmation_provider_id": None, "quorum": 1,
            "evidence_mode": "SINGLE_PRIMARY_FORK_VERIFIED",
            "results": [{"request_id": "item", "error": None, "response": {"schema_version": "phoenix.rpc.aave-simulate-response.v6", "request_id": "item"}}],
        }
        with tempfile.TemporaryDirectory() as raw:
            state = Path(raw) / "state.json"
            state.write_text(json.dumps(provenance()), encoding="utf-8")
            rotation.verify_simulation(request, response, state, plan())
            self.assertTrue(json.loads(state.read_text())["pre_cutover_spl_absent"])
            response["results"][0] = {"request_id": "item", "response": None, "error": {"error_class": "provider_unavailable"}}
            with self.assertRaisesRegex(ValueError, "ROTATION_SPL_PROOF_FAILED"):
                rotation.verify_simulation(request, response, state, plan())

    def test_host_script_has_no_authority_or_shadow_mutation(self) -> None:
        root = Path(__file__).resolve().parents[2]
        source = (root / "scripts" / "rotate-phoenix-executor-live.sh").read_text(encoding="utf-8")
        self.assertIn("RELEASE_ENV=$DEPLOY_ROOT/current-release.env", source)
        self.assertNotIn(
            "RELEASE_ENV=/var/lib/phoenix-release/current-release.env", source
        )
        self.assertNotIn("production_mode.py shadow", source)
        self.assertNotIn("autonomous-control disarm", source)
        self.assertNotIn("arm-revenue", source)
        self.assertIn('$1=="RPC_AUTH_PROVIDER_ID"', source)
        self.assertNotIn('$1=="RPC_PRIMARY_PROVIDER_ID"', source)
        self.assertIn(
            'expected={"atlas-observer","economic-supervisor","live-executor","phoenix-engine"}',
            source,
        )
        self.assertIn("command_identity and name not in expected", source)
        self.assertIn("if set(names)!=expected: raise SystemExit(1)", source)
        self.assertIn(
            'key in {"LIVE_EXECUTOR_EXECUTOR_ADDRESS","EXECUTOR_ADDRESS"}',
            source,
        )
        self.assertGreaterEqual(source.count("addresses=sorted(set("), 2)
        self.assertGreaterEqual(source.count("hashes=sorted(set("), 2)
        self.assertIn("fail identity_consumer_environment_invalid", source)
        self.assertIn(
            '[ -z "$address" ] || [ -z "$command_address" ] || [ "$address" = "$command_address" ] || fail mixed_executor_address',
            source,
        )
        self.assertIn(
            '[ -z "$hash" ] || [ -z "$command_hash" ] || [ "$hash" = "$command_hash" ] || fail mixed_executor_hash',
            source,
        )
        self.assertIn("PHOENIX_MODE)\" = LIVE", source)
        self.assertIn("claim-rollback", source)
        self.assertIn("control_snapshot_changed", source)
        self.assertIn("drain-store", source)
        self.assertIn("--executor-address=", source)
        self.assertIn('"$SELF" drain "$PLAN" "$STATE"', source)
        store = (root / "live-executor" / "src" / "store.rs").read_text(encoding="utf-8")
        drain = store[store.index("pub async fn drain_executor_identity"):store.index("#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]")]
        self.assertIn("pg_advisory_xact_lock", drain)
        self.assertIn("FOR UPDATE", drain)
        self.assertIn("submission_unknown", drain)
        self.assertIn("status IN ('draft','approved')", drain)
        self.assertIn("solver_operation->>'from'", drain)
        self.assertIn("status = 'expired'", drain)
        for forbidden in ("revenue_lane_controls", "economic_control", "autonomous_global_control"):
            self.assertNotIn(forbidden, drain)

    def test_recover_existing_host_path_is_bounded_and_fail_closed(self) -> None:
        root = Path(__file__).resolve().parents[2]
        source = (root / "scripts" / "rotate-phoenix-executor-live.sh").read_text(encoding="utf-8")

        self.assertIn(
            "LEGACY_ROTATION_SOURCE_SHA="
            "79c364f8aa56b6b6e27cd74cd2167e75a0b13610",
            source,
        )
        self.assertIn("recover-existing)", source)
        self.assertIn("verify_recovery_authority_snapshot", source)
        self.assertIn("verify_rotation_lineage", source)
        self.assertIn("verify-tree", source)
        self.assertIn("PHOENIX_EXECUTOR_ROTATION_RECOVERY_OK", source)

        start = source.index('if [ "$mode" = recover-existing ]; then')
        end = source.index(
            "snapshot=$(authority_snapshot); "
            'verify_authority_snapshot "$snapshot"',
            start,
        )
        recovery = source[start:end]

        self.assertIn('verify_consumer_identity "$OLD_EXECUTOR" "$OLD_HASH"', recovery)
        self.assertIn("LIVE_EXECUTOR_RPC_HEADER_FILE", recovery)

        for forbidden in (
            "send_raw_transaction",
            "materialize-new",
            "mark-cutover",
            "claim-rollback",
            "production_mode.py",
            "autonomous-control disarm",
            "arm-revenue",
        ):
            self.assertNotIn(forbidden, recovery)

        rotation_main = (
            root / "live-executor" / "src" /
            "phoenix_executor_rotation_main.rs"
        ).read_text(encoding="utf-8")

        self.assertLess(
            rotation_main.index('if mode == "recover-existing"'),
            rotation_main.index(
                'required("PHOENIX_EXECUTOR_ROTATION_SIGNER_FILE")'
            ),
        )



if __name__ == "__main__":
    unittest.main()
