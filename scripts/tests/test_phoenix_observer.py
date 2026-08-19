from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
OBSERVER = ROOT / "scripts" / "phoenix-observer.sh"
INSTALLER = ROOT / "scripts" / "install-phoenix-release-platform.sh"


class LiveExecutorExpectationTests(unittest.TestCase):
    def evidence(self) -> dict[str, object]:
        return {
            "activation_completed": False,
            "activation_path": {
                "active": True,
                "enabled": True,
                "sub_state": "waiting",
            },
            "active_attempts": 0,
            "armed": False,
            "contract_paused": True,
            "economic_phase": "DISARMED_EVIDENCE",
            "executable_route_count": 0,
            "execution_request_count": 0,
            "kill_switch": True,
            "observed_state": "stopped",
            "unresolved_submissions": 0,
        }

    def evaluate(self, evidence: dict[str, object]) -> dict[str, object]:
        completed = subprocess.run(
            ["/bin/bash", str(OBSERVER), "--evaluate-live-executor-expectation"],
            input=json.dumps(evidence),
            check=True,
            capture_output=True,
            text=True,
        )
        return json.loads(completed.stdout)

    def assert_critical(
        self,
        evidence: dict[str, object],
        expected_reason: str,
    ) -> None:
        result = self.evaluate(evidence)
        self.assertEqual(result["result"], "critical")
        self.assertEqual(result["observed_state"], "stopped")
        self.assertEqual(result["reason"], expected_reason)

    def test_valid_disarmed_evidence_with_stopped_executor_is_expected(self) -> None:
        result = self.evaluate(self.evidence())
        self.assertEqual(result["expected_state"], "stopped")
        self.assertEqual(result["observed_state"], "stopped")
        self.assertEqual(result["result"], "healthy_expected")
        self.assertEqual(result["reason"], "disarmed_evidence_contract_paused")

    def test_armed_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["armed"] = True
        self.assert_critical(evidence, "armed_control_executor_missing")

    def test_unpaused_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["contract_paused"] = False
        self.assert_critical(evidence, "unpaused_contract_executor_missing")

    def test_execution_request_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["execution_request_count"] = 1
        self.assert_critical(evidence, "execution_request_executor_missing")

    def test_active_attempt_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["active_attempts"] = 1
        self.assert_critical(evidence, "active_attempt_executor_missing")

    def test_unresolved_submission_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["unresolved_submissions"] = 1
        self.assert_critical(evidence, "unresolved_submission_executor_missing")

    def test_executable_phase_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["economic_phase"] = "LIVE_CANARY_MIN"
        self.assert_critical(evidence, "executable_phase_executor_missing")

    def test_completed_activation_with_missing_executor_is_critical(self) -> None:
        evidence = self.evidence()
        evidence["activation_completed"] = True
        self.assert_critical(evidence, "activation_completed_executor_missing")

    def test_running_executor_in_disarmed_evidence_is_hunting_standby(
        self,
    ) -> None:
        evidence = self.evidence()
        evidence["observed_state"] = "running"
        result = self.evaluate(evidence)
        self.assertEqual(result["expected_state"], "running")
        self.assertEqual(result["observed_state"], "running")
        self.assertEqual(result["result"], "healthy_expected")
        self.assertEqual(
            result["reason"], "hunting_standby_disarmed_evidence"
        )

    def test_running_executor_with_open_control_is_healthy_observed(self) -> None:
        evidence = self.evidence()
        evidence["observed_state"] = "running"
        evidence["armed"] = True
        result = self.evaluate(evidence)
        self.assertEqual(result["result"], "healthy_observed")
        self.assertEqual(result["reason"], "executable_runtime_observed")

    def test_running_executor_with_unpaused_contract_is_healthy_observed(
        self,
    ) -> None:
        evidence = self.evidence()
        evidence["observed_state"] = "running"
        evidence["contract_paused"] = False
        result = self.evaluate(evidence)
        self.assertEqual(result["result"], "healthy_observed")
        self.assertEqual(result["reason"], "executable_runtime_observed")

    def test_platform_installer_installs_the_versioned_observer(self) -> None:
        installer = INSTALLER.read_text(encoding="utf-8")
        self.assertIn("observer=/usr/local/sbin/phoenix-observer", installer)
        self.assertIn(
            'install -m 0755 -o root -g root "$script_dir/phoenix-observer.sh" "$observer"',
            installer,
        )


if __name__ == "__main__":
    unittest.main()
