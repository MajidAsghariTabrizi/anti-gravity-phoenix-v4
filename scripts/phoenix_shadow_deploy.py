#!/usr/bin/env python3
"""Fail-closed root orchestration for ordinary Phoenix SHADOW deployments."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, NoReturn

try:
    import fcntl
    import pwd
except ImportError:  # Importable on Windows for structural unit tests.
    fcntl = None  # type: ignore[assignment]
    pwd = None  # type: ignore[assignment]

if __package__:
    from scripts import release_assets, release_provenance
else:  # Installed direct execution under Python isolated mode.
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import release_assets  # type: ignore[no-redef]
    import release_provenance  # type: ignore[no-redef]


GATEWAY_PATH = Path("/usr/local/sbin/phoenix-shadow-deploy-gateway")
LIBEXEC_DIR = Path("/usr/local/libexec/phoenix-shadow-deploy")
STATE_ROOT = Path("/var/lib/phoenix-shadow-deploy")
LOCK_PATH = Path("/run/lock/phoenix-shadow-deploy.lock")
DEPLOY_ROOT = Path("/opt/phoenix")
ENV_FILE = Path("/etc/phoenix/phoenix.env")
PHOENIX_USER = "phoenix"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
INTEGER_RE = re.compile(r"^[1-9][0-9]{0,19}$")
SCHEMA = "phoenix.shadow-deploy-evidence.v1"
MAX_JSON_BYTES = 2 * 1024 * 1024
MAX_CHECKSUM_BYTES = 16 * 1024
MAX_ARCHIVE_BYTES = 72 * 1024 * 1024
MAX_STAGE_BYTES = 84 * 1024 * 1024
SYSTEMD_TIMEOUT_SECONDS = 2400

TRUSTED_HELPERS = {
    "phoenix_shadow_deploy.py": 0o700,
    "release_assets.py": 0o600,
    "release-components.json": 0o600,
    "release_components.py": 0o600,
    "release_provenance.py": 0o600,
    "install-release-assets.sh": 0o700,
    "install-production-release-context.sh": 0o700,
    "production-healthcheck.sh": 0o700,
    "prelive-protected-maintenance.sh": 0o700,
    "prelive_protected_maintenance.py": 0o600,
    "prelive-protected-maintenance-launch.sh": 0o700,
    "prelive-protected-maintenance-unit.sh": 0o700,
    "rollback-release.sh": 0o700,
}


class GatewayError(RuntimeError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


@dataclass(frozen=True)
class Identity:
    candidate_sha: str
    rollback_sha: str
    candidate_build_run_id: str
    rollback_build_run_id: str
    github_run_id: str
    github_run_attempt: str

    @classmethod
    def parse(cls, values: list[str]) -> "Identity":
        if len(values) != 6:
            raise GatewayError("argument_count_invalid")
        candidate, rollback, candidate_run, rollback_run, run_id, attempt = values
        if not SHA_RE.fullmatch(candidate) or not SHA_RE.fullmatch(rollback):
            raise GatewayError("release_sha_invalid")
        if candidate == rollback:
            raise GatewayError("release_sha_pair_invalid")
        if not all(
            INTEGER_RE.fullmatch(value)
            for value in (candidate_run, rollback_run, run_id, attempt)
        ):
            raise GatewayError("run_identity_invalid")
        return cls(candidate, rollback, candidate_run, rollback_run, run_id, attempt)

    @property
    def stage(self) -> Path:
        return Path(
            f"/tmp/phoenix-shadow-deploy-{self.github_run_id}-"
            f"{self.github_run_attempt}-{self.candidate_sha}"
        )

    @property
    def unit(self) -> str:
        return (
            f"phoenix-shadow-deploy-{self.github_run_id}-"
            f"{self.github_run_attempt}"
        )

    @property
    def state(self) -> Path:
        return STATE_ROOT / self.unit

    @property
    def archive_name(self) -> str:
        return f"phoenix-release-assets-{self.candidate_sha}.tar.gz"

    @property
    def staged_files(self) -> dict[str, int]:
        return {
            "release-manifest.json": MAX_JSON_BYTES,
            "release-provenance.json": MAX_JSON_BYTES,
            "rollback-manifest.json": MAX_JSON_BYTES,
            "rollback-provenance.json": MAX_JSON_BYTES,
            self.archive_name: MAX_ARCHIVE_BYTES,
            "release-assets-manifest.json": MAX_JSON_BYTES,
            "release-assets-checksums.txt": MAX_CHECKSUM_BYTES,
        }

    def as_dict(self) -> dict[str, str]:
        return {
            "candidate_sha": self.candidate_sha,
            "rollback_sha": self.rollback_sha,
            "candidate_build_run_id": self.candidate_build_run_id,
            "rollback_build_run_id": self.rollback_build_run_id,
            "github_run_id": self.github_run_id,
            "github_run_attempt": self.github_run_attempt,
            "unit": f"{self.unit}.service",
        }


def _fail(code: str) -> NoReturn:
    raise GatewayError(code)


def _utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def _mode(value: os.stat_result) -> int:
    return stat.S_IMODE(value.st_mode)


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _read_fd_bounded(descriptor: int, maximum: int) -> bytes:
    chunks: list[bytes] = []
    remaining = maximum + 1
    while remaining > 0:
        chunk = os.read(descriptor, min(1024 * 1024, remaining))
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _read_bounded(path: Path, maximum: int, code: str) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise GatewayError(code) from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_size <= 0
        or metadata.st_size > maximum
    ):
        _fail(code)
    try:
        with path.open("rb") as handle:
            payload = handle.read(maximum + 1)
    except OSError as exc:
        raise GatewayError(code) from exc
    if len(payload) > maximum:
        _fail(code)
    return payload


def _atomic_write(path: Path, payload: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chown(temporary, 0, 0)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _atomic_json(path: Path, value: object) -> None:
    _atomic_write(
        path,
        (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8"),
    )


def _require_root() -> None:
    if os.name != "posix" or fcntl is None or pwd is None or os.geteuid() != 0:
        _fail("root_required")


def _validate_owned_file(
    path: Path, expected_mode: int, expected_uid: int = 0, expected_gid: int = 0
) -> None:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise GatewayError("trusted_installation_invalid") from exc
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != expected_uid
        or metadata.st_gid != expected_gid
        or metadata.st_nlink != 1
        or _mode(metadata) != expected_mode
    ):
        _fail("trusted_installation_invalid")


def verify_installation(
    gateway_path: Path = GATEWAY_PATH,
    libexec_dir: Path = LIBEXEC_DIR,
    *,
    expected_uid: int = 0,
    expected_gid: int = 0,
) -> None:
    try:
        directory = libexec_dir.lstat()
    except OSError as exc:
        raise GatewayError("trusted_installation_invalid") from exc
    if (
        not stat.S_ISDIR(directory.st_mode)
        or directory.st_uid != expected_uid
        or directory.st_gid != expected_gid
        or _mode(directory) != 0o750
    ):
        _fail("trusted_installation_invalid")
    _validate_owned_file(
        gateway_path, 0o755, expected_uid=expected_uid, expected_gid=expected_gid
    )
    for name, expected_mode in TRUSTED_HELPERS.items():
        _validate_owned_file(
            libexec_dir / name,
            expected_mode,
            expected_uid=expected_uid,
            expected_gid=expected_gid,
        )


def _safe_root_directory(path: Path, mode: int = 0o700) -> None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        try:
            path.mkdir(mode=mode)
            os.chown(path, 0, 0)
            os.chmod(path, mode)
            metadata = path.lstat()
        except OSError as exc:
            raise GatewayError("state_root_invalid") from exc
    except OSError as exc:
        raise GatewayError("state_root_invalid") from exc
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != 0
        or metadata.st_gid != 0
        or _mode(metadata) != mode
    ):
        _fail("state_root_invalid")


def _identity_matches(path: Path, identity: Identity) -> bool:
    try:
        value = json.loads(_read_bounded(path, MAX_JSON_BYTES, "state_identity_invalid"))
    except (json.JSONDecodeError, UnicodeDecodeError):
        return False
    return value == identity.as_dict()


def _write_evidence(
    identity: Identity,
    *,
    phase: str,
    result: str,
    error_code: str = "",
    rollback_result: str = "not_required",
    input_hashes: dict[str, str] | None = None,
) -> None:
    existing_hashes: dict[str, str] = {}
    evidence_path = identity.state / "evidence.json"
    if input_hashes is None and evidence_path.exists():
        try:
            current = json.loads(
                _read_bounded(evidence_path, MAX_JSON_BYTES, "evidence_invalid")
            )
            if isinstance(current.get("input_sha256"), dict):
                existing_hashes = current["input_sha256"]
        except (GatewayError, json.JSONDecodeError, UnicodeDecodeError):
            existing_hashes = {}
    value = {
        "schema": SCHEMA,
        **identity.as_dict(),
        "phase": phase,
        "result": result,
        "error_code": error_code,
        "rollback_result": rollback_result,
        "input_sha256": input_hashes if input_hashes is not None else existing_hashes,
        "updated_at": _utc_now(),
    }
    _atomic_json(evidence_path, value)


def _deployment_lock():
    LOCK_PATH.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    descriptor = os.open(
        LOCK_PATH,
        os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != 0
        or metadata.st_gid != 0
        or metadata.st_nlink != 1
        or _mode(metadata) != 0o600
    ):
        os.close(descriptor)
        _fail("deployment_lock_invalid")
    os.fchown(descriptor, 0, 0)
    os.fchmod(descriptor, 0o600)
    return os.fdopen(descriptor, "r+")


def inspect_stage(
    identity: Identity,
    *,
    phoenix_uid: int,
    phoenix_gid: int,
    stage: Path | None = None,
) -> None:
    stage = stage or identity.stage
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        stage_fd = os.open(stage, flags)
    except OSError as exc:
        raise GatewayError("stage_directory_invalid") from exc
    try:
        stage_metadata = os.fstat(stage_fd)
        if (
            not stat.S_ISDIR(stage_metadata.st_mode)
            or stage_metadata.st_uid != phoenix_uid
            or stage_metadata.st_gid != phoenix_gid
            or _mode(stage_metadata) != 0o700
        ):
            _fail("stage_directory_invalid")
        observed = set(os.listdir(stage_fd))
        expected = set(identity.staged_files)
        if observed != expected:
            _fail("stage_member_set_invalid")

        total = 0
        for name, maximum in identity.staged_files.items():
            try:
                metadata = os.stat(name, dir_fd=stage_fd, follow_symlinks=False)
            except OSError as exc:
                raise GatewayError("stage_file_invalid") from exc
            if (
                not stat.S_ISREG(metadata.st_mode)
                or metadata.st_uid != phoenix_uid
                or metadata.st_gid != phoenix_gid
                or metadata.st_nlink != 1
                or _mode(metadata) != 0o600
                or metadata.st_size <= 0
                or metadata.st_size > maximum
            ):
                _fail("stage_file_invalid")
            total += metadata.st_size
            if total > MAX_STAGE_BYTES:
                _fail("stage_size_invalid")
    finally:
        os.close(stage_fd)


def _read_stable_stage_payload(
    stage_fd: int, name: str, descriptor: int, maximum: int
) -> bytes:
    first_metadata = os.fstat(descriptor)
    os.lseek(descriptor, 0, os.SEEK_SET)
    payload = _read_fd_bounded(descriptor, maximum)
    if (
        not payload
        or len(payload) > maximum
        or len(payload) != first_metadata.st_size
    ):
        _fail("stage_file_changed_during_lock")

    reopened = os.open(
        name,
        os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0),
        dir_fd=stage_fd,
    )
    try:
        second_metadata = os.fstat(reopened)
        second = _read_fd_bounded(reopened, maximum)
    finally:
        os.close(reopened)
    if (
        not stat.S_ISREG(second_metadata.st_mode)
        or second_metadata.st_nlink != 1
        or second_metadata.st_dev != first_metadata.st_dev
        or second_metadata.st_ino != first_metadata.st_ino
        or second_metadata.st_size != first_metadata.st_size
        or len(second) != second_metadata.st_size
        or payload != second
    ):
        _fail("stage_file_changed_during_lock")
    return payload


def lock_stage(
    identity: Identity,
    *,
    phoenix_uid: int,
    phoenix_gid: int,
    state_root: Path = STATE_ROOT,
) -> dict[str, str]:
    stage = identity.stage
    inspect_stage(
        identity,
        phoenix_uid=phoenix_uid,
        phoenix_gid=phoenix_gid,
        stage=stage,
    )
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        stage_fd = os.open(stage, flags)
    except OSError as exc:
        raise GatewayError("stage_directory_invalid") from exc
    open_files: dict[str, int] = {}
    try:
        stage_metadata = os.fstat(stage_fd)
        if (
            not stat.S_ISDIR(stage_metadata.st_mode)
            or stage_metadata.st_uid != phoenix_uid
            or stage_metadata.st_gid != phoenix_gid
            or _mode(stage_metadata) != 0o700
            or set(os.listdir(stage_fd)) != set(identity.staged_files)
        ):
            _fail("stage_directory_changed_before_lock")
        # Lock the directory first; the SSH account can no longer replace entries.
        os.fchown(stage_fd, 0, 0)
        os.fchmod(stage_fd, 0o700)
        total = 0
        for name, maximum in identity.staged_files.items():
            try:
                before = os.stat(name, dir_fd=stage_fd, follow_symlinks=False)
                if not stat.S_ISREG(before.st_mode):
                    _fail("stage_file_invalid")
                descriptor = os.open(
                    name,
                    os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0),
                    dir_fd=stage_fd,
                )
            except OSError as exc:
                raise GatewayError("stage_file_invalid") from exc
            opened = os.fstat(descriptor)
            if (
                before.st_dev != opened.st_dev
                or before.st_ino != opened.st_ino
                or not stat.S_ISREG(opened.st_mode)
                or opened.st_uid != phoenix_uid
                or opened.st_gid != phoenix_gid
                or opened.st_nlink != 1
                or _mode(opened) != 0o600
                or opened.st_size <= 0
                or opened.st_size > maximum
            ):
                os.close(descriptor)
                _fail("stage_file_invalid")
            total += opened.st_size
            if total > MAX_STAGE_BYTES:
                os.close(descriptor)
                _fail("stage_size_invalid")
            os.fchown(descriptor, 0, 0)
            os.fchmod(descriptor, 0o600)
            open_files[name] = descriptor

        _safe_root_directory(state_root)
        state = state_root / identity.unit
        if state.exists():
            _fail("deployment_identity_already_used")
        state.mkdir(mode=0o700)
        os.chown(state, 0, 0)
        inputs = state / "inputs"
        inputs.mkdir(mode=0o700)
        os.chown(inputs, 0, 0)

        input_hashes: dict[str, str] = {}
        for name, descriptor in open_files.items():
            maximum = identity.staged_files[name]
            payload = _read_stable_stage_payload(
                stage_fd, name, descriptor, maximum
            )
            _atomic_write(inputs / name, payload)
            input_hashes[name] = _sha256(payload)

        _atomic_json(state / "identity.json", identity.as_dict())
        _atomic_json(state / "input-lock.json", {"sha256": input_hashes})
        _write_evidence(
            identity,
            phase="inputs_locked",
            result="validating",
            input_hashes=input_hashes,
        )
        return input_hashes
    finally:
        for descriptor in open_files.values():
            os.close(descriptor)
        os.close(stage_fd)


def validate_locked_inputs(identity: Identity) -> dict[str, str]:
    if not _identity_matches(identity.state / "identity.json", identity):
        _fail("state_identity_invalid")
    try:
        lock_value = json.loads(
            _read_bounded(identity.state / "input-lock.json", MAX_JSON_BYTES, "input_lock_invalid")
        )
        expected_hashes = lock_value["sha256"]
    except (json.JSONDecodeError, UnicodeDecodeError, KeyError, TypeError) as exc:
        raise GatewayError("input_lock_invalid") from exc
    if not isinstance(expected_hashes, dict) or set(expected_hashes) != set(
        identity.staged_files
    ):
        _fail("input_lock_invalid")
    observed: dict[str, str] = {}
    inputs = identity.state / "inputs"
    if set(path.name for path in inputs.iterdir()) != set(identity.staged_files):
        _fail("locked_input_set_invalid")
    for name, maximum in identity.staged_files.items():
        payload = _read_bounded(inputs / name, maximum, "locked_input_invalid")
        observed[name] = _sha256(payload)
    if observed != expected_hashes:
        _fail("locked_input_hash_invalid")
    return observed


def validate_release_inputs(identity: Identity, inputs: Path | None = None) -> None:
    inputs = inputs or identity.state / "inputs"
    try:
        release_provenance.validate_deploy_pair(
            inputs / "release-manifest.json",
            inputs / "release-provenance.json",
            identity.candidate_sha,
            identity.candidate_build_run_id,
            inputs / "rollback-manifest.json",
            inputs / "rollback-provenance.json",
            identity.rollback_sha,
            identity.rollback_build_run_id,
        )
    except (OSError, ValueError) as exc:
        raise GatewayError("release_pair_invalid") from exc
    try:
        release_assets.verify_release_assets(
            inputs / identity.archive_name,
            inputs / "release-assets-manifest.json",
            inputs / "release-assets-checksums.txt",
            identity.candidate_sha,
        )
    except (OSError, ValueError) as exc:
        raise GatewayError("release_assets_invalid") from exc


def _parse_env_contract(path: Path) -> dict[str, str]:
    payload = _read_bounded(path, MAX_JSON_BYTES, "production_env_invalid")
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise GatewayError("production_env_invalid") from exc
    values: dict[str, str] = {}
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:].lstrip()
        if "=" not in line:
            _fail("production_env_invalid")
        name, value = line.split("=", 1)
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) or name in values:
            _fail("production_env_invalid")
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values[name] = value
    return values


def validate_shadow_controls(env_file: Path = ENV_FILE) -> None:
    metadata = env_file.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != 0
        or metadata.st_gid != 0
        or metadata.st_nlink != 1
        or _mode(metadata) != 0o600
    ):
        _fail("production_env_permissions_invalid")
    values = _parse_env_contract(env_file)
    validate_shadow_values(values)


def validate_shadow_values(values: dict[str, str]) -> None:
    required = {
        "PHOENIX_ENV": "production",
        "PHOENIX_MODE": "SHADOW",
        "LIVE_EXECUTION": "false",
        "CHAIN_ID": "42161",
        "LIVE_EXECUTOR_ARMED": "false",
        "LIVE_EXECUTOR_KILL_SWITCH": "true",
    }
    if any(values.get(name) != expected for name, expected in required.items()):
        _fail("shadow_controls_invalid")
    blank = (
        "SIGNER_PRIVATE_KEY",
        "SIGNER_PRIVATE_KEY_FILE",
        "LIVE_EXECUTOR_SIGNER_FILE",
        "WALLET_ADDRESS",
        "EXECUTOR_ADDRESS",
        "PUBLIC_TRANSACTION_SUBMISSION",
        "PRIVATE_RELAY_SUBMISSION",
        "TRANSACTION_BROADCAST_URL",
    )
    if any(values.get(name, "") for name in blank):
        _fail("shadow_controls_invalid")


def _read_pointer(path: Path, code: str) -> str:
    payload = _read_bounded(path, 128, code)
    try:
        value = payload.decode("ascii").strip()
    except UnicodeDecodeError as exc:
        raise GatewayError(code) from exc
    if not SHA_RE.fullmatch(value):
        _fail(code)
    return value


def validate_control_context(
    deploy_dir: Path, *, phoenix_user: str = PHOENIX_USER
) -> None:
    if pwd is None:
        _fail("deploy_context_permissions_invalid")
    try:
        account = pwd.getpwnam(phoenix_user)
    except KeyError as exc:
        raise GatewayError("deploy_context_permissions_invalid") from exc

    directories = {
        deploy_dir: (0, account.pw_gid, 0o750),
        deploy_dir / "manifests": (0, account.pw_gid, 0o750),
        deploy_dir / ".deploy-runtime": (0, 0, 0o700),
    }
    for path, expected in directories.items():
        try:
            metadata = path.lstat()
        except OSError as exc:
            raise GatewayError("deploy_context_permissions_invalid") from exc
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or (metadata.st_uid, metadata.st_gid, _mode(metadata)) != expected
        ):
            _fail("deploy_context_permissions_invalid")

    executable_names = (
        "deploy-release.sh",
        "install-production-release-context.sh",
        "install-release-assets.sh",
        "production-healthcheck.sh",
        "production_context.py",
        "release_components.py",
        "release_assets.py",
        "render-production-compose.sh",
        "rollback-release.sh",
        "validate-production-env.sh",
        "validate-production-release-context.sh",
    )
    for name in executable_names:
        path = deploy_dir / name
        try:
            metadata = path.lstat()
        except OSError as exc:
            raise GatewayError("deploy_context_permissions_invalid") from exc
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != 0
            or metadata.st_gid != account.pw_gid
            or metadata.st_nlink != 1
            or _mode(metadata) != 0o750
        ):
            _fail("deploy_context_permissions_invalid")
    for name in (
        "compose.prod.yml",
        "current-release",
        "release-assets.sha",
        "release-components.json",
    ):
        path = deploy_dir / name
        try:
            metadata = path.lstat()
        except OSError as exc:
            raise GatewayError("deploy_context_permissions_invalid") from exc
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != 0
            or metadata.st_gid != account.pw_gid
            or metadata.st_nlink != 1
            or _mode(metadata) != 0o640
        ):
            _fail("deploy_context_permissions_invalid")


def validate_immutable_tree(root: Path, release_sha: str) -> None:
    try:
        root_metadata = root.lstat()
    except OSError as exc:
        raise GatewayError("rollback_tree_invalid") from exc
    if (
        not stat.S_ISDIR(root_metadata.st_mode)
        or root_metadata.st_uid != 0
        or root_metadata.st_gid != 0
        or _mode(root_metadata) & 0o022
    ):
        _fail("rollback_tree_invalid")
    for directory, names, files in os.walk(root, followlinks=False):
        directory_path = Path(directory)
        metadata = directory_path.lstat()
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != 0
            or metadata.st_gid != 0
            or _mode(metadata) & 0o022
        ):
            _fail("rollback_tree_invalid")
        for name in names + files:
            path = directory_path / name
            item = path.lstat()
            if stat.S_ISLNK(item.st_mode):
                _fail("rollback_tree_invalid")
            if name in files and (
                not stat.S_ISREG(item.st_mode)
                or item.st_uid != 0
                or item.st_gid != 0
                or item.st_nlink != 1
                or _mode(item) & 0o022
            ):
                _fail("rollback_tree_invalid")
    manifest = root / "release-assets-manifest.json"
    try:
        release_assets.verify_release_tree(root, manifest, release_sha)
    except (OSError, ValueError) as exc:
        raise GatewayError("rollback_tree_invalid") from exc


def _service_is_running(service: str) -> bool:
    try:
        completed = subprocess.run(
            [
                "/usr/bin/docker",
                "ps",
                "--quiet",
                "--filter",
                f"label=com.docker.compose.service={service}",
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise GatewayError("docker_state_unavailable") from exc
    return bool(completed.stdout.strip())


def validate_host(
    identity: Identity,
    *,
    deploy_root: Path = DEPLOY_ROOT,
    env_file: Path = ENV_FILE,
    service_probe: Callable[[str], bool] = _service_is_running,
    enforce_control_permissions: bool = True,
) -> None:
    deploy_dir = deploy_root / "deploy"
    if enforce_control_permissions:
        validate_control_context(deploy_dir)
    if _read_pointer(deploy_dir / "current-release", "current_release_invalid") != identity.rollback_sha:
        _fail("current_release_mismatch")
    if _read_pointer(deploy_dir / "release-assets.sha", "release_assets_pointer_invalid") != identity.rollback_sha:
        _fail("release_assets_pointer_mismatch")
    validate_shadow_controls(env_file)
    if service_probe("live-executor"):
        _fail("live_executor_active")
    if service_probe("migration-runner"):
        _fail("migration_runner_active")
    validate_immutable_tree(deploy_root / "releases" / identity.rollback_sha, identity.rollback_sha)


def harden_context(
    deploy_root: Path = DEPLOY_ROOT,
    *,
    phoenix_user: str = PHOENIX_USER,
) -> None:
    if pwd is None:
        _fail("deploy_context_invalid")
    deploy_dir = deploy_root / "deploy"
    try:
        account = pwd.getpwnam(phoenix_user)
        root_metadata = deploy_dir.lstat()
    except (KeyError, OSError) as exc:
        raise GatewayError("deploy_context_invalid") from exc
    if not stat.S_ISDIR(root_metadata.st_mode) or stat.S_ISLNK(root_metadata.st_mode):
        _fail("deploy_context_invalid")
    runtime = deploy_dir / ".runtime"
    root_runtime = deploy_dir / ".deploy-runtime"
    if runtime.exists():
        metadata = runtime.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            _fail("deploy_context_invalid")
        os.chown(runtime, account.pw_uid, account.pw_gid)
        os.chmod(runtime, 0o750)
    else:
        runtime.mkdir(mode=0o750)
        os.chown(runtime, account.pw_uid, account.pw_gid)
    if root_runtime.exists():
        metadata = root_runtime.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            _fail("deploy_context_invalid")
    else:
        root_runtime.mkdir(mode=0o700)
    os.chown(root_runtime, 0, 0)
    os.chmod(root_runtime, 0o700)

    for directory, names, files in os.walk(deploy_dir, topdown=True, followlinks=False):
        directory_path = Path(directory)
        for name in names:
            path = directory_path / name
            item = path.lstat()
            if not stat.S_ISDIR(item.st_mode) or stat.S_ISLNK(item.st_mode):
                _fail("deploy_context_invalid")
        names[:] = [name for name in names if directory_path / name not in (runtime, root_runtime)]
        metadata = directory_path.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            _fail("deploy_context_invalid")
        os.chown(directory_path, 0, account.pw_gid)
        os.chmod(directory_path, 0o750)
        for name in files:
            path = directory_path / name
            item = path.lstat()
            if not stat.S_ISREG(item.st_mode) or item.st_nlink != 1:
                _fail("deploy_context_invalid")
            mode = 0o750 if (_mode(item) & 0o111 or path.suffix in (".sh", ".py")) else 0o640
            if path.name == "nats-server.conf" or path.as_posix().endswith(
                "/prometheus/prometheus.yml"
            ):
                mode = 0o644
            os.chown(path, 0, account.pw_gid)
            os.chmod(path, mode)


def _atomic_install_control_file(source: Path, target: Path, gid: int) -> None:
    payload = _read_bounded(source, MAX_JSON_BYTES, "control_evidence_invalid")
    _atomic_write(target, payload, 0o640)
    os.chown(target, 0, gid)


def _run_checked(command: list[str], *, environment: dict[str, str] | None = None) -> None:
    try:
        subprocess.run(command, check=True, env=environment)
    except (OSError, subprocess.CalledProcessError) as exc:
        raise GatewayError("deployment_command_failed") from exc


def _restore_rollback(identity: Identity, environment: dict[str, str]) -> str:
    try:
        _run_checked(["/bin/sh", str(LIBEXEC_DIR / "rollback-release.sh")], environment=environment)
        if (
            _read_pointer(DEPLOY_ROOT / "deploy/current-release", "rollback_pointer_invalid")
            != identity.rollback_sha
            or _read_pointer(DEPLOY_ROOT / "deploy/release-assets.sha", "rollback_pointer_invalid")
            != identity.rollback_sha
        ):
            return "failed"
        validate_shadow_controls(ENV_FILE)
        return "succeeded"
    except Exception:
        return "failed"


def worker(identity: Identity) -> int:
    if fcntl is None or pwd is None:
        _fail("platform_invalid")
    rollback_result = "not_required"
    installation_started = False
    with _deployment_lock() as lock_handle:
        fcntl.flock(lock_handle.fileno(), fcntl.LOCK_EX)
        try:
            verify_installation()
            input_hashes = validate_locked_inputs(identity)
            validate_release_inputs(identity)
            validate_host(identity)
            _write_evidence(
                identity,
                phase="installing_candidate",
                result="running",
                input_hashes=input_hashes,
            )
            account = pwd.getpwnam(PHOENIX_USER)
            deploy_dir = DEPLOY_ROOT / "deploy"
            manifests = deploy_dir / "manifests"
            manifests.mkdir(mode=0o750, exist_ok=True)
            os.chown(manifests, 0, account.pw_gid)
            os.chmod(manifests, 0o750)
            inputs = identity.state / "inputs"
            _atomic_install_control_file(
                inputs / "release-manifest.json",
                manifests / f"{identity.candidate_sha}.json",
                account.pw_gid,
            )
            _atomic_install_control_file(
                inputs / "release-provenance.json",
                manifests / f"{identity.candidate_sha}.provenance.json",
                account.pw_gid,
            )
            _atomic_install_control_file(
                inputs / "rollback-manifest.json",
                manifests / f"{identity.rollback_sha}.json",
                account.pw_gid,
            )
            _atomic_install_control_file(
                inputs / "rollback-provenance.json",
                manifests / f"{identity.rollback_sha}.provenance.json",
                account.pw_gid,
            )
            _atomic_write(deploy_dir / "previous-release", f"{identity.rollback_sha}\n".encode("ascii"), 0o640)
            os.chown(deploy_dir / "previous-release", 0, account.pw_gid)

            environment = {
                "PATH": "/usr/sbin:/usr/bin:/sbin:/bin",
                "HOME": "/root",
                "LANG": "C",
                "LC_ALL": "C",
                "PYTHONDONTWRITEBYTECODE": "1",
                "PHOENIX_CONTEXT_INSTALLER": str(
                    LIBEXEC_DIR / "install-production-release-context.sh"
                ),
                "PHOENIX_ROLLBACK_SCRIPT": str(LIBEXEC_DIR / "rollback-release.sh"),
                "PHOENIX_DEPLOY_RUNTIME_DIR": str(identity.state / "deploy-runtime"),
            }
            runtime = identity.state / "deploy-runtime"
            runtime.mkdir(mode=0o700, exist_ok=True)
            os.chown(runtime, 0, 0)
            os.chmod(runtime, 0o700)
            installation_started = True
            _run_checked(
                [
                    "/bin/sh",
                    str(LIBEXEC_DIR / "install-release-assets.sh"),
                    identity.candidate_sha,
                    str(inputs / identity.archive_name),
                    str(inputs / "release-assets-manifest.json"),
                    str(inputs / "release-assets-checksums.txt"),
                ],
                environment=environment,
            )
            harden_context()
            candidate_tree = DEPLOY_ROOT / "releases" / identity.candidate_sha
            validate_immutable_tree(candidate_tree, identity.candidate_sha)
            deploy_script = candidate_tree / "scripts/deploy-release.sh"
            _validate_owned_file(deploy_script, 0o755)
            _write_evidence(identity, phase="deploying_candidate", result="running")
            _run_checked(
                ["/bin/sh", str(deploy_script), identity.candidate_sha],
                environment=environment,
            )
            harden_context()
            if (
                _read_pointer(deploy_dir / "current-release", "candidate_pointer_invalid")
                != identity.candidate_sha
                or _read_pointer(deploy_dir / "release-assets.sha", "candidate_pointer_invalid")
                != identity.candidate_sha
            ):
                _fail("candidate_pointer_invalid")
            validate_shadow_controls(ENV_FILE)
            if _service_is_running("live-executor") or _service_is_running(
                "migration-runner"
            ):
                _fail("unsafe_service_active_after_deploy")
            _write_evidence(identity, phase="complete", result="success")
            return 0
        except Exception as exc:
            error_code = exc.code if isinstance(exc, GatewayError) else "internal_error"
            if installation_started:
                rollback_environment = {
                    "PATH": "/usr/sbin:/usr/bin:/sbin:/bin",
                    "HOME": "/root",
                    "LANG": "C",
                    "LC_ALL": "C",
                    "PYTHONDONTWRITEBYTECODE": "1",
                    "PHOENIX_CONTEXT_INSTALLER": str(
                        LIBEXEC_DIR / "install-production-release-context.sh"
                    ),
                    "PHOENIX_DEPLOY_RUNTIME_DIR": str(
                        identity.state / "deploy-runtime"
                    ),
                }
                rollback_result = _restore_rollback(identity, rollback_environment)
            _write_evidence(
                identity,
                phase="failed",
                result="failure",
                error_code=error_code,
                rollback_result=rollback_result,
            )
            print(
                f"PHOENIX_SHADOW_DEPLOY_WORKER_ERROR: {error_code}",
                file=sys.stderr,
            )
            return 1


def _systemd_unit_exists(unit: str) -> bool:
    completed = subprocess.run(
        [
            "/usr/bin/systemctl",
            "show",
            f"{unit}.service",
            "--property=LoadState",
            "--value",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    return completed.stdout.strip() not in ("", "not-found")


def _systemd_unit_state(unit: str) -> dict[str, str]:
    try:
        completed = subprocess.run(
            [
                "/usr/bin/systemctl",
                "show",
                f"{unit}.service",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=SubState",
                "--property=Result",
            ],
            capture_output=True,
            text=True,
            check=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        raise GatewayError("systemd_status_unavailable") from exc
    values: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        if "=" in line:
            name, value = line.split("=", 1)
            values[name] = value
    return values


def _another_deployment_is_active(identity: Identity) -> bool:
    if not STATE_ROOT.exists():
        return False
    for candidate in STATE_ROOT.iterdir():
        if candidate == identity.state:
            continue
        if not candidate.is_dir() or candidate.is_symlink():
            _fail("state_root_invalid")
        if not re.fullmatch(r"phoenix-shadow-deploy-[1-9][0-9]{0,19}-[1-9][0-9]{0,19}", candidate.name):
            _fail("state_root_invalid")
        evidence_path = candidate / "evidence.json"
        try:
            value = json.loads(
                _read_bounded(evidence_path, MAX_JSON_BYTES, "state_root_invalid")
            )
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            raise GatewayError("state_root_invalid") from exc
        if value.get("result") in ("running", "validating") and _systemd_unit_exists(
            candidate.name
        ):
            return True
    return False


def start(identity: Identity) -> None:
    if fcntl is None or pwd is None:
        _fail("platform_invalid")
    verify_installation()
    try:
        account = pwd.getpwnam(PHOENIX_USER)
    except KeyError as exc:
        raise GatewayError("phoenix_account_invalid") from exc
    with _deployment_lock() as lock_handle:
        fcntl.flock(lock_handle.fileno(), fcntl.LOCK_EX)
        _safe_root_directory(STATE_ROOT)
        if _another_deployment_is_active(identity):
            _fail("deployment_already_active")
        if identity.state.exists():
            if _identity_matches(identity.state / "identity.json", identity) and _systemd_unit_exists(
                identity.unit
            ):
                print(f"PHOENIX_SHADOW_DEPLOY_STARTED: {identity.unit}.service")
                return
            _fail("deployment_identity_already_used")
        input_hashes = lock_stage(
            identity,
            phoenix_uid=account.pw_uid,
            phoenix_gid=account.pw_gid,
        )
        try:
            validate_locked_inputs(identity)
            validate_release_inputs(identity)
            validate_host(identity)
        except GatewayError as exc:
            _write_evidence(
                identity,
                phase="preflight_failed",
                result="failure",
                error_code=exc.code,
                input_hashes=input_hashes,
            )
            raise
        command = [
            "/usr/bin/systemd-run",
            "--no-block",
            f"--unit={identity.unit}",
            f"--description=Phoenix SHADOW deployment {identity.github_run_id}/{identity.github_run_attempt}",
            "--property=Type=oneshot",
            "--property=RemainAfterExit=yes",
            f"--property=TimeoutStartSec={SYSTEMD_TIMEOUT_SECONDS}",
            "--property=TimeoutStopSec=300",
            "--property=KillMode=control-group",
            "--property=UMask=0077",
            "--property=StandardOutput=journal",
            "--property=StandardError=journal",
            "--quiet",
            "/usr/bin/python3",
            "-I",
            "-B",
            str(LIBEXEC_DIR / "phoenix_shadow_deploy.py"),
            "worker",
            identity.candidate_sha,
            identity.rollback_sha,
            identity.candidate_build_run_id,
            identity.rollback_build_run_id,
            identity.github_run_id,
            identity.github_run_attempt,
        ]
        try:
            subprocess.run(command, check=True)
        except (OSError, subprocess.CalledProcessError) as exc:
            _write_evidence(
                identity,
                phase="launch_failed",
                result="failure",
                error_code="systemd_launch_failed",
            )
            raise GatewayError("systemd_launch_failed") from exc
        _write_evidence(identity, phase="worker_started", result="running")
    print(f"PHOENIX_SHADOW_DEPLOY_STARTED: {identity.unit}.service")


def status(identity: Identity) -> int:
    if not _identity_matches(identity.state / "identity.json", identity):
        _fail("state_identity_invalid")
    try:
        evidence = json.loads(
            _read_bounded(identity.state / "evidence.json", MAX_JSON_BYTES, "evidence_invalid")
        )
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise GatewayError("evidence_invalid") from exc
    result = evidence.get("result")
    if result == "success":
        print("PHOENIX_SHADOW_DEPLOY_STATUS: success")
        return 0
    if result == "failure":
        code = evidence.get("error_code", "deployment_failed")
        if not isinstance(code, str) or not re.fullmatch(r"[a-z0-9_]{1,64}", code):
            code = "deployment_failed"
        print(f"PHOENIX_SHADOW_DEPLOY_STATUS: failure {code}")
        return 1
    unit_state = _systemd_unit_state(identity.unit)
    if unit_state.get("LoadState") == "not-found" or unit_state.get(
        "ActiveState"
    ) in ("failed", "inactive"):
        _write_evidence(
            identity,
            phase="failed",
            result="failure",
            error_code="worker_terminated_without_evidence",
        )
        print(
            "PHOENIX_SHADOW_DEPLOY_STATUS: failure "
            "worker_terminated_without_evidence"
        )
        return 1
    print("PHOENIX_SHADOW_DEPLOY_STATUS: pending")
    return 3


def evidence(identity: Identity) -> None:
    if not _identity_matches(identity.state / "identity.json", identity):
        _fail("state_identity_invalid")
    payload = _read_bounded(
        identity.state / "evidence.json", MAX_JSON_BYTES, "evidence_invalid"
    )
    try:
        value = json.loads(payload)
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise GatewayError("evidence_invalid") from exc
    if value.get("schema") != SCHEMA or any(
        value.get(name) != expected for name, expected in identity.as_dict().items()
    ):
        _fail("evidence_invalid")
    print(json.dumps(value, indent=2, sort_keys=True))


def cleanup(identity: Identity) -> None:
    if status(identity) != 0:
        _fail("cleanup_requires_success")
    stage = identity.stage
    metadata = stage.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != 0
        or metadata.st_gid != 0
        or _mode(metadata) != 0o700
    ):
        _fail("stage_cleanup_invalid")
    observed = set(path.name for path in stage.iterdir())
    if observed != set(identity.staged_files):
        _fail("stage_cleanup_invalid")
    for name in identity.staged_files:
        path = stage / name
        item = path.lstat()
        if (
            not stat.S_ISREG(item.st_mode)
            or item.st_uid != 0
            or item.st_gid != 0
            or item.st_nlink != 1
            or _mode(item) != 0o600
        ):
            _fail("stage_cleanup_invalid")
        path.unlink()
    stage.rmdir()
    print("PHOENIX_SHADOW_DEPLOY_CLEANUP: success")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    gateway = subparsers.add_parser("gateway")
    gateway.add_argument("action", choices=("start", "status", "evidence", "cleanup"))
    gateway.add_argument("identity", nargs="*")
    worker_parser = subparsers.add_parser("worker")
    worker_parser.add_argument("identity", nargs="*")
    subparsers.add_parser("harden-context")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        _require_root()
        if args.command == "harden-context":
            harden_context()
            validate_shadow_controls()
            return 0
        identity = Identity.parse(args.identity)
        if args.command == "worker":
            return worker(identity)
        verify_installation()
        if args.action == "start":
            start(identity)
            return 0
        if args.action == "status":
            return status(identity)
        if args.action == "evidence":
            evidence(identity)
            return 0
        cleanup(identity)
        return 0
    except GatewayError as exc:
        print(f"PHOENIX_SHADOW_DEPLOY_GATEWAY_ERROR: {exc.code}", file=sys.stderr)
        return 1
    except Exception:
        print("PHOENIX_SHADOW_DEPLOY_GATEWAY_ERROR: internal_error", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
