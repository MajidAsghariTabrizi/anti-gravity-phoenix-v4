import json
import unittest
from pathlib import Path

from scripts import hunter_contracts, release_assets


ROOT = Path(__file__).resolve().parents[2]


class HunterA1Tests(unittest.TestCase):
    def test_committed_candidate_is_canonical_a0_contract(self) -> None:
        validator = hunter_contracts._validator(ROOT)
        candidate = hunter_contracts.load_json(
            ROOT / "fixtures/hunter-a1/v1/autonomous-candidate.json"
        )
        validated = hunter_contracts.validate_document(candidate, validator)
        self.assertEqual(
            validated["candidate_hash"],
            "4fd4f9a648d4c83f3b690ffad030d4d47269b869c9dcda2e6ec309b4fc8c5144",
        )
        self.assertEqual(validated["status"], "materialized")

    def test_release_policy_is_the_reviewed_a0_policy(self) -> None:
        release_policy = hunter_contracts.load_json(
            ROOT / "config/phoenix-route-policy-v1.json"
        )
        reviewed_policy = hunter_contracts.load_json(
            ROOT / "fixtures/autonomous-hunter/v1/valid/route-policy.json"
        )
        hunter_contracts.validate_document(release_policy, hunter_contracts._validator(ROOT))
        for field in (
            "chain_id",
            "route_fingerprint",
            "route_universe_hash",
            "settlement_asset",
            "token_path",
            "pool_addresses",
            "factory_addresses",
            "protocol_ids",
            "fees",
            "directions",
        ):
            self.assertEqual(release_policy[field], reviewed_policy[field])
        self.assertTrue(release_policy["enabled_for_shadow"])
        self.assertTrue(release_policy["enabled_for_autonomous_live"])

    def test_revenue_report_is_bounded_and_honest(self) -> None:
        report = json.loads(
            (ROOT / "fixtures/hunter-a1/v1/revenue-replay-evidence.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(report["evidence_class"], "deterministic_fixture_replay")
        self.assertEqual(report["baseline_routes"], 1)
        self.assertEqual(report["enumerable_routes"], 2)
        self.assertEqual(report["reviewed_pools"], 2)
        self.assertEqual(report["new_reviewed_pools"], 0)
        self.assertEqual(report["events_processed"], 3)
        self.assertEqual(report["affected_routes_evaluated"], 1)
        self.assertEqual(report["qualified_candidates"], 1)
        self.assertEqual(report["duplicate_candidate_outputs"], 0)
        self.assertEqual(report["unmatched_candidate_outputs"], 0)
        self.assertIsNone(report["prediction_vs_fork_simulation_error_bps"])
        self.assertGreater(int(report["evaluation_latency_p50_ns"]), 0)
        self.assertGreaterEqual(
            int(report["evaluation_latency_p95_ns"]),
            int(report["evaluation_latency_p50_ns"]),
        )

    def test_cross_tick_fixture_is_bounded_and_bidirectional(self) -> None:
        fixture = json.loads(
            (ROOT / "fixtures/hunter-a1/v1/pinned-fork-cross-tick.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            fixture["schema_version"], "phoenix.hunter-pinned-fork-parity.v1"
        )
        self.assertEqual(
            fixture["evidence_class"], "deterministic_offline_pinned_state_vector"
        )
        self.assertEqual(
            {vector["direction"] for vector in fixture["vectors"]},
            {"zero_for_one", "one_for_zero"},
        )
        self.assertTrue(
            all(vector["expected_ticks_crossed"] == 1 for vector in fixture["vectors"])
        )

    def test_core_has_explicit_bounds_and_no_submission_surface(self) -> None:
        core = (ROOT / "phoenix-engine/src/hunter/mod.rs").read_text(encoding="utf-8")
        state = (ROOT / "rpc-gateway/src/hunter_state.rs").read_text(encoding="utf-8")
        for required in (
            "maximum_assets",
            "maximum_pools",
            "maximum_routes",
            "maximum_cycles_per_settlement_asset",
            "maximum_routes_per_pool",
            "maximum_affected_routes_per_event",
            "maximum_tick_words_per_pool",
            "maximum_initialized_ticks",
            "maximum_tick_crossings_per_leg",
            "maximum_size_probes",
            "maximum_local_refinements",
            "maximum_concurrent_evaluations",
            "maximum_candidate_outputs_per_event",
        ):
            self.assertIn(required, core)
        self.assertIn("MAX_CACHE_ENTRIES", state)
        self.assertIn("HunterMode::Live", core)
        self.assertNotIn("eth_sendRaw" + "Transaction", core)
        self.assertNotIn("SIGNER_PRIVATE_KEY", core)
        self.assertNotIn("private_key", core.lower())

    def test_release_components_remain_exactly_seven(self) -> None:
        components = json.loads(
            (ROOT / "release-components.json").read_text(encoding="utf-8")
        )["components"]
        self.assertEqual(len(components), 7)
        self.assertEqual(
            {item["name"] for item in components},
            {
                "dashboard",
                "feed-ingestor",
                "fork-sandbox",
                "live-executor",
                "phoenix-engine",
                "recorder",
                "rpc-gateway",
            },
        )

    def test_a1_artifacts_are_immutable_release_assets(self) -> None:
        paths = set(
            release_assets._collect_sources(
                ROOT, ROOT / "fork-sandbox/abi/PhoenixExecutor.json"
            )
        )
        for expected in (
            "config/phoenix-route-policy-v1.json",
            "docs/AUTONOMOUS_HUNTER_A1_REVENUE_EVIDENCE.md",
            "fixtures/hunter-a1/v1/autonomous-candidate.json",
            "fixtures/hunter-a1/v1/pinned-fork-cross-tick.json",
            "fixtures/hunter-a1/v1/revenue-replay-evidence.json",
            "phoenix-engine/examples/hunter_a1_replay.rs",
        ):
            self.assertIn(expected, paths)


if __name__ == "__main__":
    unittest.main()
