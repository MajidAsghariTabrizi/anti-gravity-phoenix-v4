from __future__ import annotations

import argparse
from copy import deepcopy
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import tempfile
import unittest

from dashboard.snapshot_model import load_snapshot, read_artifact
from scripts import prelive_dashboard_live as live
from scripts import prelive_money_path_report as money_report
from scripts import prelive_shadow_control as control


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "fixtures"


class LiveDashboardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.generated_at = "2026-07-16T12:00:00Z"
        self.source = self.root / "source.json"
        self.source.write_bytes((FIXTURES / "dashboard" / "live-source.json").read_bytes())
        self.money_source = money_report.validate_source(
            money_report.load_path(
                FIXTURES / "reports" / "prelive_money_path_source.json",
                "source fixture",
            ),
            24,
        )
        self.money_metrics = money_report.validate_metrics(
            money_report.load_path(
                FIXTURES / "reports" / "prelive_money_path_metrics.json",
                "metrics fixture",
            )
        )
        self.money = self.root / "money.json"
        self.money.write_text(
            json.dumps(
                money_report.build_report(self.money_source, self.money_metrics)
            )
        )
        self.jetstream = FIXTURES / "control-plane" / "jetstream.json"
        self.runtime_fixture = json.loads(
            (FIXTURES / "dashboard" / "live-runtime-regression.json").read_text()
        )

        images = {}
        service_rows = []
        for index, service in enumerate(control.FULL_SERVICES, start=1):
            digest = f"sha256:{index:064x}"
            images[service] = f"registry.invalid/{service}@{digest}"
            service_rows.append(
                {
                    "service": service,
                    "state": "running_healthy",
                    "image_digest": digest,
                    "started_at": "2026-07-16T11:55:00Z",
                    "exit_code": None,
                    "oom": False,
                    "restart_count": 0,
                }
            )
        self.services = self.root / "services.json"
        self.services.write_text(
            json.dumps(
                {
                    "schema_version": live.SERVICES_SCHEMA,
                    "observed_at": self.generated_at,
                    "services": service_rows,
                }
            )
        )
        self.metadata = self.root / "metadata.json"
        self.metadata.write_text(
            json.dumps(
                {
                    "schema": "phoenix.production-render.v1",
                    "status": "ok",
                    "mode": "SHADOW",
                    "live_execution": False,
                    "release_sha": "a" * 40,
                    "route_registry_hash": "sha256:" + "b" * 64,
                    "images": images,
                }
            )
        )
        route_key = json.loads(self.source.read_text())["routes"][0]["route_key"]
        self.rendered = self.root / "compose.json"
        self.rendered.write_text(
            json.dumps(
                {
                    "services": {
                        "phoenix-engine": {
                            "environment": {
                                "ENGINE_ROUTE_REGISTRY_JSON": json.dumps(
                                    [{"route_id": route_key, "legs": []}],
                                    separators=(",", ":"),
                                )
                            }
                        }
                    }
                }
            )
        )
        self.identity_before = self.root / "identity-before.json"
        self.identity_current = self.root / "identity-current.json"
        identity = {"fingerprint_sha256": "sha256:" + "c" * 64}
        self.identity_before.write_text(json.dumps(identity))
        self.identity_current.write_text(json.dumps(identity))
        self.preflight = self.root / "preflight.tsv"
        self.preflight.write_text(
            "".join(
                f"{check}\tpass\t{self.generated_at}\n" for check in control.PREFLIGHT_CHECKS
            )
        )
        self.manifest = self.root / "release-manifest.json"
        self.checksum = self.root / "release-state.json"
        self.manifest.write_text('{"release":"bounded"}\n')
        self.checksum.write_text('{"checksum":"bounded"}\n')
        self.history = self.root / "history.ndjson"
        self.output = self.root / "dashboard"
        self.output.mkdir()
        self.candidate = self.output / "candidate-dashboard.json"
        self.artifact_manifest = self.root / "artifacts.json"

    def args(self) -> argparse.Namespace:
        return argparse.Namespace(
            money_path=str(self.money),
            source=str(self.source),
            jetstream=str(self.jetstream),
            services=str(self.services),
            release_metadata=str(self.metadata),
            rendered_compose=str(self.rendered),
            identity_before=str(self.identity_before),
            identity_current=str(self.identity_current),
            preflight=str(self.preflight),
            release_manifest=str(self.manifest),
            release_checksum=str(self.checksum),
            history=str(self.history),
            output_dir=str(self.output),
            candidate=str(self.candidate),
            artifact_manifest=str(self.artifact_manifest),
            disk_headroom_bytes="10737418240",
            rpc_calls_per_second="1",
        )

    def write_services_at(self, generated_at: str) -> None:
        services = json.loads(self.services.read_text())
        services["observed_at"] = generated_at
        self.services.write_text(json.dumps(services))

    def write_money_sample(
        self, generated_at: str, *, candidates: int, recorder_persisted: int
    ) -> None:
        source = deepcopy(self.money_source)
        source["generated_at"] = generated_at
        for section in ("engine", "outbox", "rpc", "fork"):
            for key in source[section]:
                source[section][key] = "0"
        source["engine"].update(
            {
                "candidate_count": str(candidates),
                "classifications_total": str(candidates),
                "decisions_total": str(candidates),
                "processing_attempts_total": str(candidates),
            }
        )
        source["outbox"].update(
            {
                "publish_attempts_total": str(candidates),
                "published_in_window": str(candidates),
            }
        )
        source["rpc"].update(
            {
                "records_total": str(candidates),
                "success_total": str(candidates),
            }
        )
        profitability = source["profitability"]
        for key in profitability:
            if key != "rejection_reasons":
                profitability[key] = "0"
        profitability.update(
            {
                "facts_total": str(candidates),
                "complete_total": str(candidates),
                "not_profitable_total": str(candidates),
                "rejected_total": str(candidates),
                "sum_expected_net_pnl": str(-10 * candidates),
                "sum_conservative_net_pnl": str(-12 * candidates),
                "sum_severe_net_pnl": str(-15 * candidates),
                "sum_total_cost": str(10 * candidates),
                "rejection_reasons": (
                    [{"reason": "liquidity_insufficient", "count": str(candidates)}]
                    if candidates
                    else []
                ),
            }
        )

        metrics = deepcopy(self.money_metrics)
        for row in metrics:
            row["value"] = "0"
            name = row["name"]
            labels = row["labels"]
            if name == "up" or name.endswith("_readiness") or name == "feed_data_completeness":
                row["value"] = "1"
            if name in {
                "feed_messages_total",
                "feed_normalized_transactions_total",
                "phoenix_official_router_inputs_total",
                "phoenix_supported_exact_input_inputs_total",
                "phoenix_configured_route_matches_total",
                "phoenix_engine_candidates_total",
                "phoenix_route_discovery_eligible_total",
                "rpc_state_requests_total",
                "rpc_primary_success_total",
            }:
                row["value"] = str(candidates)
            if name == "phoenix_profitability_primary_total":
                row["value"] = (
                    str(candidates) if labels.get("status") == "not_profitable" else "0"
                )
            if name in {
                "recorder_messages_received_total",
                "recorder_messages_persisted_total",
            }:
                row["value"] = str(recorder_persisted)
        self.money.write_text(json.dumps(money_report.build_report(source, metrics)))

    def assert_source_error(
        self, source: dict[str, object], expected: str
    ) -> None:
        with self.assertRaises(live.LiveDashboardError) as raised:
            live.validate_source(source)
        self.assertEqual(raised.exception.code, expected)

    def test_live_source_builds_redacted_integrity_checked_snapshot(self) -> None:
        snapshot, artifacts = live.build_snapshot(self.args())
        self.candidate.write_bytes(live.canonical_bytes(snapshot))
        loaded = load_snapshot(
            self.candidate,
            now=datetime(2026, 7, 16, 12, 1, tzinfo=timezone.utc),
        )
        self.assertEqual(loaded.data["safety"]["mode"], "SHADOW")
        self.assertFalse(loaded.data["safety"]["live_execution"])
        self.assertFalse(loaded.data["safety"]["execution_eligible"])
        self.assertFalse(loaded.data["safety"]["execution_request_created"])
        self.assertFalse(loaded.data["safety"]["signer_configured"])
        self.assertFalse(loaded.data["safety"]["wallet_configured"])
        self.assertFalse(loaded.data["safety"]["executor_configured"])
        self.assertEqual(loaded.data["business"]["sample_count"], "10")
        self.assertEqual(loaded.data["routes"][0]["active_shadow"], True)
        self.assertRegex(loaded.data["routes"][0]["route_id"], r"^route-[0-9a-f]{12}$")
        self.assertEqual(
            [row["consumer_id"] for row in loaded.data["jetstream"]["consumers"]],
            ["PHOENIX_RECORDER", "PHOENIX_ENGINE_SHADOW"],
        )
        self.assertEqual(len(artifacts), 7)
        serialized = json.dumps(snapshot)
        self.assertNotIn("arb1-weth", serialized)
        self.assertNotIn("registry.invalid", serialized)
        self.assertNotIn("primary-slot-a", serialized)

    def test_first_sample_preserves_unavailable_history_as_null(self) -> None:
        snapshot, _ = live.build_snapshot(self.args())
        self.assertIsNone(snapshot["postgres"]["growth_bytes_1h"])
        self.assertIsNone(snapshot["jetstream"]["persistence"]["throughput_per_second"])

    def test_full_history_windows_and_jetstream_interval_are_measured(self) -> None:
        rows = [
            ("2026-07-15T12:00:00Z", "500000000", "40", "2"),
            ("2026-07-16T06:00:00Z", "510000000", "50", "3"),
            ("2026-07-16T11:00:00Z", "520000000", "60", "4"),
            ("2026-07-16T11:59:30Z", "524000000", "70", "5"),
        ]
        self.history.write_text(
            "".join(
                json.dumps(
                    {
                        "schema_version": live.HISTORY_SCHEMA,
                        "observed_at": observed,
                        "database_size_bytes": size,
                        "recorder_messages_persisted": messages,
                        "jetstream_pending": pending,
                    },
                    separators=(",", ":"),
                )
                + "\n"
                for observed, size, messages, pending in rows
            )
        )
        snapshot, _ = live.build_snapshot(self.args())
        postgres = snapshot["postgres"]
        self.assertEqual(postgres["growth_bytes_1h"], "4288000")
        self.assertEqual(postgres["growth_bytes_6h"], "14288000")
        self.assertEqual(postgres["growth_bytes_24h"], "24288000")
        persistence = snapshot["jetstream"]["persistence"]
        self.assertEqual(persistence["throughput_per_second"], "1")
        self.assertEqual(persistence["backlog_growth"], "2")

    def test_stale_history_does_not_claim_an_exact_growth_window(self) -> None:
        now = datetime(2026, 7, 16, 12, 0, tzinfo=timezone.utc)
        rows = [
            {
                "schema_version": live.HISTORY_SCHEMA,
                "observed_at": live._canonical_timestamp(now - timedelta(hours=2)),
                "database_size_bytes": "500000000",
                "recorder_messages_persisted": "40",
                "jetstream_pending": "5",
            }
        ]
        self.assertIsNone(
            live._history_growth(rows, now, 1, live.Decimal("524288000"))
        )

    def test_normalize_services_uses_real_exit_and_oom_state(self) -> None:
        rows = []
        for index, service in enumerate(control.FULL_SERVICES, start=1):
            status = "exited" if service == "dashboard" else "running"
            health = "none" if status == "exited" else "healthy"
            exit_code = "137" if status == "exited" else "0"
            started = "2026-07-16T11:55:00Z"
            oom = "true" if status == "exited" else "false"
            rows.append(
                f"{service}\t{status}\t{health}\tsha256:{index:064x}\t0\t{exit_code}\t{started}\t{oom}"
            )
        raw = self.root / "services.tsv"
        raw.write_text("\n".join(rows) + "\n")
        normalized = live.normalize_services(raw, self.generated_at)
        dashboard = normalized["services"][-1]
        self.assertEqual(dashboard["state"], "stopped_failed")
        self.assertEqual(dashboard["exit_code"], 137)
        self.assertTrue(dashboard["oom"])

    def test_source_accounting_failure_is_rejected(self) -> None:
        source = json.loads(self.source.read_text())
        source["routes"][0]["total_cost"] = "2101"
        with self.assertRaises(live.LiveDashboardError) as raised:
            live.validate_source(source)
        self.assertEqual(raised.exception.code, "source_accounting_invalid")

    def test_future_evidence_window_is_rejected(self) -> None:
        source = json.loads(self.source.read_text())
        source["evidence_window_started_at"] = "2026-07-16T12:00:01Z"
        with self.assertRaisesRegex(live.LiveDashboardError, "source_window_invalid"):
            live.validate_source(source)

    def test_realistic_liquidity_rejection_builds_a_safe_snapshot(self) -> None:
        evidence = self.runtime_fixture["positive_route_evidence"]
        self.assertEqual(evidence["classification"], "candidate_rejected")
        self.assertEqual(evidence["rejection_reason"], "liquidity_insufficient")
        self.assertEqual(evidence["primary_provider_result"], "publicnode")
        self.assertEqual(evidence["verification_status"], "primary_only")
        self.assertEqual(
            evidence["independent_verification_lifecycle"], ["not_requested"]
        )
        self.assertTrue(evidence["primary_screen_rejected"])
        self.assertTrue(evidence["secondary_skipped"])
        self.assertFalse(evidence["execution_eligible"])
        self.assertFalse(evidence["execution_request_created"])
        self.assertFalse(any(key.startswith("secondary_") and key != "secondary_skipped" for key in evidence))

        source = deepcopy(self.runtime_fixture["dashboard_source"])
        live.validate_source(source)
        self.source.write_text(json.dumps(source))
        self.write_money_sample(source["generated_at"], candidates=1, recorder_persisted=1)
        self.write_services_at(source["generated_at"])
        snapshot, _ = live.build_snapshot(self.args())
        self.assertEqual(snapshot["business"]["sample_count"], "1")
        self.assertEqual(snapshot["profitability"]["summary"]["expected_net_pnl"], "-10")
        self.assertEqual(snapshot["fork"]["simulations"], "0")
        self.assertFalse(snapshot["safety"]["execution_eligible"])
        self.assertFalse(snapshot["safety"]["execution_request_created"])

    def test_post_restart_history_moves_from_zero_to_nonempty_monotonically(self) -> None:
        runtime_source = deepcopy(self.runtime_fixture["dashboard_source"])
        empty_source = deepcopy(runtime_source)
        empty_source.update(
            {
                "generated_at": "2026-07-16T12:00:00Z",
                "database_clock": "2026-07-16T12:00:00Z",
                "evidence_window_started_at": "2026-07-16T12:00:00Z",
                "routes": [],
                "distribution": [],
                "prediction_error": [],
                "daily_trend": [],
                "weekly_trend": [],
                "model_comparison": [],
                "providers": [],
            }
        )
        empty_source["database"]["oldest_relevant_event"] = None
        empty_source["database"]["newest_relevant_event"] = None
        empty_source["route_registry"] = {
            "fact_count": "0",
            "mismatch_count": "0",
            "self_verification_collisions": "0",
        }
        self.source.write_text(json.dumps(empty_source))
        self.write_money_sample(
            empty_source["generated_at"], candidates=0, recorder_persisted=0
        )
        self.write_services_at(empty_source["generated_at"])
        initial, _ = live.build_snapshot(self.args())
        self.assertEqual(initial["business"]["sample_count"], "0")
        self.assertTrue(all(row["count"] == "0" for row in initial["funnel"]))
        self.assertEqual(len(self.history.read_text().splitlines()), 1)

        self.source.write_text(json.dumps(runtime_source))
        self.write_money_sample(
            runtime_source["generated_at"], candidates=1, recorder_persisted=1
        )
        self.write_services_at(runtime_source["generated_at"])
        first_nonempty, _ = live.build_snapshot(self.args())
        self.assertEqual(first_nonempty["business"]["sample_count"], "1")
        self.assertEqual(len(self.history.read_text().splitlines()), 2)

        next_source = deepcopy(runtime_source)
        next_source["generated_at"] = "2026-07-16T12:00:20Z"
        next_source["database_clock"] = "2026-07-16T12:00:20Z"
        next_source["database"]["newest_relevant_event"] = "2026-07-16T12:00:15Z"
        next_source["routes"][0]["last_observed_at"] = "2026-07-16T12:00:15Z"
        self.source.write_text(json.dumps(next_source))
        self.write_money_sample(
            next_source["generated_at"], candidates=1, recorder_persisted=2
        )
        self.write_services_at(next_source["generated_at"])
        second_nonempty, _ = live.build_snapshot(self.args())
        self.assertEqual(second_nonempty["business"]["sample_count"], "1")
        self.assertEqual(len(self.history.read_text().splitlines()), 3)
        self.assertEqual(
            [json.loads(line)["observed_at"] for line in self.history.read_text().splitlines()],
            [
                "2026-07-16T12:00:00Z",
                "2026-07-16T12:00:10Z",
                "2026-07-16T12:00:20Z",
            ],
        )

    def test_source_value_failures_name_the_field_without_echoing_values(self) -> None:
        cases = (
            ("routes[0].total_cost", "10.5"),
            ("routes[0].expected", "+1"),
            ("routes[0].fork_balance_delta", "0.0000"),
            ("routes[0].liquidity_score_bps", "10001"),
            ("routes[0].route_key", ""),
            ("providers[0].provider_key", ""),
            ("daily_trend[0].period", "2026-02-30"),
            ("distribution[0].scenario", "unreviewed"),
            ("providers[0].p50_latency_ms", "1.5"),
        )
        for path, invalid in cases:
            with self.subTest(path=path):
                source = deepcopy(self.runtime_fixture["dashboard_source"])
                if path == "routes[0].total_cost":
                    source["routes"][0]["total_cost"] = invalid
                elif path == "routes[0].expected":
                    source["routes"][0]["expected"] = invalid
                elif path == "routes[0].fork_balance_delta":
                    source["routes"][0]["fork_balance_delta"] = invalid
                elif path == "routes[0].liquidity_score_bps":
                    source["routes"][0]["liquidity_score_bps"] = invalid
                elif path == "routes[0].route_key":
                    source["routes"][0]["route_key"] = invalid
                elif path == "providers[0].provider_key":
                    source["providers"][0]["provider_key"] = invalid
                elif path == "daily_trend[0].period":
                    source["daily_trend"][0]["period"] = invalid
                elif path == "distribution[0].scenario":
                    source["distribution"][0]["scenario"] = invalid
                elif path == "providers[0].p50_latency_ms":
                    source["providers"][0]["p50_latency_ms"] = invalid
                with self.assertRaises(live.LiveDashboardError) as raised:
                    live.validate_source(source)
                self.assertEqual(
                    raised.exception.code, f"source_value_invalid:{path}"
                )
                if invalid:
                    self.assertNotIn(invalid, raised.exception.code)

    def test_route_and_provider_accounting_remain_fail_closed(self) -> None:
        route_source = deepcopy(self.runtime_fixture["dashboard_source"])
        route_source["routes"][0]["total_cost"] = "11"
        self.assert_source_error(route_source, "source_accounting_invalid")

        provider_source = deepcopy(self.runtime_fixture["dashboard_source"])
        provider_source["providers"][0]["requests"] = "2"
        self.assert_source_error(provider_source, "source_accounting_invalid")

    def test_optional_provider_latency_accepts_null(self) -> None:
        source = deepcopy(self.runtime_fixture["dashboard_source"])
        source["providers"][0]["p50_latency_ms"] = None
        live.validate_source(source)

    def test_dashboard_rejects_each_missing_real_consumer(self) -> None:
        fixture = json.loads(self.jetstream.read_text())
        for consumer in control.JETSTREAM_CONSUMER_NAMES:
            with self.subTest(consumer=consumer):
                value = deepcopy(fixture)
                value["consumers"] = [
                    row for row in value["consumers"] if row["name"] != consumer
                ]
                path = self.root / f"{consumer}.json"
                path.write_text(json.dumps(value))
                args = self.args()
                args.jetstream = str(path)
                with self.assertRaises(ValueError) as raised:
                    live.build_snapshot(args)
                self.assertEqual(
                    getattr(raised.exception, "code", None),
                    "jetstream_metrics_invalid",
                )

    def test_missing_jetstream_count_is_not_rendered_as_zero(self) -> None:
        with self.assertRaisesRegex(live.LiveDashboardError, "jetstream_value_invalid"):
            live._resource_count({}, "num_pending")

    def test_identity_drift_remains_visible(self) -> None:
        self.identity_current.write_text(
            json.dumps({"fingerprint_sha256": "sha256:" + "d" * 64})
        )
        snapshot, _ = live.build_snapshot(self.args())
        self.assertEqual(
            snapshot["reliability"]["protected_service_identity_status"], "changed"
        )

    def test_final_control_evidence_is_attached_with_integrity(self) -> None:
        snapshot, _ = live.build_snapshot(self.args())
        latest = self.output / "latest-dashboard.json"
        latest.write_bytes(live.canonical_bytes(snapshot))
        candidate = self.output / "candidate-dashboard.json"
        live.attach_evidence(
            FIXTURES / "control-plane" / "valid-evidence.json",
            latest,
            candidate,
        )
        attached = load_snapshot(
            candidate,
            now=datetime(2026, 7, 16, 12, 1, tzinfo=timezone.utc),
        )
        artifact = next(
            row for row in attached.data["artifacts"] if row["kind"] == "evidence_bundle"
        )
        self.assertTrue(artifact["available"])
        self.assertGreater(len(read_artifact(attached, artifact)), 0)

    def test_evidence_attachment_cannot_escape_snapshot_directory(self) -> None:
        snapshot, _ = live.build_snapshot(self.args())
        latest = self.output / "latest-dashboard.json"
        latest.write_bytes(live.canonical_bytes(snapshot))
        outside = self.root / "outside"
        outside.mkdir()
        with self.assertRaisesRegex(live.LiveDashboardError, "snapshot_output_path_invalid"):
            live.attach_evidence(
                FIXTURES / "control-plane" / "valid-evidence.json",
                latest,
                outside / "candidate.json",
            )

    def test_sql_clips_to_control_baseline_and_audits_all_complete_facts(self) -> None:
        sql = (ROOT / "scripts" / "sql" / "prelive-dashboard-source.sql").read_text()
        self.assertIn("greatest(window_start, :'evidence_start'::timestamptz)", sql)
        self.assertIn("canonical_route_rows AS (", sql)
        self.assertIn(
            "CASE WHEN route.total_cost = trunc(route.total_cost)", sql
        )
        self.assertIn(
            "THEN route.total_cost::numeric(78,0)::text END AS total_cost_text",
            sql,
        )
        self.assertIn("canonical_daily_rows AS (", sql)
        self.assertIn("canonical_weekly_rows AS (", sql)
        self.assertIn("canonical_model_rows AS (", sql)
        self.assertIn("canonical_database_stats AS (", sql)
        self.assertNotIn("'total_cost', row.total_cost::text", sql)
        route_registry = sql.split("route_registry_stats AS (", 1)[1].split(")\nSELECT", 1)[0]
        self.assertIn("FROM complete_facts AS fact", route_registry)

    def test_artifact_retention_is_bounded_and_preserves_current_files(self) -> None:
        snapshot, _ = live.build_snapshot(self.args())
        latest = self.output / "latest-dashboard.json"
        latest.write_bytes(live.canonical_bytes(snapshot))
        current = {
            row["path"] for row in snapshot["artifacts"] if row["available"]
        }
        for index in range(40):
            (self.output / f"technical-{index:012x}.json").write_text("{}\n")
        live.prune_artifacts(self.output, latest, 2)
        remaining = {
            path.name
            for path in self.output.iterdir()
            if live.GENERATION_FILE_RE.fullmatch(path.name)
        }
        self.assertTrue(current.issubset(remaining))
        self.assertLessEqual(len(remaining), len(current) + 2 * len(live.ARTIFACT_KINDS))


if __name__ == "__main__":
    unittest.main()
