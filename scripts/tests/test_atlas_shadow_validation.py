"""Tests for the read-only SHADOW Atlas validation report analyzer."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = REPO_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

import atlas_shadow_validation_report as report  # noqa: E402


def canonical_payload() -> dict:
    return {
        "schema": "phoenix.atlas-shadow-validation.v1",
        "window": {
            "start": "2026-08-20T21:18:00Z",
            "end": "2026-08-21T21:18:00Z",
        },
        "coverage": {"relevant_ingress": 3000, "shadow_evaluated": 2970},
        "callback_simulation": {"attempted": 120, "passed": 119},
        "bid_ability": {
            "evaluated_rows": 2970,
            "eligible_rows": 9,
            "eligible_with_maximum_bid": 9,
            "rejected_rows": 2961,
        },
        "value_proxy": {
            "expected_net_after_bid_sum": "123456789",
            "conservative_net_after_bid_sum": "110000000",
        },
        "zero_invariants": {
            "atlas_solver_requests_total": 0,
            "execution_requests_total": 0,
            "active_attempts": 0,
            "unresolved_submissions": 0,
            "eligible_rows_with_rejection_reason": 0,
            "eligible_rows_without_maximum_bid": 0,
        },
    }


def run_analyzer(payload: str, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPTS / "atlas_shadow_validation_report.py"), *args],
        input=payload.encode("utf-8"),
        capture_output=True,
        timeout=60,
    )


class AtlasShadowValidationTests(unittest.TestCase):
    def test_canonical_payload_analyzes(self) -> None:
        result = report.analyze(canonical_payload())
        self.assertEqual(result["schema"], report.REPORT_SCHEMA)
        self.assertEqual(result["coverage"]["svr_coverage_bp"], 9900)
        self.assertEqual(result["callback_simulation"]["success_bp"], 9917)
        self.assertEqual(result["bid_ability"]["eligible_rows"], 9)
        self.assertEqual(result["value_proxy"]["expected_net_after_bid_sum"], "123456789")
        self.assertEqual(result["mode"], "SHADOW")
        self.assertEqual(result["financial_authority"], "CLOSED")
        self.assertEqual(result["warnings"], [])

    def test_canonical_stdin_exits_zero(self) -> None:
        completed = run_analyzer(json.dumps(canonical_payload()))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        text = completed.stdout.decode("utf-8")
        self.assertIn("SVR coverage: 9900 bp (2970/3000)", text)
        self.assertIn("Zero invariants: all zero", text)

    def test_zero_invariant_violation_fails(self) -> None:
        payload = canonical_payload()
        payload["zero_invariants"]["active_attempts"] = 1
        completed = run_analyzer(json.dumps(payload))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"zero_invariants_violated", completed.stderr)

    def test_eligible_row_with_rejection_reason_fails(self) -> None:
        payload = canonical_payload()
        payload["zero_invariants"]["eligible_rows_with_rejection_reason"] = 2
        with self.assertRaises(report.ValidationError) as caught:
            report.analyze(payload)
        self.assertIn("zero_invariants_violated", str(caught.exception))

    def test_schema_identity_is_enforced(self) -> None:
        payload = canonical_payload()
        payload["schema"] = "phoenix.other.v1"
        completed = run_analyzer(json.dumps(payload))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"schema_identity_invalid", completed.stderr)

    def test_window_order_is_enforced(self) -> None:
        payload = canonical_payload()
        payload["window"]["start"], payload["window"]["end"] = (
            payload["window"]["end"],
            payload["window"]["start"],
        )
        completed = run_analyzer(json.dumps(payload))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"window_order_invalid", completed.stderr)

    def test_negative_count_is_rejected(self) -> None:
        payload = canonical_payload()
        payload["coverage"]["relevant_ingress"] = -1
        completed = run_analyzer(json.dumps(payload))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"count_coverage_relevant_ingress_invalid", completed.stderr)

    def test_sum_must_be_signed_integer_text(self) -> None:
        payload = canonical_payload()
        payload["value_proxy"]["conservative_net_after_bid_sum"] = "12.5"
        completed = run_analyzer(json.dumps(payload))
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"sum_conservative_net_after_bid_sum_invalid", completed.stderr)

    def test_duplicate_json_keys_are_rejected(self) -> None:
        completed = run_analyzer('{"schema":"phoenix.atlas-shadow-validation.v1",'
                                 '"schema":"phoenix.atlas-shadow-validation.v1"}')
        self.assertEqual(completed.returncode, 1)

    def test_empty_ingress_has_null_coverage(self) -> None:
        payload = canonical_payload()
        payload["coverage"] = {"relevant_ingress": 0, "shadow_evaluated": 0}
        result = report.analyze(payload)
        self.assertIsNone(result["coverage"]["svr_coverage_bp"])

    def test_evaluated_above_ingress_is_capped_with_warning(self) -> None:
        payload = canonical_payload()
        payload["coverage"] = {"relevant_ingress": 10, "shadow_evaluated": 12}
        result = report.analyze(payload)
        self.assertEqual(result["coverage"]["svr_coverage_bp"], 10000)
        self.assertIn("shadow_evaluated_exceeds_relevant_ingress", result["warnings"])

    def test_window_argument_mismatch_fails(self) -> None:
        completed = run_analyzer(
            json.dumps(canonical_payload()),
            "--window-start",
            "2026-08-19T21:18:00Z",
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"window start mismatch", completed.stderr)

    def test_output_dir_writes_atomic_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            completed = run_analyzer(
                json.dumps(canonical_payload()),
                "--format",
                "json",
                "--output-dir",
                directory,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            artifact = Path(directory) / "phoenix-atlas-shadow-validation-2026-08-20.json"
            self.assertTrue(artifact.is_file())
            if sys.platform != "win32":
                self.assertEqual(artifact.stat().st_mode & 0o777, 0o644)
            persisted = json.loads(artifact.read_bytes())
            self.assertEqual(persisted["window"]["start"], "2026-08-20T21:18:00Z")
            self.assertEqual(persisted["zero_invariants"]["active_attempts"], 0)
            stdout = json.loads(completed.stdout)
            self.assertEqual(stdout, persisted)
            self.assertEqual(
                len([p for p in Path(directory).iterdir() if p.name.endswith(".tmp")]), 0
            )

    def test_missing_output_dir_fails(self) -> None:
        completed = run_analyzer(
            json.dumps(canonical_payload()), "--output-dir", "/nonexistent-phoenix-dir"
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn(b"output dir is unavailable", completed.stderr)


if __name__ == "__main__":
    unittest.main()
