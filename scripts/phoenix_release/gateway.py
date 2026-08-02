"""Root-side bounded release receiver and resumable gateway."""

from __future__ import annotations

import hashlib
import io
import json
import os
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Iterable

from .chain_reconciliation import (
    ReconciliationError,
    build_evidence as build_chain_reconciliation_evidence,
    collect_provider_evidence,
    evidence_digest as chain_reconciliation_digest,
    evidence_path as chain_reconciliation_path,
    read_evidence as read_chain_reconciliation_evidence,
    write_evidence as write_chain_reconciliation_evidence,
)
from .model import (
    PHASES,
    PROTOCOL_VERSION,
    SHA_RE,
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
    retry_failed_pre_mutation,
    retry_rolled_back_release,
    set_mutation_started,
    sha256_file,
)


REQUEST_SCHEMA = "phoenix.release-request.v1"
MAX_PACKAGE_BYTES = 80 * 1024 * 1024
MAX_MEMBER_BYTES = 72 * 1024 * 1024
MAX_MEMBERS = 8
RUN_RE = re.compile(r"^[1-9][0-9]*$")
SAFE_CODE_RE = re.compile(r"^[A-Z][A-Z0-9_]{2,63}$")
SENSITIVE_OUTPUT_RE = re.compile(
    r"(?i)\b(?:https?|wss?|postgres(?:ql)?|nats)://[^\s\"']+"
)
CONTROL_EVIDENCE_KEYS = {
    "active_attempts",
    "armed",
    "execution_mode",
    "kill_switch",
    "open_routes",
    "outbox_ack_pending",
    "outbox_claimable",
    "outbox_pending",
    "unresolved_submissions",
}
FAIL_CLOSED_CONTROL_EVIDENCE = {
    "active_attempts": 0,
    "armed": False,
    "execution_mode": "disarmed",
    "kill_switch": True,
    "open_routes": 0,
    "unresolved_submissions": 0,
}

FIXED_MEMBERS = {
    "request.json",
    "release-manifest.json",
    "release-provenance.json",
    "rollback-manifest.json",
    "rollback-provenance.json",
    "release-assets-manifest.json",
    "release-assets-checksums.txt",
}

REQUEST_KEYS = {
    "schema",
    "protocol_version",
    "release_sha",
    "rollback_sha",
    "source_ci_run_id",
    "source_ci_run_attempt",
    "build_run_id",
    "rollback_build_run_id",
    "deploy_run_id",
    "deploy_run_attempt",
}


class GatewayError(ValueError):
    """A bounded gateway operation failed closed."""

    def __init__(self, code: str, evidence: dict[str, Any] | None = None):
        if not SAFE_CODE_RE.fullmatch(code):
            raise ValueError("invalid gateway error code")
        super().__init__(code)
        self.code = code
        self.evidence = evidence or {}


def _bounded_output(value: str | None) -> str:
    redacted = SENSITIVE_OUTPUT_RE.sub("[REDACTED_URL]", value or "")
    if len(redacted) <= 4096:
        return redacted
    diagnostic_lines = [
        line
        for line in redacted.splitlines()
        if (
            '"code":"' in line
            or line.startswith("DEPLOY_FAILED:")
            or line.startswith("DEPLOY_COMPENSATION_")
            or line.startswith("HEALTH_FAIL:")
            or line.startswith("ROLLBACK_")
        )
    ]
    diagnostics = "\n".join(dict.fromkeys(diagnostic_lines))
    if diagnostics:
        diagnostics = diagnostics[:2048]
        tail_budget = 4096 - len(diagnostics) - 1
        return f"{diagnostics}\n{redacted[-tail_budget:]}"
    return redacted[-4096:]


@dataclass(frozen=True)
class HostPaths:
    state_root: Path
    deploy_root: Path
    env_file: Path
    libexec: Path

    @property
    def deploy_dir(self) -> Path:
        return self.deploy_root / "deploy"

    @property
    def releases(self) -> Path:
        return self.deploy_root / "releases"

    @property
    def incoming(self) -> Path:
        return self.state_root / "incoming"

    @property
    def release_states(self) -> Path:
        return self.state_root / "releases"

    @property
    def state_updater(self) -> Path:
        return self.libexec / "phoenix_release" / "phase_update.py"


def host_paths() -> HostPaths:
    return HostPaths(
        state_root=Path(
            os.environ.get("PHOENIX_RELEASE_STATE_ROOT", "/var/lib/phoenix-release")
        ),
        deploy_root=Path(os.environ.get("PHOENIX_DEPLOY_ROOT", "/opt/phoenix")),
        env_file=Path(os.environ.get("PHOENIX_ENV_FILE", "/etc/phoenix/phoenix.env")),
        libexec=Path(
            os.environ.get(
                "PHOENIX_RELEASE_LIBEXEC", "/usr/local/libexec/phoenix-release"
            )
        ),
    )


def production_compose_command(
    paths: HostPaths,
    *,
    mode: str,
    release_env: Path,
) -> list[str]:
    """Return the single root-side entry to canonical Compose construction."""
    if mode not in {"SHADOW", "LIVE"}:
        raise GatewayError("PRODUCTION_COMPOSE_MODE_INVALID")
    command = [
        "/usr/bin/python3",
        "-I",
        "-B",
        str(paths.libexec / "production_compose.py"),
        "--mode",
        mode,
        "--env-file",
        str(paths.env_file),
        "--release-env",
        str(release_env),
        "--compose-file",
        str(paths.deploy_dir / "compose.prod.yml"),
    ]
    if mode == "LIVE":
        command.extend(
            [
                "--overlay-file",
                str(paths.deploy_dir / "compose.live-autonomous.yml"),
            ]
        )
    command.append("--")
    return command


def _active_runtime_services(
    paths: HostPaths, active_release: str, mode: str
) -> list[str]:
    if not SHA_RE.fullmatch(active_release) or mode not in {"SHADOW", "LIVE"}:
        raise GatewayError("READINESS_TOPOLOGY_INVALID")
    output = _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_components.py"),
            "topology",
            "--manifest",
            str(paths.deploy_dir / "manifests" / f"{active_release}.json"),
            "--mode",
            mode,
            "--field",
            "running_services",
        ],
        "READINESS_TOPOLOGY_FAILED",
    )
    services = output.split()
    if (
        not services
        or len(services) > 64
        or len(services) != len(set(services))
        or any(
            re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", item) is None
            for item in services
        )
    ):
        raise GatewayError("READINESS_TOPOLOGY_INVALID")
    return services


def _canonical(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _atomic_bytes(path: Path, payload: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.parent.is_symlink():
        raise GatewayError("UNSAFE_DESTINATION")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _atomic_archive(path: Path, payload: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.parent.is_symlink():
        raise GatewayError("RETRY_ARCHIVE_DESTINATION_UNSAFE")
    if path.exists():
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or path.read_bytes() != payload
        ):
            raise GatewayError("RETRY_ARCHIVE_CONFLICT")
        return
    descriptor, temporary = tempfile.mkstemp(
        prefix=f".{path.name}.", dir=path.parent
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, 0o600)
        try:
            os.link(temporary, path)
        except FileExistsError as exc:
            raise GatewayError("RETRY_ARCHIVE_CONFLICT") from exc
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _read_json(path: Path, maximum: int = 1024 * 1024) -> Any:
    try:
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_size > maximum
        ):
            raise GatewayError("EVIDENCE_FILE_UNSAFE", {"path": str(path)})
        return json.loads(path.read_text(encoding="utf-8"))
    except GatewayError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise GatewayError("EVIDENCE_JSON_INVALID", {"path": str(path)}) from exc


def validate_request(value: object) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != REQUEST_KEYS:
        raise GatewayError("REQUEST_SCHEMA_INVALID")
    if value["schema"] != REQUEST_SCHEMA or value["protocol_version"] != PROTOCOL_VERSION:
        raise GatewayError("REQUEST_PROTOCOL_INVALID")
    for key in ("release_sha", "rollback_sha"):
        if not isinstance(value[key], str) or not SHA_RE.fullmatch(value[key]):
            raise GatewayError("REQUEST_SHA_INVALID", {"field": key})
    if value["release_sha"] == value["rollback_sha"]:
        raise GatewayError("REQUEST_SHA_PAIR_INVALID")
    for key in (
        "source_ci_run_id",
        "source_ci_run_attempt",
        "build_run_id",
        "rollback_build_run_id",
        "deploy_run_id",
        "deploy_run_attempt",
    ):
        if not isinstance(value[key], int) or not RUN_RE.fullmatch(str(value[key])):
            raise GatewayError("REQUEST_RUN_ID_INVALID", {"field": key})
    return value


def expected_members(release_sha: str) -> set[str]:
    if not SHA_RE.fullmatch(release_sha):
        raise GatewayError("REQUEST_SHA_INVALID")
    return FIXED_MEMBERS | {f"phoenix-release-assets-{release_sha}.tar.gz"}


def receive_package(
    stream: BinaryIO,
    paths: HostPaths,
    *,
    expected_release_sha: str | None = None,
) -> dict[str, Any]:
    paths.incoming.mkdir(parents=True, exist_ok=True)
    if paths.incoming.is_symlink():
        raise GatewayError("INCOMING_ROOT_UNSAFE")
    package = stream.read(MAX_PACKAGE_BYTES + 1)
    if not package or len(package) > MAX_PACKAGE_BYTES:
        raise GatewayError("PACKAGE_SIZE_INVALID")
    package_digest = f"sha256:{hashlib.sha256(package).hexdigest()}"
    with tempfile.TemporaryDirectory(prefix=".receive.", dir=paths.incoming) as temporary:
        staging = Path(temporary)
        try:
            with tarfile.open(fileobj=io.BytesIO(package), mode="r:gz") as archive:
                members = archive.getmembers()
                if len(members) > MAX_MEMBERS:
                    raise GatewayError("PACKAGE_MEMBER_COUNT_INVALID")
                names = {member.name for member in members}
                request_member = next(
                    (member for member in members if member.name == "request.json"), None
                )
                if request_member is None or not request_member.isfile():
                    raise GatewayError("PACKAGE_REQUEST_MISSING")
                request_handle = archive.extractfile(request_member)
                if request_handle is None:
                    raise GatewayError("PACKAGE_REQUEST_INVALID")
                request_data = request_handle.read(64 * 1024 + 1)
                if len(request_data) > 64 * 1024:
                    raise GatewayError("PACKAGE_REQUEST_INVALID")
                try:
                    request = validate_request(json.loads(request_data.decode("utf-8")))
                except (UnicodeError, json.JSONDecodeError) as exc:
                    raise GatewayError("PACKAGE_REQUEST_INVALID") from exc
                if (
                    expected_release_sha is not None
                    and request["release_sha"] != expected_release_sha
                ):
                    raise GatewayError("REQUEST_SHA_ARGUMENT_MISMATCH")
                required = expected_members(request["release_sha"])
                if names != required:
                    raise GatewayError(
                        "PACKAGE_MEMBER_SET_INVALID",
                        {"expected": sorted(required), "actual": sorted(names)},
                    )
                total = 0
                for member in members:
                    if (
                        not member.isfile()
                        or member.issym()
                        or member.islnk()
                        or member.name.startswith("/")
                        or "\\" in member.name
                        or "/" in member.name
                        or member.size < 0
                        or member.size > MAX_MEMBER_BYTES
                    ):
                        raise GatewayError(
                            "PACKAGE_MEMBER_INVALID", {"member": member.name}
                        )
                    total += member.size
                    if total > MAX_PACKAGE_BYTES:
                        raise GatewayError("PACKAGE_EXPANDED_SIZE_INVALID")
                    source = archive.extractfile(member)
                    if source is None:
                        raise GatewayError(
                            "PACKAGE_MEMBER_UNREADABLE", {"member": member.name}
                        )
                    payload = source.read(member.size + 1)
                    if len(payload) != member.size:
                        raise GatewayError(
                            "PACKAGE_MEMBER_UNREADABLE", {"member": member.name}
                        )
                    _atomic_bytes(staging / member.name, payload)
        except GatewayError:
            raise
        except tarfile.TarError as exc:
            raise GatewayError("PACKAGE_ARCHIVE_INVALID") from exc
        destination = paths.incoming / request["release_sha"]
        if destination.exists() or destination.is_symlink():
            existing_request = _read_json(destination / "request.json", 64 * 1024)
            if validate_request(existing_request) == request:
                existing_state = load_state(
                    state_file(paths, request["release_sha"])
                )
                existing_package = destination / "release-package.tar.gz"
                if (
                    existing_state["package_digest"] != package_digest
                    or not existing_package.is_file()
                    or existing_package.is_symlink()
                    or sha256_file(existing_package) != package_digest
                ):
                    raise GatewayError("PACKAGE_IDENTITY_MISMATCH")
                return request
            raise GatewayError("PACKAGE_ALREADY_EXISTS")
        _atomic_bytes(staging / "release-package.tar.gz", package)
        os.chmod(staging, 0o700)
        os.replace(staging, destination)
        state_path = state_file(paths, request["release_sha"])
        state = new_state(
            release_sha=request["release_sha"],
            rollback_sha=request["rollback_sha"],
            source_ci_run_id=request["source_ci_run_id"],
            source_ci_run_attempt=request["source_ci_run_attempt"],
            build_run_id=request["build_run_id"],
            deploy_run_id=request["deploy_run_id"],
            deploy_run_attempt=request["deploy_run_attempt"],
            package_digest=package_digest,
        )
        atomic_write(state_path, state)
        return request


def state_file(paths: HostPaths, release_sha: str) -> Path:
    if not SHA_RE.fullmatch(release_sha):
        raise GatewayError("RELEASE_SHA_INVALID")
    return paths.release_states / release_sha / "state.json"


def request_file(paths: HostPaths, release_sha: str) -> Path:
    return paths.incoming / release_sha / "request.json"


def _run(
    arguments: list[str],
    *,
    env: dict[str, str] | None = None,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    merged = os.environ.copy()
    if env:
        merged.update(env)
    return subprocess.run(
        arguments,
        check=False,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        env=merged,
        timeout=3600,
    )


def _require_success(
    arguments: list[str],
    code: str,
    *,
    env: dict[str, str] | None = None,
) -> str:
    result = _run(arguments, env=env)
    if result.returncode != 0:
        raise GatewayError(
            code,
            {
                "command": _bounded_output(
                    SENSITIVE_OUTPUT_RE.sub(
                        "[REDACTED_URL]", shlex.join(arguments)
                    )
                ),
                "exit_code": result.returncode,
                "output": _bounded_output(
                    "\n".join(
                        part
                        for part in (
                            result.stdout,
                            getattr(result, "stderr", None),
                        )
                        if part
                    )
                ),
                "stderr": _bounded_output(getattr(result, "stderr", None)),
                "stdout": _bounded_output(result.stdout),
            },
        )
    return result.stdout or ""


def _pointer(path: Path) -> str:
    try:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise GatewayError("ACTIVE_POINTER_UNSAFE", {"path": str(path)})
        value = path.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeError) as exc:
        raise GatewayError("ACTIVE_POINTER_INVALID", {"path": str(path)}) from exc
    if not SHA_RE.fullmatch(value):
        raise GatewayError("ACTIVE_POINTER_INVALID", {"path": str(path)})
    return value


def status(paths: HostPaths) -> dict[str, Any]:
    current = _pointer(paths.deploy_dir / "current-release")
    assets = _pointer(paths.deploy_dir / "release-assets.sha")
    if current != assets:
        raise GatewayError(
            "ACTIVE_POINTER_MISMATCH",
            {"current_release": current, "release_assets": assets},
        )
    provenance = _read_json(
        paths.deploy_dir / "manifests" / f"{current}.provenance.json"
    )
    build_run_id = (
        provenance.get("build_run_id") if isinstance(provenance, dict) else None
    )
    if not isinstance(build_run_id, (int, str)) or not RUN_RE.fullmatch(
        str(build_run_id)
    ):
        raise GatewayError("ACTIVE_BUILD_IDENTITY_INVALID")
    mode = "UNKNOWN"
    live_execution = None
    autonomous_execution = None
    try:
        for raw in paths.env_file.read_text(encoding="utf-8").splitlines():
            key, separator, value = raw.partition("=")
            if not separator:
                continue
            if key == "PHOENIX_MODE":
                mode = value
            elif key == "LIVE_EXECUTION":
                live_execution = value == "true"
            elif key == "AUTONOMOUS_EXECUTION":
                autonomous_execution = value == "true"
    except (OSError, UnicodeError) as exc:
        raise GatewayError("PRODUCTION_ENV_INVALID") from exc
    return {
        "schema": "phoenix.release-status.v1",
        "protocol_version": PROTOCOL_VERSION,
        "active_release": current,
        "release_assets_sha": assets,
        "active_build_run_id": int(build_run_id),
        "phoenix_mode": mode,
        "live_execution": live_execution,
        "autonomous_execution": autonomous_execution,
    }


def _path_evidence(path: Path) -> dict[str, Any]:
    metadata = path.lstat()
    return {
        "gid": metadata.st_gid,
        "inode": metadata.st_ino,
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
        "path": str(path),
        "sha256": sha256_file(path) if stat.S_ISREG(metadata.st_mode) else None,
        "uid": metadata.st_uid,
    }


def _selected_environment(path: Path, names: set[str]) -> dict[str, str]:
    try:
        metadata = path.lstat()
        raw = path.read_bytes()
    except OSError as exc:
        raise GatewayError("PRODUCTION_ENV_INVALID") from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_nlink != 1
        or len(raw) > 1024 * 1024
    ):
        raise GatewayError("PRODUCTION_ENV_INVALID")
    try:
        lines = raw.decode("utf-8-sig").splitlines()
    except UnicodeError as exc:
        raise GatewayError("PRODUCTION_ENV_INVALID") from exc
    selected: dict[str, str] = {}
    for raw_line in lines:
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        name, candidate = line.split("=", 1)
        name = name.strip()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
            raise GatewayError("PRODUCTION_ENV_INVALID")
        if name not in names:
            continue
        candidate = candidate.strip()
        if len(candidate) >= 2 and candidate[0] == candidate[-1] == "'":
            candidate = candidate[1:-1]
        elif (
            len(candidate) >= 2
            and candidate[0] == candidate[-1] == '"'
        ):
            try:
                decoded = json.loads(candidate)
            except json.JSONDecodeError as exc:
                raise GatewayError("PRODUCTION_ENV_INVALID") from exc
            if not isinstance(decoded, str):
                raise GatewayError("PRODUCTION_ENV_INVALID")
            candidate = decoded
        selected[name] = candidate
    if set(selected) != names or any(not value for value in selected.values()):
        raise GatewayError("PRODUCTION_ENV_INCOMPLETE")
    return selected


def _control_evidence(
    paths: HostPaths,
    active: dict[str, Any],
) -> dict[str, Any]:
    release_env = paths.deploy_dir / "current-release.env"
    mode = "LIVE" if active["phoenix_mode"] == "LIVE" else "SHADOW"
    compose = production_compose_command(
        paths,
        mode=mode,
        release_env=release_env,
    )
    control_sql = (
        "SELECT json_build_object("
        "'armed', global_control.armed,"
        "'kill_switch', global_control.kill_switch,"
        "'execution_mode', global_control.execution_mode,"
        "'open_routes', (SELECT count(*) FROM "
        "live_canary.autonomous_route_controls "
        "WHERE enabled OR NOT kill_switch),"
        "'active_attempts', (SELECT count(*) FROM "
        "live_canary.execution_attempts WHERE status IN "
        "('claimed','nonce_allocated','submission_unknown','pending','timed_out')),"
        "'unresolved_submissions', (SELECT count(*) FROM "
        "live_canary.execution_attempts WHERE status IN "
        "('submission_unknown','pending','timed_out')),"
        "'outbox_pending', (SELECT count(*) FROM engine_outbox "
        "WHERE published_at IS NULL),"
        "'outbox_ack_pending', (SELECT count(*) FROM engine_outbox "
        "WHERE published_at IS NOT NULL AND jetstream_ack_sequence IS NULL),"
        "'outbox_claimable', (SELECT count(*) FROM engine_outbox "
        "WHERE published_at IS NULL AND available_at <= now() "
        "AND (claim_expires_at IS NULL OR claim_expires_at <= now())))::text "
        "FROM live_canary.autonomous_global_control AS global_control "
        "WHERE global_control.singleton"
    )
    output = _require_success(
        compose
        + [
            "exec",
            "-T",
            "postgres",
            "/bin/sh",
            "-c",
            (
                "exec psql -X -qAt -v ON_ERROR_STOP=1 "
                '-U "$POSTGRES_USER" -d "$POSTGRES_DB" -c '
                + shlex.quote(control_sql)
            ),
        ],
        "READINESS_CONTROL_QUERY_FAILED",
    )
    try:
        controls = json.loads(output)
    except (json.JSONDecodeError, TypeError) as exc:
        raise GatewayError("READINESS_CONTROL_EVIDENCE_INVALID") from exc
    if (
        not isinstance(controls, dict)
        or set(controls) != CONTROL_EVIDENCE_KEYS
    ):
        raise GatewayError("READINESS_CONTROL_EVIDENCE_INVALID")
    return controls


def _require_fail_closed_controls(controls: dict[str, Any]) -> None:
    for field, expected in FAIL_CLOSED_CONTROL_EVIDENCE.items():
        if controls.get(field) != expected:
            raise GatewayError(
                "READINESS_CONTROL_OPEN",
                {
                    "actual": controls.get(field),
                    "expected": expected,
                    "field": field,
                },
            )


def _live_executor_stopped() -> dict[str, Any]:
    output = _require_success(
        [
            "/usr/bin/docker",
            "ps",
            "-q",
            "--filter",
            "label=com.docker.compose.service=live-executor",
        ],
        "READINESS_LIVE_EXECUTOR_LOOKUP_FAILED",
    )
    containers = [line for line in output.splitlines() if line]
    evidence_value = {
        "running_container_ids": containers[:8],
        "stopped": not containers,
    }
    if containers:
        raise GatewayError(
            "READINESS_LIVE_EXECUTOR_ACTIVE",
            evidence_value,
        )
    return evidence_value


def _is_stopped_live_executor(
    service: str, observed: dict[str, Any]
) -> bool:
    return service == "live-executor" and observed.get("running") is not True


def _reconciliation_runtime(
    controls: dict[str, Any],
) -> dict[str, object]:
    return {
        "active_attempts": controls["active_attempts"],
        "armed": controls["armed"],
        "execution_mode": controls["execution_mode"],
        "kill_switch": controls["kill_switch"],
        "live_executor_stopped": True,
        "open_routes": controls["open_routes"],
        "unresolved_submissions": controls["unresolved_submissions"],
    }


def _historical_contract_evidence(
    paths: HostPaths,
    active_release: str,
    *,
    expected_uid: int = 0,
    expected_gid: int = 0,
) -> dict[str, object]:
    path = state_file(paths, active_release)
    try:
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or path.is_symlink()
            or metadata.st_nlink != 1
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_uid != expected_uid
            or metadata.st_gid != expected_gid
            or metadata.st_size > 256 * 1024
        ):
            raise GatewayError("ACTIVE_RELEASE_HISTORICAL_STATE_INVALID")
        raw = path.read_bytes()
        historical_value = json.loads(raw)
        if raw != _canonical(historical_value):
            raise GatewayError("ACTIVE_RELEASE_HISTORICAL_STATE_INVALID")
        try:
            value = load_state(path)
        except StateError:
            value = historical_value
    except GatewayError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise GatewayError(
            "ACTIVE_RELEASE_HISTORICAL_STATE_INVALID"
        ) from exc

    completed = value.get("completed_phases")
    timestamps = value.get("phase_timestamps")
    owner_transaction_hash = value.get("owner_transaction_hash")
    if (
        value.get("schema_version") != "phoenix.release-state.v1"
        or value.get("controller_protocol_version") != PROTOCOL_VERSION
        or value.get("release_sha") != active_release
        or value.get("active_release_pointer") != active_release
        or value.get("current_phase") != "COMPLETED"
        or not isinstance(completed, list)
        or not completed
        or completed[0] != "REQUESTED"
        or completed[-1] != "COMPLETED"
        or len(completed) != len(set(completed))
        or not isinstance(timestamps, dict)
        or set(timestamps) != set(completed)
        or any(
            not isinstance(timestamps[phase], str)
            or not timestamps[phase].endswith("Z")
            for phase in completed
        )
        or type(value.get("contract_paused")) is not bool
        or (
            owner_transaction_hash is not None
            and (
                not isinstance(owner_transaction_hash, str)
                or not re.fullmatch(
                    r"0x[0-9a-f]{64}",
                    owner_transaction_hash,
                )
            )
        )
    ):
        raise GatewayError("ACTIVE_RELEASE_HISTORICAL_STATE_INVALID")
    return {
        "contract_paused": value["contract_paused"],
        "owner_transaction_hash": owner_transaction_hash,
    }


def _readiness_chain_reconciliation(
    paths: HostPaths,
    *,
    active_release: str,
    release_assets_sha: str,
    candidate_sha: str,
    platform_manifest_sha256: str,
    controls: dict[str, Any],
    historical_contract_paused: bool,
    owner_transaction_hash: str,
) -> dict[str, Any]:
    environment = _selected_environment(
        paths.env_file,
        {"LIVE_EXECUTOR_EXECUTOR_ADDRESS"},
    )
    expected = {
        "active_release_sha": active_release,
        "executor_address": environment[
            "LIVE_EXECUTOR_EXECUTOR_ADDRESS"
        ].lower(),
        "historical_release_evidence": {
            "contract_paused": historical_contract_paused,
            "owner_transaction_hash": owner_transaction_hash,
        },
        "owner_transaction_hash": owner_transaction_hash,
        "protected_main_sha": candidate_sha,
        "release_assets_sha": release_assets_sha,
        "release_platform_manifest_sha256": platform_manifest_sha256,
        "runtime": _reconciliation_runtime(controls),
    }
    try:
        value = read_chain_reconciliation_evidence(
            chain_reconciliation_path(
                paths.state_root,
                active_release,
                candidate_sha,
            ),
            expected=expected,
        )
    except ReconciliationError as exc:
        raise GatewayError(
            "READINESS_CHAIN_RECONCILIATION_INVALID",
            {"code": exc.code},
        ) from exc
    return {
        "evidence_sha256": chain_reconciliation_digest(value),
        "historical_contract_paused": historical_contract_paused,
        "historical_owner_transaction_hash": owner_transaction_hash,
        "provider_agreement": value["provider_agreement"],
        "status": "accepted",
    }


def production_readiness(
    paths: HostPaths, candidate_sha: str
) -> dict[str, Any]:
    """Aggregate every read-only host gate before any candidate image build."""
    if not SHA_RE.fullmatch(candidate_sha):
        raise GatewayError("RELEASE_SHA_INVALID")
    failures: list[dict[str, Any]] = []
    checks: dict[str, Any] = {}
    controls: dict[str, Any] | None = None
    installed_sha: str | None = None
    platform_manifest_digest: str | None = None

    def failed(code: str, evidence_value: dict[str, Any] | None = None) -> None:
        failures.append({"code": code, "evidence": evidence_value or {}})

    try:
        active = status(paths)
        checks["active_release"] = active
    except GatewayError as exc:
        failed(exc.code, exc.evidence)
        active = None

    required_paths = (
        paths.env_file,
        paths.deploy_dir / "current-release",
        paths.deploy_dir / "release-assets.sha",
        paths.deploy_dir / "current-release.env",
        paths.deploy_dir / "compose.prod.yml",
        paths.deploy_dir / "compose.live-autonomous.yml",
        paths.libexec / "platform-manifest.json",
        paths.libexec / "production_compose.py",
    )
    path_checks = []
    for required in required_paths:
        try:
            evidence_value = _path_evidence(required)
            if (
                not required.is_file()
                or required.is_symlink()
                or required.stat().st_nlink != 1
            ):
                failed("READINESS_FILE_UNSAFE", {"path": str(required)})
            path_checks.append(evidence_value)
        except (OSError, StateError) as exc:
            failed(
                "READINESS_FILE_INVALID",
                {"message": _bounded_output(str(exc)), "path": str(required)},
            )
    checks["files"] = path_checks

    try:
        platform_manifest = _read_json(
            paths.libexec / "platform-manifest.json", 256 * 1024
        )
        installed_sha = (
            platform_manifest.get("release_sha")
            if isinstance(platform_manifest, dict)
            else None
        )
        platform_manifest_digest = sha256_file(
            paths.libexec / "platform-manifest.json"
        )
        checks["release_platform"] = {
            "candidate_sha": candidate_sha,
            "installed_sha": installed_sha,
            "manifest_sha256": platform_manifest_digest,
            "upgrade_required": installed_sha != candidate_sha,
        }
        if not isinstance(installed_sha, str) or not SHA_RE.fullmatch(
            installed_sha
        ):
            failed("READINESS_PLATFORM_MANIFEST_INVALID")
        else:
            try:
                _require_success(
                    [
                        "/usr/bin/python3",
                        "-I",
                        "-B",
                        str(paths.libexec / "release_platform.py"),
                        "verify",
                        "--installed-root",
                        "/",
                        "--expected-sha",
                        installed_sha,
                    ],
                    "READINESS_PLATFORM_DRIFT",
                )
            except GatewayError as exc:
                failed(exc.code, exc.evidence)
    except GatewayError as exc:
        failed(exc.code, exc.evidence)

    capacity = []
    for root in (paths.state_root, paths.deploy_root, paths.env_file.parent):
        try:
            usage = shutil.disk_usage(root)
            available_inodes = (
                os.statvfs(root).f_favail
                if hasattr(os, "statvfs")
                else None
            )
            value = {
                "available_bytes": usage.free,
                "available_inodes": available_inodes,
                "path": str(root),
            }
            capacity.append(value)
            if usage.free < 5 * 1024 * 1024 * 1024:
                failed("READINESS_DISK_CAPACITY_LOW", value)
            if (
                available_inodes is not None
                and available_inodes < 100_000
            ):
                failed("READINESS_INODE_CAPACITY_LOW", value)
        except OSError as exc:
            failed(
                "READINESS_CAPACITY_UNAVAILABLE",
                {"message": _bounded_output(str(exc)), "path": str(root)},
            )
    checks["capacity"] = capacity

    if active is not None:
        release_env = paths.deploy_dir / "current-release.env"
        mode = "LIVE" if active["phoenix_mode"] == "LIVE" else "SHADOW"
        compose = production_compose_command(
            paths, mode=mode, release_env=release_env
        )
        try:
            expected_services = _active_runtime_services(
                paths, active["active_release"], mode
            )
            checks["expected_services"] = expected_services
        except GatewayError as exc:
            failed(exc.code, exc.evidence)
            expected_services = []
        try:
            rendered = _require_success(
                compose + ["config", "--format", "json"],
                "READINESS_COMPOSE_RENDER_FAILED",
            )
            rendered_value = json.loads(rendered)
            checks["compose_service_count"] = len(
                rendered_value.get("services", {})
            )
        except GatewayError as exc:
            failed(exc.code, exc.evidence)
            rendered_value = {}
        except json.JSONDecodeError:
            failed("READINESS_COMPOSE_RENDER_INVALID")
            rendered_value = {}

        service_evidence = []
        services = rendered_value.get("services", {})
        if isinstance(services, dict):
            for service in expected_services:
                if service not in services:
                    failed(
                        "READINESS_SERVICE_UNCONFIGURED",
                        {"service": service},
                    )
                    continue
                try:
                    container_id = _require_success(
                        compose + ["ps", "-a", "-q", service],
                        "READINESS_CONTAINER_LOOKUP_FAILED",
                    ).strip()
                    if not container_id:
                        if service == "live-executor":
                            service_evidence.append(
                                {
                                    "container_id": None,
                                    "service": service,
                                    "state": "stopped",
                                }
                            )
                            continue
                        failed(
                            "READINESS_SERVICE_MISSING",
                            {"service": service},
                        )
                        continue
                    inspection = _require_success(
                        [
                            "/usr/bin/docker",
                            "inspect",
                            "--format",
                            "{{json .}}",
                            container_id,
                        ],
                        "READINESS_CONTAINER_INSPECT_FAILED",
                    )
                    inspected = json.loads(inspection)
                    state_value = inspected.get("State", {})
                    health = state_value.get("Health", {}).get("Status")
                    observed = {
                        "configured_image": inspected.get("Config", {}).get(
                            "Image"
                        ),
                        "container_id": container_id,
                        "health": health,
                        "image_id": inspected.get("Image"),
                        "mounts": [
                            {
                                "destination": mount.get("Destination"),
                                "mode": mount.get("Mode"),
                                "source": mount.get("Source"),
                                "type": mount.get("Type"),
                            }
                            for mount in inspected.get("Mounts", [])
                            if isinstance(mount, dict)
                        ],
                        "running": state_value.get("Running"),
                        "service": service,
                    }
                    service_evidence.append(observed)
                    if _is_stopped_live_executor(service, observed):
                        continue
                    if (
                        observed["running"] is not True
                        or health not in {None, "healthy"}
                    ):
                        failed("READINESS_SERVICE_UNHEALTHY", observed)
                    if service == "live-executor":
                        failed("READINESS_LIVE_EXECUTOR_ACTIVE", observed)
                except GatewayError as exc:
                    failed(exc.code, exc.evidence)
                except (json.JSONDecodeError, TypeError):
                    failed(
                        "READINESS_CONTAINER_INSPECT_INVALID",
                        {"service": service},
                    )
        checks["services"] = service_evidence

        try:
            controls = _control_evidence(paths, active)
            checks["controls"] = controls
            _require_fail_closed_controls(controls)
        except GatewayError as exc:
            failed(exc.code, exc.evidence)

        try:
            active_state = _historical_contract_evidence(
                paths,
                active["active_release"],
            )
            contract = {
                "contract_paused": active_state["contract_paused"],
                "owner_transaction_hash": active_state[
                    "owner_transaction_hash"
                ],
            }
            checks["contract"] = contract
            historical_safe = (
                contract["contract_paused"] is True
                and contract["owner_transaction_hash"] is None
            )
            if not historical_safe:
                try:
                    checks["live_executor"] = _live_executor_stopped()
                except GatewayError as exc:
                    failed(exc.code, exc.evidence)
                if (
                    not isinstance(contract["owner_transaction_hash"], str)
                    or controls is None
                    or platform_manifest_digest is None
                    or installed_sha != candidate_sha
                ):
                    failed(
                        "READINESS_CHAIN_RECONCILIATION_INVALID",
                        {"code": "CHAIN_EVIDENCE_PREREQUISITE_INVALID"},
                    )
                else:
                    try:
                        checks["chain_reconciliation"] = (
                            _readiness_chain_reconciliation(
                                paths,
                                active_release=active["active_release"],
                                release_assets_sha=active[
                                    "release_assets_sha"
                                ],
                                candidate_sha=candidate_sha,
                                platform_manifest_sha256=(
                                    platform_manifest_digest
                                ),
                                controls=controls,
                                historical_contract_paused=contract[
                                    "contract_paused"
                                ],
                                owner_transaction_hash=contract[
                                    "owner_transaction_hash"
                                ],
                            )
                        )
                    except GatewayError as exc:
                        failed(exc.code, exc.evidence)
        except (OSError, StateError, GatewayError) as exc:
            failed(
                "READINESS_ACTIVE_STATE_INVALID",
                {"message": _bounded_output(str(exc))},
            )

    result = {
        "candidate_sha": candidate_sha,
        "checks": checks,
        "failure_count": len(failures),
        "failures": failures,
        "schema": "phoenix.production-readiness.v1",
        "status": "passed" if not failures else "failed",
    }
    if failures:
        raise GatewayError("PRODUCTION_READINESS_FAILED", result)
    return result


def reconcile_chain_evidence(
    paths: HostPaths,
    protected_main_sha: str,
) -> dict[str, Any]:
    if os.environ.get("PHOENIX_RELEASE_LOCK_HELD") != "1":
        raise GatewayError("CHAIN_EVIDENCE_RELEASE_LOCK_REQUIRED")
    if not SHA_RE.fullmatch(protected_main_sha):
        raise GatewayError("RELEASE_SHA_INVALID")
    initial = status(paths)
    if (
        initial["phoenix_mode"] != "SHADOW"
        or initial["live_execution"] is not False
        or initial["autonomous_execution"] is not False
    ):
        raise GatewayError(
            "CHAIN_EVIDENCE_RUNTIME_MODE_UNSAFE",
            {
                "autonomous_execution": initial["autonomous_execution"],
                "live_execution": initial["live_execution"],
                "phoenix_mode": initial["phoenix_mode"],
            },
        )
    active_release = initial["active_release"]
    release_assets_sha = initial["release_assets_sha"]
    if active_release != release_assets_sha:
        raise GatewayError(
            "ACTIVE_POINTER_MISMATCH",
            {
                "current_release": active_release,
                "release_assets": release_assets_sha,
            },
        )
    active_state = _historical_contract_evidence(paths, active_release)
    owner_transaction_hash = active_state["owner_transaction_hash"]
    historical_contract_paused = active_state["contract_paused"]
    if not isinstance(owner_transaction_hash, str):
        raise GatewayError("CHAIN_EVIDENCE_HISTORICAL_TRANSACTION_MISSING")

    controls = _control_evidence(paths, initial)
    _require_fail_closed_controls(controls)
    _live_executor_stopped()

    platform_manifest_path = paths.libexec / "platform-manifest.json"
    platform_manifest = _read_json(platform_manifest_path, 256 * 1024)
    if (
        not isinstance(platform_manifest, dict)
        or platform_manifest.get("release_sha") != protected_main_sha
    ):
        raise GatewayError("CHAIN_EVIDENCE_PLATFORM_IDENTITY_INVALID")
    _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_platform.py"),
            "verify",
            "--installed-root",
            "/",
            "--expected-sha",
            protected_main_sha,
        ],
        "CHAIN_EVIDENCE_PLATFORM_DRIFT",
    )
    platform_manifest_sha256 = sha256_file(platform_manifest_path)

    environment = _selected_environment(
        paths.env_file,
        {
            "LIVE_EXECUTOR_EXECUTOR_ADDRESS",
            "PRODUCTION_RPC_URL",
            "RPC_PROVIDER_URLS",
            "SECONDARY_RPC_URL",
        },
    )
    providers = [
        value.strip()
        for value in environment["RPC_PROVIDER_URLS"].split(",")
        if value.strip()
    ]
    if providers != [
        environment["PRODUCTION_RPC_URL"],
        environment["SECONDARY_RPC_URL"],
    ]:
        raise GatewayError("CHAIN_EVIDENCE_PROVIDER_IDENTITY_INVALID")
    try:
        provider_evidence = collect_provider_evidence(
            providers,
            environment["LIVE_EXECUTOR_EXECUTOR_ADDRESS"],
            owner_transaction_hash,
        )
        evidence_value = build_chain_reconciliation_evidence(
            active_release_sha=active_release,
            release_assets_sha=release_assets_sha,
            protected_main_sha=protected_main_sha,
            release_platform_manifest_sha256=platform_manifest_sha256,
            executor_address=environment[
                "LIVE_EXECUTOR_EXECUTOR_ADDRESS"
            ],
            owner_transaction_hash=owner_transaction_hash,
            historical_contract_paused=historical_contract_paused,
            runtime=_reconciliation_runtime(controls),
            providers=provider_evidence,
        )
    except ReconciliationError as exc:
        raise GatewayError(exc.code) from exc

    final = status(paths)
    if (
        final["active_release"] != active_release
        or final["release_assets_sha"] != release_assets_sha
        or final["active_release"] != final["release_assets_sha"]
    ):
        raise GatewayError(
            "CHAIN_EVIDENCE_ACTIVE_POINTER_CHANGED",
            {
                "actual_active_release": final["active_release"],
                "actual_release_assets": final["release_assets_sha"],
                "expected_active_release": active_release,
                "expected_release_assets": release_assets_sha,
            },
        )
    path = chain_reconciliation_path(
        paths.state_root,
        active_release,
        protected_main_sha,
    )
    try:
        created = write_chain_reconciliation_evidence(path, evidence_value)
    except ReconciliationError as exc:
        raise GatewayError(exc.code) from exc
    return {
        "active_release": active_release,
        "evidence_sha256": chain_reconciliation_digest(evidence_value),
        "idempotent": not created,
        "protected_main_sha": protected_main_sha,
        "release_assets_sha": release_assets_sha,
        "schema": "phoenix.chain-reconciliation-result.v1",
        "status": "reconciled",
    }


def _deployment_diagnostics(
    paths: HostPaths, release_sha: str
) -> dict[str, Any]:
    try:
        return production_readiness(paths, release_sha)
    except GatewayError as exc:
        if exc.code == "PRODUCTION_READINESS_FAILED":
            return exc.evidence
        return {
            "failure_count": 1,
            "failures": [{"code": exc.code, "evidence": exc.evidence}],
            "schema": "phoenix.production-readiness.v1",
            "status": "failed",
        }


def reconcile_snapshot(snapshot: dict[str, Any]) -> dict[str, Any]:
    required = {
        "environment_sha",
        "current_release",
        "release_assets_sha",
        "state_sha",
        "context_sha",
        "manifest_sha",
        "configured_images",
        "running_images",
    }
    if set(snapshot) != required:
        raise GatewayError("RECONCILE_SNAPSHOT_INVALID")
    target = snapshot["current_release"]
    for field in ("environment_sha", "release_assets_sha", "manifest_sha"):
        if snapshot[field] != target:
            return {
                "action": "reject",
                "code": "ACTIVE_IDENTITY_MISMATCH",
                "field": field,
                "expected": target,
                "actual": snapshot[field],
            }
    configured = snapshot["configured_images"]
    running = snapshot["running_images"]
    if not isinstance(configured, dict) or not isinstance(running, dict):
        raise GatewayError("RECONCILE_SNAPSHOT_INVALID")
    for service, expected in configured.items():
        actual = running.get(service)
        if actual != expected:
            return {
                "action": "reject",
                "code": "RUNNING_IMAGE_MISMATCH",
                "service": service,
                "expected_image": expected,
                "actual_image": actual,
            }
    stale = [
        field
        for field in ("state_sha", "context_sha")
        if snapshot[field] != target
    ]
    return {
        "action": "rewrite-metadata" if stale else "unchanged",
        "release_sha": target,
        "stale_fields": stale,
        "container_mutation": False,
        "contract_mutation": False,
    }


def _service_absence_allowed(
    component: dict[str, Any], phoenix_mode: str
) -> bool:
    return phoenix_mode == "SHADOW" and component.get("live_canary_only") is True


def _live_executor_absence_is_fail_closed(
    release_evidence: dict[str, Any], runtime_evidence: object
) -> bool:
    return (
        release_evidence.get("contract_paused") is True
        and release_evidence.get("autonomous_armed") is False
        and release_evidence.get("kill_switch") is True
        and isinstance(runtime_evidence, dict)
        and set(runtime_evidence)
        == {
            "active_attempts",
            "armed",
            "execution_mode",
            "kill_switch",
            "open_routes",
            "unresolved_submissions",
        }
        and runtime_evidence["armed"] is False
        and runtime_evidence["kill_switch"] is True
        and runtime_evidence["execution_mode"] == "disarmed"
        and type(runtime_evidence["active_attempts"]) is int
        and runtime_evidence["active_attempts"] == 0
        and type(runtime_evidence["open_routes"]) is int
        and runtime_evidence["open_routes"] == 0
        and type(runtime_evidence["unresolved_submissions"]) is int
        and runtime_evidence["unresolved_submissions"] == 0
    )


def _require_fail_closed_live_executor_absence(
    paths: HostPaths, compose_arguments: list[str], release_sha: str
) -> None:
    release_evidence = load_state(state_file(paths, release_sha))
    output = _require_success(
        compose_arguments
        + [
            "exec",
            "-T",
            "postgres",
            "/bin/sh",
            "-c",
            (
                "exec psql -X -qAt -v ON_ERROR_STOP=1 "
                '-U "$POSTGRES_USER" -d "$POSTGRES_DB" -c '
                "\"SELECT json_build_object("
                "'armed', global_control.armed, "
                "'kill_switch', global_control.kill_switch, "
                "'execution_mode', global_control.execution_mode, "
                "'open_routes', (SELECT count(*) "
                "FROM live_canary.autonomous_route_controls "
                "WHERE enabled OR NOT kill_switch), "
                "'active_attempts', (SELECT count(*) "
                "FROM live_canary.execution_attempts "
                "WHERE status IN ("
                "'claimed', 'nonce_allocated', 'submission_unknown', "
                "'pending', 'timed_out')), "
                "'unresolved_submissions', (SELECT count(*) "
                "FROM live_canary.execution_attempts "
                "WHERE status IN ("
                "'submission_unknown', 'pending', 'timed_out'))"
                ")::text "
                "FROM live_canary.autonomous_global_control AS global_control "
                "WHERE global_control.singleton\""
            ),
        ],
        "ACTIVE_SERVICE_EVIDENCE_FAILED",
    )
    try:
        runtime_evidence = json.loads(output)
    except json.JSONDecodeError as exc:
        raise GatewayError("ACTIVE_SERVICE_EVIDENCE_INVALID") from exc
    if not _live_executor_absence_is_fail_closed(
        release_evidence, runtime_evidence
    ):
        bounded_runtime = (
            runtime_evidence if isinstance(runtime_evidence, dict) else {}
        )
        raise GatewayError(
            "ACTIVE_SERVICE_ABSENCE_UNSAFE",
            {
                "service": "live-executor",
                "contract_paused": release_evidence.get("contract_paused"),
                "armed": bounded_runtime.get("armed"),
                "kill_switch": bounded_runtime.get("kill_switch"),
                "active_attempts": bounded_runtime.get("active_attempts"),
                "execution_mode": bounded_runtime.get("execution_mode"),
                "open_routes": bounded_runtime.get("open_routes"),
                "unresolved_submissions": bounded_runtime.get(
                    "unresolved_submissions"
                ),
            },
        )


def reconcile_active_context(paths: HostPaths) -> dict[str, Any]:
    active = status(paths)
    release_sha = active["active_release"]
    release_root = paths.releases / release_sha
    manifest = _read_json(paths.deploy_dir / "manifests" / f"{release_sha}.json")
    components = _read_json(release_root / "release-components.json")
    if (
        not isinstance(manifest, dict)
        or not isinstance(manifest.get("images"), dict)
        or not isinstance(components, dict)
        or not isinstance(components.get("components"), list)
    ):
        raise GatewayError("ACTIVE_MANIFEST_INVALID")
    release_env = paths.deploy_dir / "current-release.env"
    compose_arguments = production_compose_command(
        paths,
        mode="LIVE" if active["phoenix_mode"] == "LIVE" else "SHADOW",
        release_env=release_env,
    )
    rendered = _require_success(
        compose_arguments + ["config", "--format", "json"],
        "ACTIVE_COMPOSE_RENDER_FAILED",
    )
    try:
        rendered_value = json.loads(rendered)
        services = rendered_value["services"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise GatewayError("ACTIVE_COMPOSE_RENDER_INVALID") from exc
    for component in components["components"]:
        if not isinstance(component, dict) or not component.get("production_compose"):
            continue
        name = component.get("name")
        image_value = manifest["images"].get(name)
        production_services = component.get("production_services")
        if (
            not isinstance(name, str)
            or not isinstance(production_services, list)
        ):
            raise GatewayError("ACTIVE_MANIFEST_INVALID")
        if image_value is None:
            for service in production_services:
                container_id = _require_success(
                    compose_arguments + ["ps", "-a", "-q", service],
                    "ACTIVE_CONTAINER_LOOKUP_FAILED",
                ).strip()
                if container_id:
                    raise GatewayError(
                        "RUNNING_IMAGE_MISMATCH",
                        {
                            "service": service,
                            "expected_image": None,
                            "configured_image": None,
                            "image_id": None,
                            "container_id": container_id,
                        },
                    )
            continue
        if not isinstance(image_value, dict):
            raise GatewayError("ACTIVE_MANIFEST_INVALID")
        expected_image = f"{image_value.get('repository')}@{image_value.get('digest')}"
        for service in production_services:
            if service not in services:
                if _service_absence_allowed(component, active["phoenix_mode"]):
                    continue
                raise GatewayError(
                    "ACTIVE_SERVICE_MISSING",
                    {"service": service, "expected_image": expected_image},
                )
            configured_image = services[service].get("image")
            if configured_image != expected_image:
                raise GatewayError(
                    "RUNNING_IMAGE_MISMATCH",
                    {
                        "service": service,
                        "expected_image": expected_image,
                        "configured_image": configured_image,
                        "image_id": None,
                        "container_id": None,
                    },
                )
            container_id = _require_success(
                compose_arguments + ["ps", "-a", "-q", service],
                "ACTIVE_CONTAINER_LOOKUP_FAILED",
            ).strip()
            if not container_id:
                if (
                    name == "live-executor"
                    and component.get("live_canary_only") is True
                    and active["phoenix_mode"] == "LIVE"
                ):
                    _require_fail_closed_live_executor_absence(
                        paths, compose_arguments, release_sha
                    )
                # Services deliberately absent in the current mode do not prove a
                # mismatch and are left untouched.
                continue
            inspection = _require_success(
                [
                    "/usr/bin/docker",
                    "inspect",
                    "--format",
                    "{{.Config.Image}}|{{.Image}}",
                    container_id,
                ],
                "ACTIVE_CONTAINER_INSPECT_FAILED",
            ).strip()
            actual_reference, separator, image_id = inspection.partition("|")
            if not separator or actual_reference != expected_image:
                raise GatewayError(
                    "RUNNING_IMAGE_MISMATCH",
                    {
                        "service": service,
                        "expected_image": expected_image,
                        "configured_image": configured_image,
                        "actual_image": actual_reference,
                        "image_id": image_id or None,
                        "container_id": container_id,
                    },
                )
    installer = release_root / "scripts" / "install-production-release-context.sh"
    if not installer.is_file() or installer.is_symlink():
        raise GatewayError(
            "ACTIVE_CONTEXT_INSTALLER_INVALID", {"path": str(installer)}
        )
    _require_success(
        ["/bin/sh", str(installer), release_sha, str(release_root)],
        "ACTIVE_CONTEXT_REPAIR_FAILED",
        env={"PHOENIX_DEPLOY_ROOT": str(paths.deploy_root)},
    )
    return {
        "schema": "phoenix.release-reconcile.v1",
        "status": "reconciled",
        "release_sha": release_sha,
        "container_mutation": False,
        "contract_mutation": False,
    }


def _verify_evidence(paths: HostPaths, request: dict[str, Any]) -> dict[str, Any]:
    root = paths.incoming / request["release_sha"]
    provenance = _read_json(root / "release-provenance.json")
    manifest = _read_json(root / "release-manifest.json")
    if not isinstance(provenance, dict) or not isinstance(manifest, dict):
        raise GatewayError("RELEASE_EVIDENCE_INVALID")
    source = provenance.get("source_ci")
    if not isinstance(source, dict) or (
        str(source.get("run_id")),
        str(source.get("run_attempt")),
    ) != (
        str(request["source_ci_run_id"]),
        str(request["source_ci_run_attempt"]),
    ):
        raise GatewayError("SOURCE_CI_IDENTITY_MISMATCH")
    if provenance.get("release_sha") != request["release_sha"]:
        raise GatewayError("RELEASE_PROVENANCE_SHA_MISMATCH")
    if manifest.get("release_sha") != request["release_sha"]:
        raise GatewayError("RELEASE_MANIFEST_SHA_MISMATCH")
    if str(provenance.get("build_run_id")) != str(request["build_run_id"]):
        raise GatewayError("RELEASE_BUILD_IDENTITY_MISMATCH")
    images = manifest.get("images")
    if not isinstance(images, dict) or len(images) not in {7, 8}:
        raise GatewayError("RELEASE_IMAGE_SET_INVALID")
    expected_images: dict[str, str] = {}
    for name, image in images.items():
        if not isinstance(image, dict):
            raise GatewayError("RELEASE_IMAGE_SET_INVALID")
        repository, digest = image.get("repository"), image.get("digest")
        if not all(isinstance(item, str) for item in (name, repository, digest)):
            raise GatewayError("RELEASE_IMAGE_SET_INVALID")
        expected_images[name] = f"{repository}@{digest}"
    return expected_images


def _host_preflight(paths: HostPaths, request: dict[str, Any]) -> None:
    active = status(paths)
    if active["active_release"] != request["rollback_sha"]:
        raise GatewayError(
            "ACTIVE_RELEASE_CHANGED",
            {
                "expected": request["rollback_sha"],
                "actual": active["active_release"],
            },
        )
    rollback_root = paths.releases / request["rollback_sha"]
    validator = paths.deploy_dir / "validate-production-release-context.sh"
    required = (
        validator,
        paths.deploy_dir / "compose.prod.yml",
        paths.deploy_dir / "current-release.env",
        paths.deploy_dir / "current-release.json",
        rollback_root / "release-assets-manifest.json",
    )
    for path in required:
        if not path.is_file() or path.is_symlink():
            raise GatewayError("HOST_PREFLIGHT_FILE_INVALID", {"path": str(path)})


def _rehearse_candidate(paths: HostPaths, request: dict[str, Any]) -> None:
    root = paths.incoming / request["release_sha"]
    release_sha = request["release_sha"]
    archive_path = root / f"phoenix-release-assets-{release_sha}.tar.gz"
    _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_assets.py"),
            "verify",
            "--archive",
            str(archive_path),
            "--manifest",
            str(root / "release-assets-manifest.json"),
            "--checksums",
            str(root / "release-assets-checksums.txt"),
            "--expected-sha",
            release_sha,
        ],
        "REHEARSAL_RELEASE_ASSETS_INVALID",
    )
    paths.state_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=f".rehearse-{release_sha}.", dir=paths.state_root
    ) as temporary:
        staging = Path(temporary)
        bundle_root = f"phoenix-release-{release_sha}"
        try:
            with tarfile.open(archive_path, mode="r:gz") as archive:
                for member in archive.getmembers():
                    if (
                        not member.isfile()
                        or not member.name.startswith(f"{bundle_root}/")
                    ):
                        raise GatewayError("REHEARSAL_ARCHIVE_INVALID")
                    relative = Path(*Path(member.name).parts[1:])
                    destination = staging / bundle_root / relative
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    extracted = archive.extractfile(member)
                    if extracted is None:
                        raise GatewayError("REHEARSAL_ARCHIVE_INVALID")
                    _atomic_bytes(
                        destination,
                        extracted.read(member.size + 1),
                        member.mode,
                    )
        except (OSError, tarfile.TarError) as exc:
            raise GatewayError("REHEARSAL_ARCHIVE_INVALID") from exc
        candidate_root = staging / bundle_root
        _require_success(
            [
                "/usr/bin/python3",
                "-I",
                "-B",
                str(candidate_root / "scripts" / "release_assets.py"),
                "verify-tree",
                "--root",
                str(candidate_root),
                "--manifest",
                str(candidate_root / "release-assets-manifest.json"),
                "--expected-sha",
                release_sha,
            ],
            "REHEARSAL_TREE_INVALID",
        )
        _require_success(
            [
                "/bin/sh",
                str(candidate_root / "scripts" / "rehearse-production-release.sh"),
                release_sha,
                str(candidate_root),
                str(root / "release-manifest.json"),
            ],
            "CANDIDATE_REHEARSAL_FAILED",
            env={
                "PHOENIX_DEPLOY_ROOT": str(paths.deploy_root),
                "PHOENIX_ENV_FILE": str(paths.env_file),
            },
        )


def _install_candidate(paths: HostPaths, request: dict[str, Any]) -> None:
    root = paths.incoming / request["release_sha"]
    release_sha = request["release_sha"]
    rollback_sha = request["rollback_sha"]
    archive = root / f"phoenix-release-assets-{release_sha}.tar.gz"
    _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_provenance.py"),
            "validate-deploy-pair",
            "--release-manifest",
            str(root / "release-manifest.json"),
            "--release-provenance",
            str(root / "release-provenance.json"),
            "--release-sha",
            release_sha,
            "--build-run-id",
            str(request["build_run_id"]),
            "--rollback-manifest",
            str(root / "rollback-manifest.json"),
            "--rollback-provenance",
            str(root / "rollback-provenance.json"),
            "--rollback-sha",
            rollback_sha,
            "--rollback-build-run-id",
            str(request["rollback_build_run_id"]),
        ],
        "RELEASE_PROVENANCE_INVALID",
    )
    _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_assets.py"),
            "verify",
            "--archive",
            str(archive),
            "--manifest",
            str(root / "release-assets-manifest.json"),
            "--checksums",
            str(root / "release-assets-checksums.txt"),
            "--expected-sha",
            release_sha,
        ],
        "RELEASE_ASSETS_INVALID",
    )
    environment = {
        "PHOENIX_RELEASE_ROOT": str(paths.releases),
    }
    _require_success(
        [
            "/bin/sh",
            str(paths.libexec / "install-release-assets.sh"),
            release_sha,
            str(archive),
            str(root / "release-assets-manifest.json"),
            str(root / "release-assets-checksums.txt"),
        ],
        "CANDIDATE_INSTALL_FAILED",
        env=environment,
    )
    release_root = paths.releases / release_sha
    authorized_keys = Path("/home/phoenix-deploy/.ssh/authorized_keys")
    if not authorized_keys.is_file() or authorized_keys.is_symlink():
        raise GatewayError("DEPLOY_PUBLIC_KEY_INVALID")
    key_digest_before = sha256_file(authorized_keys)
    _require_success(
        [
            "/bin/sh",
            str(release_root / "scripts" / "install-phoenix-release-platform.sh"),
            "--release-sha",
            release_sha,
            "--reuse-existing-key",
        ],
        "RELEASE_PLATFORM_INSTALL_FAILED",
    )
    if sha256_file(authorized_keys) != key_digest_before:
        raise GatewayError("DEPLOY_PUBLIC_KEY_CHANGED")
    _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_platform.py"),
            "verify",
            "--installed-root",
            "/",
            "--expected-sha",
            release_sha,
        ],
        "RELEASE_PLATFORM_IDENTITY_MISMATCH",
    )
    _require_success(
        [
            "/bin/sh",
            str(
                release_root
                / "scripts"
                / "install-production-release-context.sh"
            ),
            release_sha,
            str(release_root),
        ],
        "EXACT_CANDIDATE_CONTEXT_INSTALL_FAILED",
        env={
            "PHOENIX_DEPLOY_ROOT": str(paths.deploy_root),
            "PHOENIX_ENV_FILE": str(paths.env_file),
        },
    )
    manifest_dir = paths.deploy_dir / "manifests"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    for source_name, suffix in (
        ("release-manifest.json", ".json"),
        ("release-provenance.json", ".provenance.json"),
    ):
        destination = manifest_dir / f"{release_sha}{suffix}"
        _atomic_bytes(destination, (root / source_name).read_bytes(), 0o640)


def _verify_exact_platform(paths: HostPaths, release_sha: str) -> None:
    _require_success(
        [
            "/usr/bin/python3",
            "-I",
            "-B",
            str(paths.libexec / "release_platform.py"),
            "verify",
            "--installed-root",
            "/",
            "--expected-sha",
            release_sha,
        ],
        "RELEASE_PLATFORM_IDENTITY_MISMATCH",
    )
    release_root = paths.releases / release_sha
    for name in (
        "deploy-release.sh",
        "production-healthcheck.sh",
        "production_compose.py",
        "render-production-compose.sh",
        "rollback-release.sh",
    ):
        installed = paths.deploy_dir / name
        candidate = release_root / "scripts" / name
        if (
            not installed.is_file()
            or installed.is_symlink()
            or not candidate.is_file()
            or candidate.is_symlink()
            or sha256_file(installed) != sha256_file(candidate)
        ):
            raise GatewayError(
                "RELEASE_CONTEXT_PLATFORM_DRIFT", {"file": name}
            )


def _write_state(paths: HostPaths, state: dict[str, Any]) -> None:
    atomic_write(state_file(paths, state["release_sha"]), state)


def _runtime_may_have_changed(state: dict[str, Any]) -> bool:
    failure_phase = state.get("failure_phase")
    return (
        failure_phase in PHASES
        and PHASES.index(failure_phase)
        >= PHASES.index("CANDIDATE_LIVE_RENDER_VERIFIED")
    )


def _rollback_failed_state(
    paths: HostPaths, state: dict[str, Any]
) -> dict[str, Any]:
    if state["current_phase"] == "FAILED_POST_MUTATION":
        state = rollback_phase(state, "ROLLBACK_STARTED")
        _write_state(paths, state)
    if state["current_phase"] not in {"ROLLBACK_STARTED", "ROLLBACK_FAILED"}:
        return state
    rollback_sha = state["rollback_sha"]
    context_installer = paths.libexec / "install-production-release-context.sh"
    try:
        runtime_may_have_changed = _runtime_may_have_changed(state)
        if runtime_may_have_changed:
            emergency_pause(paths)
        context_sha = (
            state["release_sha"]
            if runtime_may_have_changed
            else rollback_sha
        )
        context_root = paths.releases / context_sha
        _require_success(
            [
                "/bin/sh",
                str(context_installer),
                context_sha,
                str(context_root),
            ],
            "ROLLBACK_CONTEXT_INSTALL_FAILED",
            env={"PHOENIX_DEPLOY_ROOT": str(paths.deploy_root)},
        )
        if runtime_may_have_changed:
            rollback_script = paths.libexec / "rollback-release.sh"
            _require_success(
                ["/bin/sh", str(rollback_script)],
                "ROLLBACK_FAILED",
                env={
                    "PHOENIX_DEPLOY_ROOT": str(paths.deploy_root),
                    "PHOENIX_RELEASE_ROOT": str(paths.releases),
                    "PHOENIX_ENV_FILE": str(paths.env_file),
                    "PHOENIX_CONTEXT_INSTALLER": str(context_installer),
                },
            )
        state = rollback_phase(
            state,
            "ROLLED_BACK",
            {
                "status": "ok",
                "active_release": rollback_sha,
                "runtime_reconciled": runtime_may_have_changed,
            },
        )
        state.update(
            {
                "active_release_pointer": rollback_sha,
                "candidate_pointer": None,
                "contract_paused": True,
                "autonomous_armed": False,
                "kill_switch": True,
            }
        )
    except (GatewayError, OSError) as exc:
        result = {
            "status": "failed",
            "code": exc.code if isinstance(exc, GatewayError) else "OS_ERROR",
            "evidence": (
                exc.evidence
                if isinstance(exc, GatewayError)
                else {"message": _bounded_output(str(exc))}
            ),
        }
        if state["current_phase"] == "ROLLBACK_FAILED":
            state["rollback_result"] = result
        else:
            state = rollback_phase(state, "ROLLBACK_FAILED", result)
    _write_state(paths, state)
    return state


def resume(paths: HostPaths, release_sha: str) -> dict[str, Any]:
    state_path = state_file(paths, release_sha)
    state = load_state(state_path)
    request = validate_request(_read_json(request_file(paths, release_sha), 64 * 1024))
    if state["current_phase"] == "COMPLETED":
        return state
    if state["current_phase"] in {
        "FAILED_PRE_MUTATION",
        "ROLLED_BACK",
    }:
        return state
    if state["current_phase"] in {
        "FAILED_POST_MUTATION",
        "ROLLBACK_STARTED",
        "ROLLBACK_FAILED",
    }:
        return _rollback_failed_state(paths, state)
    if state["mutation_started"] and state["current_phase"] not in {
        "CANDIDATE_INSTALLED",
        "FAILED_POST_MUTATION",
        "ROLLBACK_STARTED",
    }:
        state = fail_state(
            state,
            code="INTERRUPTED_AFTER_MUTATION",
            evidence={
                "decision": "do-not-retry-owner-transaction",
                "last_phase": state["current_phase"],
            },
        )
        _write_state(paths, state)
        return _rollback_failed_state(paths, state)
    try:
        if state["current_phase"] == "REQUESTED":
            expected_images = _verify_evidence(paths, request)
            state = advance(
                state,
                "SOURCE_CI_VERIFIED",
                updates={"expected_images": expected_images},
            )
            _write_state(paths, state)
        if state["current_phase"] == "SOURCE_CI_VERIFIED":
            root = paths.incoming / release_sha
            state = advance(
                state,
                "BUILD_VERIFIED",
                updates={
                    "release_manifest_digest": sha256_file(
                        root / "release-manifest.json"
                    ),
                    "release_assets_digest": sha256_file(
                        root / f"phoenix-release-assets-{release_sha}.tar.gz"
                    ),
                },
            )
            _write_state(paths, state)
        if state["current_phase"] == "BUILD_VERIFIED":
            state = advance(state, "HOST_PREFLIGHT_STARTED")
            _write_state(paths, state)
        if state["current_phase"] == "HOST_PREFLIGHT_STARTED":
            _host_preflight(paths, request)
            state = advance(
                state,
                "HOST_PREFLIGHT_OK",
                updates={
                    "contract_paused": True,
                    "autonomous_armed": False,
                    "kill_switch": True,
                },
            )
            _write_state(paths, state)
        if state["current_phase"] == "HOST_PREFLIGHT_OK":
            reconcile_active_context(paths)
            state = advance(state, "ACTIVE_CONTEXT_RECONCILED")
            _write_state(paths, state)
        if state["current_phase"] == "ACTIVE_CONTEXT_RECONCILED":
            rollback_root = paths.releases / request["rollback_sha"]
            _require_success(
                [
                    "/usr/bin/python3",
                    "-I",
                    "-B",
                    str(paths.libexec / "release_assets.py"),
                    "verify-tree",
                    "--root",
                    str(rollback_root),
                    "--manifest",
                    str(rollback_root / "release-assets-manifest.json"),
                    "--expected-sha",
                    request["rollback_sha"],
                ],
                "ROLLBACK_RELEASE_INVALID",
            )
            state = advance(state, "ROLLBACK_VERIFIED")
            _write_state(paths, state)
        if state["current_phase"] == "ROLLBACK_VERIFIED":
            _rehearse_candidate(paths, request)
            state = advance(state, "CANDIDATE_REHEARSED")
            _write_state(paths, state)
        if state["current_phase"] == "CANDIDATE_REHEARSED":
            state = set_mutation_started(state)
            _write_state(paths, state)
            _install_candidate(paths, request)
            state = advance(state, "CANDIDATE_INSTALLED")
            _write_state(paths, state)
        if state["current_phase"] == "CANDIDATE_INSTALLED":
            _verify_exact_platform(paths, release_sha)
            environment = {
                "PHOENIX_DEPLOY_ROOT": str(paths.deploy_root),
                "PHOENIX_RELEASE_ROOT": str(paths.releases),
                "PHOENIX_ENV_FILE": str(paths.env_file),
                "PHOENIX_RELEASE_STATE_ROOT": str(paths.state_root),
                "PHOENIX_RELEASE_STATE_UPDATER": str(paths.state_updater),
                "PHOENIX_CONTEXT_INSTALLER": str(
                    paths.libexec / "install-production-release-context.sh"
                ),
            }
            result = _run(
                [
                    "/bin/sh",
                    str(paths.deploy_dir / "deploy-release.sh"),
                    release_sha,
                ],
                env=environment,
            )
            state = load_state(state_path)
            if result.returncode != 0:
                combined_output = "\n".join(
                    part
                    for part in (result.stdout, result.stderr)
                    if part
                )
                failure_evidence = {
                    "command": shlex.join(
                        [
                            "/bin/sh",
                            str(paths.deploy_dir / "deploy-release.sh"),
                            release_sha,
                        ]
                    ),
                    "exit_code": result.returncode,
                    "stderr": _bounded_output(result.stderr),
                    "stdout": _bounded_output(result.stdout),
                    "output": _bounded_output(combined_output),
                    "source": "deploy-release",
                    "diagnostics": _deployment_diagnostics(
                        paths, release_sha
                    ),
                }
                if state["current_phase"] not in {
                    "FAILED_PRE_MUTATION",
                    "FAILED_POST_MUTATION",
                    "ROLLED_BACK",
                    "ROLLBACK_FAILED",
                }:
                    state = fail_state(
                        state,
                        code="DEPLOYMENT_FAILED",
                        evidence=failure_evidence,
                    )
                    _write_state(paths, state)
                elif (
                    state["failure_code"] == "DEPLOYMENT_FAILED"
                    and state["failure_evidence"]
                    == {"source": "deploy-release", "detail": "deployment_failed"}
                ):
                    state = complete_failure_evidence(state, failure_evidence)
                    _write_state(paths, state)
                raise GatewayError(
                    "DEPLOYMENT_FAILED",
                    {
                        "phase": state["current_phase"],
                        "exit_code": result.returncode,
                        "stderr": _bounded_output(result.stderr),
                        "stdout": _bounded_output(result.stdout),
                        "output": _bounded_output(combined_output),
                    },
                )
            state = load_state(state_path)
            if state["current_phase"] != "COMPLETED":
                raise GatewayError(
                    "DEPLOYMENT_STATE_INCOMPLETE",
                    {"current_phase": state["current_phase"]},
                )
        return state
    except GatewayError as exc:
        current = load_state(state_path)
        if current["current_phase"] not in {
            "FAILED_PRE_MUTATION",
            "FAILED_POST_MUTATION",
            "ROLLED_BACK",
            "ROLLBACK_FAILED",
        }:
            current = fail_state(current, code=exc.code, evidence=exc.evidence)
            _write_state(paths, current)
        if current["current_phase"] in {"FAILED_POST_MUTATION", "ROLLBACK_STARTED"}:
            _rollback_failed_state(paths, current)
        raise
    except (OSError, StateError) as exc:
        current = load_state(state_path)
        if current["current_phase"] not in {
            "FAILED_PRE_MUTATION",
            "FAILED_POST_MUTATION",
            "ROLLED_BACK",
            "ROLLBACK_FAILED",
        }:
            current = fail_state(
                current,
                code="RELEASE_STATE_ERROR",
                evidence={"message": str(exc)[:1024]},
            )
            _write_state(paths, current)
        if current["current_phase"] in {"FAILED_POST_MUTATION", "ROLLBACK_STARTED"}:
            _rollback_failed_state(paths, current)
        raise GatewayError("RELEASE_STATE_ERROR") from exc


def retry_pre_mutation(paths: HostPaths, release_sha: str) -> dict[str, Any]:
    state_path = state_file(paths, release_sha)
    state = load_state(state_path)
    if state["current_phase"] != "FAILED_PRE_MUTATION":
        raise GatewayError("PRE_MUTATION_RETRY_PHASE_INVALID")
    if state["mutation_started"]:
        raise GatewayError("PRE_MUTATION_RETRY_AFTER_MUTATION")
    if state["owner_transaction_hash"] is not None:
        raise GatewayError("PRE_MUTATION_RETRY_OWNER_TRANSACTION_RECORDED")

    request = validate_request(_read_json(request_file(paths, release_sha), 64 * 1024))
    identity_fields = (
        "release_sha",
        "rollback_sha",
        "source_ci_run_id",
        "source_ci_run_attempt",
        "build_run_id",
        "deploy_run_id",
        "deploy_run_attempt",
    )
    if any(state[field] != request[field] for field in identity_fields):
        raise GatewayError("PRE_MUTATION_RETRY_REQUEST_MISMATCH")
    if (
        state["candidate_pointer"] != release_sha
        or state["active_release_pointer"] != state["rollback_sha"]
    ):
        raise GatewayError("PRE_MUTATION_RETRY_POINTER_MISMATCH")

    active = status(paths)
    if active["active_release"] != state["rollback_sha"]:
        raise GatewayError(
            "PRE_MUTATION_RETRY_ACTIVE_RELEASE_CHANGED",
            {
                "expected": state["rollback_sha"],
                "actual": active["active_release"],
            },
        )

    root = paths.incoming / release_sha
    expected_images = _verify_evidence(paths, request)
    if expected_images != state["expected_images"]:
        raise GatewayError("PRE_MUTATION_RETRY_IMAGE_EVIDENCE_MISMATCH")
    package_path = root / "release-package.tar.gz"
    if (
        sha256_file(root / "release-manifest.json")
        != state["release_manifest_digest"]
        or sha256_file(root / f"phoenix-release-assets-{release_sha}.tar.gz")
        != state["release_assets_digest"]
        or not package_path.is_file()
        or package_path.is_symlink()
        or state["package_digest"] is None
        or sha256_file(package_path) != state["package_digest"]
    ):
        raise GatewayError("PRE_MUTATION_RETRY_BUILD_EVIDENCE_MISMATCH")

    failed_state = json.loads(json.dumps(state))
    try:
        retried = retry_failed_pre_mutation(json.loads(json.dumps(state)))
    except StateError as exc:
        raise GatewayError("PRE_MUTATION_RETRY_STATE_INVALID") from exc
    failed_at = state["phase_timestamps"]["FAILED_PRE_MUTATION"]
    archive_payload = _canonical(
        {
            "schema": "phoenix.release-pre-mutation-retry-archive.v1",
            "failed_at": failed_at,
            "failed_state": failed_state,
            "failure_evidence": failed_state["failure_evidence"],
        }
    )
    archive_digest = hashlib.sha256(archive_payload).hexdigest()
    archive_name = (
        f"failed-pre-mutation-{failed_at.replace(':', '').replace('-', '')}-"
        f"{archive_digest}.json"
    )
    archive_path = state_path.parent / "retry-archives" / archive_name
    _atomic_archive(archive_path, archive_payload)

    atomic_write(state_path, retried)
    return {
        "schema": "phoenix.release-pre-mutation-retry.v1",
        "status": "reset",
        "release_sha": release_sha,
        "current_phase": retried["current_phase"],
        "archive": f"retry-archives/{archive_name}",
    }


def retry_rolled_back(paths: HostPaths, release_sha: str) -> dict[str, Any]:
    """Create a new durable attempt from an unchanged safe rollback."""
    state_path = state_file(paths, release_sha)
    state = load_state(state_path)
    if state["current_phase"] != "ROLLED_BACK":
        raise GatewayError("ROLLED_BACK_RETRY_PHASE_INVALID")
    if state["owner_transaction_hash"] is not None:
        raise GatewayError("ROLLED_BACK_RETRY_OWNER_TRANSACTION_RECORDED")
    request = validate_request(
        _read_json(request_file(paths, release_sha), 64 * 1024)
    )
    identity_fields = (
        "release_sha",
        "rollback_sha",
        "source_ci_run_id",
        "source_ci_run_attempt",
        "build_run_id",
        "deploy_run_id",
        "deploy_run_attempt",
    )
    if any(state[field] != request[field] for field in identity_fields):
        raise GatewayError("ROLLED_BACK_RETRY_REQUEST_MISMATCH")
    active = status(paths)
    if active["active_release"] != state["rollback_sha"]:
        raise GatewayError(
            "ROLLED_BACK_RETRY_ACTIVE_RELEASE_CHANGED",
            {
                "actual": active["active_release"],
                "expected": state["rollback_sha"],
            },
        )
    root = paths.incoming / release_sha
    if _verify_evidence(paths, request) != state["expected_images"]:
        raise GatewayError("ROLLED_BACK_RETRY_IMAGE_EVIDENCE_MISMATCH")
    package_path = root / "release-package.tar.gz"
    if (
        sha256_file(root / "release-manifest.json")
        != state["release_manifest_digest"]
        or sha256_file(root / f"phoenix-release-assets-{release_sha}.tar.gz")
        != state["release_assets_digest"]
        or not package_path.is_file()
        or package_path.is_symlink()
        or state["package_digest"] is None
        or sha256_file(package_path) != state["package_digest"]
    ):
        raise GatewayError("ROLLED_BACK_RETRY_PACKAGE_EVIDENCE_MISMATCH")

    release_env = paths.deploy_dir / "current-release.env"
    compose = production_compose_command(
        paths,
        mode="LIVE" if active["phoenix_mode"] == "LIVE" else "SHADOW",
        release_env=release_env,
    )
    _require_fail_closed_live_executor_absence(
        paths, compose, release_sha
    )

    failed_state = json.loads(json.dumps(state))
    try:
        retried = retry_rolled_back_release(json.loads(json.dumps(state)))
    except StateError as exc:
        raise GatewayError("ROLLED_BACK_RETRY_STATE_INVALID") from exc
    rolled_back_at = state["phase_timestamps"]["ROLLED_BACK"]
    archive_payload = _canonical(
        {
            "failed_state": failed_state,
            "rollback_evidence": failed_state["rollback_result"],
            "rolled_back_at": rolled_back_at,
            "schema": "phoenix.release-rolled-back-retry-archive.v1",
        }
    )
    archive_digest = hashlib.sha256(archive_payload).hexdigest()
    archive_name = (
        f"rolled-back-{rolled_back_at.replace(':', '').replace('-', '')}-"
        f"{archive_digest}.json"
    )
    archive_path = state_path.parent / "retry-archives" / archive_name
    _atomic_archive(archive_path, archive_payload)
    atomic_write(state_path, retried)
    return {
        "archive": f"retry-archives/{archive_name}",
        "current_phase": retried["current_phase"],
        "release_attempt": retried["release_attempt"],
        "release_sha": release_sha,
        "schema": "phoenix.release-rolled-back-retry.v1",
        "status": "reset",
    }


def history(paths: HostPaths) -> list[dict[str, Any]]:
    if not paths.release_states.exists():
        return []
    values = []
    for path in sorted(paths.release_states.glob("*/state.json")):
        try:
            state = load_state(path)
        except (OSError, StateError):
            continue
        values.append(
            {
                "release_sha": state["release_sha"],
                "current_phase": state["current_phase"],
                "requested_at": state["phase_timestamps"]["REQUESTED"],
                "completed_at": state["phase_timestamps"].get("COMPLETED"),
            }
        )
    return values[-100:]


def evidence(paths: HostPaths, release_sha: str) -> dict[str, Any]:
    return load_state(state_file(paths, release_sha))


def emergency_pause(paths: HostPaths) -> dict[str, Any]:
    active = status(paths)
    release_sha = active["active_release"]
    release_env = paths.deploy_dir / "current-release.env"
    compose = production_compose_command(
        paths, mode="LIVE", release_env=release_env
    )
    _run(compose + ["stop", "-t", "30", "live-executor"])
    pause = _run(
        compose
        + [
            "run",
            "--rm",
            "--no-deps",
            "-e",
            f"PHOENIX_RELEASE_SHA={release_sha}",
            "-e",
            "PHOENIX_EXECUTOR_OWNER_PAUSE_ACK=PAUSE_EXECUTOR_AFTER_FAILED_DEPLOY_42161",
            "--entrypoint",
            "/usr/local/bin/autonomous-live-control",
            "live-executor",
            "owner-pause",
        ]
    )
    if pause.returncode != 0:
        raise GatewayError(
            "EMERGENCY_CONTRACT_PAUSE_FAILED",
            {"exit_code": pause.returncode, "output": _bounded_output(pause.stdout)},
        )
    _require_success(
        [
            "/usr/bin/python3",
            str(paths.deploy_dir / "production_mode.py"),
            "shadow",
            "--env-file",
            str(paths.env_file),
        ],
        "EMERGENCY_SHADOW_RESTORE_FAILED",
    )
    return {
        "schema": "phoenix.release-emergency-pause.v1",
        "status": "paused",
        "release_sha": release_sha,
        "live_executor_stopped": True,
        "shadow_restored": True,
    }


def rollback_release(paths: HostPaths, target_sha: str) -> dict[str, Any]:
    active = status(paths)
    previous = _pointer(paths.deploy_dir / "previous-release")
    if target_sha != previous:
        raise GatewayError(
            "ROLLBACK_TARGET_INVALID",
            {"expected": previous, "actual": target_sha},
        )
    active_root = paths.releases / active["active_release"]
    rollback_script = active_root / "scripts" / "rollback-release.sh"
    context_installer = paths.libexec / "install-production-release-context.sh"
    for path in (rollback_script, context_installer):
        if not path.is_file() or path.is_symlink():
            raise GatewayError("ROLLBACK_RELEASE_INCOMPATIBLE", {"path": str(path)})
    _require_success(
        [
            "/bin/sh",
            str(context_installer),
            target_sha,
            str(paths.releases / target_sha),
        ],
        "ROLLBACK_CONTEXT_INSTALL_FAILED",
        env={"PHOENIX_DEPLOY_ROOT": str(paths.deploy_root)},
    )
    _require_success(
        ["/bin/sh", str(rollback_script)],
        "ROLLBACK_FAILED",
        env={
            "PHOENIX_DEPLOY_ROOT": str(paths.deploy_root),
            "PHOENIX_RELEASE_ROOT": str(paths.releases),
            "PHOENIX_ENV_FILE": str(paths.env_file),
            "PHOENIX_CONTEXT_INSTALLER": str(context_installer),
        },
    )
    final = status(paths)
    if final["active_release"] != target_sha:
        raise GatewayError(
            "ROLLBACK_POINTER_MISMATCH",
            {"expected": target_sha, "actual": final["active_release"]},
        )
    return {
        "schema": "phoenix.release-rollback.v1",
        "status": "rolled-back",
        "release_sha": target_sha,
    }


def mark_release_phase(paths: HostPaths, release_sha: str, phase: str) -> dict[str, Any]:
    state = load_state(state_file(paths, release_sha))
    mutation = True if phase == "EVIDENCE_MODE_INSTALLED" else None
    updates: dict[str, Any] = {}
    if phase == "DISARMED_CONTROL_INSTALLED":
        updates = {
            "contract_paused": True,
            "autonomous_armed": False,
            "kill_switch": True,
        }
    elif phase == "COMPLETED":
        updates = {
            "active_release_pointer": release_sha,
            "candidate_pointer": None,
            "actual_images": dict(state["expected_images"]),
            "contract_paused": True,
            "autonomous_armed": False,
            "kill_switch": True,
        }
    state = advance(
        state,
        phase,
        mutation_started=mutation,
        updates=updates or None,
    )
    _write_state(paths, state)
    return state


def mark_release_failure(
    paths: HostPaths, release_sha: str, code: str, evidence_value: dict[str, Any]
) -> dict[str, Any]:
    state = load_state(state_file(paths, release_sha))
    state = fail_state(state, code=code, evidence=evidence_value)
    _write_state(paths, state)
    return state


def mark_mutation_started(paths: HostPaths, release_sha: str) -> dict[str, Any]:
    state = load_state(state_file(paths, release_sha))
    state = set_mutation_started(state)
    _write_state(paths, state)
    return state


def mark_owner_transaction(
    paths: HostPaths, release_sha: str, transaction_hash: str
) -> dict[str, Any]:
    state = load_state(state_file(paths, release_sha))
    state = record_owner_transaction(state, transaction_hash)
    _write_state(paths, state)
    return state


def mark_engine_baseline(
    paths: HostPaths,
    release_sha: str,
    *,
    container_id: str,
    restart_count: int,
    terminal_integrity: int,
    process_fatal_integrity: int,
) -> dict[str, Any]:
    state = load_state(state_file(paths, release_sha))
    state = advance(state, "ENGINE_BURN_IN_STARTED")
    state = record_engine_baseline(
        state,
        container_id=container_id,
        restart_count=restart_count,
        terminal_integrity=terminal_integrity,
        process_fatal_integrity=process_fatal_integrity,
    )
    _write_state(paths, state)
    return state


def mark_rollback(
    paths: HostPaths, release_sha: str, phase: str, result: dict[str, Any] | None
) -> dict[str, Any]:
    state = load_state(state_file(paths, release_sha))
    state = rollback_phase(state, phase, result)
    if phase == "ROLLED_BACK":
        state.update(
            {
                "active_release_pointer": state["rollback_sha"],
                "candidate_pointer": None,
                "contract_paused": True,
                "autonomous_armed": False,
                "kill_switch": True,
            }
        )
    _write_state(paths, state)
    return state


def json_result(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def json_error(exc: GatewayError, phase: str = "GATEWAY") -> str:
    return json_result(
        {
            "status": "error",
            "phase": phase,
            "code": exc.code,
            "evidence": exc.evidence,
        }
    )
