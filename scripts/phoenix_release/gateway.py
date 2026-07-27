"""Root-side bounded release receiver and resumable gateway."""

from __future__ import annotations

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
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Iterable

from .model import (
    PHASES,
    PROTOCOL_VERSION,
    SHA_RE,
    StateError,
    advance,
    atomic_write,
    fail_state,
    load_state,
    new_state,
    rollback_phase,
    record_owner_transaction,
    record_engine_baseline,
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
                return request
            raise GatewayError("PACKAGE_ALREADY_EXISTS")
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
        stderr=subprocess.STDOUT if capture else None,
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
                "command": Path(arguments[0]).name,
                "exit_code": result.returncode,
                "output": _bounded_output(result.stdout),
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
    compose_arguments = [
        "/usr/bin/docker",
        "compose",
        "--env-file",
        str(paths.env_file),
        "--env-file",
        str(release_env),
        "-f",
        str(paths.deploy_dir / "compose.prod.yml"),
    ]
    if active["phoenix_mode"] == "LIVE":
        compose_arguments.extend(
            ["-f", str(paths.deploy_dir / "compose.live-autonomous.yml")]
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
            or not isinstance(image_value, dict)
            or not isinstance(production_services, list)
        ):
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
    if not isinstance(images, dict) or len(images) != 7:
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
        "PHOENIX_CONTEXT_INSTALLER": str(
            paths.libexec / "install-production-release-context.sh"
        ),
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
    manifest_dir = paths.deploy_dir / "manifests"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    for source_name, suffix in (
        ("release-manifest.json", ".json"),
        ("release-provenance.json", ".provenance.json"),
    ):
        destination = manifest_dir / f"{release_sha}{suffix}"
        _atomic_bytes(destination, (root / source_name).read_bytes(), 0o640)


def _write_state(paths: HostPaths, state: dict[str, Any]) -> None:
    atomic_write(state_file(paths, state["release_sha"]), state)


def _runtime_may_have_changed(state: dict[str, Any]) -> bool:
    failure_phase = state.get("failure_phase")
    return (
        failure_phase in PHASES
        and PHASES.index(failure_phase) >= PHASES.index("MIGRATIONS_APPLIED")
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
    rollback_root = paths.releases / rollback_sha
    context_installer = (
        rollback_root / "scripts" / "install-production-release-context.sh"
    )
    try:
        runtime_may_have_changed = _runtime_may_have_changed(state)
        if runtime_may_have_changed:
            emergency_pause(paths)
        _require_success(
            [
                "/bin/sh",
                str(context_installer),
                rollback_sha,
                str(rollback_root),
            ],
            "ROLLBACK_CONTEXT_INSTALL_FAILED",
            env={"PHOENIX_DEPLOY_ROOT": str(paths.deploy_root)},
        )
        if runtime_may_have_changed:
            rollback_script = rollback_root / "scripts" / "rollback-release.sh"
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
            state = set_mutation_started(state)
            _write_state(paths, state)
            _install_candidate(paths, request)
            state = advance(state, "CANDIDATE_INSTALLED")
            _write_state(paths, state)
        if state["current_phase"] == "CANDIDATE_INSTALLED":
            environment = {
                "PHOENIX_DEPLOY_ROOT": str(paths.deploy_root),
                "PHOENIX_RELEASE_ROOT": str(paths.releases),
                "PHOENIX_ENV_FILE": str(paths.env_file),
                "PHOENIX_RELEASE_STATE_ROOT": str(paths.state_root),
                "PHOENIX_RELEASE_STATE_UPDATER": str(paths.state_updater),
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
                if state["current_phase"] not in {
                    "FAILED_PRE_MUTATION",
                    "FAILED_POST_MUTATION",
                    "ROLLED_BACK",
                    "ROLLBACK_FAILED",
                }:
                    state = fail_state(
                        state,
                        code="DEPLOYMENT_FAILED",
                        evidence={
                            "exit_code": result.returncode,
                            "output": _bounded_output(result.stdout),
                        },
                    )
                    _write_state(paths, state)
                raise GatewayError(
                    "DEPLOYMENT_FAILED",
                    {
                        "phase": state["current_phase"],
                        "exit_code": result.returncode,
                        "output": _bounded_output(result.stdout),
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
    compose = [
        "/usr/bin/docker",
        "compose",
        "--env-file",
        str(paths.env_file),
        "--env-file",
        str(release_env),
        "-f",
        str(paths.deploy_dir / "compose.prod.yml"),
        "-f",
        str(paths.deploy_dir / "compose.live-autonomous.yml"),
        "--profile",
        "live-autonomous",
    ]
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
    context_installer = (
        paths.releases
        / target_sha
        / "scripts"
        / "install-production-release-context.sh"
    )
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
    mutation = True if phase == "LIVE_MODE_INSTALLED" else None
    updates: dict[str, Any] = {}
    if phase == "AUTONOMOUS_ACTIVATED":
        updates = {
            "contract_paused": True,
            "autonomous_armed": True,
            "kill_switch": False,
        }
    elif phase == "EXECUTOR_UNPAUSED":
        updates = {
            "contract_paused": False,
            "autonomous_armed": True,
            "kill_switch": False,
        }
    elif phase == "COMPLETED":
        updates = {
            "active_release_pointer": release_sha,
            "candidate_pointer": None,
            "actual_images": dict(state["expected_images"]),
            "contract_paused": False,
            "autonomous_armed": True,
            "kill_switch": False,
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
