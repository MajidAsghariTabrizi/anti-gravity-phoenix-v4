from __future__ import annotations

import copy
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts.phoenix_release import PROTOCOL_VERSION
from scripts.phoenix_release.controller import (
    ControllerError,
    REQUIRED_JOBS,
    is_docs_only,
    validate_source_ci,
)
from scripts.phoenix_release.gateway import (
    _bounded_output,
    _rollback_failed_state,
    _runtime_may_have_changed,
    _service_absence_allowed,
    GatewayError,
    HostPaths,
    expected_members,
    receive_package,
    reconcile_snapshot,
    request_file,
    resume,
    state_file,
    validate_request,
)
from scripts.phoenix_release.model import (
    PHASES,
    STATE_SCHEMA,
    StateError,
    advance,
    atomic_write,
    complete_failure_evidence,
    fail_state,
    load_state,
    new_state,
    rollback_phase,
    record_owner_transaction,
    record_engine_baseline,
    set_mutation_started,
)


ROOT = Path(__file__).resolve().parents[2]
RELEASE_SHA = "a" * 40
ROLLBACK_SHA = "b" * 40


def request() -> dict[str, object]:
    return {
        "schema": "phoenix.release-request.v1",
        "protocol_version": PROTOCOL_VERSION,
        "release_sha": RELEASE_SHA,
        "rollback_sha": ROLLBACK_SHA,
        "source_ci_run_id": 101,
        "source_ci_run_attempt": 1,
        "build_run_id": 202,
        "rollback_build_run_id": 203,
        "deploy_run_id": 303,
        "deploy_run_attempt": 1,
    }


def state() -> dict[str, object]:
    return new_state(
        release_sha=RELEASE_SHA,
        rollback_sha=ROLLBACK_SHA,
        source_ci_run_id=101,
        source_ci_run_attempt=1,
        build_run_id=202,
        deploy_run_id=303,
        deploy_run_attempt=1,
    )


def source_run() -> dict[str, object]:
    return {
        "id": 101,
        "run_attempt": 1,
        "head_sha": RELEASE_SHA,
        "head_branch": "main",
        "event": "push",
        "status": "completed",
        "conclusion": "success",
        "name": "Phoenix CI",
        "repository": {"full_name": "MajidAsghariTabrizi/anti-gravity-phoenix-v4"},
    }


def source_jobs() -> dict[str, object]:
    return {
        "jobs": [
            {"name": name, "status": "completed", "conclusion": "success"}
            for name in REQUIRED_JOBS
        ]
    }


def package_bytes(
    release_sha: str = RELEASE_SHA,
    *,
    link_member: str | None = None,
    extra_member: str | None = None,
) -> bytes:
    payloads = {
        name: (
            json.dumps(request()).encode()
            if name == "request.json"
            else f"fixture:{name}\n".encode()
        )
        for name in expected_members(release_sha)
    }
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for name, payload in sorted(payloads.items()):
            member = tarfile.TarInfo(name)
            if name == link_member:
                member.type = tarfile.SYMTYPE
                member.linkname = "request.json"
                member.size = 0
                archive.addfile(member)
            else:
                member.size = len(payload)
                archive.addfile(member, io.BytesIO(payload))
        if extra_member:
            payload = b"unexpected"
            member = tarfile.TarInfo(extra_member)
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))
    return output.getvalue()


class ReleaseStateTests(unittest.TestCase):
    def test_all_required_phases_are_canonical_and_ordered(self) -> None:
        self.assertEqual(
            PHASES,
            (
                "REQUESTED",
                "SOURCE_CI_VERIFIED",
                "BUILD_VERIFIED",
                "HOST_PREFLIGHT_STARTED",
                "HOST_PREFLIGHT_OK",
                "ACTIVE_CONTEXT_RECONCILED",
                "ROLLBACK_VERIFIED",
                "CANDIDATE_INSTALLED",
                "CANDIDATE_LIVE_RENDER_VERIFIED",
                "MIGRATIONS_APPLIED",
                "EVIDENCE_MODE_INSTALLED",
                "DISARMED_CONTROL_INSTALLED",
                "RPC_GATEWAY_HEALTHY",
                "ENGINE_HEALTHY",
                "ENGINE_BURN_IN_STARTED",
                "ENGINE_BURN_IN_PASSED",
                "POST_DISARMED_VERIFYING",
                "POST_DISARMED_VERIFIED",
                "DISARMED_EVIDENCE_STARTED",
                "COMPLETED",
            ),
        )

    def test_state_contains_the_full_release_identity(self) -> None:
        value = state()
        self.assertEqual(value["schema_version"], STATE_SCHEMA)
        self.assertEqual(value["release_sha"], RELEASE_SHA)
        self.assertEqual(value["rollback_sha"], ROLLBACK_SHA)
        self.assertFalse(value["mutation_started"])
        self.assertIn("owner_transaction_hash", value)
        self.assertIn("process_fatal_integrity_baseline", value)

    def test_successful_transitions_cannot_skip_a_postcondition(self) -> None:
        with self.assertRaisesRegex(StateError, "expected next phase"):
            advance(state(), "BUILD_VERIFIED")

    def test_completed_transition_is_idempotent(self) -> None:
        value = state()
        value = advance(value, "SOURCE_CI_VERIFIED")
        self.assertIs(advance(value, "SOURCE_CI_VERIFIED"), value)

    def test_pre_mutation_failure_is_distinct(self) -> None:
        value = fail_state(state(), code="HOST_PREFLIGHT_FAILED", evidence={})
        self.assertEqual(value["current_phase"], "FAILED_PRE_MUTATION")

    def test_post_mutation_failure_rolls_back_in_order(self) -> None:
        value = set_mutation_started(state())
        value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
        value = rollback_phase(value, "ROLLBACK_STARTED")
        value = rollback_phase(value, "ROLLED_BACK", {"status": "ok"})
        self.assertEqual(value["current_phase"], "ROLLED_BACK")
        self.assertEqual(value["rollback_result"], {"status": "ok"})

    def test_deploy_failure_evidence_can_be_completed_after_rollback(self) -> None:
        value = set_mutation_started(state())
        value = fail_state(
            value,
            code="DEPLOYMENT_FAILED",
            evidence={"source": "deploy-release", "detail": "deployment_failed"},
        )
        value = rollback_phase(value, "ROLLBACK_STARTED")
        value = rollback_phase(value, "ROLLED_BACK", {"status": "ok"})
        evidence = {
            "source": "deploy-release",
            "exit_code": 1,
            "output": '{"code":"RUNNING_IMAGE_MISMATCH"}',
        }
        value = complete_failure_evidence(value, evidence)
        self.assertEqual(value["failure_evidence"], evidence)
        self.assertIs(complete_failure_evidence(value, evidence), value)
        with self.assertRaisesRegex(StateError, "cannot be replaced"):
            complete_failure_evidence(
                value,
                {**evidence, "output": '{"code":"DIFFERENT_FAILURE"}'},
            )

    def test_failed_rollback_can_be_reconciled_to_rolled_back(self) -> None:
        value = set_mutation_started(state())
        value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
        value = rollback_phase(value, "ROLLBACK_STARTED")
        value = rollback_phase(value, "ROLLBACK_FAILED", {"status": "failed"})
        value = rollback_phase(value, "ROLLED_BACK", {"status": "ok"})
        self.assertEqual(value["current_phase"], "ROLLED_BACK")
        self.assertEqual(value["rollback_result"], {"status": "ok"})

    def test_owner_transaction_identity_cannot_be_replaced(self) -> None:
        transaction_hash = "0x" + "1" * 64
        value = record_owner_transaction(state(), transaction_hash)
        self.assertEqual(value["owner_transaction_hash"], transaction_hash)
        with self.assertRaisesRegex(StateError, "cannot be replaced"):
            record_owner_transaction(value, "0x" + "2" * 64)

    def test_engine_burn_in_baselines_are_persisted(self) -> None:
        value = record_engine_baseline(
            state(),
            container_id="1" * 64,
            restart_count=0,
            terminal_integrity=4,
            process_fatal_integrity=0,
        )
        self.assertEqual(value["engine_container_id"], "1" * 64)
        self.assertEqual(value["engine_restart_baseline"], 0)
        self.assertEqual(value["engine_terminal_integrity_baseline"], 4)
        self.assertEqual(value["process_fatal_integrity_baseline"], 0)

    def test_atomic_state_round_trip_is_root_style_single_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "releases" / RELEASE_SHA / "state.json"
            atomic_write(path, state())
            self.assertEqual(load_state(path)["release_sha"], RELEASE_SHA)
            self.assertEqual(path.stat().st_nlink, 1)

    def test_symlink_state_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target"
            target.write_text("{}")
            link = root / "state.json"
            try:
                link.symlink_to(target)
            except OSError:
                self.skipTest("symlink creation is unavailable")
            with self.assertRaises(StateError):
                load_state(link)


class ControllerEligibilityTests(unittest.TestCase):
    def verify(
        self,
        run: dict[str, object] | None = None,
        jobs: dict[str, object] | None = None,
        current_main: str = RELEASE_SHA,
    ) -> dict[str, object]:
        return validate_source_ci(
            run or source_run(),
            jobs or source_jobs(),
            repository="MajidAsghariTabrizi/anti-gravity-phoenix-v4",
            expected_sha=RELEASE_SHA,
            expected_run_id=101,
            expected_attempt=1,
            current_main_sha=current_main,
        )

    def test_exact_main_and_all_twelve_jobs_pass(self) -> None:
        evidence = self.verify()
        self.assertEqual(set(evidence["jobs"]), set(REQUIRED_JOBS))

    def test_stale_main_sha_is_rejected(self) -> None:
        with self.assertRaisesRegex(ControllerError, "current main"):
            self.verify(current_main="c" * 40)

    def test_failed_required_job_is_rejected(self) -> None:
        jobs = source_jobs()
        jobs["jobs"][0]["conclusion"] = "failure"  # type: ignore[index]
        with self.assertRaisesRegex(ControllerError, "did not pass"):
            self.verify(jobs=jobs)

    def test_missing_required_job_is_rejected(self) -> None:
        jobs = source_jobs()
        jobs["jobs"].pop()  # type: ignore[union-attr]
        with self.assertRaisesRegex(ControllerError, "missing"):
            self.verify(jobs=jobs)

    def test_duplicate_required_job_is_rejected(self) -> None:
        jobs = source_jobs()
        jobs["jobs"].append(copy.deepcopy(jobs["jobs"][0]))  # type: ignore[union-attr,index]
        with self.assertRaisesRegex(ControllerError, "duplicate"):
            self.verify(jobs=jobs)

    def test_foreign_repository_is_rejected(self) -> None:
        run = source_run()
        run["repository"] = {"full_name": "attacker/fork"}
        with self.assertRaisesRegex(ControllerError, "repository"):
            self.verify(run=run)

    def test_docs_only_release_is_skipped(self) -> None:
        self.assertTrue(is_docs_only(["docs/release-operations.md", "README.md"]))

    def test_workflow_or_runtime_change_is_not_docs_only(self) -> None:
        self.assertFalse(is_docs_only(["docs/note.md", ".github/workflows/ci.yml"]))
        self.assertFalse(is_docs_only(["scripts/deploy-release.sh"]))


class BoundedTransportTests(unittest.TestCase):
    def paths(self, root: Path) -> HostPaths:
        return HostPaths(
            state_root=root / "state",
            deploy_root=root / "deploy-root",
            env_file=root / "phoenix.env",
            libexec=root / "libexec",
        )

    def test_valid_package_is_received_once_with_durable_state(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            received = receive_package(io.BytesIO(package_bytes()), paths)
            self.assertEqual(received["release_sha"], RELEASE_SHA)
            state_path = paths.release_states / RELEASE_SHA / "state.json"
            self.assertEqual(load_state(state_path)["current_phase"], "REQUESTED")
            self.assertEqual(
                receive_package(io.BytesIO(package_bytes()), paths),
                received,
            )

    def test_unexpected_member_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(GatewayError, "PACKAGE_MEMBER"):
                receive_package(
                    io.BytesIO(package_bytes(extra_member="../escape")),
                    self.paths(Path(temporary)),
                )

    def test_receive_argument_is_bound_before_extraction(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            with self.assertRaisesRegex(
                GatewayError, "REQUEST_SHA_ARGUMENT_MISMATCH"
            ):
                receive_package(
                    io.BytesIO(package_bytes()),
                    paths,
                    expected_release_sha="c" * 40,
                )
            self.assertFalse((paths.incoming / RELEASE_SHA).exists())

    def test_symbolic_link_member_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(GatewayError, "PACKAGE_MEMBER_INVALID"):
                receive_package(
                    io.BytesIO(
                        package_bytes(link_member="release-manifest.json")
                    ),
                    self.paths(Path(temporary)),
                )

    def test_request_schema_rejects_unknown_keys(self) -> None:
        value = request()
        value["arbitrary_command"] = "sh"
        with self.assertRaisesRegex(GatewayError, "REQUEST_SCHEMA_INVALID"):
            validate_request(value)

    def test_request_rejects_same_release_and_rollback(self) -> None:
        value = request()
        value["rollback_sha"] = RELEASE_SHA
        with self.assertRaisesRegex(GatewayError, "REQUEST_SHA_PAIR_INVALID"):
            validate_request(value)

    def test_failure_output_redacts_rpc_and_database_urls(self) -> None:
        output = _bounded_output(
            "rpc=https://user:token@example.invalid/path "
            "db=postgres://user:password@example.invalid/db"
        )
        self.assertNotIn("token", output)
        self.assertNotIn("password", output)
        self.assertEqual(output.count("[REDACTED_URL]"), 2)

    def test_failure_output_preserves_early_structured_diagnostic(self) -> None:
        diagnostic = (
            '{"code":"RUNNING_IMAGE_MISMATCH","evidence":'
            '{"service":"phoenix-engine"}}'
        )
        output = _bounded_output(
            f"{diagnostic}\nHEALTH_FAIL: shadow-mode\n"
            + ("rollback-noise\n" * 500)
            + "ROLLBACK_OK\n"
        )
        self.assertIn(diagnostic, output)
        self.assertIn("HEALTH_FAIL: shadow-mode", output)
        self.assertIn("ROLLBACK_OK", output)
        self.assertLessEqual(len(output), 4096)

    def test_transport_has_no_eval_or_general_shell(self) -> None:
        transport = (ROOT / "scripts/phoenix-release-transport.sh").read_text()
        self.assertNotIn("eval ", transport)
        self.assertNotIn("sh -c", transport)
        self.assertIn("SSH_ORIGINAL_COMMAND", transport)
        self.assertIn("COMMAND_REJECTED", transport)
        for command in (
            "status",
            "history",
            "plan",
            "resume",
            "rollback",
            "emergency-pause",
            "evidence",
            "reconcile-active-context",
        ):
            self.assertIn(command, transport)

    def test_permanent_identity_has_forced_command_and_no_forwarding(self) -> None:
        installer = (ROOT / "scripts/install-phoenix-release-platform.sh").read_text()
        self.assertIn('restrict,command="%s"', installer)
        self.assertIn("AllowAgentForwarding no", installer)
        self.assertIn("AllowTcpForwarding no", installer)
        self.assertIn("PermitTTY no", installer)
        self.assertIn("PermitTunnel no", installer)
        self.assertIn("PermitUserRC no", installer)
        self.assertNotIn("NOPASSWD: ALL", installer)

    def test_platform_installs_every_context_safety_dependency(self) -> None:
        context_installer = (
            ROOT / "scripts/install-production-release-context.sh"
        ).read_text()
        dependency_block = re.search(
            r"for safety_script in \\\n(?P<body>.*?)\ndo\n",
            context_installer,
            re.DOTALL,
        )
        self.assertIsNotNone(dependency_block)
        dependencies = set(
            re.findall(
                r"^\s{2}([A-Za-z0-9_.-]+)(?: \\)?$",
                dependency_block.group("body"),
                re.MULTILINE,
            )
        )
        self.assertTrue(dependencies)
        platform_installer = (
            ROOT / "scripts/install-phoenix-release-platform.sh"
        ).read_text()
        installed = set(
            re.findall(r"'([A-Za-z0-9_.-]+):[0-7]{4}'", platform_installer)
        )
        self.assertEqual(dependencies - installed, set())

    def test_candidate_install_failure_rolls_back_in_same_resume(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            value = state()
            for phase in PHASES[1 : PHASES.index("CANDIDATE_INSTALLED")]:
                value = advance(value, phase)
            atomic_write(state_file(paths, RELEASE_SHA), value)
            request_path = request_file(paths, RELEASE_SHA)
            request_path.parent.mkdir(parents=True)
            request_path.write_text(json.dumps(request()), encoding="utf-8")

            with (
                patch(
                    "scripts.phoenix_release.gateway._install_candidate",
                    side_effect=GatewayError("CANDIDATE_INSTALL_FAILED"),
                ),
                patch(
                    "scripts.phoenix_release.gateway._rollback_failed_state"
                ) as rollback,
            ):
                with self.assertRaisesRegex(
                    GatewayError, "CANDIDATE_INSTALL_FAILED"
                ):
                    resume(paths, RELEASE_SHA)

            rollback.assert_called_once()
            rollback_state = rollback.call_args.args[1]
            self.assertEqual(rollback_state["current_phase"], "FAILED_POST_MUTATION")
            self.assertTrue(rollback_state["mutation_started"])

    def test_candidate_render_failure_needs_context_only_reconciliation(self) -> None:
        value = state()
        value = set_mutation_started(value)
        value = fail_state(
            value,
            code="DEPLOYMENT_FAILED",
            evidence={},
        )
        value["failure_phase"] = "CANDIDATE_LIVE_RENDER_VERIFIED"
        self.assertFalse(_runtime_may_have_changed(value))
        value["failure_phase"] = "MIGRATIONS_APPLIED"
        self.assertTrue(_runtime_may_have_changed(value))

    def test_failed_context_reconciliation_preserves_bounded_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            value = set_mutation_started(state())
            value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
            value = rollback_phase(value, "ROLLBACK_STARTED")
            with patch(
                "scripts.phoenix_release.gateway._require_success",
                side_effect=GatewayError(
                    "ROLLBACK_CONTEXT_INSTALL_FAILED",
                    {"exit_code": 17, "output": "expected context evidence"},
                ),
            ):
                recovered = _rollback_failed_state(paths, value)
            self.assertEqual(recovered["current_phase"], "ROLLBACK_FAILED")
            self.assertEqual(
                recovered["rollback_result"],
                {
                    "status": "failed",
                    "code": "ROLLBACK_CONTEXT_INSTALL_FAILED",
                    "evidence": {
                        "exit_code": 17,
                        "output": "expected context evidence",
                    },
                },
            )

    def test_resume_reconciles_context_only_rollback_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            value = state()
            for phase in PHASES[1 : PHASES.index("MIGRATIONS_APPLIED")]:
                value = advance(value, phase)
            value = set_mutation_started(value)
            value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
            value = rollback_phase(value, "ROLLBACK_STARTED")
            value = rollback_phase(
                value,
                "ROLLBACK_FAILED",
                {"status": "failed", "code": "ROLLBACK_FAILED"},
            )
            atomic_write(state_file(paths, RELEASE_SHA), value)
            request_path = request_file(paths, RELEASE_SHA)
            request_path.parent.mkdir(parents=True)
            request_path.write_text(json.dumps(request()), encoding="utf-8")

            with (
                patch(
                    "scripts.phoenix_release.gateway._require_success",
                    return_value="",
                ) as require_success,
                patch(
                    "scripts.phoenix_release.gateway.emergency_pause"
                ) as emergency_pause,
            ):
                recovered = resume(paths, RELEASE_SHA)

            require_success.assert_called_once()
            emergency_pause.assert_not_called()
            self.assertEqual(recovered["current_phase"], "ROLLED_BACK")
            self.assertIsNone(recovered["candidate_pointer"])
            self.assertEqual(
                recovered["rollback_result"]["runtime_reconciled"],
                False,
            )

    def test_failed_rollback_retry_keeps_terminal_state_and_new_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            value = set_mutation_started(state())
            value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
            value = rollback_phase(value, "ROLLBACK_STARTED")
            value = rollback_phase(
                value,
                "ROLLBACK_FAILED",
                {"status": "failed", "code": "FIRST_FAILURE"},
            )
            with patch(
                "scripts.phoenix_release.gateway._require_success",
                side_effect=GatewayError(
                    "ROLLBACK_CONTEXT_INSTALL_FAILED",
                    {"exit_code": 23},
                ),
            ):
                recovered = _rollback_failed_state(paths, value)

            self.assertEqual(recovered["current_phase"], "ROLLBACK_FAILED")
            self.assertEqual(
                recovered["completed_phases"].count("ROLLBACK_FAILED"),
                1,
            )
            self.assertEqual(
                recovered["rollback_result"]["evidence"],
                {"exit_code": 23},
            )

    def test_state_updater_uses_installed_package_layout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            self.assertEqual(
                paths.state_updater,
                paths.libexec / "phoenix_release" / "phase_update.py",
            )


class ReconciliationTests(unittest.TestCase):
    def snapshot(self) -> dict[str, object]:
        return {
            "environment_sha": RELEASE_SHA,
            "current_release": RELEASE_SHA,
            "release_assets_sha": RELEASE_SHA,
            "state_sha": RELEASE_SHA,
            "context_sha": RELEASE_SHA,
            "manifest_sha": RELEASE_SHA,
            "configured_images": {"phoenix-engine": "image@sha256:" + "1" * 64},
            "running_images": {"phoenix-engine": "image@sha256:" + "1" * 64},
        }

    def test_metadata_only_drift_rewrites_without_container_mutation(self) -> None:
        snapshot = self.snapshot()
        snapshot["state_sha"] = ROLLBACK_SHA
        result = reconcile_snapshot(snapshot)
        self.assertEqual(result["action"], "rewrite-metadata")
        self.assertFalse(result["container_mutation"])
        self.assertFalse(result["contract_mutation"])

    def test_genuine_image_mismatch_is_rejected_without_write(self) -> None:
        snapshot = self.snapshot()
        snapshot["running_images"] = {
            "phoenix-engine": "image@sha256:" + "2" * 64
        }
        result = reconcile_snapshot(snapshot)
        self.assertEqual(result["action"], "reject")
        self.assertEqual(result["code"], "RUNNING_IMAGE_MISMATCH")
        self.assertEqual(result["service"], "phoenix-engine")

    def test_old_live_state_with_actual_shadow_identity_is_metadata_drift(self) -> None:
        snapshot = self.snapshot()
        snapshot["state_sha"] = ROLLBACK_SHA
        snapshot["context_sha"] = ROLLBACK_SHA
        result = reconcile_snapshot(snapshot)
        self.assertEqual(
            result["stale_fields"], ["state_sha", "context_sha"]
        )

    def test_shadow_allows_explicit_live_only_service_to_be_absent(self) -> None:
        component = {"live_canary_only": True}
        self.assertTrue(_service_absence_allowed(component, "SHADOW"))

    def test_live_requires_explicit_live_only_service_to_exist(self) -> None:
        component = {"live_canary_only": True}
        self.assertFalse(_service_absence_allowed(component, "LIVE"))

    def test_shadow_still_requires_every_non_live_only_service(self) -> None:
        self.assertFalse(_service_absence_allowed({}, "SHADOW"))
        self.assertFalse(
            _service_absence_allowed({"live_canary_only": False}, "SHADOW")
        )


class WorkflowAndDeploymentContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (
            ROOT / ".github/workflows/phoenix-release-controller.yml"
        ).read_text()
        self.deploy = (ROOT / "scripts/deploy-release.sh").read_text()
        self.rollback = (ROOT / "scripts/rollback-release.sh").read_text()

    def test_controller_is_automatic_resumable_and_serialized(self) -> None:
        self.assertIn('workflows: ["Phoenix CI"]', self.workflow)
        self.assertIn("workflow_dispatch:", self.workflow)
        self.assertIn('cron: "*/10 * * * *"', self.workflow)
        self.assertIn("group: phoenix-production-release", self.workflow)
        self.assertIn("cancel-in-progress: false", self.workflow)

    def test_controller_requires_gate_exact_main_and_source_ci(self) -> None:
        self.assertIn("PHOENIX_AUTORELEASE_ENABLED", self.workflow)
        self.assertIn("exact protected main tip", self.workflow)
        self.assertIn("verify-source-ci", self.workflow)
        self.assertIn("docs-only", self.workflow)

    def test_controller_uses_no_powershell_or_general_scp(self) -> None:
        self.assertNotIn("powershell", self.workflow.lower())
        self.assertNotIn("pwsh", self.workflow.lower())
        self.assertNotIn("scp ", self.workflow)
        self.assertIn('"receive ${RELEASE_SHA}"', self.workflow)
        self.assertIn('"resume ${RELEASE_SHA}"', self.workflow)

    def test_controller_creates_deployment_and_bounded_evidence(self) -> None:
        self.assertIn("/deployments", self.workflow)
        self.assertIn("state=success", self.workflow)
        self.assertIn("state=failure", self.workflow)
        self.assertIn("phoenix-release-evidence-", self.workflow)
        self.assertIn("GITHUB_STEP_SUMMARY", self.workflow)
        self.assertIn('"evidence ${RELEASE_SHA}" >failed-release-evidence.json', self.workflow)
        self.assertIn(
            "mv failed-release-evidence.json release-evidence.json", self.workflow
        )

    def test_candidate_render_precedes_any_runtime_mutation(self) -> None:
        candidate = self.deploy.index("mark_phase CANDIDATE_LIVE_RENDER_VERIFIED")
        mutation = self.deploy.index("state_update mutation mutation_started")
        live_mode = self.deploy.index(
            'production_mode.py" live --env-file "$env_file"'
        )
        self.assertLess(candidate, mutation)
        self.assertLess(mutation, live_mode)

    def test_burn_in_preserves_the_disarmed_owner_boundary(self) -> None:
        burn_start = self.deploy.index("mark_engine_burn_in_started \\")
        burn_pass = self.deploy.index("mark_phase ENGINE_BURN_IN_PASSED")
        health = self.deploy.index('"$deploy_dir/production-healthcheck.sh"')
        fail_closed = self.deploy.index(
            "runtime controls are not fail-closed before evidence-start"
        )
        runtime_transition = self.deploy.index("autonomous-control evidence-start")
        runtime_verified = self.deploy.index(
            "runtime did not enter fail-closed DISARMED_EVIDENCE"
        )
        evidence = self.deploy.index("mark_phase DISARMED_EVIDENCE_STARTED")
        self.assertLess(burn_start, burn_pass)
        self.assertLess(burn_pass, health)
        self.assertLess(health, fail_closed)
        self.assertLess(fail_closed, runtime_transition)
        self.assertLess(runtime_transition, runtime_verified)
        self.assertLess(runtime_verified, evidence)
        self.assertIn("PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161", self.deploy)
        self.assertIn("verify_runtime_control_phase DISARMED_DEPLOY", self.deploy)
        self.assertIn("verify_runtime_control_phase DISARMED_EVIDENCE", self.deploy)
        self.assertIn("[ -z \"$(compose ps -q live-executor", self.deploy)
        self.assertIn("engine_process_fatal_integrity_total", self.deploy)
        self.assertNotIn("live-executor activate", self.deploy)
        self.assertNotIn("live-executor owner-unpause", self.deploy)
        self.assertNotIn("compose up -d --no-deps live-executor", self.deploy)
        self.assertNotIn("LIVE_EXECUTOR_SIGNER_FILE", self.deploy)

    def test_owner_bootstrap_is_separate_from_normal_deployment(self) -> None:
        activation = (ROOT / "scripts/activate-economic-canary.sh").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("activate-economic-canary.sh", self.deploy)
        self.assertIn("activate-ready-canary", activation)
        self.assertIn("live-executor owner-unpause", activation)
        self.assertIn("compose up -d --no-deps live-executor", activation)
        self.assertIn("PHOENIX_CANARY_READINESS_FILE", activation)
        self.assertIn("PHOENIX_AUTOMATION_AUTHORIZATION_FILE", activation)

    def test_post_live_verification_precedes_pointer_promotion(self) -> None:
        verify = self.deploy.index("mark_phase POST_DISARMED_VERIFIED")
        promote = self.deploy.index(
            'install_active_file "$pointer_candidate" "$current_file"'
        )
        complete = self.deploy.index("mark_phase COMPLETED")
        self.assertLess(verify, promote)
        self.assertLess(promote, complete)
        self.assertIn("context_validation_output=$(", self.deploy)
        self.assertIn("production release context validation failed", self.deploy)

    def test_health_checks_receive_explicit_release_phase_mode(self) -> None:
        self.assertIn("PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE", self.deploy)
        self.assertIn('PHOENIX_ENV_FILE="$env_file"', self.deploy)
        self.assertIn("PHOENIX_HEALTH_EXPECTED_MODE=SHADOW", self.rollback)
        self.assertIn('PHOENIX_ENV_FILE="$env_file"', self.rollback)
        live_health = self.deploy.index(
            "PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE"
        )
        live_reload = self.deploy.rfind("reload_environment", 0, live_health)
        live_assertion = self.deploy.find(
            "assert_live_environment", live_reload, live_health
        )
        self.assertGreaterEqual(live_reload, 0)
        self.assertGreaterEqual(live_assertion, 0)
        shadow_install = self.rollback.index('production_mode.py" shadow')
        shadow_health = self.rollback.index("PHOENIX_HEALTH_EXPECTED_MODE=SHADOW")
        self.assertLess(shadow_install, shadow_health)

    def test_failure_path_persists_rollback_phases(self) -> None:
        self.assertIn("state_update failure deployment_failed", self.deploy)
        self.assertIn("state_update rollback ROLLBACK_STARTED", self.deploy)
        self.assertIn("state_update rollback ROLLED_BACK", self.deploy)
        self.assertIn("state_update rollback ROLLBACK_FAILED", self.deploy)

    def test_isolated_python_entrypoint_works_without_executable_bits(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            destination = root / "phoenix_release"
            shutil.copytree(ROOT / "scripts/phoenix_release", destination)
            for path in destination.glob("*.py"):
                path.chmod(0o644)
            for entrypoint in ("cli.py", "phase_update.py"):
                with self.subTest(entrypoint=entrypoint):
                    result = subprocess.run(
                        [
                            sys.executable,
                            "-I",
                            str(destination / entrypoint),
                            "--help",
                        ],
                        check=False,
                        capture_output=True,
                        text=True,
                        cwd=root,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_no_secret_or_raw_transaction_is_written_to_evidence(self) -> None:
        gateway = (ROOT / "scripts/phoenix_release/gateway.py").read_text()
        self.assertNotIn("os.environ.copy().update", gateway)
        self.assertNotIn("private_key", gateway.lower())
        self.assertNotIn("signed_raw", gateway.lower())
        self.assertIn("Never leak paths", (
            ROOT / "scripts/phoenix_release/cli.py"
        ).read_text())


if __name__ == "__main__":
    unittest.main()
