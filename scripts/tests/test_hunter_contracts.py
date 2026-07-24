import copy
import json
import tempfile
import unittest
from pathlib import Path

from scripts import hunter_contracts, release_assets


ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = ROOT / "fixtures" / "autonomous-hunter" / "v1"


class HunterContractsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.schema = hunter_contracts.load_json(
            ROOT / hunter_contracts.SCHEMA_RELATIVE_PATH
        )
        cls.valid = {
            path.stem: hunter_contracts.load_json(path)
            for path in sorted((FIXTURE_ROOT / "valid").glob("*.json"))
        }

    def test_fixture_suite_is_strict_hash_bound_and_cross_linked(self) -> None:
        self.assertEqual(
            hunter_contracts.validate_fixture_suite(ROOT),
            (9, 9),
        )

    def test_schema_exposes_every_a0_contract_as_a_strict_variant(self) -> None:
        expected = {
            "route_universe",
            "route_policy",
            "global_control",
            "route_control",
            "risk_snapshot",
            "submission_quote",
            "autonomous_candidate",
            "automatic_approval",
            "outcome",
        }
        self.assertTrue(expected.issubset(self.schema["$defs"]))
        for name in expected:
            with self.subTest(name=name):
                contract = self.schema["$defs"][name]
                self.assertEqual(contract["type"], "object")
                self.assertFalse(contract["additionalProperties"])
                self.assertIn("schema_version", contract["required"])

    def test_candidate_contains_every_durable_program_binding(self) -> None:
        required = set(self.schema["$defs"]["autonomous_candidate"]["required"])
        self.assertTrue(
            {
                "candidate_id",
                "opportunity_id",
                "origin_event_id",
                "chain_id",
                "route_fingerprint",
                "route_universe_hash",
                "route_policy_hash",
                "risk_policy_hash",
                "state_block_number",
                "state_block_hash",
                "state_hash",
                "selected_size",
                "predicted_gross_profit",
                "predicted_total_cost",
                "conservative_predicted_net_pnl",
                "plan_hash",
                "calldata_hash",
                "executor_address",
                "executor_code_hash",
                "submission_channel",
                "submission_quote_hash",
                "risk_snapshot_hash",
                "candidate_created_at",
                "candidate_expires_at",
                "status",
                "candidate_hash",
            }.issubset(required)
        )

    def test_outcome_classes_are_exactly_bounded(self) -> None:
        observed = set(
            self.schema["$defs"]["outcome"]["properties"]["outcome_class"]["enum"]
        )
        self.assertEqual(
            observed,
            {
                "confirmed_profitable",
                "confirmed_below_prediction",
                "confirmed_negative",
                "reverted",
                "not_included",
                "transaction_replaced",
                "receipt_timed_out",
                "submission_unknown",
                "submitted_too_late",
                "competitor_or_state_changed",
                "ordering_bid_too_low",
                "rpc_failure",
                "model_mismatch",
                "policy_rejected",
                "risk_rejected",
                "integrity_failure",
                "operator_killed",
            },
        )

    def test_hash_is_key_order_independent_and_value_sensitive(self) -> None:
        candidate = self.valid["autonomous-candidate"]
        reversed_candidate = dict(reversed(list(candidate.items())))
        self.assertEqual(
            hunter_contracts.canonical_hash(candidate),
            hunter_contracts.canonical_hash(reversed_candidate),
        )
        mutated = copy.deepcopy(candidate)
        mutated["selected_size"] = str(int(mutated["selected_size"]) + 1)
        self.assertNotEqual(
            hunter_contracts.canonical_hash(candidate),
            hunter_contracts.canonical_hash(mutated),
        )

    def test_automatic_approval_binds_candidate_economics_transitively(self) -> None:
        candidate = copy.deepcopy(self.valid["autonomous-candidate"])
        approval = copy.deepcopy(self.valid["automatic-approval"])
        original = hunter_contracts.canonical_hash(approval)
        candidate["predicted_total_cost"] = str(
            int(candidate["predicted_total_cost"]) + 1
        )
        approval["candidate_hash"] = hunter_contracts.canonical_hash(candidate)
        self.assertNotEqual(original, hunter_contracts.canonical_hash(approval))

    def test_loader_rejects_duplicate_keys_and_binary_floats(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            duplicate = root / "duplicate.json"
            duplicate.write_text('{"a":1,"a":2}\n', encoding="utf-8")
            floating = root / "floating.json"
            floating.write_text('{"amount":0.25}\n', encoding="utf-8")
            with self.assertRaisesRegex(
                hunter_contracts.ContractError, "duplicate JSON object key"
            ):
                hunter_contracts.load_json(duplicate)
            with self.assertRaisesRegex(
                hunter_contracts.ContractError, "floating point"
            ):
                hunter_contracts.load_json(floating)

    def test_release_universe_uses_only_previously_reviewed_identities(self) -> None:
        universe = hunter_contracts.load_json(
            ROOT / "config" / "phoenix-route-universe-v1.json"
        )
        proofs = json.loads(
            (
                ROOT / "fixtures" / "routes" / "arbitrum_uniswap_v3_pool_proofs.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(
            {factory["address"] for factory in universe["factories"]},
            {proofs["factory"]},
        )
        self.assertEqual(
            {pool["address"] for pool in universe["pools"]},
            {pool["pool_address"] for pool in proofs["pools"]},
        )
        self.assertEqual(universe["maximum_route_legs"], 4)

    def test_database_schema_is_additive_safe_and_service_owned(self) -> None:
        root_migrations = sorted(path.name for path in (ROOT / "migrations").glob("*.sql"))
        self.assertEqual(root_migrations[-1], "011_money_path_selective_persistence.sql")
        self.assertEqual(len(root_migrations), 11)
        sql = (
            ROOT / "live-executor" / "schema" / "003_autonomous_hunter_contracts.sql"
        ).read_text(encoding="utf-8")
        for table in (
            "live_canary.autonomous_global_control",
            "live_canary.autonomous_route_controls",
            "live_canary.autonomous_candidates",
            "live_canary.autonomous_approvals",
            "live_canary.autonomous_outcome_attributions",
        ):
            with self.subTest(table=table):
                self.assertIn(table, sql)
        self.assertIn("'phoenix.live-canary-schema.v2'", sql)
        self.assertIn("'phoenix.live-canary-schema.v3'", sql)
        self.assertIn("false,\n    true,\n    'disabled'", sql)
        self.assertIn("risk_policy_hash = route_policy_hash", sql)
        self.assertNotIn("SIGNER", sql.upper())
        self.assertNotIn("nonce_state", sql.lower())
        self.assertNotRegex(sql.lower(), r"\bnonce\s+numeric")

    def test_hunter_contracts_are_immutable_release_assets(self) -> None:
        static = set(release_assets.STATIC_PATHS)
        for path in (
            "config/phoenix-route-universe-v1.json",
            "docs/AUTONOMOUS_HUNTER_CONTRACTS_V1.md",
            "live-executor/schema/003_autonomous_hunter_contracts.sql",
            "scripts/hunter_contracts.py",
        ):
            self.assertIn(path, static)
        self.assertIn(
            "fixtures/autonomous-hunter/v1/**/*.json",
            release_assets.GLOB_PATHS,
        )
        self.assertIn("schemas/*.json", release_assets.GLOB_PATHS)

    def test_codeowners_covers_every_hunter_contract_surface(self) -> None:
        codeowners = (ROOT / ".github" / "CODEOWNERS").read_text(encoding="utf-8")
        for path in (
            "/schemas/phoenix-autonomous-hunter-v1.schema.json",
            "/config/phoenix-route-universe-v1.json",
            "/fixtures/autonomous-hunter/**",
            "/docs/AUTONOMOUS_HUNTER_CONTRACTS_V1.md",
            "/scripts/hunter_contracts.py",
            "/live-executor/**",
        ):
            self.assertIn(path, codeowners)


if __name__ == "__main__":
    unittest.main()
