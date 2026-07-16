from __future__ import annotations

import argparse
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
        source = money_report.validate_source(
            money_report.load_path(
                FIXTURES / "reports" / "prelive_money_path_source.json",
                "source fixture",
            ),
            24,
        )
        metrics = money_report.validate_metrics(
            money_report.load_path(
                FIXTURES / "reports" / "prelive_money_path_metrics.json",
                "metrics fixture",
            )
        )
        self.money = self.root / "money.json"
        self.money.write_text(json.dumps(money_report.build_report(source, metrics)))
        self.jetstream = FIXTURES / "control-plane" / "jetstream.json"

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

    def test_live_source_builds_redacted_integrity_checked_snapshot(self) -> None:
        snapshot, artifacts = live.build_snapshot(self.args())
        self.candidate.write_bytes(live.canonical_bytes(snapshot))
        loaded = load_snapshot(
            self.candidate,
            now=datetime(2026, 7, 16, 12, 1, tzinfo=timezone.utc),
        )
        self.assertEqual(loaded.data["safety"]["mode"], "SHADOW")
        self.assertFalse(loaded.data["safety"]["live_execution"])
        self.assertEqual(loaded.data["business"]["sample_count"], "10")
        self.assertEqual(loaded.data["routes"][0]["active_shadow"], True)
        self.assertRegex(loaded.data["routes"][0]["route_id"], r"^route-[0-9a-f]{12}$")
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
            ("2026-07-15T12:00:00Z", "500000000", "40", "5"),
            ("2026-07-16T06:00:00Z", "510000000", "50", "6"),
            ("2026-07-16T11:00:00Z", "520000000", "60", "7"),
            ("2026-07-16T11:59:30Z", "524000000", "70", "8"),
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
