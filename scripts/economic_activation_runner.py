#!/usr/bin/env python3
"""Consume one bounded Phoenix economic activation request on the Production host."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Sequence

try:
    import fcntl
except ModuleNotFoundError:  # pragma: no cover - the runner is Linux-only.
    fcntl = None  # type: ignore[assignment]


REQUEST_SCHEMA = "phoenix.economic-activation-request.v1"
MATERIALIZATION_SCHEMA = "phoenix.activation-materialization.v1"
READINESS_SCHEMA = "phoenix.canary-readiness.v1"
AUTHORIZATION_SCHEMA = "phoenix.automation-authorization.v1"
MAX_REQUEST_BYTES = 256 * 1024
MAX_MATERIALIZATION_BYTES = 512 * 1024
REQUEST_OWNER_UID = 65532
REQUEST_OWNER_GID = 65532
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
IMAGE_RE = re.compile(r"^(?:[^@\s]+@)?sha256:([0-9a-f]{64})$")
REQUEST_NAME_RE = re.compile(
    r"^activation-request-([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-"
    r"[89ab][0-9a-f]{3}-[0-9a-f]{12})\.json$"
)


class ActivationRunnerError(ValueError):
    """A bounded activation request or host invariant is invalid."""


@dataclass(frozen=True)
class RunnerPaths:
    outbox: Path = Path("/opt/phoenix/evidence/activation-requests")
    state: Path = Path("/var/lib/phoenix-economic-activation")
    authorization: Path = Path("/root/phoenix-authorization")
    deploy: Path = Path("/opt/phoenix/deploy")
    environment: Path = Path("/etc/phoenix/phoenix.env")
    lock: Path = Path("/run/lock/phoenix-economic-activation.lock")
    python: Path = Path("/usr/bin/python3")

    @property
    def processed(self) -> Path:
        return self.state / "processed"

    @property
    def results(self) -> Path:
        return self.state / "results"

    @property
    def consumed(self) -> Path:
        return self.state / "consumed"


def _canonical(value: object) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    ).encode("utf-8")


def _contract_hash(value: dict[str, object], field: str, domain: str, schema: str) -> str:
    if field not in value:
        raise ActivationRunnerError("canonical_hash_field_missing")
    body = dict(value)
    body.pop(field)
    prefix = f"phoenix.canonical-json.v1:{domain}:{schema}\n".encode()
    return hashlib.sha256(prefix + _canonical(body)).hexdigest()


def _timestamp(value: object) -> dt.datetime:
    if not isinstance(value, str) or len(value) > 64:
        raise ActivationRunnerError("timestamp_invalid")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ActivationRunnerError("timestamp_invalid") from exc
    if parsed.tzinfo is None or parsed.utcoffset() != dt.timedelta(0):
        raise ActivationRunnerError("timestamp_invalid")
    return parsed


def _secure_directory(path: Path, uid: int, gid: int, mode: int) -> None:
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_uid != uid
        or metadata.st_gid != gid
        or stat.S_IMODE(metadata.st_mode) != mode
    ):
        raise ActivationRunnerError("directory_metadata_invalid")


def _load_request(path: Path) -> tuple[dict[str, object], bytes]:
    match = REQUEST_NAME_RE.fullmatch(path.name)
    if match is None:
        raise ActivationRunnerError("request_name_invalid")
    before = path.lstat()
    if (
        not stat.S_ISREG(before.st_mode)
        or path.is_symlink()
        or before.st_uid != REQUEST_OWNER_UID
        or before.st_gid != REQUEST_OWNER_GID
        or stat.S_IMODE(before.st_mode) != 0o600
        or before.st_nlink != 1
        or before.st_size <= 0
        or before.st_size > MAX_REQUEST_BYTES
    ):
        raise ActivationRunnerError("request_metadata_invalid")
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        current = os.fstat(descriptor)
        if (current.st_dev, current.st_ino) != (before.st_dev, before.st_ino):
            raise ActivationRunnerError("request_identity_changed")
        raw = b""
        while len(raw) <= MAX_REQUEST_BYTES:
            chunk = os.read(descriptor, min(65536, MAX_REQUEST_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
    finally:
        os.close(descriptor)
    if not raw or len(raw) > MAX_REQUEST_BYTES:
        raise ActivationRunnerError("request_size_invalid")
    try:
        value = json.loads(raw)
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ActivationRunnerError("request_json_invalid") from exc
    expected_keys = {
        "schema_version",
        "request_id",
        "binding",
        "evidence",
        "candidate",
        "created_at",
        "expires_at",
        "request_hash",
    }
    if not isinstance(value, dict) or set(value) != expected_keys:
        raise ActivationRunnerError("request_contract_invalid")
    if value["schema_version"] != REQUEST_SCHEMA:
        raise ActivationRunnerError("request_schema_invalid")
    try:
        request_id = str(uuid.UUID(str(value["request_id"])))
    except (ValueError, AttributeError) as exc:
        raise ActivationRunnerError("request_id_invalid") from exc
    if request_id != match.group(1) or request_id != value["request_id"]:
        raise ActivationRunnerError("request_id_invalid")
    request_hash = value["request_hash"]
    if (
        not isinstance(request_hash, str)
        or not DIGEST_RE.fullmatch(request_hash)
        or request_hash
        != _contract_hash(
            value,
            "request_hash",
            "economic-activation-request",
            REQUEST_SCHEMA,
        )
    ):
        raise ActivationRunnerError("request_hash_invalid")
    created_at = _timestamp(value["created_at"])
    expires_at = _timestamp(value["expires_at"])
    now = dt.datetime.now(dt.timezone.utc)
    if (
        created_at >= expires_at
        or expires_at - created_at > dt.timedelta(seconds=60)
        or now >= expires_at
    ):
        raise ActivationRunnerError("request_expired")
    binding = value["binding"]
    candidate = value["candidate"]
    if not isinstance(binding, dict) or not isinstance(candidate, dict):
        raise ActivationRunnerError("request_binding_invalid")
    release_sha = binding.get("release_sha")
    engine_digest = binding.get("engine_image_digest")
    candidate_hash = candidate.get("candidate_hash")
    fork_result_hash = candidate.get("fork_result_hash")
    if (
        not isinstance(release_sha, str)
        or not SHA_RE.fullmatch(release_sha)
        or not isinstance(engine_digest, str)
        or IMAGE_RE.fullmatch(engine_digest) is None
        or not isinstance(candidate_hash, str)
        or not DIGEST_RE.fullmatch(candidate_hash)
        or not isinstance(fork_result_hash, str)
        or not DIGEST_RE.fullmatch(fork_result_hash)
    ):
        raise ActivationRunnerError("request_binding_invalid")
    return value, raw


def _read_regular(path: Path, maximum: int = 1024 * 1024) -> str:
    metadata = path.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_nlink != 1
        or metadata.st_uid != 0
        or metadata.st_size <= 0
        or metadata.st_size > maximum
        or metadata.st_mode & 0o022
    ):
        raise ActivationRunnerError("host_file_metadata_invalid")
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise ActivationRunnerError("host_file_read_failed") from exc


def _release_environment(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in _read_regular(path).splitlines():
        if not line or line.startswith("#"):
            continue
        name, separator, value = line.partition("=")
        if (
            separator != "="
            or not re.fullmatch(r"[A-Z][A-Z0-9_]{0,127}", name)
            or name in values
            or "\x00" in value
        ):
            raise ActivationRunnerError("release_environment_invalid")
        values[name] = value
    return values


def _validate_active_release(paths: RunnerPaths, request: dict[str, object]) -> str:
    release_sha = str(request["binding"]["release_sha"])  # type: ignore[index]
    active = _read_regular(paths.deploy / "current-release", 128).strip()
    if active != release_sha:
        raise ActivationRunnerError("active_release_mismatch")
    environment = _release_environment(paths.deploy / "current-release.env")
    if environment.get("PHOENIX_RELEASE_SHA") != release_sha:
        raise ActivationRunnerError("active_release_environment_mismatch")
    image = environment.get("PHOENIX_ENGINE_IMAGE", "")
    match = IMAGE_RE.fullmatch(image)
    requested_image = str(request["binding"]["engine_image_digest"])  # type: ignore[index]
    if match is None or f"sha256:{match.group(1)}" != requested_image:
        raise ActivationRunnerError("active_engine_image_mismatch")
    return release_sha


def _fixed_compose_command(paths: RunnerPaths, request: Path) -> list[str]:
    return [
        str(paths.python),
        str(paths.deploy / "production_compose.py"),
        "--mode",
        "LIVE",
        "--env-file",
        str(paths.environment),
        "--release-env",
        str(paths.deploy / "current-release.env"),
        "--compose-file",
        str(paths.deploy / "compose.prod.yml"),
        "--overlay-file",
        str(paths.deploy / "compose.live-autonomous.yml"),
        "--",
        "run",
        "--rm",
        "--no-deps",
        "-v",
        f"{request}:/run/phoenix/economic-activation-request.json:ro",
        "-e",
        "PHOENIX_ACTIVATION_REQUEST_FILE=/run/phoenix/economic-activation-request.json",
        "-e",
        "PHOENIX_ACTIVATION_MATERIALIZATION_ACK=MATERIALIZE_VALIDATED_MIN_CANARY_42161",
        "autonomous-control",
        "materialize-activation-contracts",
    ]


def _validate_materialization(
    raw: bytes, request: dict[str, object]
) -> tuple[dict[str, object], dict[str, object]]:
    if not raw or len(raw) > MAX_MATERIALIZATION_BYTES:
        raise ActivationRunnerError("materialization_size_invalid")
    try:
        value = json.loads(raw)
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ActivationRunnerError("materialization_json_invalid") from exc
    if (
        not isinstance(value, dict)
        or set(value)
        != {
            "schema_version",
            "request_id",
            "request_hash",
            "readiness",
            "authorization",
        }
        or value["schema_version"] != MATERIALIZATION_SCHEMA
        or value["request_id"] != request["request_id"]
        or value["request_hash"] != request["request_hash"]
    ):
        raise ActivationRunnerError("materialization_binding_invalid")
    readiness = value["readiness"]
    authorization = value["authorization"]
    if not isinstance(readiness, dict) or not isinstance(authorization, dict):
        raise ActivationRunnerError("materialization_contract_invalid")
    if (
        readiness.get("schema_version") != READINESS_SCHEMA
        or readiness.get("readiness_hash")
        != _contract_hash(
            readiness,
            "readiness_hash",
            "canary-readiness",
            READINESS_SCHEMA,
        )
        or authorization.get("schema_version") != AUTHORIZATION_SCHEMA
        or authorization.get("authorization_hash")
        != _contract_hash(
            authorization,
            "authorization_hash",
            "automation-authorization",
            AUTHORIZATION_SCHEMA,
        )
    ):
        raise ActivationRunnerError("materialization_hash_invalid")
    binding = readiness.get("binding")
    bounds = authorization.get("authorization")
    if (
        not isinstance(binding, dict)
        or not isinstance(bounds, dict)
        or binding.get("release_sha") != request["binding"]["release_sha"]  # type: ignore[index]
        or binding.get("engine_image_digest")
        != request["binding"]["engine_image_digest"]  # type: ignore[index]
        or binding.get("route_fingerprint")
        != request["binding"]["route_fingerprint"]  # type: ignore[index]
        or bounds.get("route_fingerprint") != binding.get("route_fingerprint")
        or bounds.get("route_policy_hash") != binding.get("route_policy_hash")
        or bounds.get("executor_code_hash") != binding.get("executor_code_hash")
        or bounds.get("maximum_reviewed_input_wei") != 10_000_000_000_000_000
        or bounds.get("one_transaction_at_a_time") is not True
        or bounds.get("reviewed_ladder_only") is not True
        or bounds.get("automatic_disarm_required") is not True
        or _timestamp(binding.get("expires_at")) <= dt.datetime.now(dt.timezone.utc)
        or _timestamp(bounds.get("expires_at")) <= dt.datetime.now(dt.timezone.utc)
    ):
        raise ActivationRunnerError("materialization_bounds_invalid")
    return readiness, authorization


def _atomic_write(path: Path, value: dict[str, object]) -> None:
    encoded = _canonical(value) + b"\n"
    if not encoded or len(encoded) > MAX_REQUEST_BYTES:
        raise ActivationRunnerError("authorization_size_invalid")
    temporary = path.parent / f".{path.name}.{uuid.uuid4()}.tmp"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        os.fchown(descriptor, 0, 0)
        os.fchmod(descriptor, 0o600)
        written = 0
        while written < len(encoded):
            written += os.write(descriptor, encoded[written:])
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
    metadata = path.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_uid != 0
        or metadata.st_gid != 0
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_nlink != 1
    ):
        raise ActivationRunnerError("authorization_metadata_invalid")


def _atomic_result(path: Path, value: dict[str, object]) -> None:
    _atomic_write(path, value)


def _consume_marker(paths: RunnerPaths, request: dict[str, object]) -> Path:
    candidate = request["candidate"]
    replay = hashlib.sha256(
        (
            str(candidate["candidate_hash"])  # type: ignore[index]
            + ":"
            + str(candidate["fork_result_hash"])  # type: ignore[index]
        ).encode()
    ).hexdigest()
    marker = paths.consumed / replay
    try:
        descriptor = os.open(
            marker,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
            0o600,
        )
    except FileExistsError as exc:
        raise ActivationRunnerError("request_replayed") from exc
    try:
        os.fchown(descriptor, 0, 0)
        os.fchmod(descriptor, 0o600)
        os.write(descriptor, f"{request['request_hash']}\n".encode())
        os.fsync(descriptor)
    except Exception:
        os.close(descriptor)
        marker.unlink(missing_ok=True)
        raise
    os.close(descriptor)
    return marker


def _archive_request(paths: RunnerPaths, request_path: Path, digest: str) -> None:
    try:
        metadata = request_path.lstat()
    except FileNotFoundError:
        return
    if stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1 and not request_path.is_symlink():
        destination = paths.processed / f"{digest}.json"
        os.replace(request_path, destination)
        os.chown(destination, 0, 0)
        os.chmod(destination, 0o600)
    else:
        request_path.unlink(missing_ok=True)


def _activation_script(paths: RunnerPaths) -> Path:
    script = paths.deploy / "activate-economic-canary.sh"
    metadata = script.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or script.is_symlink()
        or metadata.st_uid != 0
        or metadata.st_nlink != 1
        or metadata.st_mode & 0o022
    ):
        raise ActivationRunnerError("activation_script_invalid")
    return script


RunCommand = Callable[..., subprocess.CompletedProcess[bytes]]


def run_once(
    paths: RunnerPaths = RunnerPaths(),
    run_command: RunCommand = subprocess.run,
) -> dict[str, object]:
    _secure_directory(paths.outbox, REQUEST_OWNER_UID, REQUEST_OWNER_GID, 0o700)
    for directory in (
        paths.state,
        paths.processed,
        paths.results,
        paths.consumed,
        paths.authorization,
    ):
        _secure_directory(directory, 0, 0, 0o700)
    requests = sorted(paths.outbox.glob("activation-request-*.json"))
    if not requests:
        return {"schema": "phoenix.economic-activation-result.v1", "status": "idle"}
    request_path = requests[0]
    raw_digest = hashlib.sha256(request_path.name.encode()).hexdigest()
    request: dict[str, object] | None = None
    try:
        request, raw = _load_request(request_path)
        raw_digest = hashlib.sha256(raw).hexdigest()
        release_sha = _validate_active_release(paths, request)
        _consume_marker(paths, request)
        prepared = run_command(
            _fixed_compose_command(paths, request_path),
            check=False,
            capture_output=True,
            timeout=60,
            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "PYTHONDONTWRITEBYTECODE": "1"},
        )
        if prepared.returncode != 0:
            raise ActivationRunnerError("authoritative_revalidation_failed")
        readiness, authorization = _validate_materialization(prepared.stdout, request)
        readiness_path = paths.authorization / "canary-readiness.json"
        authorization_path = paths.authorization / "automation-authorization.json"
        _atomic_write(readiness_path, readiness)
        _atomic_write(authorization_path, authorization)
        activation = run_command(
            [
                str(_activation_script(paths)),
                release_sha,
                str(readiness_path),
                str(authorization_path),
            ],
            check=False,
            capture_output=True,
            timeout=600,
            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "PHOENIX_DEPLOY_ROOT": "/opt/phoenix"},
        )
        if activation.returncode != 0:
            raise ActivationRunnerError("official_activation_failed")
        result: dict[str, object] = {
            "schema": "phoenix.economic-activation-result.v1",
            "status": "activated",
            "request_id": request["request_id"],
            "request_hash": request["request_hash"],
            "release_sha": release_sha,
        }
    except (ActivationRunnerError, OSError, subprocess.SubprocessError) as exc:
        result = {
            "schema": "phoenix.economic-activation-result.v1",
            "status": "rejected",
            "code": str(exc) if isinstance(exc, ActivationRunnerError) else "runner_failure",
            "request_id": request.get("request_id") if request else None,
            "request_hash": request.get("request_hash") if request else None,
        }
    _atomic_result(paths.results / f"{raw_digest}.json", result)
    _archive_request(paths, request_path, raw_digest)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    if argv:
        print("ECONOMIC_ACTIVATION_RUNNER_FAILED: arguments_forbidden", file=sys.stderr)
        return 64
    if os.geteuid() != 0 or sys.platform != "linux" or fcntl is None:
        print("ECONOMIC_ACTIVATION_RUNNER_FAILED: root_linux_required", file=sys.stderr)
        return 1
    paths = RunnerPaths()
    paths.lock.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(paths.lock, os.O_RDWR | os.O_CREAT | os.O_CLOEXEC, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        os.close(descriptor)
        return 0
    try:
        result = run_once(paths)
        print(json.dumps(result, separators=(",", ":"), sort_keys=True))
        return 0
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    raise SystemExit(main())
