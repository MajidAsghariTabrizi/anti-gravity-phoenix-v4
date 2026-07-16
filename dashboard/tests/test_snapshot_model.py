from __future__ import annotations

import copy
import hashlib
import json
import shutil
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

from dashboard.snapshot_model import (
    MAX_LOG_ROWS,
    MAX_SNAPSHOT_BYTES,
    SnapshotError,
    canonical_snapshot_bytes,
    load_snapshot,
    read_artifact,
)


ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = ROOT / "fixtures" / "dashboard"
FIXTURE_PATH = FIXTURE_ROOT / "latest-dashboard.json"
FIXTURE_NOW = datetime(2026, 7, 16, 0, 1, tzinfo=timezone.utc)


class SnapshotModelTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))

    def write_snapshot(self, data: dict | None = None) -> Path:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        shutil.copyfile(FIXTURE_ROOT / "technical.json", root / "technical.json")
        shutil.copyfile(FIXTURE_ROOT / "business.json", root / "business.json")
        path = root / "latest-dashboard.json"
        path.write_text(json.dumps(data or self.fixture), encoding="utf-8")
        return path

    def assert_snapshot_error(self, code: str, path: Path) -> None:
        with self.assertRaises(SnapshotError) as raised:
            load_snapshot(path, now=FIXTURE_NOW)
        self.assertEqual(raised.exception.code, code)

    def test_complete_fixture_loads_with_review_conditions(self) -> None:
        snapshot = load_snapshot(FIXTURE_PATH, now=FIXTURE_NOW)
        self.assertEqual(snapshot.data["safety"]["mode"], "SHADOW")
        self.assertEqual(snapshot.gate_status, "review_required")
        self.assertEqual(
            [item["code"] for item in snapshot.alerts],
            [
                "feed_data_incomplete",
                "jetstream_backlog_growth",
                "verification_disagreement",
            ],
        )
        self.assertEqual(len(snapshot.data["services"]), 11)
        self.assertEqual(len(snapshot.data["funnel"]), 8)

    def test_valid_zeroes_are_not_treated_as_unavailable(self) -> None:
        data = copy.deepcopy(self.fixture)
        data["feed"].update(
            {
                "gap_count": "0",
                "missing_sequences": "0",
                "most_recent_gap_at": None,
                "completeness_status": "complete",
                "affected_windows": [],
            }
        )
        data["jetstream"]["persistence"]["backlog_growth"] = "0"
        data["rpc"]["disagreed"] = "0"
        snapshot = load_snapshot(self.write_snapshot(data), now=FIXTURE_NOW)
        self.assertEqual(snapshot.gate_status, "evidence_clear")
        self.assertEqual(snapshot.alerts, ())
        self.assertEqual(snapshot.data["feed"]["gap_count"], "0")

    def test_unavailable_observations_are_null_and_blocking(self) -> None:
        data = copy.deepcopy(self.fixture)
        data["rpc"]["providers"][0]["success_rate_bps"] = None
        data["rpc"]["providers"][0]["budget_utilization_bps"] = None
        data["jetstream"]["persistence"]["throughput_per_second"] = None
        data["jetstream"]["persistence"]["backlog_growth"] = None
        data["jetstream"]["persistence"]["database_write_latency_ms"] = {
            "p50": None,
            "p95": None,
            "p99": None,
        }
        data["postgres"]["growth_bytes_1h"] = None
        data["postgres"]["growth_bytes_6h"] = None
        data["postgres"]["growth_bytes_24h"] = None
        snapshot = load_snapshot(self.write_snapshot(data), now=FIXTURE_NOW)
        self.assertEqual(snapshot.gate_status, "blocked")
        self.assertTrue(
            {
                "database_growth_evidence_unavailable",
                "jetstream_rate_evidence_unavailable",
                "recorder_latency_distribution_unavailable",
                "rpc_provider_observation_unavailable",
            }.issubset(snapshot.gate_reasons)
        )
        self.assertIsNone(snapshot.data["postgres"]["growth_bytes_1h"])

    def test_missing_snapshot_is_not_rendered_as_zero(self) -> None:
        self.assert_snapshot_error(
            "snapshot_missing",
            Path(tempfile.gettempdir()) / "missing-phoenix-dashboard.json",
        )

    def test_snapshot_size_is_bounded(self) -> None:
        temp = tempfile.NamedTemporaryFile(delete=False)
        path = Path(temp.name)
        temp.write(b"x" * (MAX_SNAPSHOT_BYTES + 1))
        temp.close()
        self.addCleanup(path.unlink, missing_ok=True)
        self.assert_snapshot_error("snapshot_size_invalid", path)

    def test_duplicate_keys_and_non_finite_values_are_rejected(self) -> None:
        for raw, code in (
            (b'{"a":1,"a":2}', "snapshot_duplicate_key"),
            (b'{"a":NaN}', "snapshot_non_finite"),
        ):
            with self.subTest(code=code):
                temp = tempfile.NamedTemporaryFile(delete=False)
                path = Path(temp.name)
                temp.write(raw)
                temp.close()
                self.addCleanup(path.unlink, missing_ok=True)
                self.assert_snapshot_error(code, path)

    def test_redaction_rejects_urls_addresses_and_secret_like_text(self) -> None:
        unsafe_values = (
            "upstream at wss://relay.invalid/feed",
            "pool 0x1111111111111111111111111111111111111111",
            "password=not-allowed",
            "POSTGRES_DSN observed",
        )
        for unsafe in unsafe_values:
            with self.subTest(value=unsafe):
                data = copy.deepcopy(self.fixture)
                data["logs"][0]["message"] = unsafe
                self.assert_snapshot_error(
                    "snapshot_redaction_invalid", self.write_snapshot(data)
                )

    def test_log_rows_are_bounded(self) -> None:
        data = copy.deepcopy(self.fixture)
        data["logs"] = [copy.deepcopy(data["logs"][0]) for _ in range(MAX_LOG_ROWS + 1)]
        self.assert_snapshot_error("snapshot_bounds_invalid", self.write_snapshot(data))

    def test_funnel_accounting_must_balance(self) -> None:
        data = copy.deepcopy(self.fixture)
        data["funnel"][3]["count"] = "121"
        self.assert_snapshot_error(
            "snapshot_accounting_invalid", self.write_snapshot(data)
        )

    def test_cross_section_counts_must_agree(self) -> None:
        mutations = (
            (
                "independent",
                lambda data: data["business"].update(
                    {"independently_verified_count": "8"}
                ),
            ),
            ("samples", lambda data: data["business"].update({"sample_count": "121"})),
            ("fork", lambda data: data["fork"].update({"success": "4"})),
            (
                "route_pnl",
                lambda data: data["profitability"]["summary"].update(
                    {"expected_net_pnl": "5001"}
                ),
            ),
            (
                "active_top_three",
                lambda data: data["routes"][0].update({"rank": 4}),
            ),
            (
                "image_manifest",
                lambda data: data["services"][0].update(
                    {"image_digest": "not_available"}
                ),
            ),
            (
                "distribution",
                lambda data: data["profitability"]["distribution"][0].update(
                    {"count": "19"}
                ),
            ),
        )
        for label, mutate in mutations:
            with self.subTest(label=label):
                data = copy.deepcopy(self.fixture)
                mutate(data)
                self.assert_snapshot_error(
                    "snapshot_accounting_invalid", self.write_snapshot(data)
                )

    def test_on_demand_fork_image_may_be_unavailable(self) -> None:
        data = copy.deepcopy(self.fixture)
        fork = next(
            row for row in data["services"] if row["service"] == "fork-sandbox"
        )
        fork["image_digest"] = "not_available"
        snapshot = load_snapshot(self.write_snapshot(data), now=FIXTURE_NOW)
        self.assertEqual(snapshot.data["governance"]["image_manifest_matches"], True)

    def test_safety_breaches_are_visible_and_blocking_without_values(self) -> None:
        data = copy.deepcopy(self.fixture)
        data["safety"].update(
            {
                "mode": "LIVE",
                "live_execution": True,
                "prelive_lock": False,
                "execution_eligible": True,
                "execution_request_created": True,
                "signer_configured": True,
                "wallet_configured": True,
                "executor_configured": True,
                "submission_method_invocations": "1",
            }
        )
        snapshot = load_snapshot(self.write_snapshot(data), now=FIXTURE_NOW)
        codes = {item["code"] for item in snapshot.alerts}
        self.assertEqual(snapshot.gate_status, "blocked")
        self.assertTrue(
            {
                "mode_not_shadow",
                "live_execution_enabled",
                "prelive_lock_open",
                "execution_eligible",
                "execution_request_created",
                "sensitive_runtime_setting",
                "submission_method_invoked",
            }.issubset(codes)
        )
        self.assertNotIn("not-allowed", json.dumps(snapshot.alerts))

    def test_stale_and_future_snapshots_fail_the_gate(self) -> None:
        stale = load_snapshot(FIXTURE_PATH, now=FIXTURE_NOW + timedelta(minutes=10))
        future = load_snapshot(FIXTURE_PATH, now=FIXTURE_NOW - timedelta(minutes=3))
        self.assertIn("dashboard_data_stale", stale.gate_reasons)
        self.assertIn("snapshot_clock_skew", future.gate_reasons)
        self.assertEqual(stale.gate_status, "blocked")
        self.assertEqual(future.gate_status, "blocked")

    def test_available_artifacts_require_matching_size_and_digest(self) -> None:
        path = self.write_snapshot()
        snapshot = load_snapshot(path, now=FIXTURE_NOW)
        available = [item for item in snapshot.data["artifacts"] if item["available"]]
        self.assertEqual(
            [len(read_artifact(snapshot, item)) for item in available], [132, 126]
        )
        (path.parent / "technical.json").write_text("changed", encoding="utf-8")
        with self.assertRaises(SnapshotError) as raised:
            read_artifact(snapshot, available[0])
        self.assertEqual(raised.exception.code, "artifact_size_mismatch")

    def test_unavailable_and_traversal_artifacts_fail_closed(self) -> None:
        snapshot = load_snapshot(FIXTURE_PATH, now=FIXTURE_NOW)
        unavailable = next(
            item for item in snapshot.data["artifacts"] if not item["available"]
        )
        with self.assertRaises(SnapshotError) as raised:
            read_artifact(snapshot, unavailable)
        self.assertEqual(raised.exception.code, "artifact_unavailable")

        data = copy.deepcopy(self.fixture)
        data["artifacts"][0]["path"] = "../technical.json"
        self.assert_snapshot_error("snapshot_shape_invalid", self.write_snapshot(data))

    def test_digest_valid_artifact_content_is_still_redaction_checked(self) -> None:
        data = copy.deepcopy(self.fixture)
        path = self.write_snapshot(data)
        unsafe = b'{"endpoint":"https://provider.invalid"}\n'
        (path.parent / "technical.json").write_bytes(unsafe)
        data["artifacts"][0]["size_bytes"] = len(unsafe)
        data["artifacts"][0]["sha256"] = "sha256:" + hashlib.sha256(unsafe).hexdigest()
        path.write_text(json.dumps(data), encoding="utf-8")
        snapshot = load_snapshot(path, now=FIXTURE_NOW)
        with self.assertRaises(SnapshotError) as raised:
            read_artifact(snapshot, snapshot.data["artifacts"][0])
        self.assertEqual(raised.exception.code, "artifact_redaction_invalid")

    def test_artifact_sensitive_json_keys_are_rejected(self) -> None:
        data = copy.deepcopy(self.fixture)
        path = self.write_snapshot(data)
        unsafe = b'{"POSTGRES_DSN":"redacted"}\n'
        (path.parent / "technical.json").write_bytes(unsafe)
        data["artifacts"][0]["size_bytes"] = len(unsafe)
        data["artifacts"][0]["sha256"] = "sha256:" + hashlib.sha256(unsafe).hexdigest()
        path.write_text(json.dumps(data), encoding="utf-8")
        snapshot = load_snapshot(path, now=FIXTURE_NOW)
        with self.assertRaises(SnapshotError) as raised:
            read_artifact(snapshot, snapshot.data["artifacts"][0])
        self.assertEqual(raised.exception.code, "artifact_redaction_invalid")

    def test_canonical_serialization_is_deterministic_and_bounded(self) -> None:
        first = canonical_snapshot_bytes(copy.deepcopy(self.fixture))
        second = canonical_snapshot_bytes(copy.deepcopy(self.fixture))
        self.assertEqual(first, second)
        self.assertLessEqual(len(first), MAX_SNAPSHOT_BYTES)
        self.assertTrue(first.endswith(b"\n"))


if __name__ == "__main__":
    unittest.main()
