#!/usr/bin/env python3
"""Load the canonical Phoenix release-component and required-CI contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path, PurePosixPath
from typing import Any


SCHEMA = "phoenix.release-components.v1"
MAX_REGISTRY_BYTES = 64 * 1024
NAME_PATTERN = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
ENV_PATTERN = re.compile(r"^[A-Z][A-Z0-9_]*$")


class ReleaseComponentError(ValueError):
    pass


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ReleaseComponentError("component registry contains a duplicate key")
        value[key] = item
    return value


def _registry_path() -> Path:
    script_dir = Path(__file__).resolve().parent
    for candidate in (
        script_dir.parent / "release-components.json",
        script_dir / "release-components.json",
    ):
        if candidate.is_file() and not candidate.is_symlink():
            return candidate
    raise ReleaseComponentError("canonical component registry is unavailable")


def _relative_path(value: object, label: str, *, allow_dot: bool = False) -> str:
    if not isinstance(value, str) or not value or "\\" in value:
        raise ReleaseComponentError(f"{label} is invalid")
    if allow_dot and value == ".":
        return value
    path = PurePosixPath(value)
    if path.is_absolute() or path.parts in ((), (".",)) or ".." in path.parts:
        raise ReleaseComponentError(f"{label} is invalid")
    return value


def load_registry(path: Path | None = None) -> dict[str, Any]:
    source = path or _registry_path()
    if source.is_symlink() or not source.is_file():
        raise ReleaseComponentError("component registry must be a regular file")
    if source.stat().st_size > MAX_REGISTRY_BYTES:
        raise ReleaseComponentError("component registry exceeds the size limit")
    try:
        value = json.loads(
            source.read_text(encoding="utf-8"), object_pairs_hook=_unique_object
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ReleaseComponentError(
            "component registry is not valid UTF-8 JSON"
        ) from exc
    if not isinstance(value, dict) or set(value) != {
        "schema",
        "required_ci",
        "components",
    }:
        raise ReleaseComponentError("component registry contract is invalid")
    if value["schema"] != SCHEMA:
        raise ReleaseComponentError("component registry schema is invalid")

    ci = value["required_ci"]
    if not isinstance(ci, dict) or set(ci) != {
        "workflow_name",
        "workflow_path",
        "event",
        "branch",
        "jobs",
    }:
        raise ReleaseComponentError("required CI contract is invalid")
    if (
        ci["workflow_name"] != "Phoenix CI"
        or ci["workflow_path"] != ".github/workflows/ci.yml"
        or ci["event"] != "push"
        or ci["branch"] != "main"
    ):
        raise ReleaseComponentError("required CI identity is invalid")
    jobs = ci["jobs"]
    if (
        not isinstance(jobs, list)
        or len(jobs) != 12
        or len(set(jobs)) != len(jobs)
        or any(not isinstance(job, str) or not NAME_PATTERN.fullmatch(job) for job in jobs)
    ):
        raise ReleaseComponentError("required CI job contract is invalid")

    components = value["components"]
    if not isinstance(components, list) or len(components) != 8:
        raise ReleaseComponentError("release component count is invalid")
    names: list[str] = []
    protected_count = 0
    live_canary_count = 0
    services: set[str] = set()
    production_orders: set[int] = set()
    expected_keys = {
        "name",
        "repository",
        "build_context",
        "dockerfile",
        "build_args",
        "protected",
        "release_included",
        "production_compose",
        "production_order",
        "production_services",
        "image_environment",
        "live_canary_only",
    }
    for component in components:
        if not isinstance(component, dict) or set(component) != expected_keys:
            raise ReleaseComponentError("release component contract is invalid")
        name = component["name"]
        if not isinstance(name, str) or not NAME_PATTERN.fullmatch(name):
            raise ReleaseComponentError("release component name is invalid")
        if name in names:
            raise ReleaseComponentError(f"duplicate release component: {name}")
        names.append(name)
        if component["repository"] != f"ghcr.io/majidasgharitabrizi/{name}":
            raise ReleaseComponentError(f"component repository is invalid for {name}")
        _relative_path(component["build_context"], f"build context for {name}", allow_dot=True)
        _relative_path(component["dockerfile"], f"Dockerfile for {name}")
        build_args = component["build_args"]
        if (
            not isinstance(build_args, dict)
            or any(
                not isinstance(key, str)
                or not ENV_PATTERN.fullmatch(key)
                or not isinstance(item, str)
                or not item
                or "\n" in item
                for key, item in build_args.items()
            )
        ):
            raise ReleaseComponentError(f"build arguments are invalid for {name}")
        if not all(
            isinstance(component[field], bool)
            for field in (
                "protected",
                "release_included",
                "production_compose",
                "live_canary_only",
            )
        ):
            raise ReleaseComponentError(f"component flags are invalid for {name}")
        if not component["release_included"]:
            raise ReleaseComponentError(f"release component is excluded: {name}")
        component_services = component["production_services"]
        production_order = component["production_order"]
        image_environment = component["image_environment"]
        if component["production_compose"]:
            if (
                not isinstance(component_services, list)
                or not component_services
                or len(component_services) != len(set(component_services))
                or any(
                    not isinstance(service, str)
                    or not NAME_PATTERN.fullmatch(service)
                    or service in services
                    for service in component_services
                )
                or not isinstance(image_environment, str)
                or not ENV_PATTERN.fullmatch(image_environment)
                or not isinstance(production_order, int)
                or isinstance(production_order, bool)
                or production_order < 0
                or production_order in production_orders
            ):
                raise ReleaseComponentError(
                    f"production Compose contract is invalid for {name}"
                )
            services.update(component_services)
            production_orders.add(production_order)
        elif (
            component_services != []
            or image_environment is not None
            or production_order is not None
        ):
            raise ReleaseComponentError(
                f"non-Compose component contract is invalid for {name}"
            )
        if component["protected"]:
            protected_count += 1
            if not component["production_compose"] or component["live_canary_only"]:
                raise ReleaseComponentError(f"protected component flags are invalid for {name}")
        if component["live_canary_only"]:
            live_canary_count += 1
            if not component["production_compose"]:
                raise ReleaseComponentError(f"live-canary component is invalid for {name}")
    if names != sorted(names) or protected_count != 2 or live_canary_count != 1:
        raise ReleaseComponentError("release component ordering or role count is invalid")
    return value


def build_matrix(
    registry: dict[str, Any] | None = None,
    built_images: set[str] | None = None,
) -> dict[str, list[dict[str, Any]]]:
    value = registry or REGISTRY
    component_names = (
        set(RELEASE_IMAGES)
        if registry is None
        else {
            component["name"]
            for component in value["components"]
            if component["release_included"]
        }
    )
    selected = component_names if built_images is None else set(built_images)
    if not selected <= component_names:
        raise ReleaseComponentError("build matrix contains an unknown image")
    include = []
    for component in value["components"]:
        if component["name"] not in component_names:
            continue
        include.append(
            {
                "image": component["name"],
                "repository": component["repository"],
                "context": component["build_context"],
                "dockerfile": component["dockerfile"],
                "build_args": "\n".join(
                    f"{key}={item}"
                    for key, item in sorted(component["build_args"].items())
                ),
                "protected": component["protected"],
                "live_canary_only": component["live_canary_only"],
                "build": component["name"] in selected,
            }
        )
    return {"include": include}


RUNTIME_MODES = ("SHADOW", "DISARMED_EVIDENCE", "LIVE")
EXTERNAL_IMAGES = {
    "nitro-feed-relay": "offchainlabs/nitro-node@sha256:ebc985e3b105980734630744981e1542001c22d74cba57509fe0d5ed8bb84c14",
    "nats": "nats@sha256:b83efabe3e7def1e0a4a31ec6e078999bb17c80363f881df35edc70fcb6bb927",
    "postgres": "postgres@sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777",
    "prometheus": "prom/prometheus@sha256:075b1ba2c4ebb04bc3a6ab86c06ec8d8099f8fda1c96ef6d104d9bb1def1d8bc",
}
BASE_SERVICE_ORDER = (
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
EPHEMERAL_SERVICES = ("migration-runner",)
PROTECTED_SERVICES = (
    "nitro-feed-relay",
    "feed-ingestor",
    "nats",
    "postgres",
    "recorder",
)
FIXED_PROTECTED_SERVICES = ("nitro-feed-relay", "nats", "postgres")
MUTABLE_PROTECTED_SERVICES = ("feed-ingestor", "recorder")
OPERATIONAL_LIVE_SERVICES = ("economic-monitor", "economic-supervisor")
DEPLOY_START_ORDER = (
    "prometheus",
    "shadow-dispatcher",
    "dashboard",
    "atlas-observer",
    "economic-monitor",
    "economic-supervisor",
    "rpc-gateway",
    "phoenix-engine",
)
HEALTH_SERVICE_ORDER = (
    "nitro-feed-relay",
    "nats",
    "postgres",
    "rpc-gateway",
    "feed-ingestor",
    "phoenix-engine",
    "shadow-dispatcher",
    "recorder",
    "atlas-observer",
    "prometheus",
    "dashboard",
)
HEALTH_CONTRACTS = {
    "postgres": "pg_isready",
    "nats": "http://127.0.0.1:8222/healthz",
    "nitro-feed-relay": "tcp-listener:9642",
    "rpc-gateway": "http://127.0.0.1:9300/readyz",
    "feed-ingestor": "http://127.0.0.1:9100/readyz",
    "phoenix-engine": "http://127.0.0.1:9200/readyz",
    "shadow-dispatcher": "http://127.0.0.1:9500/readyz",
    "recorder": "http://127.0.0.1:9400/readyz",
    "atlas-observer": "binary:/usr/local/bin/atlas-observer;http://127.0.0.1:9700/readyz",
    "prometheus": "http://127.0.0.1:9090/-/ready",
    "dashboard": "http://127.0.0.1:8501/_stcore/health",
    "live-executor": "process:pid-1;autonomous-control:status",
}
CURSOR_CONTRACTS = {
    "recorder": "durable-money-path-outbox",
    "atlas-observer": "monotonic-auction-ledger",
}


REGISTRY_PATH = _registry_path()
REGISTRY = load_registry(REGISTRY_PATH)
COMPONENTS = tuple(REGISTRY["components"])
COMPONENTS_BY_NAME = {component["name"]: component for component in COMPONENTS}
CURRENT_RELEASE_IMAGES = tuple(component["name"] for component in COMPONENTS)
LEGACY_RELEASE_IMAGES = tuple(
    component["name"]
    for component in COMPONENTS
    if component["name"] != "atlas-observer"
)
_release_image_set = os.environ.get("PHOENIX_RELEASE_IMAGE_SET", "current")
if _release_image_set == "current":
    RELEASE_IMAGES = CURRENT_RELEASE_IMAGES
elif _release_image_set == "legacy":
    RELEASE_IMAGES = LEGACY_RELEASE_IMAGES
else:
    raise ReleaseComponentError("release image-set selector is invalid")
PROTECTED_IMAGES = tuple(
    component["name"] for component in COMPONENTS if component["protected"]
)
BUILT_IMAGES = tuple(name for name in RELEASE_IMAGES if name not in PROTECTED_IMAGES)
IMAGE_ENVIRONMENT_COMPONENTS = tuple(
    sorted(
        (
            component
            for component in COMPONENTS
            if component["production_compose"]
            and component["image_environment"] is not None
        ),
        key=lambda component: component["production_order"],
    )
)
DEFAULT_PRODUCTION_COMPONENTS = tuple(
    component
    for component in IMAGE_ENVIRONMENT_COMPONENTS
    if not component["live_canary_only"]
)
OPTIONAL_LIVE_COMPONENTS = tuple(
    component
    for component in IMAGE_ENVIRONMENT_COMPONENTS
    if component["live_canary_only"]
)
REQUIRED_CI = REGISTRY["required_ci"]
REQUIRED_CI_JOBS = tuple(REQUIRED_CI["jobs"])
REGISTRY_SHA256 = "sha256:" + hashlib.sha256(REGISTRY_PATH.read_bytes()).hexdigest()


def generation_for_images(image_names: set[str] | tuple[str, ...]) -> str:
    names = tuple(sorted(image_names))
    if names == CURRENT_RELEASE_IMAGES:
        return "current"
    if names == LEGACY_RELEASE_IMAGES:
        return "legacy"
    raise ReleaseComponentError("release image set is invalid")


def manifest_images(path: Path) -> tuple[str, ...]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ReleaseComponentError("release manifest is invalid") from exc
    if not isinstance(value, dict) or not isinstance(value.get("images"), dict):
        raise ReleaseComponentError("release manifest is invalid")
    names = tuple(sorted(value["images"]))
    generation_for_images(names)
    return names


def load_build_plan(path: Path) -> dict[str, Any]:
    try:
        if (
            path.is_symlink()
            or not path.is_file()
            or path.stat().st_size > MAX_REGISTRY_BYTES
        ):
            raise ReleaseComponentError("build plan is invalid")
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ReleaseComponentError("build plan is invalid") from exc
    if not isinstance(value, dict) or value.get("schema") != "phoenix.change-impact.v1":
        raise ReleaseComponentError("build plan is invalid")
    for field in ("built_images", "inherited_images"):
        items = value.get(field)
        if (
            not isinstance(items, list)
            or len(items) != len(set(items))
            or any(not isinstance(item, str) for item in items)
        ):
            raise ReleaseComponentError("build plan image selection is invalid")
    return value


def resolve_protected_build_plan(
    plan: dict[str, Any], protected_base_images: set[str] | tuple[str, ...]
) -> dict[str, Any]:
    base = set(protected_base_images)
    generation_for_images(base)
    built = set(plan.get("built_images", []))
    inherited = set(plan.get("inherited_images", []))
    expected = set(RELEASE_IMAGES)
    if built & inherited or built | inherited != expected:
        raise ReleaseComponentError("build plan does not cover the release image set")
    missing_from_base = expected - base
    resolved = dict(plan)
    resolved["built_images"] = sorted(built | missing_from_base)
    resolved["inherited_images"] = sorted(inherited & base)
    return resolved


def runtime_topology(
    image_names: set[str] | tuple[str, ...],
    mode: str,
    *,
    source_image_names: set[str] | tuple[str, ...] | None = None,
) -> dict[str, Any]:
    if mode not in RUNTIME_MODES:
        raise ReleaseComponentError("runtime mode is invalid")
    target_names = set(image_names)
    generation = generation_for_images(target_names)
    source_names = set(source_image_names) if source_image_names is not None else target_names
    source_generation = generation_for_images(source_names)

    component_services: list[str] = []
    image_bindings: dict[str, str] = {}
    for component in COMPONENTS:
        if component["name"] in target_names:
            component_services.extend(component["production_services"])
            for service in component["production_services"]:
                image_bindings[service] = component["name"]
    image_bindings.update(
        {
            service: reference
            for service, reference in EXTERNAL_IMAGES.items()
            if service in BASE_SERVICE_ORDER
        }
    )
    image_bindings["economic-monitor"] = EXTERNAL_IMAGES["postgres"]
    image_bindings["economic-supervisor"] = "live-executor"

    configured = list(BASE_SERVICE_ORDER)
    if "atlas-observer" in target_names:
        configured.insert(configured.index("dashboard"), "atlas-observer")
    if mode != "SHADOW":
        configured.append("live-executor")

    persistent = [
        service
        for service in configured
        if service not in EPHEMERAL_SERVICES and service != "live-executor"
    ]
    if mode != "SHADOW":
        persistent.extend(OPERATIONAL_LIVE_SERVICES)
    if mode == "LIVE":
        persistent.append("live-executor")

    start_services = [
        service
        for service in DEPLOY_START_ORDER
        if service in persistent
    ]
    inspect_services = [
        service
        for service in HEALTH_SERVICE_ORDER
        if service in persistent
    ]
    health_services = list(inspect_services)
    if mode == "LIVE":
        inspect_services.append("live-executor")

    intentional_absence: list[str] = []
    if "atlas-observer" not in target_names:
        intentional_absence.append("atlas-observer")
    if mode != "LIVE":
        intentional_absence.append("live-executor")
    if mode == "SHADOW":
        intentional_absence.extend(OPERATIONAL_LIVE_SERVICES)

    source_persistent = set(
        runtime_topology(source_names, mode)["running_services"]
        if source_image_names is not None and source_names != target_names
        else persistent
    )
    remove_services = [
        service
        for service in reversed(DEPLOY_START_ORDER)
        if service in source_persistent and service not in persistent
    ]
    service_contracts = {
        service: {
            "image_binding": image_bindings[service],
            "lifecycle": "persistent",
            "protection": (
                "fixed"
                if service in FIXED_PROTECTED_SERVICES
                else "mutable"
                if service in MUTABLE_PROTECTED_SERVICES
                else "ordinary"
            ),
            "health": HEALTH_CONTRACTS.get(service),
            "cursor": CURSOR_CONTRACTS.get(service),
            "expected_modes": (
                ["LIVE"]
                if service == "live-executor"
                else ["DISARMED_EVIDENCE", "LIVE"]
                if service in OPERATIONAL_LIVE_SERVICES
                else list(RUNTIME_MODES)
            ),
        }
        for service in persistent
    }

    return {
        "schema": "phoenix.release-topology.v1",
        "generation": generation,
        "source_generation": source_generation,
        "manifest_images": sorted(target_names),
        "manifest_services": sorted(component_services),
        "rendered_expected_services": configured,
        "running_services": persistent,
        "start_services": start_services,
        "stop_services": list(reversed(start_services)),
        "inspect_services": inspect_services,
        "health_services": health_services,
        "ephemeral_services": [
            service for service in EPHEMERAL_SERVICES if service in configured
        ],
        "protected_services": list(PROTECTED_SERVICES),
        "fixed_protected_services": list(FIXED_PROTECTED_SERVICES),
        "mutable_protected_services": list(MUTABLE_PROTECTED_SERVICES),
        "ordinary_services": [
            service for service in persistent if service not in PROTECTED_SERVICES
        ],
        "intentional_absence": intentional_absence,
        "remove_services": remove_services,
        "image_bindings": image_bindings,
        "health_contracts": {
            service: HEALTH_CONTRACTS[service]
            for service in health_services
        },
        "service_contracts": service_contracts,
        "cursor_services": [
            service for service in ("recorder", "atlas-observer") if service in persistent
        ],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=(
            "validate",
            "build-matrix",
            "resolve-protected-build-plan",
            "topology",
        ),
    )
    parser.add_argument("--built-images-json")
    parser.add_argument("--manifest")
    parser.add_argument("--plan")
    parser.add_argument("--protected-base-manifest")
    parser.add_argument("--source-manifest")
    parser.add_argument("--mode", choices=RUNTIME_MODES)
    parser.add_argument("--field")
    args = parser.parse_args()
    if args.command == "build-matrix":
        selected = None
        if args.built_images_json is not None:
            try:
                raw = json.loads(args.built_images_json)
            except json.JSONDecodeError as exc:
                raise ReleaseComponentError(
                    "built image selection is invalid JSON"
                ) from exc
            if (
                not isinstance(raw, list)
                or len(raw) != len(set(raw))
                or any(not isinstance(item, str) for item in raw)
            ):
                raise ReleaseComponentError("built image selection is invalid")
            selected = set(raw)
        print(
            json.dumps(
                build_matrix(built_images=selected),
                sort_keys=True,
                separators=(",", ":"),
            )
        )
    elif args.command == "resolve-protected-build-plan":
        if not args.plan or not args.protected_base_manifest:
            raise ReleaseComponentError(
                "build plan and protected base manifest are required"
            )
        plan = load_build_plan(Path(args.plan))
        protected_images = manifest_images(Path(args.protected_base_manifest))
        print(
            json.dumps(
                resolve_protected_build_plan(plan, protected_images),
                indent=2,
                sort_keys=True,
            )
        )
    elif args.command == "topology":
        if not args.manifest or not args.mode:
            raise ReleaseComponentError("topology manifest and mode are required")
        target = manifest_images(Path(args.manifest))
        source = (
            manifest_images(Path(args.source_manifest))
            if args.source_manifest
            else None
        )
        value = runtime_topology(target, args.mode, source_image_names=source)
        if args.field:
            field = value.get(args.field)
            if not isinstance(field, list) or any(
                not isinstance(item, str) or not NAME_PATTERN.fullmatch(item)
                for item in field
            ):
                raise ReleaseComponentError("topology field is invalid")
            print(" ".join(field))
        else:
            print(json.dumps(value, indent=2, sort_keys=True))
    else:
        print("RELEASE_COMPONENTS_OK")


if __name__ == "__main__":
    main()
