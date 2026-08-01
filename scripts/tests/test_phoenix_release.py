from __future__ import annotations

import copy
import hashlib
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
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

from scripts.phoenix_release import PROTOCOL_VERSION
from scripts.phoenix_release.controller import (
    ControllerError,
    REQUIRED_JOBS,
    is_docs_only,
    validate_source_ci,
)
from scripts.phoenix_release.cli import parser as release_parser
from scripts.phoenix_release.gateway import (
    _bounded_output,
    _is_stopped_live_executor,
    _live_executor_absence_is_fail_closed,
    _require_success,
    _rollback_failed_state,
    _runtime_may_have_changed,
    _service_absence_allowed,
    GatewayError,
    HostPaths,
    expected_members,
    production_readiness,
    receive_package,
    reconcile_snapshot,
    request_file,
    retry_rolled_back,
    retry_pre_mutation,
    resume,
    state_file,
    validate_request,
)
from scripts.production_context import (
    ContextError,
    validate_active as validate_active_context,
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
    sha256_file,
    retry_rolled_back_release,
)
from scripts.production_compose import (
    ProductionComposeError,
    build_compose_command,
    compose_environment,
)
from scripts.release_platform import (
    MANIFEST_PATH,
    PLATFORM_FILES,
    PlatformError,
    create_manifest,
    verify_installed,
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
        package_digest=f"sha256:{'9' * 64}",
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
    content_suffix: bytes = b"",
) -> bytes:
    payloads = {
        name: (
            json.dumps(request()).encode()
            if name == "request.json"
            else f"fixture:{name}\n".encode() + content_suffix
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
                "CANDIDATE_REHEARSED",
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
        self.assertEqual(value["release_attempt"], 1)

    def test_successful_rollback_can_start_one_new_exact_package_attempt(self) -> None:
        value = state()
        for phase in PHASES[1 : PHASES.index("CANDIDATE_REHEARSED") + 1]:
            value = advance(value, phase)
        value = set_mutation_started(value)
        value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
        value = rollback_phase(value, "ROLLBACK_STARTED")
        value = rollback_phase(value, "ROLLED_BACK", {"status": "ok"})
        value.update(
            {
                "active_release_pointer": ROLLBACK_SHA,
                "autonomous_armed": False,
                "candidate_pointer": None,
                "contract_paused": True,
                "kill_switch": True,
            }
        )

        retried = retry_rolled_back_release(value)

        self.assertEqual(retried["current_phase"], "BUILD_VERIFIED")
        self.assertEqual(retried["release_attempt"], 2)
        self.assertFalse(retried["mutation_started"])
        self.assertEqual(retried["candidate_pointer"], RELEASE_SHA)
        self.assertIsNone(retried["rollback_result"])

    def test_rolled_back_retry_rejects_owner_transaction_or_open_control(self) -> None:
        value = state()
        for phase in PHASES[1 : PHASES.index("CANDIDATE_REHEARSED") + 1]:
            value = advance(value, phase)
        value = set_mutation_started(value)
        value = fail_state(value, code="DEPLOYMENT_FAILED", evidence={})
        value = rollback_phase(value, "ROLLBACK_STARTED")
        value = rollback_phase(value, "ROLLED_BACK", {"status": "ok"})
        value.update(
            {
                "active_release_pointer": ROLLBACK_SHA,
                "autonomous_armed": False,
                "candidate_pointer": None,
                "contract_paused": True,
                "kill_switch": True,
            }
        )
        owner_transaction = copy.deepcopy(value)
        owner_transaction["owner_transaction_hash"] = "0x" + "1" * 64
        with self.assertRaisesRegex(StateError, "owner transaction"):
            retry_rolled_back_release(owner_transaction)
        value["autonomous_armed"] = True
        with self.assertRaisesRegex(StateError, "fail-closed"):
            retry_rolled_back_release(value)

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


class CanonicalComposeAndPlatformTests(unittest.TestCase):
    def test_route_registry_cannot_bypass_exact_env_file_precedence(self) -> None:
        environment = compose_environment(
            Path("/etc/phoenix/phoenix.env"),
            Path("/opt/phoenix/deploy/current-release.env"),
            {
                "ENGINE_ROUTE_REGISTRY_JSON": '[{"route_id":"stale"}]',
                "UNRELATED_PARENT_VALUE": "preserved",
            },
        )
        self.assertNotIn("ENGINE_ROUTE_REGISTRY_JSON", environment)
        self.assertEqual(environment["UNRELATED_PARENT_VALUE"], "preserved")
        self.assertEqual(
            environment["PHOENIX_RELEASE_ENV"],
            str(Path("/opt/phoenix/deploy/current-release.env")),
        )

    def test_one_builder_preserves_exact_live_overlay_without_duplicates(self) -> None:
        command = build_compose_command(
            mode="LIVE",
            env_file=Path("/etc/phoenix/phoenix.env"),
            release_env=Path("/opt/phoenix/deploy/current-release.env"),
            compose_file=Path("/opt/phoenix/deploy/compose.prod.yml"),
            overlay_file=Path(
                "/opt/phoenix/deploy/compose.live-autonomous.yml"
            ),
            arguments=["config", "--format", "json"],
        )
        self.assertEqual(command.count("--env-file"), 2)
        self.assertEqual(command.count("-f"), 2)
        self.assertEqual(command.count("--profile"), 1)
        self.assertEqual(
            command,
            [
                "/usr/bin/docker",
                "compose",
                "--env-file",
                "/etc/phoenix/phoenix.env",
                "--env-file",
                "/opt/phoenix/deploy/current-release.env",
                "-f",
                "/opt/phoenix/deploy/compose.prod.yml",
                "-f",
                "/opt/phoenix/deploy/compose.live-autonomous.yml",
                "--profile",
                "live-autonomous",
                "--project-directory",
                "/opt/phoenix/deploy",
                "config",
                "--format",
                "json",
            ],
        )

    def test_shadow_context_rejects_a_wrong_overlay(self) -> None:
        with self.assertRaisesRegex(
            ProductionComposeError, "must not include an overlay"
        ):
            build_compose_command(
                mode="SHADOW",
                env_file=Path("/etc/phoenix/phoenix.env"),
                release_env=Path(
                    "/opt/phoenix/deploy/current-release.env"
                ),
                compose_file=Path("/opt/phoenix/deploy/compose.prod.yml"),
                overlay_file=Path(
                    "/opt/phoenix/deploy/compose.live-autonomous.yml"
                ),
                arguments=["config"],
            )

    def test_platform_manifest_binds_every_installed_file_to_exact_sha(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_root = root / "source"
            installed_root = root / "installed"
            for index, (source, installed, mode) in enumerate(
                PLATFORM_FILES
            ):
                source_path = source_root / source
                source_path.parent.mkdir(parents=True, exist_ok=True)
                source_path.write_text(f"platform-{index}\n", encoding="utf-8")
                destination = installed_root / installed.removeprefix("/")
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source_path, destination)
                destination.chmod(mode)
            manifest = create_manifest(source_root, RELEASE_SHA)
            manifest_path = installed_root / MANIFEST_PATH.removeprefix("/")
            manifest_path.parent.mkdir(parents=True, exist_ok=True)
            manifest_path.write_text(
                json.dumps(manifest, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )

            real_lstat = Path.lstat

            def root_owned_lstat(path: Path) -> os.stat_result:
                metadata = list(real_lstat(path))
                metadata[4] = 0
                metadata[5] = 0
                return os.stat_result(metadata)

            # This manifest/hash fixture runs unprivileged in CI. Preserve the
            # real file metadata while modeling the root ownership enforced by
            # the production installer and verifier.
            with patch.object(Path, "lstat", root_owned_lstat):
                verified = verify_installed(installed_root, RELEASE_SHA)
                self.assertEqual(verified["release_sha"], RELEASE_SHA)
                tampered = (
                    installed_root
                    / PLATFORM_FILES[0][1].removeprefix("/")
                )
                tampered.write_text("stale\n", encoding="utf-8")
                with self.assertRaisesRegex(
                    PlatformError, "does not match manifest"
                ):
                    verify_installed(installed_root, RELEASE_SHA)


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

    def failed_pre_mutation_fixture(
        self, paths: HostPaths
    ) -> dict[str, object]:
        incoming = paths.incoming / RELEASE_SHA
        incoming.mkdir(parents=True)
        request_file(paths, RELEASE_SHA).write_text(
            json.dumps(request()), encoding="utf-8"
        )
        images = {
            f"service-{index}": {
                "repository": f"ghcr.io/example/service-{index}",
                "digest": f"sha256:{index}" + "0" * 63,
            }
            for index in range(1, 8)
        }
        (incoming / "release-provenance.json").write_text(
            json.dumps(
                {
                    "release_sha": RELEASE_SHA,
                    "build_run_id": 202,
                    "source_ci": {"run_id": 101, "run_attempt": 1},
                }
            ),
            encoding="utf-8",
        )
        manifest = incoming / "release-manifest.json"
        manifest.write_text(
            json.dumps({"release_sha": RELEASE_SHA, "images": images}),
            encoding="utf-8",
        )
        assets = incoming / f"phoenix-release-assets-{RELEASE_SHA}.tar.gz"
        assets.write_bytes(b"verified release assets")
        package = incoming / "release-package.tar.gz"
        package.write_bytes(b"exact immutable release package")

        value = state()
        value["package_digest"] = sha256_file(package)
        value = advance(
            value,
            "SOURCE_CI_VERIFIED",
            updates={
                "expected_images": {
                    name: f"{image['repository']}@{image['digest']}"
                    for name, image in images.items()
                }
            },
        )
        value = advance(
            value,
            "BUILD_VERIFIED",
            updates={
                "release_manifest_digest": sha256_file(manifest),
                "release_assets_digest": sha256_file(assets),
            },
        )
        value = advance(value, "HOST_PREFLIGHT_STARTED")
        value = advance(
            value,
            "HOST_PREFLIGHT_OK",
            updates={
                "contract_paused": True,
                "autonomous_armed": False,
                "kill_switch": True,
            },
        )
        value = fail_state(
            value,
            code="ACTIVE_SERVICE_MISSING",
            evidence={"service": "live-executor"},
        )
        atomic_write(state_file(paths, RELEASE_SHA), value)
        return value

    def test_production_readiness_aggregates_every_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            with self.assertRaisesRegex(
                GatewayError, "PRODUCTION_READINESS_FAILED"
            ) as raised:
                production_readiness(paths, RELEASE_SHA)
            evidence = raised.exception.evidence
            self.assertEqual(evidence["status"], "failed")
            self.assertGreater(evidence["failure_count"], 1)
            self.assertEqual(
                evidence["failure_count"], len(evidence["failures"])
            )

    def test_failed_pre_mutation_retry_archives_and_resets_to_build(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            failed = self.failed_pre_mutation_fixture(paths)
            with patch(
                "scripts.phoenix_release.gateway.status",
                return_value={"active_release": ROLLBACK_SHA},
            ):
                result = retry_pre_mutation(paths, RELEASE_SHA)

            retried = load_state(state_file(paths, RELEASE_SHA))
            self.assertEqual(result["status"], "reset")
            self.assertEqual(retried["current_phase"], "BUILD_VERIFIED")
            self.assertEqual(
                retried["completed_phases"],
                ["REQUESTED", "SOURCE_CI_VERIFIED", "BUILD_VERIFIED"],
            )
            self.assertIsNone(retried["failure_phase"])
            self.assertIsNone(retried["failure_code"])
            self.assertIsNone(retried["failure_evidence"])
            for field in (
                "release_sha",
                "rollback_sha",
                "source_ci_run_id",
                "source_ci_run_attempt",
                "build_run_id",
                "deploy_run_id",
                "deploy_run_attempt",
                "release_manifest_digest",
                "release_assets_digest",
                "package_digest",
                "expected_images",
                "contract_paused",
                "autonomous_armed",
                "kill_switch",
            ):
                self.assertEqual(retried[field], failed[field], field)

            archive = json.loads(
                (state_file(paths, RELEASE_SHA).parent / result["archive"]).read_text(
                    encoding="utf-8"
                )
            )
            self.assertEqual(archive["failed_state"], failed)
            self.assertEqual(archive["failure_evidence"], failed["failure_evidence"])

    def test_failed_pre_mutation_retry_rejects_mutation_started(self) -> None:
        value = fail_state(
            state(),
            code="ACTIVE_SERVICE_MISSING",
            evidence={"service": "live-executor"},
        )
        value["mutation_started"] = True
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch(
                "scripts.phoenix_release.gateway.load_state",
                return_value=value,
            ),
        ):
            with self.assertRaisesRegex(
                GatewayError, "PRE_MUTATION_RETRY_AFTER_MUTATION"
            ):
                retry_pre_mutation(self.paths(Path(temporary)), RELEASE_SHA)

    def test_failed_pre_mutation_retry_rejects_changed_active_release(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            self.failed_pre_mutation_fixture(paths)
            with patch(
                "scripts.phoenix_release.gateway.status",
                return_value={"active_release": "c" * 40},
            ):
                with self.assertRaisesRegex(
                    GatewayError, "PRE_MUTATION_RETRY_ACTIVE_RELEASE_CHANGED"
                ):
                    retry_pre_mutation(paths, RELEASE_SHA)
            self.assertEqual(
                load_state(state_file(paths, RELEASE_SHA))["current_phase"],
                "FAILED_PRE_MUTATION",
            )

    def test_failed_pre_mutation_retry_rejects_terminal_nonretry_states(self) -> None:
        post_mutation = set_mutation_started(state())
        post_mutation = fail_state(
            post_mutation, code="DEPLOYMENT_FAILED", evidence={}
        )
        rolled_back = rollback_phase(
            rollback_phase(copy.deepcopy(post_mutation), "ROLLBACK_STARTED"),
            "ROLLED_BACK",
            {"status": "ok"},
        )
        rollback_failed = rollback_phase(
            rollback_phase(copy.deepcopy(post_mutation), "ROLLBACK_STARTED"),
            "ROLLBACK_FAILED",
            {"status": "failed"},
        )
        for value in (post_mutation, rolled_back, rollback_failed):
            with (
                self.subTest(phase=value["current_phase"]),
                tempfile.TemporaryDirectory() as temporary,
                patch(
                    "scripts.phoenix_release.gateway.load_state",
                    return_value=value,
                ),
            ):
                with self.assertRaisesRegex(
                    GatewayError, "PRE_MUTATION_RETRY_PHASE_INVALID"
                ):
                    retry_pre_mutation(self.paths(Path(temporary)), RELEASE_SHA)

    def test_failed_pre_mutation_retry_rejects_evidence_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            self.failed_pre_mutation_fixture(paths)
            mismatched = request()
            mismatched["build_run_id"] = 999
            request_file(paths, RELEASE_SHA).write_text(
                json.dumps(mismatched), encoding="utf-8"
            )
            with self.assertRaisesRegex(
                GatewayError, "PRE_MUTATION_RETRY_REQUEST_MISMATCH"
            ):
                retry_pre_mutation(paths, RELEASE_SHA)

        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            self.failed_pre_mutation_fixture(paths)
            (paths.incoming / RELEASE_SHA / "release-manifest.json").write_text(
                json.dumps(
                    {
                        "release_sha": RELEASE_SHA,
                        "images": {
                            f"service-{index}": {
                                "repository": f"ghcr.io/example/service-{index}",
                                "digest": f"sha256:{index}" + "0" * 63,
                            }
                            for index in range(1, 8)
                        },
                        "tampered": True,
                    }
                ),
                encoding="utf-8",
            )
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    return_value={"active_release": ROLLBACK_SHA},
                ),
                self.assertRaisesRegex(
                    GatewayError, "PRE_MUTATION_RETRY_BUILD_EVIDENCE_MISMATCH"
                ),
            ):
                retry_pre_mutation(paths, RELEASE_SHA)

        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            self.failed_pre_mutation_fixture(paths)
            (
                paths.incoming
                / RELEASE_SHA
                / "release-package.tar.gz"
            ).write_bytes(b"tampered immutable package")
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    return_value={"active_release": ROLLBACK_SHA},
                ),
                self.assertRaisesRegex(
                    GatewayError, "PRE_MUTATION_RETRY_BUILD_EVIDENCE_MISMATCH"
                ),
            ):
                retry_pre_mutation(paths, RELEASE_SHA)

    def test_failed_pre_mutation_retry_refuses_after_reset(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            self.failed_pre_mutation_fixture(paths)
            with patch(
                "scripts.phoenix_release.gateway.status",
                return_value={"active_release": ROLLBACK_SHA},
            ):
                retry_pre_mutation(paths, RELEASE_SHA)
            with self.assertRaisesRegex(
                GatewayError, "PRE_MUTATION_RETRY_PHASE_INVALID"
            ):
                retry_pre_mutation(paths, RELEASE_SHA)

    def test_retry_pre_mutation_cli_requires_a_release_sha(self) -> None:
        arguments = release_parser().parse_args(
            ["retry-pre-mutation", RELEASE_SHA]
        )
        self.assertEqual(arguments.command, "retry-pre-mutation")
        self.assertEqual(arguments.release_sha, RELEASE_SHA)

    def test_rolled_back_gateway_retry_reuses_unchanged_package(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            failed = self.failed_pre_mutation_fixture(paths)
            value = state()
            value["package_digest"] = failed["package_digest"]
            value = advance(
                value,
                "SOURCE_CI_VERIFIED",
                updates={"expected_images": failed["expected_images"]},
            )
            value = advance(
                value,
                "BUILD_VERIFIED",
                updates={
                    "release_assets_digest": failed[
                        "release_assets_digest"
                    ],
                    "release_manifest_digest": failed[
                        "release_manifest_digest"
                    ],
                },
            )
            for phase in PHASES[
                PHASES.index("HOST_PREFLIGHT_STARTED") :
                PHASES.index("CANDIDATE_REHEARSED") + 1
            ]:
                updates = (
                    {
                        "autonomous_armed": False,
                        "contract_paused": True,
                        "kill_switch": True,
                    }
                    if phase == "HOST_PREFLIGHT_OK"
                    else None
                )
                value = advance(value, phase, updates=updates)
            value = set_mutation_started(value)
            value = fail_state(
                value, code="DEPLOYMENT_FAILED", evidence={}
            )
            value = rollback_phase(value, "ROLLBACK_STARTED")
            value = rollback_phase(value, "ROLLED_BACK", {"status": "ok"})
            value.update(
                {
                    "active_release_pointer": ROLLBACK_SHA,
                    "autonomous_armed": False,
                    "candidate_pointer": None,
                    "contract_paused": True,
                    "kill_switch": True,
                }
            )
            atomic_write(state_file(paths, RELEASE_SHA), value)
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    return_value={
                        "active_release": ROLLBACK_SHA,
                        "phoenix_mode": "LIVE",
                    },
                ),
                patch(
                    "scripts.phoenix_release.gateway."
                    "_require_fail_closed_live_executor_absence"
                ),
            ):
                result = retry_rolled_back(paths, RELEASE_SHA)
            retried = load_state(state_file(paths, RELEASE_SHA))
            self.assertEqual(result["release_attempt"], 2)
            self.assertEqual(retried["current_phase"], "BUILD_VERIFIED")
            self.assertEqual(
                retried["release_assets_digest"],
                failed["release_assets_digest"],
            )
            self.assertEqual(
                retried["package_digest"],
                failed["package_digest"],
            )
            self.assertTrue(
                (
                    state_file(paths, RELEASE_SHA).parent
                    / result["archive"]
                ).is_file()
            )

    def test_retry_rolled_back_cli_requires_exact_sha(self) -> None:
        arguments = release_parser().parse_args(
            ["retry-rolled-back", RELEASE_SHA]
        )
        self.assertEqual(arguments.command, "retry-rolled-back")
        self.assertEqual(arguments.release_sha, RELEASE_SHA)

    def test_valid_package_is_received_once_with_durable_state(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            package = package_bytes()
            received = receive_package(io.BytesIO(package), paths)
            self.assertEqual(received["release_sha"], RELEASE_SHA)
            state_path = paths.release_states / RELEASE_SHA / "state.json"
            persisted = load_state(state_path)
            self.assertEqual(persisted["current_phase"], "REQUESTED")
            self.assertEqual(
                persisted["package_digest"],
                f"sha256:{hashlib.sha256(package).hexdigest()}",
            )
            self.assertEqual(
                (
                    paths.incoming
                    / RELEASE_SHA
                    / "release-package.tar.gz"
                ).read_bytes(),
                package,
            )
            self.assertEqual(
                receive_package(io.BytesIO(package), paths),
                received,
            )
            with self.assertRaisesRegex(
                GatewayError, "PACKAGE_IDENTITY_MISMATCH"
            ):
                receive_package(
                    io.BytesIO(package_bytes(content_suffix=b"changed")),
                    paths,
                )
            (
                paths.incoming
                / RELEASE_SHA
                / "release-package.tar.gz"
            ).write_bytes(b"tampered stored package")
            with self.assertRaisesRegex(
                GatewayError, "PACKAGE_IDENTITY_MISMATCH"
            ):
                receive_package(io.BytesIO(package), paths)

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

    def test_command_failure_preserves_exact_bounded_stdout_and_stderr(
        self,
    ) -> None:
        completed = subprocess.CompletedProcess(
            ["/bin/example", "check"],
            19,
            stdout="container=engine health=unhealthy\n",
            stderr="mounted_sql_inode=441 expected_inode=442\n",
        )
        with (
            patch(
                "scripts.phoenix_release.gateway._run",
                return_value=completed,
            ),
            self.assertRaisesRegex(GatewayError, "FOCUSED_CHECK_FAILED") as raised,
        ):
            _require_success(
                ["/bin/example", "check"], "FOCUSED_CHECK_FAILED"
            )
        evidence = raised.exception.evidence
        self.assertEqual(evidence["command"], "/bin/example check")
        self.assertEqual(evidence["exit_code"], 19)
        self.assertIn("container=engine", evidence["stdout"])
        self.assertIn("mounted_sql_inode", evidence["stderr"])

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
            "readiness",
            "resume",
            "retry-pre-mutation",
            "retry-rolled-back",
            "rollback",
            "emergency-pause",
            "evidence",
            "reconcile-active-context",
            "reconcile-chain-evidence",
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

    def test_candidate_rehearsal_failure_precedes_mutation_and_install(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            value = state()
            for phase in PHASES[
                1 : PHASES.index("CANDIDATE_REHEARSED")
            ]:
                value = advance(value, phase)
            atomic_write(state_file(paths, RELEASE_SHA), value)
            request_path = request_file(paths, RELEASE_SHA)
            request_path.parent.mkdir(parents=True)
            request_path.write_text(
                json.dumps(request()), encoding="utf-8"
            )
            with (
                patch(
                    "scripts.phoenix_release.gateway._rehearse_candidate",
                    side_effect=GatewayError(
                        "CANDIDATE_REHEARSAL_FAILED"
                    ),
                ),
                patch(
                    "scripts.phoenix_release.gateway._install_candidate"
                ) as install_candidate,
                self.assertRaisesRegex(
                    GatewayError, "CANDIDATE_REHEARSAL_FAILED"
                ),
            ):
                resume(paths, RELEASE_SHA)
            failed = load_state(state_file(paths, RELEASE_SHA))
            self.assertEqual(
                failed["current_phase"], "FAILED_PRE_MUTATION"
            )
            self.assertFalse(failed["mutation_started"])
            install_candidate.assert_not_called()

    def test_candidate_render_failure_needs_runtime_reconciliation(self) -> None:
        value = state()
        value = set_mutation_started(value)
        value = fail_state(
            value,
            code="DEPLOYMENT_FAILED",
            evidence={},
        )
        value["failure_phase"] = "CANDIDATE_LIVE_RENDER_VERIFIED"
        self.assertTrue(_runtime_may_have_changed(value))
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

    def test_resume_reconciles_candidate_runtime_rollback_failure(self) -> None:
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

            self.assertEqual(require_success.call_count, 2)
            emergency_pause.assert_called_once()
            self.assertEqual(
                require_success.call_args_list[0].args[0][2:],
                [
                    RELEASE_SHA,
                    str(paths.releases / RELEASE_SHA),
                ],
            )
            self.assertEqual(
                require_success.call_args_list[1].args[0][1],
                str(paths.libexec / "rollback-release.sh"),
            )
            self.assertEqual(recovered["current_phase"], "ROLLED_BACK")
            self.assertIsNone(recovered["candidate_pointer"])
            self.assertEqual(
                recovered["rollback_result"]["runtime_reconciled"],
                True,
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


class ActiveReleaseContextTests(unittest.TestCase):
    def _validate(
        self,
        root: Path,
        *,
        allow_stopped_live_executor: bool,
        missing_service: str,
    ) -> None:
        engine_image = (
            "ghcr.io/example/phoenix-engine@sha256:" + "1" * 64
        )
        executor_image = (
            "ghcr.io/example/live-executor@sha256:" + "2" * 64
        )
        expected = {
            "autonomous_execution": True,
            "images": {
                "phoenix-engine": engine_image,
                "live-executor": executor_image,
            },
            "live_execution": True,
            "mode": "LIVE",
            "release_sha": RELEASE_SHA,
            "route_registry_hash": "sha256:" + "3" * 64,
        }
        release_state = root / "release-state.json"
        current_release = root / "current-release"
        running_images = root / "running-images.json"
        output = root / "result.json"
        release_state.write_text(
            json.dumps(expected), encoding="utf-8"
        )
        current_release.write_text(RELEASE_SHA + "\n", encoding="utf-8")
        services = {
            "phoenix-engine": {
                "configured_image": engine_image,
                "container_id": "a" * 64,
                "image_id": "sha256:" + "4" * 64,
            },
            "live-executor": {
                "configured_image": executor_image,
                "container_id": "b" * 64,
                "image_id": "sha256:" + "5" * 64,
            },
        }
        services.pop(missing_service)
        running_images.write_text(
            json.dumps(
                {
                    "schema": "phoenix.running-images.v1",
                    "services": services,
                }
            ),
            encoding="utf-8",
        )
        arguments = Namespace(
            allow_stopped_live_executor=allow_stopped_live_executor,
            current_release=str(current_release),
            output=str(output),
            release_state=str(release_state),
            running_images=str(running_images),
        )
        with patch(
            "scripts.production_context.state_payload",
            return_value=expected,
        ):
            validate_active_context(arguments)

    def test_stopped_live_executor_requires_the_explicit_disarmed_flag(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                ContextError, "RUNNING_IMAGE_MISMATCH"
            ):
                self._validate(
                    Path(temporary),
                    allow_stopped_live_executor=False,
                    missing_service="live-executor",
                )
        with tempfile.TemporaryDirectory() as temporary:
            self._validate(
                Path(temporary),
                allow_stopped_live_executor=True,
                missing_service="live-executor",
            )

    def test_disarmed_flag_never_allows_another_service_to_be_missing(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                ContextError, "RUNNING_IMAGE_MISMATCH"
            ):
                self._validate(
                    Path(temporary),
                    allow_stopped_live_executor=True,
                    missing_service="phoenix-engine",
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

    def test_stopped_live_executor_requires_complete_fail_closed_evidence(
        self,
    ) -> None:
        release_evidence = {
            "contract_paused": True,
            "autonomous_armed": False,
            "kill_switch": True,
        }
        runtime_evidence = {
            "armed": False,
            "kill_switch": True,
            "active_attempts": 0,
            "execution_mode": "disarmed",
            "open_routes": 0,
            "unresolved_submissions": 0,
        }
        self.assertTrue(
            _live_executor_absence_is_fail_closed(
                release_evidence, runtime_evidence
            )
        )

        unsafe_values = (
            ("contract_paused", False, "release"),
            ("autonomous_armed", True, "release"),
            ("kill_switch", False, "release"),
            ("armed", True, "runtime"),
            ("kill_switch", False, "runtime"),
            ("active_attempts", 1, "runtime"),
            ("active_attempts", False, "runtime"),
            ("execution_mode", "live", "runtime"),
            ("open_routes", 1, "runtime"),
            ("unresolved_submissions", 1, "runtime"),
        )
        for field, value, source in unsafe_values:
            with self.subTest(field=field, source=source):
                candidate_release = dict(release_evidence)
                candidate_runtime = dict(runtime_evidence)
                target = (
                    candidate_release
                    if source == "release"
                    else candidate_runtime
                )
                target[field] = value
                self.assertFalse(
                    _live_executor_absence_is_fail_closed(
                        candidate_release, candidate_runtime
                    )
                )

    def test_readiness_treats_only_a_nonrunning_live_executor_as_stopped(
        self,
    ) -> None:
        self.assertTrue(
            _is_stopped_live_executor(
                "live-executor", {"running": False}
            )
        )
        self.assertTrue(
            _is_stopped_live_executor(
                "live-executor", {"running": None}
            )
        )
        self.assertFalse(
            _is_stopped_live_executor(
                "live-executor", {"running": True}
            )
        )
        self.assertFalse(
            _is_stopped_live_executor(
                "phoenix-engine", {"running": False}
            )
        )

    def test_stopped_live_executor_rejects_unbounded_runtime_evidence(
        self,
    ) -> None:
        release_evidence = {
            "contract_paused": True,
            "autonomous_armed": False,
            "kill_switch": True,
        }
        runtime_evidence = {
            "armed": False,
            "kill_switch": True,
            "active_attempts": 0,
            "execution_mode": "disarmed",
            "open_routes": 0,
            "unresolved_submissions": 0,
            "unreviewed": True,
        }
        self.assertFalse(
            _live_executor_absence_is_fail_closed(
                release_evidence, runtime_evidence
            )
        )


class WorkflowAndDeploymentContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = (
            ROOT / ".github/workflows/phoenix-release-controller.yml"
        ).read_text()
        self.ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.deploy = (ROOT / "scripts/deploy-release.sh").read_text()
        self.rollback = (ROOT / "scripts/rollback-release.sh").read_text()
        self.rehearsal = (
            ROOT / "scripts/rehearse-production-release.sh"
        ).read_text()

    def test_controller_is_automatic_resumable_and_serialized(self) -> None:
        self.assertIn('workflows: ["Phoenix CI"]', self.workflow)
        self.assertIn("branches: [main]", self.workflow)
        self.assertIn("workflow_dispatch:", self.workflow)
        self.assertNotIn("schedule:", self.workflow)
        self.assertNotIn("cron:", self.workflow)
        self.assertIn("RECOVER_EXACT_PHOENIX_RELEASE", self.workflow)
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

    def test_controller_gates_build_on_aggregated_readiness_and_safe_retry(
        self,
    ) -> None:
        self.assertIn('"readiness ${RELEASE_SHA}"', self.workflow)
        self.assertIn("needs: [prepare, host-plan]", self.workflow)
        self.assertIn("phoenix-production-readiness-", self.workflow)
        self.assertIn('"retry-pre-mutation ${RELEASE_SHA}"', self.workflow)
        self.assertIn('"retry-rolled-back ${RELEASE_SHA}"', self.workflow)
        self.assertIn("package_digest", self.workflow)
        self.assertIn("gzip -n", self.workflow)
        self.assertNotIn("GITHUB_RUN_ATTEMPT", self.workflow)
        self.assertIn("Probe durable exact-package evidence", self.workflow)
        probe = self.workflow.split(
            "- name: Probe durable exact-package evidence",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn(
            '"evidence ${RELEASE_SHA}" >existing-evidence.json 2>/dev/null &&',
            probe,
        )
        self.assertLess(probe.index("jq -e"), probe.index("then"))

    def test_deploy_checkout_preserves_rollback_history(self) -> None:
        checkout = self.workflow.split(
            "- name: Checkout exact protected main release",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("fetch-depth: 0", checkout)

    def test_release_environment_binds_the_exact_versioned_route_registry(self) -> None:
        self.assertIn(
            '--route-registry "$deploy_dir/routes/weth_usdc_uniswap_v3.json"',
            self.deploy,
        )
        self.assertIn(
            '--route-registry "$release_assets_root/fixtures/routes/weth_usdc_uniswap_v3.json"',
            self.rollback,
        )
        self.assertIn(
            '--route-registry "$candidate_root/fixtures/routes/weth_usdc_uniswap_v3.json"',
            self.rehearsal,
        )

    def test_ci_preserves_jobs_and_runs_expensive_suites_only_on_main(
        self,
    ) -> None:
        for job in REQUIRED_JOBS:
            self.assertIn(f"\n    name: {job}\n", self.ci)
        self.assertIn("scripts/change_impact.py github-output", self.ci)
        self.assertIn(
            "integration fixtures run only once for affected exact-main changes",
            self.ci,
        )
        self.assertIn(
            "JetStream integration runs only once for affected exact-main changes",
            self.ci,
        )
        self.assertIn("github.event_name != 'pull_request'", self.ci)
        self.assertIn(
            "Validate affected Dockerfiles without building",
            self.ci,
        )
        self.assertIn("docker buildx build --check", self.ci)
        self.assertIn('--cache-from "type=gha,scope=$image"', self.ci)
        self.assertIn(
            '--cache-to "type=gha,mode=max,scope=$image"',
            self.ci,
        )

    def test_rehearsal_proves_exact_schema_monitor_inode_uid_and_health(
        self,
    ) -> None:
        self.assertIn("migrations/*.sql", self.rehearsal)
        self.assertIn("live-executor/schema/*.sql", self.rehearsal)
        self.assertIn("BEGIN TRANSACTION READ ONLY", self.rehearsal)
        self.assertIn(
            "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql",
            self.rehearsal,
        )
        self.assertIn("candidate_monitor_sql_inode_mismatch", self.rehearsal)
        self.assertIn("1000:1000", self.rehearsal)
        self.assertIn("candidate_monitor_unhealthy", self.rehearsal)
        self.assertIn("deadline=$(( $(date +%s) + 720 ))", self.rehearsal)
        self.assertIn("candidate_monitor_exited", self.rehearsal)
        self.assertIn('docker logs --tail 20 "$monitor_container"', self.rehearsal)
        self.assertIn("candidate_health_contract_failed", self.rehearsal)
        self.assertIn(
            'docker rm -f -v "$monitor_container"',
            self.rehearsal,
        )
        self.assertIn(
            "PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE",
            self.rehearsal,
        )
        self.assertNotIn(
            "PHOENIX_HEALTH_EXPECTED_MODE=SHADOW",
            self.rehearsal,
        )
        self.assertIn(
            'image_volume = "/var/lib/postgresql/data"',
            self.rehearsal,
        )
        self.assertIn(
            'r"/var/lib/docker/volumes/[0-9a-f]{64}/_data"',
            self.rehearsal,
        )

    def test_rehearsal_renders_disarmed_evidence_candidate_with_live_overlay(
        self,
    ) -> None:
        render = self.rehearsal.split(
            '"$candidate_root/scripts/render-production-compose.sh"',
            maxsplit=1,
        )[1].split("postgres_image=", maxsplit=1)[0]
        self.assertIn('--overlay-file "$overlay_file"', render)

    def test_candidate_render_precedes_any_runtime_mutation(self) -> None:
        preflight_render = self.deploy.index(
            '"$deploy_dir/render-production-compose.sh"'
        )
        preflight_failure = self.deploy.index(
            'fail "preflight production rendering failed"', preflight_render
        )
        self.assertIn(
            '--overlay-file "$overlay_file"',
            self.deploy[preflight_render:preflight_failure],
        )
        live_candidate = self.deploy.index(
            'production_mode.py" live --env-file "$candidate_live_env"'
        )
        candidate_render = self.deploy.index(
            '"$deploy_dir/render-production-compose.sh"', live_candidate
        )
        candidate_failure = self.deploy.index(
            'fail "candidate LIVE overlay rendering failed"', candidate_render
        )
        self.assertIn(
            '--overlay-file "$overlay_file"',
            self.deploy[candidate_render:candidate_failure],
        )
        candidate = self.deploy.index("mark_phase CANDIDATE_LIVE_RENDER_VERIFIED")
        mutation = self.deploy.index("state_update mutation mutation_started")
        live_mode = self.deploy.index(
            'production_mode.py" live --env-file "$env_file"'
        )
        self.assertLess(candidate, mutation)
        self.assertLess(mutation, live_mode)
        self.assertIn("PROTECTED_SERVICE_UNAVAILABLE:", self.deploy)
        self.assertIn(
            'compose up -d --no-deps --force-recreate "$service"', self.deploy
        )

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
        context_validation = self.deploy.index(
            '"$deploy_dir/validate-production-release-context.sh"'
        )
        evidence_phase = self.deploy.index(
            "mark_phase DISARMED_EVIDENCE_STARTED"
        )
        self.assertNotIn("mark_phase POST_LIVE_VERIFIED", self.deploy)
        self.assertLess(evidence_phase, promote)
        stopped_executor = self.deploy.rfind(
            'compose ps -q live-executor', 0, context_validation
        )
        self.assertLess(evidence_phase, context_validation)
        self.assertLess(stopped_executor, evidence_phase)
        self.assertIn(
            "--allow-stopped-live-executor",
            self.deploy[evidence_phase:context_validation + 500],
        )

    def test_health_checks_receive_explicit_release_phase_mode(self) -> None:
        health = (ROOT / "scripts/production-healthcheck.sh").read_text()
        self.assertIn("PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE", self.deploy)
        self.assertIn('PHOENIX_ENV_FILE="$env_file"', self.deploy)
        self.assertIn("PHOENIX_HEALTH_EXPECTED_MODE=SHADOW", self.rollback)
        self.assertIn('PHOENIX_ENV_FILE="$env_file"', self.rollback)
        self.assertNotIn(
            'compose() {\n  set -- --env-file "$env_file"', health
        )
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
