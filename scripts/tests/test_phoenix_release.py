from __future__ import annotations

import copy
import hashlib
import io
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import textwrap
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
from scripts.phoenix_release.phase_update import main as phase_update_main
from scripts.phoenix_release.gateway import (
    _active_runtime_services,
    _bounded_output,
    _historical_contract_evidence,
    _is_stopped_live_executor,
    _live_executor_absence_is_fail_closed,
    _require_success,
    _rpc_provider_installer_contract,
    _rollback_failed_state,
    _require_exact_reconstruction_topology,
    _runtime_may_have_changed,
    _service_absence_allowed,
    _snapshot_reconstruction_container_ids,
    _validate_reconstruction_readiness_evidence,
    GatewayError,
    HostPaths,
    expected_members,
    production_readiness,
    receive_package,
    reconcile_snapshot,
    reconstruct_active_historical_state,
    request_file,
    retry_rolled_back,
    retry_pre_mutation,
    resume,
    state_file,
    validate_request,
    buffer_rpc_provider_secret,
    emergency_pause,
    enter_post_recovery_live_mode,
)
from scripts.phoenix_release.rpc_provider_secret import (
    SecretError as RpcProviderSecretError,
    install_from_stream,
    legacy_recovery_stream,
    persist_secret,
    read_existing_secret,
    read_secret_once,
)
from scripts.phoenix_release.chain_reconciliation import ReconciliationError
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


class ProviderSecretStreamTests(unittest.TestCase):
    class CountingStream(io.BytesIO):
        def __init__(self, value: bytes):
            super().__init__(value)
            self.read_calls = 0

        def read(self, size: int = -1) -> bytes:
            self.read_calls += 1
            return super().read(size)

    def test_release_input_is_consumed_exactly_once(self) -> None:
        stream = self.CountingStream(b"fixture-secret-value")
        self.assertEqual(
            buffer_rpc_provider_secret(stream), b"fixture-secret-value"
        )
        self.assertEqual(stream.read_calls, 1)

    def test_empty_input_is_bounded_missing_input(self) -> None:
        stream = self.CountingStream(b"")
        self.assertIsNone(read_secret_once(stream))
        self.assertEqual(stream.read_calls, 1)

    def test_malformed_or_oversized_input_is_rejected(self) -> None:
        for value in (b"invalid\nvalue", b"x" * 4097):
            with self.subTest(size=len(value)):
                with self.assertRaises(RpcProviderSecretError):
                    read_secret_once(io.BytesIO(value))

    def test_first_install_contract_uses_explicit_stdin_only(self) -> None:
        secret = b"fixture-secret-value"
        with patch(
            "scripts.phoenix_release.gateway.read_existing_secret",
            return_value=None,
        ):
            arguments, installer_input = _rpc_provider_installer_contract(secret)
        self.assertEqual(
            arguments,
            ["--reuse-existing-deploy-key", "--rpc-provider-secret-stdin"],
        )
        self.assertEqual(installer_input, secret.decode("ascii"))
        self.assertNotIn(secret.decode("ascii"), arguments)

    def test_valid_staged_secret_uses_reuse_and_requires_identity(self) -> None:
        secret = b"fixture-secret-value"
        with patch(
            "scripts.phoenix_release.gateway.read_existing_secret",
            return_value=secret,
        ):
            arguments, installer_input = _rpc_provider_installer_contract(secret)
        self.assertEqual(
            arguments,
            [
                "--reuse-existing-deploy-key",
                "--reuse-existing-rpc-provider-secret",
            ],
        )
        self.assertIsNone(installer_input)
        with (
            patch(
                "scripts.phoenix_release.gateway.read_existing_secret",
                return_value=secret,
            ),
            self.assertRaisesRegex(GatewayError, "RPC_PROVIDER_SECRET_MISMATCH"),
        ):
            _rpc_provider_installer_contract(b"different-fixture-value")

    def test_missing_input_and_missing_staged_secret_reject(self) -> None:
        with (
            patch(
                "scripts.phoenix_release.gateway.read_existing_secret",
                return_value=None,
            ),
            self.assertRaisesRegex(GatewayError, "RPC_PROVIDER_SECRET_MISSING"),
        ):
            _rpc_provider_installer_contract(None)

    def test_invalid_staged_metadata_rejects_before_install(self) -> None:
        with (
            patch(
                "scripts.phoenix_release.gateway.read_existing_secret",
                side_effect=RpcProviderSecretError("metadata_invalid"),
            ),
            self.assertRaisesRegex(
                GatewayError, "RPC_PROVIDER_SECRET_STAGED_INVALID"
            ),
        ):
            _rpc_provider_installer_contract(b"fixture-secret-value")

    def test_rehearsal_closes_its_stdin_and_installer_receives_explicit_copy(
        self,
    ) -> None:
        rehearsal = (ROOT / "scripts/rehearse-production-release.sh").read_text()
        gateway = (ROOT / "scripts/phoenix_release/gateway.py").read_text()
        self.assertIn("exec </dev/null", rehearsal)
        self.assertIn("input_text=installer_input", gateway)
        self.assertIn('{"stdin": subprocess.DEVNULL}', gateway)


@unittest.skipUnless(os.name == "posix", "requires POSIX ownership semantics")
class ProviderSecretFilesystemTests(unittest.TestCase):
    secret = b"fixture-secret-value"

    def kwargs(self) -> dict[str, int]:
        return {
            "expected_uid": os.getuid(),
            "expected_gid": os.getgid(),
            "expected_mode": 0o640,
        }

    def test_first_install_is_atomic_and_verified(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "provider-key"
            install_from_stream(io.BytesIO(self.secret), path, **self.kwargs())
            self.assertEqual(
                read_existing_secret(path, **self.kwargs()), self.secret
            )
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o640)
            self.assertEqual(list(root.glob(".rpc-provider-slot-1.*")), [])

    def test_empty_first_install_and_unexpected_existing_reject(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "provider-key"
            with self.assertRaisesRegex(
                RpcProviderSecretError, "rpc_provider_secret_missing"
            ):
                install_from_stream(io.BytesIO(b""), path, **self.kwargs())
            persist_secret(self.secret, path, **self.kwargs())
            with self.assertRaisesRegex(
                RpcProviderSecretError, "unexpected_existing"
            ):
                install_from_stream(
                    io.BytesIO(self.secret), path, **self.kwargs()
                )

    def test_invalid_mode_and_symlink_reject(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "provider-key"
            persist_secret(self.secret, path, **self.kwargs())
            path.chmod(0o600)
            with self.assertRaisesRegex(
                RpcProviderSecretError, "metadata_invalid"
            ):
                read_existing_secret(path, **self.kwargs())
            path.unlink()
            target = root / "target"
            target.write_bytes(self.secret)
            target.chmod(0o640)
            path.symlink_to(target)
            with self.assertRaisesRegex(
                RpcProviderSecretError, "metadata_invalid"
            ):
                read_existing_secret(path, **self.kwargs())

    def test_legacy_recovery_bridges_once_then_reuses_verified_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "provider-key"
            legacy_recovery_stream(
                io.BytesIO(self.secret), path, **self.kwargs()
            )
            legacy_recovery_stream(io.BytesIO(b""), path, **self.kwargs())
            legacy_recovery_stream(
                io.BytesIO(self.secret), path, **self.kwargs()
            )
            with self.assertRaisesRegex(
                RpcProviderSecretError, "rpc_provider_secret_mismatch"
            ):
                legacy_recovery_stream(
                    io.BytesIO(b"different-fixture-value"),
                    path,
                    **self.kwargs(),
                )

    def test_old_rehearsal_exhaustion_case_is_rejected_then_fixed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            exhausted_path = root / "exhausted"
            exhausted = io.BytesIO(self.secret)
            exhausted.read()
            with self.assertRaisesRegex(
                RpcProviderSecretError, "rpc_provider_secret_missing"
            ):
                legacy_recovery_stream(
                    exhausted, exhausted_path, **self.kwargs()
                )
            preserved_path = root / "preserved"
            preserved = io.BytesIO(self.secret)
            legacy_recovery_stream(
                preserved, preserved_path, **self.kwargs()
            )
            self.assertEqual(
                read_existing_secret(preserved_path, **self.kwargs()), self.secret
            )


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
                "HUNTING_STANDBY_STARTED",
                "COMPLETED",
            ),
        )

    def test_hunting_standby_is_an_explicit_successful_transition(self) -> None:
        value = state()
        for phase in PHASES[1 : PHASES.index("HUNTING_STANDBY_STARTED") + 1]:
            value = advance(value, phase)
        self.assertEqual(value["current_phase"], "HUNTING_STANDBY_STARTED")
        self.assertEqual(advance(value, "COMPLETED")["current_phase"], "COMPLETED")

    def test_hunting_standby_failure_rolls_back_and_enables_exact_retry(self) -> None:
        value = state()
        for phase in PHASES[1 : PHASES.index("HUNTING_STANDBY_STARTED") + 1]:
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

    def test_state_callback_persists_sanitized_rollback_result(self) -> None:
        with (
            patch("scripts.phoenix_release.phase_update.host_paths") as paths,
            patch(
                "scripts.phoenix_release.phase_update.mark_rollback",
                return_value={"current_phase": "ROLLBACK_FAILED"},
            ) as mark_rollback,
            patch("builtins.print"),
        ):
            result = phase_update_main(
                [
                    RELEASE_SHA,
                    "rollback",
                    "ROLLBACK_FAILED",
                    "--result",
                    "failed",
                    "--code",
                    "ROLLBACK_SCRIPT_FAILED",
                ]
            )
        self.assertEqual(result, 0)
        mark_rollback.assert_called_once_with(
            paths.return_value,
            RELEASE_SHA,
            "ROLLBACK_FAILED",
            {"status": "failed", "code": "ROLLBACK_SCRIPT_FAILED"},
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
    def test_protected_authority_cannot_bypass_exact_env_file_precedence(self) -> None:
        environment = compose_environment(
            Path("/etc/phoenix/phoenix.env"),
            Path("/opt/phoenix/deploy/current-release.env"),
            {
                "ENGINE_ROUTE_REGISTRY_JSON": '[{"route_id":"stale"}]',
                "PHOENIX_MODE": "LIVE",
                "LIVE_EXECUTION": "true",
                "AUTONOMOUS_EXECUTION": "true",
                "UNRELATED_PARENT_VALUE": "preserved",
            },
        )
        for protected_name in (
            "ENGINE_ROUTE_REGISTRY_JSON",
            "PHOENIX_MODE",
            "LIVE_EXECUTION",
            "AUTONOMOUS_EXECUTION",
        ):
            self.assertNotIn(protected_name, environment)
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

    def test_readiness_runtime_services_follow_the_active_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            with patch(
                "scripts.phoenix_release.gateway._require_success",
                return_value="postgres phoenix-engine live-executor\n",
            ) as required:
                services = _active_runtime_services(
                    paths, ROLLBACK_SHA, "LIVE"
                )
            self.assertEqual(
                services, ["postgres", "phoenix-engine", "live-executor"]
            )
            command = required.call_args.args[0]
            self.assertIn(
                str(
                    paths.deploy_dir
                    / "manifests"
                    / f"{ROLLBACK_SHA}.json"
                ),
                command,
            )
            self.assertEqual(
                command[-4:],
                ["--mode", "LIVE", "--field", "running_services"],
            )

    def test_readiness_runtime_services_reject_duplicate_topology(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.paths(Path(temporary))
            with patch(
                "scripts.phoenix_release.gateway._require_success",
                return_value="postgres postgres\n",
            ), self.assertRaisesRegex(
                GatewayError, "READINESS_TOPOLOGY_INVALID"
            ):
                _active_runtime_services(paths, ROLLBACK_SHA, "LIVE")

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


@unittest.skipUnless(os.name == "posix", "requires POSIX release-state metadata")
class ActiveHistoricalStateReconstructionTests(unittest.TestCase):
    images = {
        "phoenix-engine": "ghcr.io/example/phoenix-engine@sha256:" + "1" * 64,
        "live-executor": "ghcr.io/example/live-executor@sha256:" + "2" * 64,
    }
    manifest_digest = "sha256:" + "3" * 64

    def paths(self, root: Path) -> HostPaths:
        return HostPaths(
            state_root=root / "state",
            deploy_root=root / "deploy-root",
            env_file=root / "phoenix.env",
            libexec=root / "libexec",
        )

    def completed_state(self) -> dict[str, object]:
        value = state()
        for phase in PHASES[1:]:
            updates = None
            if phase == "SOURCE_CI_VERIFIED":
                updates = {"expected_images": dict(self.images)}
            elif phase == "BUILD_VERIFIED":
                updates = {
                    "release_manifest_digest": self.manifest_digest,
                    "release_assets_digest": "sha256:" + "4" * 64,
                }
            elif phase == "HOST_PREFLIGHT_OK":
                updates = {
                    "contract_paused": True,
                    "autonomous_armed": False,
                    "kill_switch": True,
                }
            elif phase == "CANDIDATE_INSTALLED":
                value = set_mutation_started(value)
            elif phase == "COMPLETED":
                updates = {
                    "active_release_pointer": RELEASE_SHA,
                    "actual_images": dict(self.images),
                    "autonomous_armed": False,
                    "candidate_pointer": None,
                    "contract_paused": True,
                    "kill_switch": True,
                }
            value = advance(value, phase, updates=updates)
            if phase == "ENGINE_BURN_IN_STARTED":
                value = record_engine_baseline(
                    value,
                    container_id="5" * 64,
                    restart_count=0,
                    terminal_integrity=0,
                    process_fatal_integrity=0,
                )
        return value

    def runtime(self) -> dict[str, object]:
        return {
            "build_run_id": 202,
            "expected_images": dict(self.images),
            "manifest_digest": self.manifest_digest,
            "source_ci_run_attempt": 1,
            "source_ci_run_id": 101,
        }

    def evidence_file(self, root: Path, value: dict[str, object] | None = None) -> Path:
        path = root / "release-evidence.json"
        atomic_write(path, value or self.completed_state())
        return path

    def readiness_evidence(self) -> dict[str, object]:
        return {
            "schema": "phoenix.production-readiness.v1",
            "status": "failed",
            "failure_count": 1,
            "failures": [
                {
                    "code": "READINESS_ACTIVE_STATE_INVALID",
                    "evidence": {
                        "message": "ACTIVE_RELEASE_HISTORICAL_STATE_INVALID"
                    },
                }
            ],
            "checks": {
                "active_release": {
                    "active_release": RELEASE_SHA,
                    "release_assets_sha": RELEASE_SHA,
                },
                "expected_services": ["postgres", "phoenix-engine"],
                "services": [
                    {
                        "container_id": "6" * 64,
                        "health": "healthy",
                        "running": True,
                        "service": "postgres",
                    },
                    {
                        "container_id": "7" * 64,
                        "health": "healthy",
                        "running": True,
                        "service": "phoenix-engine",
                    },
                ],
                "controls": {
                    "active_attempts": 0,
                    "unresolved_submissions": 0,
                },
            },
        }

    def test_valid_missing_active_state_is_reconstructed_and_normally_validated(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self.paths(root)
            expected = self.completed_state()
            evidence = self.evidence_file(root, expected)
            with patch(
                "scripts.phoenix_release.gateway._active_reconstruction_runtime",
                return_value=self.runtime(),
            ):
                result = reconstruct_active_historical_state(
                    paths, RELEASE_SHA, io.BytesIO(evidence.read_bytes())
                )
            restored = load_state(state_file(paths, RELEASE_SHA))
            self.assertEqual(result["status"], "reconstructed")
            self.assertEqual(restored, expected)
            metadata = state_file(paths, RELEASE_SHA).stat()
            self.assertEqual(
                _historical_contract_evidence(
                    paths,
                    RELEASE_SHA,
                    expected_uid=metadata.st_uid,
                    expected_gid=metadata.st_gid,
                ),
                {"contract_paused": True, "owner_transaction_hash": None},
            )

    def test_existing_state_is_never_overwritten(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self.paths(root)
            existing = state_file(paths, RELEASE_SHA)
            atomic_write(existing, self.completed_state())
            before = existing.read_bytes()
            evidence = self.evidence_file(root)
            with self.assertRaisesRegex(
                GatewayError, "ACTIVE_RECONSTRUCTION_STATE_EXISTS"
            ):
                reconstruct_active_historical_state(
                    paths, RELEASE_SHA, io.BytesIO(evidence.read_bytes())
                )
            self.assertEqual(existing.read_bytes(), before)

    def test_full_container_ids_are_canonicalized_and_match(self) -> None:
        first = "A" * 64
        second = "b" * 64
        self.assertEqual(
            _require_exact_reconstruction_topology(
                [first, second],
                [second.upper(), first.lower()],
            ),
            [first.lower(), second],
        )

    def test_production_snapshot_requires_untruncated_container_ids(self) -> None:
        container_id = "a" * 64
        with patch(
            "scripts.phoenix_release.gateway._require_success",
            return_value=container_id,
        ) as require_success:
            self.assertEqual(
                _snapshot_reconstruction_container_ids(),
                [container_id],
            )
        command = require_success.call_args.args[0]
        self.assertEqual(
            command,
            [
                "/usr/bin/docker",
                "ps",
                "-a",
                "--no-trunc",
                "-q",
                "--filter",
                "label=com.docker.compose.service",
            ],
        )
        with (
            patch(
                "scripts.phoenix_release.gateway._require_success",
                return_value="a" * 12,
            ),
            self.assertRaisesRegex(
                GatewayError, "ACTIVE_RECONSTRUCTION_TOPOLOGY_INVALID"
            ),
        ):
            _snapshot_reconstruction_container_ids()

    def test_missing_extra_malformed_and_duplicate_ids_fail_closed(self) -> None:
        first = "1" * 64
        second = "2" * 64
        invalid_cases = [
            ([first, second], [first]),
            ([first], [first, second]),
            ([first], ["not-a-container-id"]),
            ([first, second], [first, first]),
        ]
        for expected, observed in invalid_cases:
            with self.subTest(
                expected=expected, observed=observed
            ), self.assertRaisesRegex(
                GatewayError,
                "ACTIVE_RECONSTRUCTION_TOPOLOGY_INVALID",
            ):
                _require_exact_reconstruction_topology(expected, observed)

    def test_readiness_rejects_duplicate_container_identity(self) -> None:
        readiness = self.readiness_evidence()
        duplicate = readiness["checks"]["services"][0][  # type: ignore[index]
            "container_id"
        ]
        readiness["checks"]["services"][1]["container_id"] = duplicate  # type: ignore[index]
        with self.assertRaisesRegex(
            GatewayError, "ACTIVE_RECONSTRUCTION_TOPOLOGY_INVALID"
        ):
            _validate_reconstruction_readiness_evidence(readiness, RELEASE_SHA)

    def test_cross_sha_evidence_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self.paths(root)
            value = self.completed_state()
            value["release_sha"] = "c" * 40
            value["active_release_pointer"] = "c" * 40
            evidence = self.evidence_file(root, value)
            with (
                patch(
                    "scripts.phoenix_release.gateway._active_reconstruction_runtime",
                    return_value=self.runtime(),
                ),
                self.assertRaisesRegex(
                    GatewayError, "ACTIVE_RECONSTRUCTION_EVIDENCE_MISMATCH"
                ),
            ):
                reconstruct_active_historical_state(
                    paths, RELEASE_SHA, io.BytesIO(evidence.read_bytes())
                )

    def test_sha_manifest_and_image_mismatches_are_rejected(self) -> None:
        mismatches = []
        wrong_sha = self.completed_state()
        wrong_sha["release_sha"] = "c" * 40
        wrong_sha["active_release_pointer"] = "c" * 40
        mismatches.append((wrong_sha, self.runtime()))
        wrong_manifest = self.runtime()
        wrong_manifest["manifest_digest"] = "sha256:" + "8" * 64
        mismatches.append((self.completed_state(), wrong_manifest))
        wrong_images = self.runtime()
        wrong_images["expected_images"] = {"phoenix-engine": "wrong-image"}
        mismatches.append((self.completed_state(), wrong_images))
        for evidence_value, runtime_value in mismatches:
            with self.subTest(runtime=runtime_value), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                paths = self.paths(root)
                evidence = self.evidence_file(root, evidence_value)
                with (
                    patch(
                        "scripts.phoenix_release.gateway._active_reconstruction_runtime",
                        return_value=runtime_value,
                    ),
                    self.assertRaisesRegex(
                        GatewayError, "ACTIVE_RECONSTRUCTION_EVIDENCE_MISMATCH"
                    ),
                ):
                    reconstruct_active_historical_state(
                        paths, RELEASE_SHA, io.BytesIO(evidence.read_bytes())
                    )

    def test_unhealthy_service_and_active_attempts_are_rejected(self) -> None:
        unhealthy = self.readiness_evidence()
        unhealthy["checks"]["services"][0]["health"] = "unhealthy"  # type: ignore[index]
        with self.assertRaisesRegex(
            GatewayError, "ACTIVE_RECONSTRUCTION_SERVICE_UNHEALTHY"
        ):
            _validate_reconstruction_readiness_evidence(unhealthy, RELEASE_SHA)
        attempts = self.readiness_evidence()
        attempts["checks"]["controls"]["active_attempts"] = 1  # type: ignore[index]
        with self.assertRaisesRegex(
            GatewayError, "ACTIVE_RECONSTRUCTION_ATTEMPTS_ACTIVE"
        ):
            _validate_reconstruction_readiness_evidence(attempts, RELEASE_SHA)

    def test_invalid_schema_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = self.paths(root)
            value = self.completed_state()
            value["schema_version"] = "invalid"
            evidence = root / "release-evidence.json"
            evidence.write_text(json.dumps(value), encoding="utf-8")
            evidence.chmod(0o600)
            with (
                patch(
                    "scripts.phoenix_release.gateway._active_reconstruction_runtime",
                    return_value=self.runtime(),
                ),
                self.assertRaisesRegex(
                    GatewayError, "ACTIVE_RECONSTRUCTION_EVIDENCE_INVALID"
                ),
            ):
                reconstruct_active_historical_state(
                    paths, RELEASE_SHA, io.BytesIO(evidence.read_bytes())
                )

    def test_cli_binds_release_and_evidence_identity(self) -> None:
        arguments = release_parser().parse_args(
            [
                "reconstruct-active-historical-state",
                RELEASE_SHA,
            ]
        )
        self.assertEqual(arguments.release_sha, RELEASE_SHA)


class BoundedTransportContinuationTests(unittest.TestCase):
    paths = BoundedTransportTests.paths
    failed_pre_mutation_fixture = BoundedTransportTests.failed_pre_mutation_fixture

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
            "enter-post-recovery-live-mode",
        ):
            self.assertIn(command, transport)

    def test_post_recovery_live_cli_and_transport_require_exact_ack(self) -> None:
        arguments = release_parser().parse_args(
            [
                "enter-post-recovery-live-mode",
                RELEASE_SHA,
                "enter-recovered-live-mode-42161",
            ]
        )
        self.assertEqual(arguments.release_sha, RELEASE_SHA)
        self.assertEqual(
            arguments.acknowledgement,
            "enter-recovered-live-mode-42161",
        )
        transport = (
            ROOT / "scripts/phoenix-release-transport.sh"
        ).read_text()
        self.assertIn("enter-post-recovery-live-mode:3", transport)
        self.assertIn(
            '[ "${3:-}" = enter-recovered-live-mode-42161 ] || deny',
            transport,
        )
        wrapper = (
            ROOT / "scripts/phoenix-release-gateway.sh"
        ).read_text()
        guarded = wrapper.split(
            'if [ "$1" = enter-post-recovery-live-mode ]; then',
            maxsplit=1,
        )[1].split("fi", maxsplit=1)[0]
        release_lock = guarded.index("/run/lock/phoenix-release.lock")
        activation_lock = guarded.index(
            "/run/lock/phoenix-economic-activation.lock"
        )
        self.assertLess(release_lock, activation_lock)
        self.assertEqual(guarded.count("/usr/bin/flock -n"), 2)

    def test_permanent_identity_has_forced_command_and_no_forwarding(self) -> None:
        installer = (ROOT / "scripts/install-phoenix-release-platform.sh").read_text()
        self.assertIn('restrict,command="%s"', installer)
        self.assertIn("AllowAgentForwarding no", installer)
        self.assertIn("AllowTcpForwarding no", installer)
        self.assertIn("PermitTTY no", installer)
        self.assertIn("PermitTunnel no", installer)
        self.assertIn("PermitUserRC no", installer)
        self.assertNotIn("NOPASSWD: ALL", installer)

    def test_authenticated_rpc_secret_is_stdin_only_and_gateway_scoped(self) -> None:
        installer = (ROOT / "scripts/install-phoenix-release-platform.sh").read_text()
        secret_helper = (
            ROOT / "scripts/phoenix_release/rpc_provider_secret.py"
        ).read_text()
        workflow = (
            ROOT / ".github/workflows/phoenix-release-controller.yml"
        ).read_text()
        compose = (ROOT / "compose.prod.yml").read_text()

        self.assertIn('install -d -m 0750 -o root -g 65532 "$rpc_secret_dir"', installer)
        self.assertIn("--rpc-provider-secret-stdin", installer)
        self.assertIn("--reuse-existing-rpc-provider-secret", installer)
        self.assertIn("os.fchown(descriptor, expected_uid, expected_gid)", secret_helper)
        self.assertIn("os.fchmod(descriptor, expected_mode)", secret_helper)
        self.assertIn("os.fsync(descriptor)", secret_helper)
        self.assertIn("os.replace(temporary, path)", secret_helper)
        self.assertIn('getattr(os, "O_NOFOLLOW", 0)', secret_helper)
        self.assertNotIn("PHOENIX_RPC_PROVIDER_SLOT_1_API_KEY", installer)
        self.assertNotIn("PHOENIX_RPC_PROVIDER_SLOT_1_API_KEY", secret_helper)
        self.assertIn(
            "PHOENIX_RPC_PROVIDER_SLOT_1_API_KEY: ${{ secrets.PHOENIX_RPC_PROVIDER_SLOT_1_API_KEY }}",
            workflow,
        )
        self.assertIn(
            'printf \'%s\' "$PHOENIX_RPC_PROVIDER_SLOT_1_API_KEY" |', workflow
        )

        secret_path = "/run/secrets/phoenix-rpc-provider-slot-1-api-key"
        self.assertEqual(compose.count(f"target: {secret_path}"), 1)
        self.assertEqual(
            compose.count(f"RPC_AUTH_PROVIDER_HEADER_FILE: {secret_path}"), 1
        )
        self.assertIn("RPC_AUTH_PROVIDER_ID: production-nownodes-arbitrum", compose)
        self.assertIn("RPC_AUTH_PROVIDER_HEADER_NAME: api-key", compose)
        self.assertIn("RPC_AUTHORITY_MODE: single_primary", compose)
        self.assertNotIn("RPC_PROVIDER_URLS:", compose)
        self.assertNotIn("RPC_PROVIDER_WEIGHTS:", compose)

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
        materialize = context_installer.index(
            'production_mode.py" materialize-release-defaults'
        )
        validate = context_installer.index(
            'validate-production-env.sh" "$env_file"'
        )
        self.assertLess(materialize, validate)

    def test_context_installer_uses_compatible_schema_copy_helper(self) -> None:
        context_installer = (
            ROOT / "scripts/install-production-release-context.sh"
        ).read_text()
        self.assertIn(
            'install_versioned_source \\\n'
            '  "live-executor/schema/007_aave_economic_diagnostics.sql"',
            context_installer,
        )
        self.assertIn(
            'install_versioned_source \\\n'
            '  "live-executor/schema/008_revenue_provider_authority.sql"',
            context_installer,
        )
        self.assertIn(
            "live-executor/schema/009_single_primary_provider_authority.sql",
            context_installer,
        )
        self.assertIn(
            'install_versioned_source \\\n'
            '  "live-executor/schema/010_atlas_auction_shadow.sql"',
            context_installer,
        )
        self.assertIn(
            'install_versioned_source \\\n'
            '  "live-executor/schema/011_atlas_liquidation_ground_truth.sql"',
            context_installer,
        )
        self.assertIn(
            '  "atlas-observer/scripts/export_rpc_transcript.py" \\\n'
            '  "$deploy_dir/atlas-export-rpc-transcript.py" 0750',
            context_installer,
        )
        self.assertNotIn("install_protected_file", context_installer)

    def test_context_installer_creates_rotation_config_directory(self) -> None:
        context_installer = (
            ROOT / "scripts/install-production-release-context.sh"
        ).read_text()
        create_config = context_installer.index('"$deploy_dir/config"')
        install_plan = context_installer.index(
            "config/phoenix-executor-rotation-plan.json"
        )
        install_artifacts = context_installer.index(
            "config/phoenix-executor-rotation-artifacts.json"
        )
        self.assertLess(create_config, install_plan)
        self.assertLess(create_config, install_artifacts)

    @staticmethod
    def emergency_status(*, shadow: bool = False) -> dict[str, object]:
        return {
            "active_build_run_id": 1,
            "active_release": RELEASE_SHA,
            "autonomous_execution": False if shadow else True,
            "live_execution": False if shadow else True,
            "phoenix_mode": "SHADOW" if shadow else "LIVE",
            "protocol_version": PROTOCOL_VERSION,
            "release_assets_sha": RELEASE_SHA,
            "schema": "phoenix.release-status.v1",
        }

    @staticmethod
    def pause_observation(paused: bool) -> dict[str, object]:
        return {
            "chain_id": "0xa4b1",
            "executor_address": "0x" + "1" * 40,
            "paused": paused,
            "provider_identity": "rpc-bf27592026588e7d",
            "runtime_code_hash": "a" * 64,
        }

    def emergency_paths(self, temporary: Path) -> HostPaths:
        paths = self.paths(temporary)
        paths.env_file.write_text(
            "LIVE_EXECUTOR_EXECUTOR_ADDRESS=0x" + "1" * 40 + "\n"
            "LIVE_EXECUTOR_EXECUTOR_CODE_HASH=" + "a" * 64 + "\n",
            encoding="utf-8",
        )
        return paths

    @staticmethod
    def emergency_controls() -> dict[str, object]:
        return {
            "active_attempts": 0,
            "active_atlas": 0,
            "aave_armed": False,
            "aave_kill_switch": True,
            "armed": False,
            "atlas_armed": False,
            "atlas_kill_switch": True,
            "execution_mode": "disarmed",
            "kill_switch": True,
            "open_routes": 0,
            "outbox_ack_pending": 0,
            "outbox_claimable": 0,
            "outbox_pending": 0,
            "submission_lock_free": True,
            "unresolved_submissions": 0,
        }

    def test_emergency_pause_uses_authenticated_already_paused_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            initial = self.emergency_status()
            final = self.emergency_status(shadow=True)
            controls = self.emergency_controls()
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    side_effect=[initial, final],
                ),
                patch(
                    "scripts.phoenix_release.gateway._require_success",
                    return_value="",
                ) as require_success,
                patch(
                    "scripts.phoenix_release.gateway._live_executor_stopped",
                    return_value={"running_container_ids": [], "stopped": True},
                ) as stopped,
                patch(
                    "scripts.phoenix_release.gateway._control_evidence",
                    side_effect=[controls, controls],
                ) as control_evidence,
                patch(
                    "scripts.phoenix_release.gateway.observe_contract_pause",
                    side_effect=[
                        self.pause_observation(True),
                        self.pause_observation(True),
                    ],
                ) as observe_pause,
                patch("scripts.phoenix_release.gateway._run") as owner_pause,
            ):
                observed = emergency_pause(paths)

            owner_pause.assert_not_called()
            self.assertEqual(stopped.call_count, 2)
            self.assertEqual(control_evidence.call_args_list[0].args, (paths, initial))
            self.assertEqual(control_evidence.call_args_list[1].args, (paths, final))
            self.assertEqual(observe_pause.call_count, 2)
            self.assertEqual(observed["pause_action"], "already-paused")
            self.assertTrue(observed["pause_evidence"]["paused"])
            self.assertEqual(require_success.call_args_list[0].args[1], "EMERGENCY_EXECUTOR_STOP_FAILED")
            self.assertEqual(
                require_success.call_args_list[1].args,
                (
                    [
                        "/usr/bin/python3",
                        "-I",
                        "-B",
                        str(paths.libexec / "production_mode.py"),
                        "shadow",
                        "--env-file",
                        str(paths.env_file),
                    ],
                    "EMERGENCY_SHADOW_RESTORE_FAILED",
                ),
            )

    def test_emergency_pause_applies_owner_pause_before_shadow(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            initial = self.emergency_status()
            final = self.emergency_status(shadow=True)
            controls = self.emergency_controls()
            with (
                patch("scripts.phoenix_release.gateway.status", side_effect=[initial, final]),
                patch("scripts.phoenix_release.gateway._require_success", return_value=""),
                patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                patch("scripts.phoenix_release.gateway._control_evidence", side_effect=[controls, controls]),
                patch(
                    "scripts.phoenix_release.gateway.observe_contract_pause",
                    side_effect=[
                        self.pause_observation(False),
                        self.pause_observation(True),
                        self.pause_observation(True),
                    ],
                ),
                patch(
                    "scripts.phoenix_release.gateway._run",
                    return_value=subprocess.CompletedProcess([], 0, "", ""),
                ) as owner_pause,
            ):
                observed = emergency_pause(paths)

            owner_pause.assert_called_once()
            self.assertEqual(owner_pause.call_args.args[0][-2:], ["live-executor", "owner-pause"])
            self.assertEqual(observed["pause_action"], "applied")

    def test_emergency_pause_never_shadows_on_failed_proof(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            initial = self.emergency_status()
            controls = self.emergency_controls()
            for failure in ("stop", "controls", "chain"):
                require_side_effect = (
                    GatewayError("EMERGENCY_EXECUTOR_STOP_FAILED")
                    if failure == "stop"
                    else None
                )
                failing_controls = dict(controls)
                if failure == "controls":
                    failing_controls["armed"] = True
                observe_side_effect = (
                    ReconciliationError("CHAIN_EVIDENCE_RPC_UNAVAILABLE")
                    if failure == "chain"
                    else (
                        [self.pause_observation(False), self.pause_observation(True)]
                        if failure == "controls"
                        else self.pause_observation(True)
                    )
                )
                with (
                    self.subTest(failure=failure),
                    patch("scripts.phoenix_release.gateway.status", return_value=initial),
                    patch(
                        "scripts.phoenix_release.gateway._require_success",
                        side_effect=require_side_effect,
                    ) as require_success,
                    patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                    patch("scripts.phoenix_release.gateway._control_evidence", return_value=failing_controls),
                    patch("scripts.phoenix_release.gateway.observe_contract_pause", side_effect=observe_side_effect),
                    patch(
                        "scripts.phoenix_release.gateway._run",
                        return_value=subprocess.CompletedProcess([], 0, "", ""),
                    ) as owner_pause,
                    self.assertRaises(GatewayError),
                ):
                    emergency_pause(paths)
                if failure == "controls":
                    owner_pause.assert_called_once()
                else:
                    owner_pause.assert_not_called()
                self.assertFalse(
                    any(
                        call.args[1] == "EMERGENCY_SHADOW_RESTORE_FAILED"
                        for call in require_success.call_args_list
                    )
                )

    def test_emergency_pause_rejects_owner_and_post_owner_failures(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            initial = self.emergency_status()
            controls = self.emergency_controls()
            for case, owner_result, observations, expected_code in (
                (
                    "owner-failed",
                    subprocess.CompletedProcess([], 1, "failed", ""),
                    [self.pause_observation(False)],
                    "EMERGENCY_CONTRACT_PAUSE_FAILED",
                ),
                (
                    "still-unpaused",
                    subprocess.CompletedProcess([], 0, "", ""),
                    [self.pause_observation(False), self.pause_observation(False)],
                    "EMERGENCY_CONTRACT_NOT_PAUSED",
                ),
                (
                    "post-owner-proof-failed",
                    subprocess.CompletedProcess([], 0, "", ""),
                    [
                        self.pause_observation(False),
                        GatewayError("CHAIN_EVIDENCE_RPC_UNAVAILABLE"),
                    ],
                    "CHAIN_EVIDENCE_RPC_UNAVAILABLE",
                ),
            ):
                with (
                    self.subTest(case=case),
                    patch("scripts.phoenix_release.gateway.status", return_value=initial),
                    patch(
                        "scripts.phoenix_release.gateway._require_success",
                        return_value="",
                    ) as require_success,
                    patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                    patch(
                        "scripts.phoenix_release.gateway._control_evidence",
                        return_value=controls,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway.observe_contract_pause",
                        side_effect=observations,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._run",
                        return_value=owner_result,
                    ),
                    self.assertRaisesRegex(GatewayError, expected_code),
                ):
                    emergency_pause(paths)
                self.assertFalse(
                    any(
                        call.args[1] == "EMERGENCY_SHADOW_RESTORE_FAILED"
                        for call in require_success.call_args_list
                    )
                )

    def test_emergency_pause_rejects_post_shadow_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            controls = self.emergency_controls()
            initial = self.emergency_status()
            cases: tuple[tuple[str, dict[str, object]], ...] = (
                (
                    "pointer",
                    {**self.emergency_status(shadow=True), "active_release": "b" * 40},
                ),
                (
                    "assets",
                    {
                        **self.emergency_status(shadow=True),
                        "release_assets_sha": "b" * 40,
                    },
                ),
                (
                    "mode",
                    {**self.emergency_status(shadow=True), "phoenix_mode": "LIVE"},
                ),
                (
                    "live-flag",
                    {**self.emergency_status(shadow=True), "live_execution": True},
                ),
                (
                    "autonomous-flag",
                    {
                        **self.emergency_status(shadow=True),
                        "autonomous_execution": True,
                    },
                ),
            )
            for case, final in cases:
                with (
                    self.subTest(case=case),
                    patch(
                        "scripts.phoenix_release.gateway.status",
                        side_effect=[initial, final],
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._require_success",
                        return_value="",
                    ),
                    patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                    patch(
                        "scripts.phoenix_release.gateway._control_evidence",
                        return_value=controls,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway.observe_contract_pause",
                        return_value=self.pause_observation(True),
                    ),
                    patch("scripts.phoenix_release.gateway._run") as owner_pause,
                    self.assertRaisesRegex(
                        GatewayError,
                        "EMERGENCY_SHADOW_POSTCONDITION_FAILED",
                    ),
                ):
                    emergency_pause(paths)
                owner_pause.assert_not_called()

    def test_emergency_pause_requires_every_final_safety_proof(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            closed = self.emergency_controls()
            initial = self.emergency_status()
            final = self.emergency_status(shadow=True)
            for case in ("executor", "controls", "pause-false", "pause-error"):
                final_controls = dict(closed)
                if case == "controls":
                    final_controls["atlas_armed"] = True
                stopped_effect: list[object] = [
                    {"running_container_ids": [], "stopped": True},
                    (
                        GatewayError("READINESS_LIVE_EXECUTOR_ACTIVE")
                        if case == "executor"
                        else {"running_container_ids": [], "stopped": True}
                    ),
                ]
                pause_effect: list[object] = [self.pause_observation(True)]
                if case == "pause-false":
                    pause_effect.append(self.pause_observation(False))
                elif case == "pause-error":
                    pause_effect.append(GatewayError("CHAIN_EVIDENCE_RPC_UNAVAILABLE"))
                else:
                    pause_effect.append(self.pause_observation(True))
                with (
                    self.subTest(case=case),
                    patch(
                        "scripts.phoenix_release.gateway.status",
                        side_effect=[initial, final],
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._require_success",
                        return_value="",
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._live_executor_stopped",
                        side_effect=stopped_effect,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._control_evidence",
                        side_effect=[closed, final_controls],
                    ),
                    patch(
                        "scripts.phoenix_release.gateway.observe_contract_pause",
                        side_effect=pause_effect,
                    ),
                    patch("scripts.phoenix_release.gateway._run") as owner_pause,
                    self.assertRaises(GatewayError),
                ):
                    emergency_pause(paths)
                owner_pause.assert_not_called()

    def test_emergency_pause_shadow_command_failure_stops_before_postconditions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            closed = self.emergency_controls()
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    return_value=self.emergency_status(),
                ) as status_call,
                patch(
                    "scripts.phoenix_release.gateway._require_success",
                    side_effect=["", GatewayError("EMERGENCY_SHADOW_RESTORE_FAILED")],
                ),
                patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                patch(
                    "scripts.phoenix_release.gateway._control_evidence",
                    return_value=closed,
                ),
                patch(
                    "scripts.phoenix_release.gateway.observe_contract_pause",
                    return_value=self.pause_observation(True),
                ) as observe_pause,
                patch("scripts.phoenix_release.gateway._run") as owner_pause,
                self.assertRaisesRegex(
                    GatewayError,
                    "EMERGENCY_SHADOW_RESTORE_FAILED",
                ),
            ):
                emergency_pause(paths)
            self.assertEqual(status_call.call_count, 1)
            self.assertEqual(observe_pause.call_count, 1)
            owner_pause.assert_not_called()

    def test_emergency_pause_rejects_forged_pause_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            controls = self.emergency_controls()
            for field, value in (
                ("chain_id", "0x1"),
                ("executor_address", "0x" + "2" * 40),
                ("provider_identity", "rpc-0123456789abcdef"),
                ("runtime_code_hash", "b" * 64),
                ("paused", 1),
            ):
                forged = {**self.pause_observation(True), field: value}
                with (
                    self.subTest(field=field),
                    patch(
                        "scripts.phoenix_release.gateway.status",
                        return_value=self.emergency_status(),
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._require_success",
                        return_value="",
                    ) as require_success,
                    patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                    patch(
                        "scripts.phoenix_release.gateway._control_evidence",
                        return_value=controls,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway.observe_contract_pause",
                        return_value=forged,
                    ),
                    patch("scripts.phoenix_release.gateway._run") as owner_pause,
                    self.assertRaisesRegex(
                        GatewayError,
                        "CHAIN_EVIDENCE_PAUSE_STATE_INVALID",
                    ),
                ):
                    emergency_pause(paths)
                owner_pause.assert_not_called()
                self.assertFalse(
                    any(
                        call.args[1] == "EMERGENCY_SHADOW_RESTORE_FAILED"
                        for call in require_success.call_args_list
                    )
                )

    def test_emergency_pause_is_idempotent_from_shadow(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            shadow = self.emergency_status(shadow=True)
            controls = self.emergency_controls()
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    side_effect=[shadow, shadow],
                ),
                patch(
                    "scripts.phoenix_release.gateway._require_success",
                    return_value="",
                ) as require_success,
                patch("scripts.phoenix_release.gateway._live_executor_stopped"),
                patch(
                    "scripts.phoenix_release.gateway._control_evidence",
                    return_value=controls,
                ),
                patch(
                    "scripts.phoenix_release.gateway.observe_contract_pause",
                    return_value=self.pause_observation(True),
                ),
                patch("scripts.phoenix_release.gateway._run") as owner_pause,
            ):
                observed = emergency_pause(paths)

            owner_pause.assert_not_called()
            self.assertEqual(observed["pause_action"], "already-paused")
            stop_command = require_success.call_args_list[0].args[0]
            self.assertIn("--overlay-file", stop_command)
            self.assertEqual(
                stop_command[-5:],
                ["--", "stop", "-t", "30", "live-executor"],
            )

    def test_post_recovery_live_mode_changes_only_operator_mode(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            shadow = self.emergency_status(shadow=True)
            live = self.emergency_status()
            controls = self.emergency_controls()
            with (
                patch(
                    "scripts.phoenix_release.gateway.status",
                    side_effect=[shadow, live],
                ),
                patch(
                    "scripts.phoenix_release.gateway._require_success",
                    return_value="",
                ) as require_success,
                patch(
                    "scripts.phoenix_release.gateway._live_executor_stopped"
                ) as stopped,
                patch(
                    "scripts.phoenix_release.gateway._control_evidence",
                    side_effect=[controls, controls],
                ) as control_evidence,
            ):
                observed = enter_post_recovery_live_mode(
                    paths,
                    RELEASE_SHA,
                    "enter-recovered-live-mode-42161",
                )

            self.assertEqual(observed["status"], "live-mode-ready")
            self.assertEqual(observed["phoenix_mode"], "LIVE")
            self.assertTrue(observed["contract_paused"])
            self.assertFalse(observed["aave_armed"])
            self.assertFalse(observed["atlas_armed"])
            self.assertTrue(observed["generic_closed"])
            self.assertEqual(stopped.call_count, 2)
            self.assertEqual(control_evidence.call_count, 2)
            self.assertEqual(require_success.call_count, 5)
            preflight_before = require_success.call_args_list[0].args[0]
            signer_before = require_success.call_args_list[1].args[0]
            live_mutation = require_success.call_args_list[2].args[0]
            preflight_after = require_success.call_args_list[3].args[0]
            signer_after = require_success.call_args_list[4].args[0]
            self.assertEqual(preflight_before, preflight_after)
            self.assertEqual(signer_before, signer_after)
            self.assertEqual(
                preflight_before[-2:],
                ["autonomous-control", "preflight-post-recovery-live-mode"],
            )
            self.assertIn(
                "PHOENIX_ENTER_LIVE_MODE_ACK=ENTER_RECOVERED_LIVE_MODE_42161",
                preflight_before,
            )
            self.assertEqual(
                signer_before[-3:],
                [
                    "/usr/local/bin/autonomous-live-control",
                    "live-executor",
                    "owner-configured-signer-preflight",
                ],
            )
            self.assertEqual(
                live_mutation,
                [
                    "/usr/bin/python3",
                    "-I",
                    "-B",
                    str(paths.libexec / "production_mode.py"),
                    "live",
                    "--env-file",
                    str(paths.env_file),
                ],
            )
            self.assertFalse(
                any("owner-unpause" in call.args[0] for call in require_success.call_args_list)
            )
            self.assertFalse(
                any("arm-revenue-lanes" in call.args[0] for call in require_success.call_args_list)
            )

    def test_post_recovery_live_mode_rejects_non_shadow_or_open_authority(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            shadow = self.emergency_status(shadow=True)
            cases = (
                ("wrong-ack", shadow, self.emergency_controls()),
                ("already-live", self.emergency_status(), self.emergency_controls()),
                (
                    "release-mismatch",
                    {**shadow, "active_release": "b" * 40},
                    self.emergency_controls(),
                ),
                (
                    "assets-mismatch",
                    {**shadow, "release_assets_sha": "b" * 40},
                    self.emergency_controls(),
                ),
                (
                    "open-aave",
                    shadow,
                    {**self.emergency_controls(), "aave_armed": True},
                ),
            )
            for case, initial, controls in cases:
                acknowledgement = (
                    "wrong" if case == "wrong-ack" else "enter-recovered-live-mode-42161"
                )
                with (
                    self.subTest(case=case),
                    patch(
                        "scripts.phoenix_release.gateway.status",
                        return_value=initial,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._live_executor_stopped"
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._control_evidence",
                        return_value=controls,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._require_success"
                    ) as require_success,
                    self.assertRaises(GatewayError),
                ):
                    enter_post_recovery_live_mode(
                        paths,
                        RELEASE_SHA,
                        acknowledgement,
                    )
                require_success.assert_not_called()

    def test_post_recovery_live_mode_restores_shadow_after_mutation_failure(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            paths = self.emergency_paths(Path(temporary))
            shadow = self.emergency_status(shadow=True)
            live = self.emergency_status()
            controls = self.emergency_controls()
            for case, effects, statuses in (
                (
                    "mutation-command",
                    [
                        "",
                        "",
                        GatewayError("POST_RECOVERY_LIVE_MODE_MUTATION_FAILED"),
                        "",
                    ],
                    [shadow, shadow],
                ),
                (
                    "final-preflight",
                    [
                        "",
                        "",
                        "",
                        GatewayError("POST_RECOVERY_LIVE_FINAL_PREFLIGHT_FAILED"),
                        "",
                    ],
                    [shadow, live, shadow],
                ),
            ):
                with (
                    self.subTest(case=case),
                    patch(
                        "scripts.phoenix_release.gateway.status",
                        side_effect=statuses,
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._require_success",
                        side_effect=effects,
                    ) as require_success,
                    patch(
                        "scripts.phoenix_release.gateway._live_executor_stopped"
                    ),
                    patch(
                        "scripts.phoenix_release.gateway._control_evidence",
                        return_value=controls,
                    ),
                    self.assertRaises(GatewayError),
                ):
                    enter_post_recovery_live_mode(
                        paths,
                        RELEASE_SHA,
                        "enter-recovered-live-mode-42161",
                    )
                shadow_commands = [
                    call.args[0]
                    for call in require_success.call_args_list
                    if "shadow" in call.args[0]
                ]
                self.assertEqual(len(shadow_commands), 1)
                self.assertEqual(
                    shadow_commands[0],
                    [
                        "/usr/bin/python3",
                        "-I",
                        "-B",
                        str(paths.libexec / "production_mode.py"),
                        "shadow",
                        "--env-file",
                        str(paths.env_file),
                    ],
                )
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
        self.required_absence = (
            ROOT / "scripts/required-service-absence.sh"
        ).read_text()
        self.rehearsal = (
            ROOT / "scripts/rehearse-production-release.sh"
        ).read_text()
        self.healthcheck = (
            ROOT / "scripts/production-healthcheck.sh"
        ).read_text()
        self.autonomous_schema = (
            ROOT / "live-executor/schema/003_autonomous_hunter_contracts.sql"
        ).read_text()
        self.dashboard_sql = (
            ROOT / "scripts/sql/economic-dashboard-snapshot.sql"
        ).read_text()
        self.live_mode_workflow = (
            ROOT
            / ".github/workflows/phoenix-enter-post-recovery-live-mode.yml"
        ).read_text()
        self.post_arm_monitor = (
            ROOT / "scripts/monitor-post-arm-revenue.sh"
        ).read_text()
        self.live_control = (
            ROOT / "live-executor/src/autonomous_live_control_main.rs"
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

    def test_post_recovery_live_workflow_is_exact_protected_and_serialized(
        self,
    ) -> None:
        workflow = self.live_mode_workflow
        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotIn("workflow_run:", workflow)
        self.assertNotIn("schedule:", workflow)
        self.assertIn("environment: production-live", workflow)
        self.assertIn("group: phoenix-production-release", workflow)
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertIn(
            "vars.PHOENIX_AUTORELEASE_ENABLED == 'false'", workflow
        )
        self.assertIn("ENTER_RECOVERED_LIVE_MODE_42161", workflow)
        self.assertIn("git/ref/heads/main", workflow)
        self.assertIn("phoenix-release-controller.yml/runs?per_page=100", workflow)
        self.assertIn('.status == "queued" or .status == "in_progress"', workflow)
        self.assertIn("PROD_SSH_PRIVATE_KEY", workflow)
        self.assertIn("StrictHostKeyChecking=yes", workflow)
        self.assertIn(
            '"enter-post-recovery-live-mode ${RELEASE_SHA} '
            'enter-recovered-live-mode-42161"',
            workflow,
        )
        self.assertIn(".contract_paused == true", workflow)
        self.assertIn(".aave_armed == false", workflow)
        self.assertIn(".atlas_armed == false", workflow)
        self.assertIn(".generic_closed == true", workflow)
        self.assertEqual(
            workflow.count(
                "actions/variables/PHOENIX_AUTORELEASE_ENABLED"
            ),
            2,
        )

    def test_arm_unpause_and_monitor_require_exact_operator_live_mode(self) -> None:
        control = self.live_control
        owner = control.split(
            "async fn owner_mutation", maxsplit=1
        )[1].split("fn preflight_request", maxsplit=1)[0]
        self.assertLess(
            owner.index("require_live_operator_mode()?"),
            owner.index("execute_from_environment(mutation).await?"),
        )
        arm = control.split(
            "async fn arm_revenue_lanes()", maxsplit=1
        )[1].split("async fn arm_revenue_lanes_in_pool", maxsplit=1)[0]
        self.assertLess(
            arm.index("require_live_operator_mode()?"),
            arm.index("database_pool().await?"),
        )
        self.assertIn(
            'identity=$(operator_mode_identity) ||', self.post_arm_monitor
        )
        self.assertIn(
            '[ "$identity" = "LIVE:true:true" ]', self.post_arm_monitor
        )
        loop = self.post_arm_monitor.split("while :; do", maxsplit=1)[1]
        self.assertGreaterEqual(loop.count("require_operator_live_mode"), 2)
        preflight_conditional = loop.split(
            "current_control_status=", maxsplit=1
        )[0].split('if [ "$authority_state" = "armed" ]; then', maxsplit=1)[1]
        armed_branch, disarmed_branch = preflight_conditional.split(
            "else", maxsplit=1
        )
        self.assertIn("live-executor owner-live-preflight", armed_branch)
        self.assertNotIn("owner-configured-preflight", armed_branch)
        self.assertIn("owner-configured-preflight", disarmed_branch)
        self.assertNotIn("owner-live-preflight", disarmed_branch)
        for required in (
            "require_hunter_ready",
            "require_latency_gauges",
            "require_latency_histograms",
            "require_disarmed_controls",
            "require_armed_controls",
            "authority_state=${3:-armed}",
            "phoenix_signal_to_prefilter_seconds",
            "phoenix_liquidatable_to_exact_enqueue_seconds",
            "phoenix_exact_queue_wait_seconds",
            "phoenix_exact_first_rpc_dispatch_seconds",
            "phoenix_exact_rpc_state_fetch_seconds",
            "phoenix_exact_compute_seconds",
            "phoenix_exact_end_to_end_seconds",
            "PHOENIX_MONITOR_INTERVAL_BOUNDARY",
            "provider_health_counter_vector",
            "provider_gateway_counter_vector",
            "runtime_evidence",
            "gateway provider/budget/error counters regressed",
            "exact runtime identity/restart/OOM evidence regressed",
            "primary Exact samples did not advance during monitoring",
        ):
            self.assertIn(required, self.post_arm_monitor)

        for required in (
            "PHOENIX_MONITOR_GAUGE_BOUNDARY",
            "POST_ARM_LATENCY_GAUGE_FAILED",
            "actionable_queue_depth",
            "in_flight_count",
            "worker_queued_count",
            "permit_availability",
            "oldest_actionable_age_seconds",
            "exact_completed_delta",
            "histogram_counter_reset",
            "insufficient_interval_observations",
            "POST_ARM_LATENCY_INTERVAL_IDLE",
            "phoenix_fork_queue_wait_seconds",
            "phoenix_fork_runtime_seconds",
            "p50_upper_bound_seconds",
        ):
            self.assertIn(required, self.post_arm_monitor)

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

    def test_controller_bootstrap_readiness_is_bound_and_fail_closed(
        self,
    ) -> None:
        host_plan = self.workflow.split(
            "- name: Read active immutable release",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn('readiness_rc=0', host_plan)
        self.assertIn('[ "$readiness_rc" -eq 1 ]', host_plan)
        self.assertIn(
            '899daa33b99863f5076bd9b39b6237da19865a5f',
            host_plan,
        )
        self.assertIn('.evidence.failure_count == 1', host_plan)
        self.assertIn('"service": "atlas-observer"', host_plan)
        self.assertIn('.evidence.checks.controls.armed == false', host_plan)
        self.assertIn('.evidence.checks.controls.kill_switch == true', host_plan)
        self.assertIn(
            '.evidence.checks.contract.contract_paused == true',
            host_plan,
        )
        self.assertIn('select(.service != "live-executor")', host_plan)
        self.assertIn('.running == true and .health == "healthy"', host_plan)
        self.assertIn('"state": "stopped"', host_plan)
        self.assertIn(
            'schema: "phoenix.production-readiness-bootstrap.v1"',
            host_plan,
        )

    def test_controller_monitor_bootstrap_is_pinned_and_fail_closed(
        self,
    ) -> None:
        host_plan = self.workflow.split(
            "- name: Read active immutable release",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("8ac529a9af7caeacb8883a51024e5970dae6f281", host_plan)
        self.assertIn(
            '.evidence.failures[0].code == "READINESS_SERVICE_UNHEALTHY"',
            host_plan,
        )
        self.assertIn(
            '.evidence.failures[0].evidence.service == "economic-monitor"',
            host_plan,
        )
        self.assertIn('accepted_false_positive: "economic-monitor"', host_plan)
        self.assertIn(".controls.submission_lock_free == true", host_plan)
        self.assertIn(".controls.outbox_pending == 0", host_plan)
        # The economic-monitor bridge must sit ahead of the final
        # fail-closed branch and accept exactly one failure class.
        self.assertLess(
            host_plan.index('.failures[0].code == "READINESS_SERVICE_UNHEALTHY"'),
            host_plan.index("PRODUCTION_READINESS_UNBRIDGED"),
        )

    def test_controller_has_no_dead_historical_forward_fix_fallback(
        self,
    ) -> None:
        self.assertNotIn("FORWARD_FIX_BOOTSTRAP_VALIDATOR_BEGIN", self.workflow)
        self.assertNotIn("FORWARD_FIX_BOOTSTRAP_VALIDATOR_END", self.workflow)
        self.assertNotIn("forward-fix-failed-release.json", self.workflow)
        self.assertNotIn(
            "1776be2f7e9d2921589e4b4eb0faf5fe057e38fb", self.workflow
        )

    def test_controller_fails_closed_on_unbridged_readiness_failure(
        self,
    ) -> None:
        host_plan = self.workflow.split(
            "- name: Read active immutable release",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("PRODUCTION_READINESS_UNBRIDGED", host_plan)
        self.assertIn("cat production-readiness-error.json >&2", host_plan)
        self.assertLess(
            host_plan.index("PRODUCTION_READINESS_UNBRIDGED"),
            host_plan.rindex("rm -f production-readiness-output.json"),
        )

    def test_deploy_checkout_preserves_rollback_history(self) -> None:
        checkout = self.workflow.split(
            "- name: Checkout exact protected main release",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        self.assertIn("fetch-depth: 0", checkout)

    def test_deploy_reconciles_generation_plan_before_provenance_check(
        self,
    ) -> None:
        deploy = self.workflow.split(
            "- name: Verify, package, receive, and resume",
            maxsplit=1,
        )[1].split("- name:", maxsplit=1)[0]
        resolver = deploy.index("resolve-protected-build-plan")
        built_check = deploy.index(
            '"$(jq -c \'.built_images\' package/release-provenance.json)"'
        )
        self.assertLess(resolver, built_check)
        self.assertIn(
            "--protected-base-manifest package/rollback-manifest.json",
            deploy,
        )

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

    def test_deploy_quiesces_live_canary_clients_before_schema_migration(self) -> None:
        operation = self.deploy.index("\ncompose pull $pull_services\n")
        stop = self.deploy.index("for schema_client in", operation)
        for service in (
            "atlas-observer",
            "economic-monitor",
            "economic-supervisor",
        ):
            self.assertIn(service, self.deploy[stop:])
        absence = self.deploy.index('compose ps -q "$schema_client"', stop)
        engine_stop = self.deploy.index(
            'compose_shadow_with_release_env "$rollback_release_env"', absence
        )
        engine_absence = self.deploy.index("ps -q phoenix-engine", engine_stop)
        migrate = self.deploy.index("autonomous-control migrate", engine_absence)
        restart = self.deploy.index(
            "compose up -d --no-deps atlas-observer", migrate
        )
        self.assertLess(stop, absence)
        self.assertLess(absence, engine_stop)
        self.assertLess(engine_stop, engine_absence)
        self.assertLess(engine_absence, migrate)
        self.assertLess(absence, migrate)
        self.assertLess(migrate, restart)
        shadow_helper = self.deploy.split(
            "compose_shadow_with_release_env()", maxsplit=1
        )[1].split("capture_service_ids()", maxsplit=1)[0]
        self.assertIn("--mode SHADOW", shadow_helper)
        self.assertNotIn("--overlay-file", shadow_helper)

    def test_deploy_compensation_shadows_before_rollback_context_install(self) -> None:
        compensation = self.deploy.split(
            "rollback_on_failure()", maxsplit=1
        )[1].split("mutation_started=0", maxsplit=1)[0]
        shadow = compensation.index(
            'python3 "$deploy_dir/production_mode.py" shadow --env-file "$env_file"'
        )
        context = compensation.index('"$rollback_context_installer"', shadow)
        rollback = compensation.index('/bin/sh "$rollback_script"', context)
        self.assertLess(shadow, context)
        self.assertLess(context, rollback)
        self.assertNotIn("autonomous-control disarm", compensation)
        self.assertIn("autonomous-control disarm", self.rollback)

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
        self.assertIn(
            "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            self.dashboard_sql,
        )
        self.assertIn(
            "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql",
            self.rehearsal,
        )
        self.assertIn("candidate_monitor_sql_inode_mismatch", self.rehearsal)
        self.assertIn("1000:1000", self.rehearsal)
        self.assertIn("candidate_monitor_unhealthy", self.rehearsal)
        self.assertIn("candidate_monitor_contract_invalid", self.rehearsal)
        self.assertIn("candidate_monitor_image_invalid", self.rehearsal)
        self.assertIn(
            "find /evidence/latest-dashboard.json -maxdepth 0 -type f "
            "-size +0c -mmin -3 -print -quit 2>/dev/null | grep -q .",
            self.rehearsal,
        )
        self.assertNotIn(
            "test -s /evidence/latest-dashboard.json", self.rehearsal
        )
        self.assertIn(
            "PHOENIX_ECONOMIC_DASHBOARD_QUERY_TIMEOUT_SECONDS=90",
            self.rehearsal,
        )
        self.assertIn(
            '"PHOENIX_ECONOMIC_DASHBOARD_QUERY_TIMEOUT_SECONDS": "90"',
            self.rehearsal,
        )
        self.assertIn('--network "$database_network"', self.rehearsal)
        self.assertIn(
            "POSTGRES_DSN=postgres://phoenix_rehearsal:", self.rehearsal
        )
        readiness = self.rehearsal.split(
            "database_deadline=", maxsplit=1
        )[1].split(
            '[ "$database_ready" -eq 1 ]', maxsplit=1
        )[0]
        self.assertIn("psql -X -qAt -v ON_ERROR_STOP=1", readiness)
        self.assertIn("-d phoenix_rehearsal", readiness)
        self.assertIn("-c 'SELECT 1'", readiness)
        self.assertNotIn("pg_isready", readiness)
        self.assertIn("PGCONNECT_TIMEOUT=5", self.rehearsal)
        self.assertIn("statement_timeout=60000", self.rehearsal)
        self.assertIn("deadline=$(( monitor_started_at + 180 ))", self.rehearsal)
        self.assertIn("log_monitor_diagnostics", self.rehearsal)
        self.assertIn("monitor_healthy_seconds", self.rehearsal)
        self.assertIn("candidate_monitor_exited", self.rehearsal)
        self.assertIn('docker logs --tail 20 "$monitor_container"', self.rehearsal)
        self.assertIn("candidate_control_contract_invalid", self.rehearsal)
        self.assertIn("candidate_control_status_invalid", self.rehearsal)
        self.assertIn('"$control_image" status', self.rehearsal)
        self.assertIn(
            'global_control.get("execution_mode") != "disabled"',
            self.rehearsal,
        )
        self.assertIn(
            "execution_mode TEXT NOT NULL DEFAULT 'disabled'",
            self.autonomous_schema,
        )
        self.assertIn(
            "/usr/bin/timeout --signal=TERM --kill-after=2s 45s",
            self.rehearsal,
        )
        self.assertIn("statement_timeout=30000", self.rehearsal)
        sql_probe = self.rehearsal.split(
            "# Prove the candidate dashboard query", maxsplit=1
        )[1].split("# Run the complete candidate monitor", maxsplit=1)[0]
        self.assertIn('/usr/bin/docker exec -i \\', sql_probe)
        self.assertIn('"$database_container"', sql_probe)
        self.assertNotIn("compose exec", sql_probe)
        health_rehearsal = self.rehearsal.split(
            "# Exercise the candidate health implementation", maxsplit=1
        )[1]
        self.assertIn(
            'PHOENIX_RELEASE_ENV="$active_release_env"', health_rehearsal
        )
        self.assertNotIn("PHOENIX_RELEASE_MANIFEST=", health_rehearsal)
        self.assertIn(
            "PHOENIX_HEALTH_COMMAND_TIMEOUT_SECONDS=15", self.rehearsal
        )
        self.assertIn("candidate_health_contract_failed", self.rehearsal)
        self.assertIn(
            'docker rm -f -v "$monitor_container"',
            self.rehearsal,
        )
        self.assertIn(
            'PHOENIX_HEALTH_EXPECTED_MODE="$active_health_mode"',
            health_rehearsal,
        )
        self.assertIn(
            'PHOENIX_ENV_FILE="$candidate_active_env"', health_rehearsal
        )
        self.assertNotIn('PHOENIX_ENV_FILE="$env_file"', health_rehearsal)
        self.assertIn(
            "PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY=true",
            health_rehearsal,
        )
        self.assertIn(
            'PHOENIX_HEALTH_ALLOW_STOPPED_STANDBY="$active_health_allow_stopped_standby"',
            health_rehearsal,
        )
        self.assertIn(
            'allow_legacy_atlas_binary="${PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY:-false}"',
            self.healthcheck,
        )
        self.assertIn(
            '[ "$actual" = /usr/local/bin/atlas-aave-hunter ]',
            self.healthcheck,
        )
        self.assertIn(
            '[ "$actual" = /usr/local/bin/atlas-observer ]',
            self.healthcheck,
        )
        self.assertIn("active_health_mode=", health_rehearsal)
        self.assertIn(
            "active_health_allow_stopped_standby=false", health_rehearsal
        )
        self.assertIn(
            "active_live_executor_recheck_required=false", health_rehearsal
        )
        self.assertIn(
            "active_live_executor_id=$(active_live_compose ps --no-trunc -q "
            "live-executor)",
            health_rehearsal,
        )
        self.assertIn("active_live_executor_probe_failed", health_rehearsal)
        self.assertIn(
            '*[!0-9a-f]*) fail active_live_executor_identity_invalid',
            health_rehearsal,
        )
        self.assertIn(
            '[ "${#active_live_executor_id}" -eq 64 ]', health_rehearsal
        )
        self.assertIn(
            "active_engine_compose exec -T phoenix-engine /bin/sh -c",
            health_rehearsal,
        )
        self.assertIn(
            "'printf \"%s:%s:%s\\n\" \"$PHOENIX_MODE\" "
            '"$LIVE_EXECUTION" "$AUTONOMOUS_EXECUTION"\'',
            health_rehearsal,
        )
        self.assertIn(
            "active_runtime_identity_probe_failed", health_rehearsal
        )
        topology = health_rehearsal.split(
            'case "$active_declared_runtime_identity:'
            '$active_engine_runtime_identity" in',
            maxsplit=1,
        )[1].split(
            'PHOENIX_DEPLOY_ROOT="$deploy_root"', maxsplit=1
        )[0]
        native_shadow = topology.split(
            "SHADOW:false:false:SHADOW:false:false)", maxsplit=1
        )[1].split("LIVE:true:true:LIVE:true:true)", maxsplit=1)[0]
        self.assertNotIn("probe_active_live_executor", native_shadow)
        self.assertIn("active_health_mode=SHADOW", native_shadow)
        live = topology.split("LIVE:true:true:LIVE:true:true)", maxsplit=1)[1].split(
            "SHADOW:false:false:LIVE:true:true)", maxsplit=1
        )[0]
        self.assertIn("probe_active_live_executor", live)
        self.assertIn("active_live_executor_recheck_required=true", live)
        self.assertIn("active_health_mode=LIVE", live)
        self.assertIn("active_health_mode=DISARMED_EVIDENCE", live)
        self.assertIn("active_health_allow_stopped_standby=true", live)
        inherited_shadow = topology.split(
            "SHADOW:false:false:LIVE:true:true)", maxsplit=1
        )[1].split(
            "*) fail active_runtime_identity_invalid", maxsplit=1
        )[0]
        self.assertIn("probe_active_live_executor", inherited_shadow)
        self.assertIn(
            '[ -z "$active_live_executor_id" ] || '
            "fail active_shadow_executor_present",
            inherited_shadow,
        )
        self.assertIn(
            "active_health_mode=DISARMED_EVIDENCE", inherited_shadow
        )
        self.assertIn(
            "active_health_allow_stopped_standby=true", inherited_shadow
        )
        self.assertIn(
            "active_live_executor_recheck_required=true", inherited_shadow
        )
        self.assertIn("active_runtime_identity_invalid", topology)
        self.assertIn(
            "active_live_executor_id_after=$(\n"
            "    active_live_compose ps --no-trunc -q live-executor\n"
            "  )",
            health_rehearsal,
        )
        self.assertIn("active_live_executor_recheck_failed", health_rehearsal)
        self.assertIn(
            '[ "$active_live_executor_id_after" = "$active_live_executor_id" ]',
            health_rehearsal,
        )
        self.assertIn("active_live_executor_identity_drift", health_rehearsal)
        self.assertIn(
            'if [ "$active_live_executor_recheck_required" = true ]; then',
            health_rehearsal,
        )
        declared_identity = health_rehearsal.split(
            "active_declared_runtime_identity=$(awk", maxsplit=1
        )[1].split("active_health_mode=", maxsplit=1)[0]
        self.assertIn('$1 == "PHOENIX_MODE"', declared_identity)
        self.assertIn('$1 == "LIVE_EXECUTION"', declared_identity)
        self.assertIn('$1 == "AUTONOMOUS_EXECUTION"', declared_identity)
        self.assertIn(
            "mode_found != 1 || live_found != 1 || autonomous_found != 1",
            declared_identity,
        )
        engine_compose = health_rehearsal.split(
            "active_engine_compose()", maxsplit=1
        )[1].split("active_live_compose()", maxsplit=1)[0]
        self.assertIn("--mode SHADOW", engine_compose)
        self.assertNotIn("--overlay-file", engine_compose)
        live_compose = health_rehearsal.split(
            "active_live_compose()", maxsplit=1
        )[1].split("probe_active_live_executor()", maxsplit=1)[0]
        self.assertIn("--mode LIVE", live_compose)
        self.assertIn('--overlay-file "$overlay_file"', live_compose)
        self.assertIn(
            'operator_env_digest_after=$(sha256sum "$env_file"', health_rehearsal
        )
        self.assertIn(
            '[ "$operator_env_digest_after" = "$operator_env_digest_before" ]',
            health_rehearsal,
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
        self.assertIn("--expected-mode DISARMED_EVIDENCE", render)
        self.assertIn('--env-file "$candidate_evidence_env"', render)
        self.assertIn("--expected-mode LIVE", render)
        self.assertIn('--env-file "$candidate_live_env"', render)
        self.assertNotIn('--env-file "$env_file"', render)
        preparation = self.rehearsal.split(
            'python3 "$candidate_root/scripts/production_context.py" manifest-env',
            maxsplit=1,
        )[1].split(
            '"$candidate_root/scripts/render-production-compose.sh"',
            maxsplit=1,
        )[0]
        self.assertIn(
            'copy_candidate_environment "$env_file" "$candidate_active_env"',
            preparation,
        )
        self.assertIn("materialize-release-defaults", preparation)
        self.assertIn(
            'validate-production-env.sh" "$candidate_live_env"', preparation
        )
        self.assertIn('production_mode.py" shadow', preparation)
        self.assertIn('--env-file "$candidate_evidence_env"', preparation)
        self.assertIn("operator_env_digest_before", preparation)
        self.assertIn("operator_env_digest_after", self.rehearsal)

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
        self.assertIn(
            "--expected-mode DISARMED_EVIDENCE",
            self.deploy[preflight_render:preflight_failure],
        )
        self.assertIn(
            '--env-file "$candidate_evidence_env"',
            self.deploy[preflight_render:preflight_failure],
        )
        self.assertNotIn(
            '--env-file "$env_file"',
            self.deploy[preflight_render:preflight_failure],
        )
        evidence_preparation = self.deploy[
            self.deploy.index('cp "$env_file" "$candidate_evidence_env"'):
            preflight_render
        ]
        self.assertIn('production_mode.py" shadow', evidence_preparation)
        self.assertIn(
            '--env-file "$candidate_evidence_env"', evidence_preparation
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
        self.assertIn(
            "--expected-mode LIVE",
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

    def test_deploy_installs_the_exact_canonical_aave_seed_after_mutation(self) -> None:
        digest = "3d8513bb05607d9a91fece589872db3f5782953d13e24fc418ea2b4b0aa0239c"
        compose = (ROOT / "compose.prod.yml").read_text(encoding="utf-8")
        self.assertIn(
            "/var/lib/phoenix-atlas-hunter/evidence/"
            "aave-borrow-archive-7736400-489813224-0f204e03/"
            "aave-borrow-discovery.json",
            self.deploy,
        )
        self.assertIn(
            "/opt/phoenix/evidence/aave-discovery/aave-borrow-discovery.json",
            self.deploy,
        )
        self.assertIn(f"aave_discovery_sha256={digest}", self.deploy)
        self.assertIn(f"AAVE_BORROW_DISCOVERY_SHA256:-{digest}", compose)
        self.assertIn('[ ! -L "$aave_discovery_source" ]', self.deploy)
        self.assertIn('[ "$source_identity" = "0:0:600" ]', self.deploy)
        self.assertIn('rmdir "$aave_discovery_target"', self.deploy)
        self.assertIn('install -m 0444 -o root -g root', self.deploy)
        self.assertIn('mv -f "$seed_candidate" "$aave_discovery_target"', self.deploy)
        mutation = self.deploy.index("state_update mutation mutation_started")
        disarmed = self.deploy.index("mark_phase DISARMED_CONTROL_INSTALLED")
        install_seed = self.deploy.index("install_aave_discovery_seed", disarmed)
        start_services = self.deploy.index("for service in $start_services", install_seed)
        self.assertLess(mutation, install_seed)
        self.assertLess(disarmed, install_seed)
        self.assertLess(install_seed, start_services)

    def test_rollback_accepts_only_the_reviewed_legacy_atlas_binary(self) -> None:
        rollback_health = self.rollback.split(
            'PHOENIX_HEALTH_EXPECTED_MODE=SHADOW', maxsplit=1
        )[1].split('"$deploy_dir/production-healthcheck.sh"', maxsplit=1)[0]
        self.assertIn(
            "PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY=true",
            rollback_health,
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
        standby = self.deploy.index(
            "compose up -d --no-deps live-executor", evidence
        )
        standby_controls = self.deploy.index(
            "hunting standby changed fail-closed runtime controls", standby
        )
        self.assertLess(evidence, standby)
        self.assertLess(standby, standby_controls)
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
            "mark_phase HUNTING_STANDBY_STARTED",
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

    def test_generation_transitions_pull_only_manifest_bound_services(self) -> None:
        pull_plan = self.deploy.split("pull_services=$(", maxsplit=1)[1].split(
            "remove_services=$(", maxsplit=1
        )[0]
        self.assertIn("--mode DISARMED_EVIDENCE", pull_plan)
        self.assertIn("--field pull_services", pull_plan)
        self.assertIn("compose pull $pull_services", self.deploy)
        self.assertNotIn("\ncompose pull\n", self.deploy)
        self.assertIn(
            '--mode SHADOW --field pull_services', self.rollback
        )
        self.assertIn("compose pull $pull_services", self.rollback)
        self.assertNotIn("\ncompose pull\n", self.rollback)
        cleanup = self.rollback.split(
            "remove_intentionally_absent_services()", maxsplit=1
        )[1].split("if [ -f \"$overlay_file\"", maxsplit=1)[0]
        self.assertIn("for service in $absent_services", cleanup)
        self.assertIn('grep -F -x "$service"', cleanup)
        self.assertIn('current_live_compose rm -f "$service"', cleanup)
        self.assertNotIn("docker rm", cleanup)
        self.assertNotIn("remove-orphans", cleanup)
        self.assertNotIn("prune", cleanup)

    def test_rollback_cleanup_is_blocked_by_attempt_submission_or_lock(self) -> None:
        safety = self.rollback.split(
            "rollback_cleanup_safe()", maxsplit=1
        )[1].split("remove_intentionally_absent_services()", maxsplit=1)[0]
        self.assertIn("live_canary.execution_attempts", safety)
        self.assertIn("'claimed'", safety)
        self.assertIn("'submission_unknown'", safety)
        self.assertIn("'pending'", safety)
        self.assertIn("'timed_out'", safety)
        self.assertIn("live_canary.global_revenue_submission_lock", safety)
        self.assertIn("[ \"$cleanup_state\" = '0|0|' ]", safety)
        gate = self.rollback.index("rollback_cleanup_safe ||")
        reconciliation_failure = self.rollback.index(
            "receipt reconciliation did not reach a safe terminal state"
        )
        removal = self.rollback.index("remove_intentionally_absent_services ||")
        self.assertLess(gate, removal)
        self.assertLess(reconciliation_failure, removal)

    def test_candidate_standby_failure_reaches_exact_rollback_cleanup(self) -> None:
        deployment_start = self.deploy.index(
            'transition_mutable_protected "$rollback_release_env" "$release_env"'
        )
        stopped = self.deploy.index(
            "compose stop -t 30 live-executor", deployment_start
        )
        removed = self.deploy.index("compose rm -f live-executor", stopped)
        absence = self.deploy.index(
            "phoenix_reconcile_required_service_absent compose live-executor",
            removed,
        )
        start = self.deploy.index("compose up -d --no-deps live-executor")
        standby_phase = self.deploy.index("mark_phase HUNTING_STANDBY_STARTED")
        self.assertLess(stopped, removed)
        self.assertLess(removed, absence)
        self.assertLess(absence, start)
        self.assertLess(start, standby_phase)
        compensation = self.deploy.split("rollback_on_failure()", maxsplit=1)[1].split(
            "mutation_started=0", maxsplit=1
        )[0]
        self.assertIn("compose rm -f live-executor", compensation)
        self.assertIn(
            "phoenix_reconcile_required_service_absent compose live-executor",
            compensation,
        )
        self.assertIn("state_update_raw failure deployment_failed", self.deploy)
        self.assertIn("state_update_raw rollback ROLLBACK_STARTED", self.deploy)
        self.assertIn(
            'current_live_compose rm -f "$service"', self.rollback
        )
        self.assertIn("for service in $absent_services", self.rollback)

    def test_required_absence_uses_one_fresh_label_poll_contract(self) -> None:
        self.assertIn(
            "phoenix_reconcile_required_service_absent()", self.required_absence
        )
        self.assertIn(
            '"$required_compose_command" config --format json',
            self.required_absence,
        )
        self.assertIn(
            'label=com.docker.compose.project=$required_project',
            self.required_absence,
        )
        self.assertIn(
            'label=com.docker.compose.service=$required_service',
            self.required_absence,
        )
        self.assertIn("while :; do", self.required_absence)
        self.assertIn('[ -n "$required_ids" ] || return 0', self.required_absence)
        self.assertIn(
            '"$required_docker_bin" container rm --force "$required_id"',
            self.required_absence,
        )
        self.assertGreaterEqual(
            self.required_absence.count(
                '"$required_docker_bin" ps --all --quiet --no-trunc'
            ),
            2,
        )
        self.assertIn(
            "Only the next fresh label", self.required_absence
        )
        self.assertNotIn("docker inspect", self.required_absence)
        self.assertNotIn("required_container_id", self.required_absence)
        self.assertGreaterEqual(
            self.deploy.count(
                "phoenix_reconcile_required_service_absent compose live-executor"
            ),
            2,
        )
        self.assertGreaterEqual(
            self.rollback.count("phoenix_reconcile_required_service_absent"),
            3,
        )
        platform = (ROOT / "scripts/release_platform.py").read_text()
        platform_installer = (
            ROOT / "scripts/install-phoenix-release-platform.sh"
        ).read_text()
        context_installer = (
            ROOT / "scripts/install-production-release-context.sh"
        ).read_text()
        release_assets = (ROOT / "scripts/release_assets.py").read_text()
        for source in (platform, platform_installer, context_installer, release_assets):
            self.assertIn("required-service-absence.sh", source)

    def test_rollback_cleanup_preserves_transaction_and_nonce_evidence(self) -> None:
        cleanup = self.rollback.split(
            "rollback_cleanup_safe()", maxsplit=1
        )[1].split('python3 "$deploy_dir/production_mode.py" shadow', maxsplit=1)[0]
        self.assertNotRegex(cleanup, r"(?im)^\s*(DELETE|UPDATE|TRUNCATE)\b")
        self.assertNotIn("nonce_state", cleanup.lower())
        self.assertNotIn("transaction_receipts", cleanup.lower())

    def test_state_callback_forwards_rollback_result_and_reason(self) -> None:
        callback = self.deploy.split("state_update_raw()", maxsplit=1)[1].split(
            "state_update()", maxsplit=1
        )[0]
        self.assertIn("shift 2", callback)
        self.assertIn('"$release_sha" "$operation" "$value" "$@"', callback)
        self.assertIn(
            "state_update_raw rollback ROLLED_BACK --result ok", self.deploy
        )
        self.assertIn("--code ROLLBACK_COHERENCE_FAILED", self.deploy)
        self.assertIn("--code ROLLBACK_REPAUSE_FAILED", self.deploy)
        self.assertIn("--code ROLLBACK_SCRIPT_FAILED", self.deploy)

    def test_failure_path_persists_rollback_phases(self) -> None:
        self.assertIn("state_update_raw failure deployment_failed", self.deploy)
        self.assertIn("state_update_raw rollback ROLLBACK_STARTED", self.deploy)
        self.assertIn(
            "state_update_raw rollback ROLLED_BACK --result ok", self.deploy
        )
        self.assertIn(
            "state_update_raw rollback ROLLBACK_FAILED --result failed", self.deploy
        )

    def test_candidate_startup_failure_preserves_bounded_log_evidence(self) -> None:
        self.assertIn("capture_service_failure_evidence()", self.deploy)
        self.assertIn(
            'docker logs --timestamps --tail 80 "$failure_id"',
            self.deploy,
        )
        start_loop = self.deploy.split(
            "for service in $start_services", maxsplit=1
        )[1].split("compose up -d --no-deps rpc-gateway", maxsplit=1)[0]
        self.assertIn('capture_service_failure_evidence "$service"', start_loop)

    def test_candidate_rpc_gateway_is_healthy_before_atlas_starts(self) -> None:
        start_loop = self.deploy.index("for service in $start_services")
        defer_atlas = self.deploy.index("atlas-observer)", start_loop)
        rpc_start = self.deploy.index(
            "compose up -d --no-deps rpc-gateway", start_loop
        )
        deferred_gate = self.deploy.index(
            'if [ "$atlas_start_deferred" = true ]', rpc_start
        )
        atlas_start = self.deploy.index(
            "compose up -d --no-deps atlas-observer", deferred_gate
        )
        engine_start = self.deploy.index(
            "compose up -d --no-deps phoenix-engine", atlas_start
        )
        self.assertLess(defer_atlas, rpc_start)
        self.assertLess(rpc_start, deferred_gate)
        self.assertLess(deferred_gate, atlas_start)
        self.assertLess(atlas_start, engine_start)

    def test_rollback_rpc_gateway_is_ready_before_atlas_and_engine(self) -> None:
        start_loop = self.rollback.index("for service in $start_services")
        loop_end = self.rollback.index(
            "compose up -d --no-deps rpc-gateway", start_loop
        )
        loop = self.rollback[start_loop:loop_end]
        self.assertIn("rpc-gateway|phoenix-engine) continue", loop)
        self.assertIn("atlas-observer)", loop)
        self.assertIn("atlas_start_deferred=true\n      continue", loop)
        rpc_start = loop_end
        rpc_healthy = self.rollback.index(
            "wait_service_healthy rpc-gateway", rpc_start
        )
        rpc_ready = self.rollback.index(
            "http://127.0.0.1:9300/readyz", rpc_healthy
        )
        atlas_start = self.rollback.index(
            "compose up -d --no-deps --force-recreate atlas-observer", rpc_ready
        )
        atlas_healthy = self.rollback.index(
            "wait_service_healthy atlas-observer", atlas_start
        )
        engine_start = self.rollback.index(
            "compose up -d --no-deps phoenix-engine", atlas_healthy
        )
        self.assertLess(rpc_start, rpc_healthy)
        self.assertLess(rpc_healthy, rpc_ready)
        self.assertLess(rpc_ready, atlas_start)
        self.assertLess(atlas_start, atlas_healthy)
        self.assertLess(atlas_healthy, engine_start)

    def test_rollback_atlas_failure_preserves_bounded_diagnostics(self) -> None:
        self.assertIn("capture_service_failure_evidence()", self.rollback)
        self.assertIn("{{.RestartCount}}", self.rollback)
        self.assertIn("{{.State.OOMKilled}}", self.rollback)
        self.assertIn("{{.State.ExitCode}}", self.rollback)
        self.assertIn(
            'docker logs --timestamps --tail 100 "$failure_id"', self.rollback
        )
        atlas_start = self.rollback.index(
            "compose up -d --no-deps --force-recreate atlas-observer"
        )
        diagnostics = self.rollback.index(
            "capture_service_failure_evidence atlas-observer", atlas_start
        )
        failure = self.rollback.index(
            "optional service did not become healthy during rollback: atlas-observer",
            diagnostics,
        )
        self.assertLess(diagnostics, failure)

    def test_candidate_failure_allows_version_matched_legacy_atlas_rollback(self) -> None:
        rollback_invocation = self.deploy.split(
            'PHOENIX_CURRENT_LIVE_RELEASE_ENV="$release_env"', maxsplit=1
        )[1].split('/bin/sh "$rollback_script"', maxsplit=1)[0]
        self.assertIn(
            "PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY=true",
            rollback_invocation,
        )

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


class PostArmMonitorSemanticsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.monitor = (ROOT / "scripts/monitor-post-arm-revenue.sh").read_text()
        cls.gauge_program = cls._embedded_program(
            "require_latency_gauges() {", "require_latency_histograms() {"
        )
        cls.histogram_program = cls._embedded_program(
            "require_latency_histograms() {", "require_disarmed_controls() {"
        )
        cls.provider_gateway_program = cls._embedded_counter_program(
            "provider_gateway_counter_vector() {", "require_hunter_ready() {"
        )

    @classmethod
    def _embedded_program(cls, start: str, end: str) -> str:
        section = cls.monitor.split(start, maxsplit=1)[1].split(end, maxsplit=1)[0]
        match = re.search(
            r"python3 -c '\n(?P<body>.*?)\n' \"\$3\" \"\$4\" \|\|",
            section,
            re.S,
        )
        if match is None:
            raise AssertionError(f"embedded monitor program not found: {start}")
        return match.group("body")

    @classmethod
    def _embedded_counter_program(cls, start: str, end: str) -> str:
        section = cls.monitor.split(start, maxsplit=1)[1].split(end, maxsplit=1)[0]
        match = re.search(r"python3 -c '\n(?P<body>.*?)\n' \|\|", section, re.S)
        if match is None:
            raise AssertionError(f"embedded monitor counter not found: {start}")
        return match.group("body")

    @staticmethod
    def _gauge_metrics(
        *,
        actionable: float = 0,
        in_flight: float = 0,
        worker_queued: float = 0,
        available: float = 12,
        oldest: float = 0,
        completed: float = 10,
        legacy_worker_queued: float | None = None,
    ) -> str:
        if legacy_worker_queued is None:
            legacy_worker_queued = worker_queued
        return "\n".join(
            (
                f"phoenix_aave_exact_eligible_now {actionable}",
                f"phoenix_aave_exact_evaluations_in_flight {in_flight}",
                f"phoenix_aave_exact_worker_queue_depth {worker_queued}",
                f"phoenix_exact_queue_depth {legacy_worker_queued}",
                f"phoenix_exact_worker_permits_available {available}",
                f"phoenix_exact_oldest_actionable_age_seconds {oldest}",
                f"phoenix_aave_exact_eval_completed_total {completed}",
            )
        )

    def _run_gauge(self, previous: str, current: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-c", self.gauge_program, "2", "2026-08-14T13:00:00Z"],
            input=previous + "\n# PHOENIX_MONITOR_GAUGE_BOUNDARY\n" + current,
            text=True,
            capture_output=True,
            check=False,
        )

    @staticmethod
    def _histogram_metrics(count: int, *, bucket_offset: int | None = None) -> str:
        names = (
            "phoenix_signal_to_prefilter_seconds",
            "phoenix_liquidatable_to_exact_enqueue_seconds",
            "phoenix_exact_queue_wait_seconds",
            "phoenix_exact_first_rpc_dispatch_seconds",
            "phoenix_exact_rpc_state_fetch_seconds",
            "phoenix_exact_compute_seconds",
            "phoenix_exact_end_to_end_seconds",
            "phoenix_fork_queue_wait_seconds",
            "phoenix_fork_runtime_seconds",
        )
        bucket_count = count if bucket_offset is None else bucket_offset
        lines: list[str] = []
        for name in names:
            lines.append(f"{name}_count {count}")
            for boundary in ("0.001", "0.025", "0.05", "0.25", "1", "2.5", "5", "+Inf"):
                lines.append(f'{name}_bucket{{le="{boundary}"}} {bucket_count}')
        return "\n".join(lines)

    def _run_histograms(self, baseline: str, current: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-c", self.histogram_program, "10", "2026-08-14T13:10:00Z"],
            input=baseline + "\n# PHOENIX_MONITOR_INTERVAL_BOUNDARY\n" + current,
            text=True,
            capture_output=True,
            check=False,
        )

    @staticmethod
    def _provider_gateway_metrics(
        *,
        scalar_delta: str | None = None,
        outcome: str | None = None,
    ) -> str:
        scalar_names = (
            "rpc_state_request_budget_rejected_total",
            "rpc_upstream_call_budget_rejected_total",
            "rpc_provider_unavailable_total",
            "rpc_provider_rate_limited_total",
            "rpc_provider_cooldown_total",
            "rpc_provider_disagreement_total",
        )
        lines = [
            f"{name} {1 if name == scalar_delta else 0}" for name in scalar_names
        ]
        lines.append(
            'rpc_upstream_calls_total{method="eth_call",outcome="success",provider_slot="primary"} 185'
        )
        if outcome is not None:
            lines.append(
                'rpc_upstream_calls_total{method="eth_call",'
                f'outcome="{outcome}",provider_slot="primary"}} 1'
            )
        return "\n".join(lines)

    def _provider_gateway_vector(self, metrics: str) -> str:
        result = subprocess.run(
            [sys.executable, "-c", self.provider_gateway_program],
            input=metrics,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_provider_gateway_vector_ignores_reverts_but_not_failures(self) -> None:
        baseline = self._provider_gateway_vector(self._provider_gateway_metrics())
        self.assertEqual(baseline, "0:0:0:0:0:0:0")
        self.assertEqual(
            self._provider_gateway_vector(
                self._provider_gateway_metrics(outcome="reverted")
            ),
            baseline,
        )
        for outcome in ("failure", "timeout", "rate_limited"):
            with self.subTest(outcome=outcome):
                self.assertNotEqual(
                    self._provider_gateway_vector(
                        self._provider_gateway_metrics(outcome=outcome)
                    ),
                    baseline,
                )
        for scalar in (
            "rpc_state_request_budget_rejected_total",
            "rpc_upstream_call_budget_rejected_total",
            "rpc_provider_unavailable_total",
            "rpc_provider_rate_limited_total",
            "rpc_provider_cooldown_total",
            "rpc_provider_disagreement_total",
        ):
            with self.subTest(scalar=scalar):
                self.assertNotEqual(
                    self._provider_gateway_vector(
                        self._provider_gateway_metrics(scalar_delta=scalar)
                    ),
                    baseline,
                )

    def test_idle_and_first_transient_actionable_samples_are_not_starvation(self) -> None:
        idle = self._gauge_metrics()
        self.assertEqual(self._run_gauge(idle, idle).returncode, 0)
        transient = self._run_gauge(
            idle,
            self._gauge_metrics(actionable=1, oldest=2),
        )
        self.assertEqual(transient.returncode, 0, transient.stderr)

    def test_persistent_actionable_age_emits_complete_scalar_evidence(self) -> None:
        result = self._run_gauge(
            self._gauge_metrics(actionable=1, oldest=0.5),
            self._gauge_metrics(actionable=1, oldest=2),
        )
        self.assertEqual(result.returncode, 1)
        prefix = "POST_ARM_LATENCY_GAUGE_FAILED: "
        self.assertIn(prefix, result.stderr)
        evidence = json.loads(result.stderr.split(prefix, maxsplit=1)[1])
        self.assertEqual(
            evidence,
            {
                "actionable_queue_depth": 1.0,
                "configured_threshold": 1.0,
                "current_value": 2.0,
                "delta": 1.5,
                "exact_completed_delta": 0.0,
                "in_flight_count": 0.0,
                "metric_name": "phoenix_exact_oldest_actionable_age_seconds",
                "oldest_actionable_age_seconds": 2.0,
                "permit_availability": 12.0,
                "previous_value": 0.5,
                "reason": "actionable_age_grew_with_available_permit",
                "sample_count": 2,
                "timestamp": "2026-08-14T13:00:00Z",
                "worker_queued_count": 0.0,
            },
        )

    def test_stale_idle_age_and_worker_queue_permit_contradiction_fail(self) -> None:
        baseline = self._gauge_metrics()
        stale = self._run_gauge(baseline, self._gauge_metrics(oldest=2))
        self.assertEqual(stale.returncode, 1)
        self.assertIn("idle_actionable_age_not_reset", stale.stderr)
        contradiction = self._run_gauge(
            baseline,
            self._gauge_metrics(in_flight=2, worker_queued=1, available=1),
        )
        self.assertEqual(contradiction.returncode, 1)
        self.assertIn("worker_queue_present_with_available_permit", contradiction.stderr)
        reset = self._run_gauge(baseline, self._gauge_metrics(completed=9))
        self.assertEqual(reset.returncode, 1)
        self.assertIn("exact_completed_counter_reset", reset.stderr)

    def test_saturated_queue_with_completion_progress_is_not_false_starvation(self) -> None:
        result = self._run_gauge(
            self._gauge_metrics(actionable=1, in_flight=12, available=0, oldest=0.5),
            self._gauge_metrics(actionable=1, in_flight=12, available=0, oldest=2, completed=11),
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_histograms_difference_intervals_and_report_p50_p95_p99(self) -> None:
        result = self._run_histograms(
            self._histogram_metrics(100),
            self._histogram_metrics(105),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = "POST_ARM_LATENCY_SLO_OK: "
        evidence = json.loads(result.stdout.split(prefix, maxsplit=1)[1])
        exact = evidence["phoenix_exact_queue_wait_seconds"]
        self.assertEqual(exact["count"], 5)
        self.assertEqual(exact["p50_upper_bound_seconds"], 0.001)
        self.assertEqual(exact["p95_upper_bound_seconds"], 0.001)
        self.assertEqual(exact["p99_upper_bound_seconds"], 0.001)
        self.assertEqual(evidence["phoenix_fork_runtime_seconds"]["status"], "observed")

    def test_idle_histogram_interval_is_not_a_latency_regression(self) -> None:
        metrics = self._histogram_metrics(100)
        result = self._run_histograms(metrics, metrics)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("POST_ARM_LATENCY_INTERVAL_IDLE", result.stdout)

    def test_histogram_reset_and_insufficient_samples_are_explicit(self) -> None:
        baseline = self._histogram_metrics(100)
        reset = self._run_histograms(baseline, self._histogram_metrics(99))
        self.assertEqual(reset.returncode, 1)
        self.assertIn("histogram_counter_reset", reset.stderr)
        insufficient = self._run_histograms(baseline, self._histogram_metrics(103))
        self.assertEqual(insufficient.returncode, 1)
        self.assertIn("insufficient_interval_observations", insufficient.stderr)


if __name__ == "__main__":
    unittest.main()
