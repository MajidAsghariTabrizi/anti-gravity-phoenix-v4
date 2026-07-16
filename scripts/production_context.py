#!/usr/bin/env python3
import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
from pathlib import Path


OWNED_IMAGES = {
    "feed-ingestor": ("FEED_INGESTOR_IMAGE", "feed-ingestor"),
    "phoenix-engine": ("PHOENIX_ENGINE_IMAGE", "phoenix-engine"),
    "rpc-gateway": ("RPC_GATEWAY_IMAGE", "rpc-gateway"),
    "recorder": ("RECORDER_IMAGE", "recorder"),
    "dashboard": ("DASHBOARD_IMAGE", "dashboard"),
}

EXPECTED_SERVICES = (
    "nitro-feed-relay",
    "nats",
    "postgres",
    "migration-runner",
    "rpc-gateway",
    "feed-ingestor",
    "phoenix-engine",
    "shadow-dispatcher",
    "recorder",
    "dashboard",
    "prometheus",
)

RUNNING_SERVICES = tuple(
    service for service in EXPECTED_SERVICES if service != "migration-runner"
)

RENDERED_OWNED_IMAGES = {
    "feed-ingestor": "FEED_INGESTOR_IMAGE",
    "migration-runner": "FEED_INGESTOR_IMAGE",
    "phoenix-engine": "PHOENIX_ENGINE_IMAGE",
    "rpc-gateway": "RPC_GATEWAY_IMAGE",
    "recorder": "RECORDER_IMAGE",
    "shadow-dispatcher": "RECORDER_IMAGE",
    "dashboard": "DASHBOARD_IMAGE",
}

EXTERNAL_IMAGES = {
    "nitro-feed-relay": "offchainlabs/nitro-node@sha256:ebc985e3b105980734630744981e1542001c22d74cba57509fe0d5ed8bb84c14",
    "nats": "nats@sha256:b83efabe3e7def1e0a4a31ec6e078999bb17c80363f881df35edc70fcb6bb927",
    "postgres": "postgres@sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777",
    "prometheus": "prom/prometheus@sha256:075b1ba2c4ebb04bc3a6ab86c06ec8d8099f8fda1c96ef6d104d9bb1def1d8bc",
}

SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
IMAGE_PATTERN = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
ROUTE_ID_PATTERN = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")
ROUTE_FINGERPRINT_PATTERN = re.compile(r"^[A-Za-z0-9._:-]{1,256}$")
MAX_ENV_BYTES = 1024 * 1024
MAX_ROUTE_BYTES = 64 * 1024
MAX_ROUTES = 256


class ContextError(Exception):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def fail(code: str) -> None:
    payload = {"code": code, "status": "error"}
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")), file=sys.stderr)
    raise SystemExit(1)


def read_json(path: Path, missing_code: str, invalid_code: str):
    if not path.is_file():
        raise ContextError(missing_code)
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise ContextError(invalid_code) from None


def read_env(path: Path, missing_code: str) -> dict[str, str]:
    if not path.is_file():
        raise ContextError(missing_code)
    try:
        raw = path.read_bytes()
    except OSError:
        raise ContextError(missing_code) from None
    if len(raw) > MAX_ENV_BYTES:
        raise ContextError("PRODUCTION_ENV_INVALID")
    try:
        lines = raw.decode("utf-8-sig").splitlines()
    except UnicodeError:
        raise ContextError("PRODUCTION_ENV_INVALID") from None

    values: dict[str, str] = {}
    for raw_line in lines:
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        name, candidate = line.split("=", 1)
        name = name.strip()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
            raise ContextError("PRODUCTION_ENV_INVALID")
        candidate = candidate.strip()
        if len(candidate) >= 2 and candidate[0] == candidate[-1] == "'":
            candidate = candidate[1:-1]
        elif len(candidate) >= 2 and candidate[0] == candidate[-1] == '"':
            try:
                decoded = json.loads(candidate)
            except json.JSONDecodeError:
                raise ContextError("PRODUCTION_ENV_INVALID") from None
            if not isinstance(decoded, str):
                raise ContextError("PRODUCTION_ENV_INVALID")
            candidate = decoded
        values[name] = candidate
    return values


def atomic_write(path: Path, content: str, mode: int = 0o640) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def paths_conflict(left: Path, right: Path) -> bool:
    left_resolved = left.resolve(strict=False)
    right_resolved = right.resolve(strict=False)
    if left_resolved == right_resolved:
        return True
    try:
        return left.is_file() and right.is_file() and os.path.samefile(left, right)
    except OSError:
        return False


def validate_output_paths(args: argparse.Namespace) -> None:
    outputs = [Path(args.output), Path(args.metadata_output)]
    inputs = [Path(value) for value in args.input]
    if paths_conflict(outputs[0], outputs[1]):
        raise ContextError("PRODUCTION_OUTPUT_PATH_CONFLICT")
    for output in outputs:
        if any(paths_conflict(output, source) for source in inputs):
            raise ContextError("PRODUCTION_OUTPUT_PATH_CONFLICT")


def canonical_json(value) -> bytes:
    return json.dumps(
        value, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def sha256_file(path: Path, missing_code: str) -> str:
    if not path.is_file():
        raise ContextError(missing_code)
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
    except OSError:
        raise ContextError(missing_code) from None
    return f"sha256:{digest.hexdigest()}"


def load_manifest(path: Path) -> tuple[dict, str, dict[str, str]]:
    manifest = read_json(path, "RELEASE_MANIFEST_MISSING", "RELEASE_IMAGE_MISMATCH")
    if not isinstance(manifest, dict) or manifest.get("schema") != "phoenix.release.v1":
        raise ContextError("RELEASE_IMAGE_MISMATCH")
    release_sha = manifest.get("release_sha")
    if not isinstance(release_sha, str) or not SHA_PATTERN.fullmatch(release_sha):
        raise ContextError("RELEASE_IMAGE_MISMATCH")
    images = manifest.get("images")
    if not isinstance(images, dict):
        raise ContextError("RELEASE_IMAGE_MISMATCH")

    references: dict[str, str] = {}
    for image_name, (_, repository_name) in OWNED_IMAGES.items():
        image = images.get(image_name)
        if not isinstance(image, dict):
            raise ContextError("RELEASE_IMAGE_MISMATCH")
        repository = image.get("repository")
        tag = image.get("tag")
        digest = image.get("digest")
        expected_repository = f"ghcr.io/majidasgharitabrizi/{repository_name}"
        if (
            repository != expected_repository
            or tag != f"sha-{release_sha}"
            or not isinstance(digest, str)
            or not DIGEST_PATTERN.fullmatch(digest)
        ):
            raise ContextError("RELEASE_IMAGE_MISMATCH")
        references[image_name] = f"{repository}@{digest}"
    return manifest, release_sha, references


def validate_release_env(
    values: dict[str, str], release_sha: str | None, references: dict[str, str] | None
) -> dict[str, str]:
    rendered: dict[str, str] = {}
    for image_name, (env_name, _) in OWNED_IMAGES.items():
        value = values.get(env_name, "")
        if value.startswith("app-") or "/app-" in value:
            raise ContextError("LOCAL_IMAGE_FALLBACK")
        if not IMAGE_PATTERN.fullmatch(value):
            raise ContextError("RELEASE_IMAGE_MISMATCH")
        if references is not None and value != references[image_name]:
            raise ContextError("RELEASE_IMAGE_MISMATCH")
        rendered[env_name] = value
    env_sha = values.get("PHOENIX_RELEASE_SHA")
    if release_sha is not None and env_sha not in (None, "", release_sha):
        raise ContextError("RELEASE_IMAGE_MISMATCH")
    return rendered


def manifest_env(args: argparse.Namespace) -> None:
    _, release_sha, references = load_manifest(Path(args.manifest))
    if args.expected_sha and args.expected_sha != release_sha:
        raise ContextError("RELEASE_IMAGE_MISMATCH")
    lines = []
    for image_name, (env_name, _) in OWNED_IMAGES.items():
        lines.append(f"{env_name}={references[image_name]}")
    lines.append(f"PHOENIX_RELEASE_SHA={release_sha}")
    atomic_write(Path(args.output), "\n".join(lines) + "\n")


def validate_route_registry(raw: str) -> tuple[list, str]:
    if not raw:
        raise ContextError("ROUTE_REGISTRY_MISSING")
    if len(raw.encode("utf-8")) > MAX_ROUTE_BYTES:
        raise ContextError("ROUTE_REGISTRY_INVALID")
    try:
        routes = json.loads(raw)
    except json.JSONDecodeError:
        raise ContextError("ROUTE_REGISTRY_INVALID_JSON") from None
    if not isinstance(routes, list):
        raise ContextError("ROUTE_REGISTRY_INVALID_JSON")
    if not routes:
        raise ContextError("ROUTE_REGISTRY_EMPTY")
    if len(routes) > MAX_ROUTES:
        raise ContextError("ROUTE_REGISTRY_INVALID")

    route_ids: set[str] = set()
    fingerprints: set[str] = set()
    for route in routes:
        if not isinstance(route, dict):
            raise ContextError("ROUTE_REGISTRY_INVALID")
        route_id = route.get("route_id")
        fingerprint = route.get("route_fingerprint")
        if (
            not isinstance(route_id, str)
            or not ROUTE_ID_PATTERN.fullmatch(route_id)
            or route_id in route_ids
            or not isinstance(fingerprint, str)
            or not ROUTE_FINGERPRINT_PATTERN.fullmatch(fingerprint)
            or fingerprint in fingerprints
        ):
            raise ContextError("ROUTE_REGISTRY_INVALID")
        route_ids.add(route_id)
        fingerprints.add(fingerprint)
    return routes, sha256_bytes(canonical_json(routes))


def service_environment(services: dict, service: str) -> dict:
    value = services.get(service)
    if not isinstance(value, dict) or not isinstance(value.get("environment"), dict):
        raise ContextError("PRODUCTION_COMPOSE_CONTEXT_MISSING")
    return value["environment"]


def validate_render(args: argparse.Namespace) -> None:
    compose = read_json(
        Path(args.compose_config),
        "PRODUCTION_COMPOSE_CONTEXT_MISSING",
        "PRODUCTION_COMPOSE_CONTEXT_MISSING",
    )
    if not isinstance(compose, dict) or not isinstance(compose.get("services"), dict):
        raise ContextError("PRODUCTION_COMPOSE_CONTEXT_MISSING")
    services = compose["services"]
    if any(service not in services for service in EXPECTED_SERVICES):
        raise ContextError("PRODUCTION_COMPOSE_CONTEXT_MISSING")

    operator_env = read_env(Path(args.env_file), "PRODUCTION_ENV_MISSING")
    release_env = read_env(Path(args.release_env), "RELEASE_ENV_MISSING")
    release_sha = None
    references = None
    if args.manifest:
        _, release_sha, references = load_manifest(Path(args.manifest))
    release_images = validate_release_env(release_env, release_sha, references)
    release_sha = release_sha or release_env.get("PHOENIX_RELEASE_SHA")
    if not isinstance(release_sha, str) or not SHA_PATTERN.fullmatch(release_sha):
        raise ContextError("RELEASE_IMAGE_MISMATCH")

    images: dict[str, str] = {}
    for service in EXPECTED_SERVICES:
        service_config = services.get(service)
        if not isinstance(service_config, dict):
            raise ContextError("PRODUCTION_COMPOSE_CONTEXT_MISSING")
        image = service_config.get("image")
        if not isinstance(image, str):
            raise ContextError("RELEASE_IMAGE_MISMATCH")
        if image.startswith("app-") or "/app-" in image:
            raise ContextError("LOCAL_IMAGE_FALLBACK")
        if not IMAGE_PATTERN.fullmatch(image):
            raise ContextError("RELEASE_IMAGE_MISMATCH")
        images[service] = image

    for service, env_name in RENDERED_OWNED_IMAGES.items():
        if images[service] != release_images[env_name]:
            raise ContextError("RELEASE_IMAGE_MISMATCH")
    for service, expected in EXTERNAL_IMAGES.items():
        if images[service] != expected:
            raise ContextError("RELEASE_IMAGE_MISMATCH")

    expected_route_raw = operator_env.get("ENGINE_ROUTE_REGISTRY_JSON")
    if expected_route_raw is None or expected_route_raw == "":
        raise ContextError("ROUTE_REGISTRY_MISSING")
    _, route_hash = validate_route_registry(expected_route_raw)
    engine_env = service_environment(services, "phoenix-engine")
    rendered_route_raw = engine_env.get("ENGINE_ROUTE_REGISTRY_JSON")
    if not isinstance(rendered_route_raw, str):
        raise ContextError("ROUTE_REGISTRY_MISSING")
    try:
        _, rendered_route_hash = validate_route_registry(rendered_route_raw)
    except ContextError as error:
        if error.code == "ROUTE_REGISTRY_MISSING":
            raise
        if error.code == "ROUTE_REGISTRY_EMPTY":
            raise
        if error.code == "ROUTE_REGISTRY_INVALID_JSON":
            raise
        raise ContextError("ROUTE_REGISTRY_RENDER_MISMATCH") from None
    if rendered_route_raw != expected_route_raw:
        raise ContextError("ROUTE_REGISTRY_RENDER_MISMATCH")
    if rendered_route_hash != route_hash:
        raise ContextError("ROUTE_REGISTRY_HASH_MISMATCH")

    if operator_env.get("CHAIN_ID") != "42161" or str(engine_env.get("CHAIN_ID")) != "42161":
        raise ContextError("CHAIN_ID_MISMATCH")
    dispatcher_env = service_environment(services, "shadow-dispatcher")
    if operator_env.get("PHOENIX_MODE") != "SHADOW" or any(
        environment.get("PHOENIX_MODE") != "SHADOW"
        for environment in (engine_env, dispatcher_env)
    ):
        raise ContextError("SHADOW_MODE_REQUIRED")
    if operator_env.get("LIVE_EXECUTION") != "false" or any(
        str(environment.get("LIVE_EXECUTION", "")).lower() != "false"
        for environment in (engine_env, dispatcher_env)
    ):
        raise ContextError("LIVE_EXECUTION_MUST_BE_FALSE")
    for name, code in (
        ("SIGNER_PRIVATE_KEY", "SIGNER_MUST_BE_EMPTY"),
        ("WALLET_ADDRESS", "WALLET_MUST_BE_EMPTY"),
        ("EXECUTOR_ADDRESS", "EXECUTOR_MUST_BE_EMPTY"),
    ):
        if operator_env.get(name, "") != "" or any(
            environment.get(name, "") != "" for environment in (engine_env, dispatcher_env)
        ):
            raise ContextError(code)

    rpc_env = service_environment(services, "rpc-gateway")
    try:
        rpc_budget = int(str(rpc_env.get("RPC_STATE_REQUESTS_PER_MINUTE")))
    except (TypeError, ValueError):
        raise ContextError("RPC_STATE_BUDGET_TOO_LOW") from None
    if rpc_budget < 12:
        raise ContextError("RPC_STATE_BUDGET_TOO_LOW")

    metadata = {
        "chain_id": 42161,
        "expected_services": list(EXPECTED_SERVICES),
        "images": images,
        "live_execution": False,
        "mode": "SHADOW",
        "release_sha": release_sha,
        "route_count": len(json.loads(expected_route_raw)),
        "route_registry_hash": route_hash,
        "rpc_state_requests_per_minute": rpc_budget,
        "schema": "phoenix.production-render.v1",
        "status": "ok",
    }
    content = json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    atomic_write(Path(args.metadata_output), content)


def load_render_metadata(path: Path) -> dict:
    metadata = read_json(
        path, "PRODUCTION_COMPOSE_CONTEXT_MISSING", "PRODUCTION_COMPOSE_CONTEXT_MISSING"
    )
    if (
        not isinstance(metadata, dict)
        or metadata.get("schema") != "phoenix.production-render.v1"
        or metadata.get("status") != "ok"
        or not DIGEST_PATTERN.fullmatch(str(metadata.get("route_registry_hash", "")))
        or not isinstance(metadata.get("images"), dict)
    ):
        raise ContextError("PRODUCTION_COMPOSE_CONTEXT_MISSING")
    return metadata


def state_payload(args: argparse.Namespace) -> dict:
    _, release_sha, references = load_manifest(Path(args.manifest))
    release_env = read_env(Path(args.release_env), "RELEASE_ENV_MISSING")
    validate_release_env(release_env, release_sha, references)
    metadata = load_render_metadata(Path(args.render_metadata))
    if metadata.get("release_sha") != release_sha:
        raise ContextError("RELEASE_IMAGE_MISMATCH")
    return {
        "compose_config_sha256": sha256_file(
            Path(args.compose_config), "PRODUCTION_COMPOSE_CONTEXT_MISSING"
        ),
        "images": metadata["images"],
        "manifest_sha256": sha256_file(Path(args.manifest), "RELEASE_MANIFEST_MISSING"),
        "release_env_sha256": sha256_file(Path(args.release_env), "RELEASE_ENV_MISSING"),
        "release_sha": release_sha,
        "route_registry_hash": metadata["route_registry_hash"],
        "schema": "phoenix.release-state.v1",
    }


def write_state(args: argparse.Namespace) -> None:
    payload = state_payload(args)
    atomic_write(Path(args.output), json.dumps(payload, indent=2, sort_keys=True) + "\n")


def running_from_tsv(args: argparse.Namespace) -> None:
    source = Path(args.input)
    if not source.is_file():
        raise ContextError("RUNNING_IMAGE_MISMATCH")
    services: dict[str, dict[str, str]] = {}
    try:
        lines = source.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        raise ContextError("RUNNING_IMAGE_MISMATCH") from None
    for line in lines:
        fields = line.split("\t")
        if len(fields) != 3:
            raise ContextError("RUNNING_IMAGE_MISMATCH")
        service, configured_image, image_id = fields
        if service in services:
            raise ContextError("RUNNING_IMAGE_MISMATCH")
        services[service] = {
            "configured_image": configured_image,
            "image_id": image_id,
        }
    atomic_write(
        Path(args.output),
        json.dumps({"schema": "phoenix.running-images.v1", "services": services}, indent=2, sort_keys=True)
        + "\n",
    )


def validate_active(args: argparse.Namespace) -> None:
    expected_state = state_payload(args)
    state = read_json(
        Path(args.release_state), "RELEASE_STATE_MISSING", "RELEASE_IMAGE_MISMATCH"
    )
    if not isinstance(state, dict):
        raise ContextError("RELEASE_IMAGE_MISMATCH")
    if state.get("route_registry_hash") != expected_state["route_registry_hash"]:
        raise ContextError("ROUTE_REGISTRY_HASH_MISMATCH")
    if state != expected_state:
        raise ContextError("RELEASE_IMAGE_MISMATCH")

    current_release = Path(args.current_release)
    if not current_release.is_file():
        raise ContextError("RELEASE_STATE_MISSING")
    try:
        pointer = current_release.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeError):
        raise ContextError("RELEASE_STATE_MISSING") from None
    if pointer != expected_state["release_sha"]:
        raise ContextError("RELEASE_IMAGE_MISMATCH")

    running = read_json(
        Path(args.running_images), "RUNNING_IMAGE_MISMATCH", "RUNNING_IMAGE_MISMATCH"
    )
    if (
        not isinstance(running, dict)
        or running.get("schema") != "phoenix.running-images.v1"
        or not isinstance(running.get("services"), dict)
    ):
        raise ContextError("RUNNING_IMAGE_MISMATCH")
    running_services = running["services"]
    for service in RUNNING_SERVICES:
        item = running_services.get(service)
        expected_image = expected_state["images"].get(service)
        if not isinstance(item, dict) or not isinstance(expected_image, str):
            raise ContextError("RUNNING_IMAGE_MISMATCH")
        configured_image = item.get("configured_image")
        image_id = item.get("image_id")
        if isinstance(configured_image, str) and (
            configured_image.startswith("app-") or "/app-" in configured_image
        ):
            raise ContextError("LOCAL_IMAGE_FALLBACK")
        if configured_image != expected_image or not DIGEST_PATTERN.fullmatch(str(image_id)):
            raise ContextError("RUNNING_IMAGE_MISMATCH")

    result = {
        "chain_id": 42161,
        "live_execution": False,
        "mode": "SHADOW",
        "release_sha": expected_state["release_sha"],
        "route_registry_hash": expected_state["route_registry_hash"],
        "schema": "phoenix.release-context.v1",
        "status": "ok",
    }
    atomic_write(Path(args.output), json.dumps(result, indent=2, sort_keys=True) + "\n")


def add_state_inputs(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--release-env", required=True)
    parser.add_argument("--render-metadata", required=True)
    parser.add_argument("--compose-config", required=True)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    subcommands = root.add_subparsers(dest="command", required=True)

    manifest = subcommands.add_parser("manifest-env")
    manifest.add_argument("--manifest", required=True)
    manifest.add_argument("--expected-sha")
    manifest.add_argument("--output", required=True)
    manifest.set_defaults(handler=manifest_env)

    paths = subcommands.add_parser("validate-output-paths")
    paths.add_argument("--output", required=True)
    paths.add_argument("--metadata-output", required=True)
    paths.add_argument("--input", action="append", required=True)
    paths.set_defaults(handler=validate_output_paths)

    render = subcommands.add_parser("validate-render")
    render.add_argument("--compose-config", required=True)
    render.add_argument("--env-file", required=True)
    render.add_argument("--release-env", required=True)
    render.add_argument("--manifest")
    render.add_argument("--metadata-output", required=True)
    render.set_defaults(handler=validate_render)

    state = subcommands.add_parser("write-state")
    add_state_inputs(state)
    state.add_argument("--output", required=True)
    state.set_defaults(handler=write_state)

    running = subcommands.add_parser("running-from-tsv")
    running.add_argument("--input", required=True)
    running.add_argument("--output", required=True)
    running.set_defaults(handler=running_from_tsv)

    active = subcommands.add_parser("validate-active")
    add_state_inputs(active)
    active.add_argument("--current-release", required=True)
    active.add_argument("--release-state", required=True)
    active.add_argument("--running-images", required=True)
    active.add_argument("--output", required=True)
    active.set_defaults(handler=validate_active)
    return root


def main() -> None:
    args = parser().parse_args()
    try:
        args.handler(args)
    except ContextError as error:
        fail(error.code)


if __name__ == "__main__":
    main()
