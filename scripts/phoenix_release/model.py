"""Strict, atomic durable state for Phoenix production releases."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


STATE_SCHEMA = "phoenix.release-state.v1"
PROTOCOL_VERSION = "phoenix-release.v1"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
RUN_ID_RE = re.compile(r"^[1-9][0-9]*$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
TRANSACTION_RE = re.compile(r"^0x[0-9a-f]{64}$")
CONTAINER_RE = re.compile(r"^[0-9a-f]{12,64}$")

PHASES = (
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
    "DISARMED_EVIDENCE_STARTED",
    "POST_DISARMED_VERIFYING",
    "POST_DISARMED_VERIFIED",
    "COMPLETED",
)

FAILURE_PHASES = (
    "FAILED_PRE_MUTATION",
    "FAILED_POST_MUTATION",
    "ROLLBACK_STARTED",
    "ROLLED_BACK",
    "ROLLBACK_FAILED",
)

TERMINAL_PHASES = {"COMPLETED", "FAILED_PRE_MUTATION", "ROLLED_BACK", "ROLLBACK_FAILED"}

REQUIRED_KEYS = {
    "schema_version",
    "controller_protocol_version",
    "release_sha",
    "rollback_sha",
    "source_ci_run_id",
    "source_ci_run_attempt",
    "build_run_id",
    "deploy_run_id",
    "deploy_run_attempt",
    "current_phase",
    "completed_phases",
    "phase_timestamps",
    "mutation_started",
    "contract_paused",
    "autonomous_armed",
    "kill_switch",
    "active_release_pointer",
    "candidate_pointer",
    "expected_images",
    "actual_images",
    "release_manifest_digest",
    "release_assets_digest",
    "engine_container_id",
    "engine_restart_baseline",
    "engine_terminal_integrity_baseline",
    "process_fatal_integrity_baseline",
    "owner_transaction_hash",
    "failure_phase",
    "failure_code",
    "failure_evidence",
    "rollback_result",
}


class StateError(ValueError):
    """A release state failed its safety contract."""


def utc_now() -> str:
    return datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def _run_id(value: int | str, label: str) -> int:
    text = str(value)
    if not RUN_ID_RE.fullmatch(text):
        raise StateError(f"{label} is invalid")
    return int(text)


def _release_sha(value: str, label: str) -> str:
    if not SHA_RE.fullmatch(value):
        raise StateError(f"{label} is invalid")
    return value


def new_state(
    *,
    release_sha: str,
    rollback_sha: str,
    source_ci_run_id: int | str,
    source_ci_run_attempt: int | str,
    build_run_id: int | str,
    deploy_run_id: int | str,
    deploy_run_attempt: int | str,
) -> dict[str, Any]:
    release_sha = _release_sha(release_sha, "release_sha")
    rollback_sha = _release_sha(rollback_sha, "rollback_sha")
    if release_sha == rollback_sha:
        raise StateError("release and rollback SHAs must differ")
    timestamp = utc_now()
    value: dict[str, Any] = {
        "schema_version": STATE_SCHEMA,
        "controller_protocol_version": PROTOCOL_VERSION,
        "release_sha": release_sha,
        "rollback_sha": rollback_sha,
        "source_ci_run_id": _run_id(source_ci_run_id, "source_ci_run_id"),
        "source_ci_run_attempt": _run_id(
            source_ci_run_attempt, "source_ci_run_attempt"
        ),
        "build_run_id": _run_id(build_run_id, "build_run_id"),
        "deploy_run_id": _run_id(deploy_run_id, "deploy_run_id"),
        "deploy_run_attempt": _run_id(deploy_run_attempt, "deploy_run_attempt"),
        "current_phase": "REQUESTED",
        "completed_phases": ["REQUESTED"],
        "phase_timestamps": {"REQUESTED": timestamp},
        "mutation_started": False,
        "contract_paused": None,
        "autonomous_armed": None,
        "kill_switch": None,
        "active_release_pointer": rollback_sha,
        "candidate_pointer": release_sha,
        "expected_images": {},
        "actual_images": {},
        "release_manifest_digest": None,
        "release_assets_digest": None,
        "engine_container_id": None,
        "engine_restart_baseline": None,
        "engine_terminal_integrity_baseline": None,
        "process_fatal_integrity_baseline": None,
        "owner_transaction_hash": None,
        "failure_phase": None,
        "failure_code": None,
        "failure_evidence": None,
        "rollback_result": None,
    }
    validate_state(value)
    return value


def validate_state(value: object) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != REQUIRED_KEYS:
        raise StateError("release state keys are invalid")
    if value["schema_version"] != STATE_SCHEMA:
        raise StateError("release state schema is invalid")
    if value["controller_protocol_version"] != PROTOCOL_VERSION:
        raise StateError("controller protocol version is invalid")
    release_sha = _release_sha(value["release_sha"], "release_sha")
    rollback_sha = _release_sha(value["rollback_sha"], "rollback_sha")
    if release_sha == rollback_sha:
        raise StateError("release and rollback SHAs must differ")
    for key in (
        "source_ci_run_id",
        "source_ci_run_attempt",
        "build_run_id",
        "deploy_run_id",
        "deploy_run_attempt",
    ):
        _run_id(value[key], key)
    phase = value["current_phase"]
    if phase not in PHASES + FAILURE_PHASES:
        raise StateError("current release phase is invalid")
    completed = value["completed_phases"]
    timestamps = value["phase_timestamps"]
    if (
        not isinstance(completed, list)
        or not completed
        or completed[0] != "REQUESTED"
        or len(completed) != len(set(completed))
        or not isinstance(timestamps, dict)
        or set(timestamps) != set(completed)
        or phase not in completed
    ):
        raise StateError("completed release phases are invalid")
    success_indexes = [
        PHASES.index(item) for item in completed if item in PHASES
    ]
    if success_indexes != sorted(success_indexes):
        raise StateError("successful release phases are out of order")
    for item in completed:
        if item not in PHASES + FAILURE_PHASES:
            raise StateError("completed release phase is invalid")
        stamp = timestamps[item]
        if not isinstance(stamp, str) or not stamp.endswith("Z"):
            raise StateError("release phase timestamp is invalid")
    if not isinstance(value["mutation_started"], bool):
        raise StateError("mutation_started must be boolean")
    if phase == "FAILED_PRE_MUTATION" and value["mutation_started"]:
        raise StateError("pre-mutation failure cannot follow mutation")
    if phase in {"FAILED_POST_MUTATION", "ROLLBACK_STARTED", "ROLLED_BACK", "ROLLBACK_FAILED"}:
        if not value["mutation_started"]:
            raise StateError("post-mutation phase requires mutation_started")
    for key in ("contract_paused", "autonomous_armed", "kill_switch"):
        if value[key] not in (None, True, False):
            raise StateError(f"{key} is invalid")
    for key in ("expected_images", "actual_images"):
        if not isinstance(value[key], dict):
            raise StateError(f"{key} is invalid")
    if not SHA_RE.fullmatch(value["active_release_pointer"]):
        raise StateError("active_release_pointer is invalid")
    if value["candidate_pointer"] is not None and not SHA_RE.fullmatch(
        value["candidate_pointer"]
    ):
        raise StateError("candidate_pointer is invalid")
    for key in ("release_manifest_digest", "release_assets_digest"):
        if value[key] is not None and not DIGEST_RE.fullmatch(value[key]):
            raise StateError(f"{key} is invalid")
    if value["owner_transaction_hash"] is not None and not TRANSACTION_RE.fullmatch(
        value["owner_transaction_hash"]
    ):
        raise StateError("owner_transaction_hash is invalid")
    if value["engine_container_id"] is not None and not CONTAINER_RE.fullmatch(
        value["engine_container_id"]
    ):
        raise StateError("engine_container_id is invalid")
    for key in (
        "engine_restart_baseline",
        "engine_terminal_integrity_baseline",
        "process_fatal_integrity_baseline",
    ):
        if value[key] is not None and (
            not isinstance(value[key], int) or value[key] < 0
        ):
            raise StateError(f"{key} is invalid")
    if value["failure_evidence"] is not None and not isinstance(
        value["failure_evidence"], dict
    ):
        raise StateError("failure_evidence is invalid")
    return value


def advance(
    value: dict[str, Any],
    phase: str,
    *,
    mutation_started: bool | None = None,
    updates: dict[str, Any] | None = None,
) -> dict[str, Any]:
    validate_state(value)
    if phase not in PHASES:
        raise StateError("successful release phase is invalid")
    if value["current_phase"] in TERMINAL_PHASES:
        if value["current_phase"] == phase:
            return value
        raise StateError("terminal release state cannot advance")
    current_success = [item for item in value["completed_phases"] if item in PHASES]
    expected = PHASES[len(current_success)]
    if phase != expected:
        if phase in value["completed_phases"]:
            return value
        raise StateError(f"expected next phase {expected}, received {phase}")
    if mutation_started is not None:
        if value["mutation_started"] and not mutation_started:
            raise StateError("mutation_started cannot be cleared")
        value["mutation_started"] = mutation_started
    if updates:
        unknown = set(updates) - REQUIRED_KEYS
        if unknown:
            raise StateError("state update contains unknown keys")
        value.update(updates)
    value["current_phase"] = phase
    value["completed_phases"].append(phase)
    value["phase_timestamps"][phase] = utc_now()
    return validate_state(value)


def fail_state(
    value: dict[str, Any],
    *,
    code: str,
    evidence: dict[str, Any],
) -> dict[str, Any]:
    validate_state(value)
    if not re.fullmatch(r"^[A-Z][A-Z0-9_]{2,63}$", code):
        raise StateError("failure code is invalid")
    phase = "FAILED_POST_MUTATION" if value["mutation_started"] else "FAILED_PRE_MUTATION"
    if value["current_phase"] in TERMINAL_PHASES:
        return value
    value["failure_phase"] = value["current_phase"]
    value["failure_code"] = code
    value["failure_evidence"] = evidence
    value["current_phase"] = phase
    value["completed_phases"].append(phase)
    value["phase_timestamps"][phase] = utc_now()
    return validate_state(value)


def complete_failure_evidence(
    value: dict[str, Any], evidence: dict[str, Any]
) -> dict[str, Any]:
    validate_state(value)
    if value["current_phase"] not in FAILURE_PHASES:
        raise StateError("failure evidence requires a failed release")
    if value["failure_code"] != "DEPLOYMENT_FAILED":
        raise StateError("failure evidence code cannot be replaced")
    existing = value["failure_evidence"]
    if not isinstance(existing, dict) or existing.get("source") != "deploy-release":
        raise StateError("failure evidence source cannot be replaced")
    if existing.get("detail") != "deployment_failed":
        if existing == evidence:
            return value
        raise StateError("completed failure evidence cannot be replaced")
    value["failure_evidence"] = evidence
    return validate_state(value)


def set_mutation_started(value: dict[str, Any]) -> dict[str, Any]:
    validate_state(value)
    if value["current_phase"] in TERMINAL_PHASES:
        raise StateError("terminal release state cannot start mutation")
    value["mutation_started"] = True
    return validate_state(value)


def record_owner_transaction(
    value: dict[str, Any], transaction_hash: str
) -> dict[str, Any]:
    validate_state(value)
    if not TRANSACTION_RE.fullmatch(transaction_hash):
        raise StateError("owner transaction hash is invalid")
    existing = value["owner_transaction_hash"]
    if existing is not None and existing != transaction_hash:
        raise StateError("owner transaction hash cannot be replaced")
    value["owner_transaction_hash"] = transaction_hash
    return validate_state(value)


def record_engine_baseline(
    value: dict[str, Any],
    *,
    container_id: str,
    restart_count: int,
    terminal_integrity: int,
    process_fatal_integrity: int,
) -> dict[str, Any]:
    validate_state(value)
    if not CONTAINER_RE.fullmatch(container_id):
        raise StateError("Engine container ID is invalid")
    for number in (restart_count, terminal_integrity, process_fatal_integrity):
        if not isinstance(number, int) or number < 0:
            raise StateError("Engine burn-in baseline is invalid")
    value.update(
        {
            "engine_container_id": container_id,
            "engine_restart_baseline": restart_count,
            "engine_terminal_integrity_baseline": terminal_integrity,
            "process_fatal_integrity_baseline": process_fatal_integrity,
        }
    )
    return validate_state(value)


def rollback_phase(
    value: dict[str, Any], phase: str, result: dict[str, Any] | None = None
) -> dict[str, Any]:
    validate_state(value)
    allowed = {
        "FAILED_POST_MUTATION": {"ROLLBACK_STARTED"},
        "ROLLBACK_STARTED": {"ROLLED_BACK", "ROLLBACK_FAILED"},
        "ROLLBACK_FAILED": {"ROLLED_BACK"},
    }
    expected = allowed.get(value["current_phase"], set())
    if phase not in expected:
        raise StateError("rollback phase transition is invalid")
    value["current_phase"] = phase
    value["completed_phases"].append(phase)
    value["phase_timestamps"][phase] = utc_now()
    if result is not None:
        value["rollback_result"] = result
    return validate_state(value)


def _assert_safe_parent(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    parent = path.parent
    if parent.is_symlink() or not parent.is_dir():
        raise StateError("release state parent is unsafe")


def atomic_write(path: Path, value: dict[str, Any]) -> None:
    validate_state(value)
    _assert_safe_parent(path)
    if path.exists():
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise StateError("existing release state file is unsafe")
    payload = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    descriptor, temporary = tempfile.mkstemp(prefix=".state.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def load_state(path: Path) -> dict[str, Any]:
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise StateError("release state file is unsafe")
    if metadata.st_size > 256 * 1024:
        raise StateError("release state file is too large")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise StateError("release state is invalid JSON") from exc
    return validate_state(value)
