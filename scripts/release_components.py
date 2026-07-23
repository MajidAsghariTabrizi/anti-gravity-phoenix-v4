#!/usr/bin/env python3
"""Load the canonical Phoenix release-component and required-CI contract."""

from __future__ import annotations

import argparse
import hashlib
import json
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
    if not isinstance(components, list) or len(components) != 7:
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


def build_matrix(registry: dict[str, Any] | None = None) -> dict[str, list[dict[str, Any]]]:
    value = registry or REGISTRY
    include = []
    for component in value["components"]:
        if not component["release_included"]:
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
            }
        )
    return {"include": include}


REGISTRY_PATH = _registry_path()
REGISTRY = load_registry(REGISTRY_PATH)
COMPONENTS = tuple(REGISTRY["components"])
COMPONENTS_BY_NAME = {component["name"]: component for component in COMPONENTS}
RELEASE_IMAGES = tuple(component["name"] for component in COMPONENTS)
PROTECTED_IMAGES = tuple(
    component["name"] for component in COMPONENTS if component["protected"]
)
BUILT_IMAGES = tuple(name for name in RELEASE_IMAGES if name not in PROTECTED_IMAGES)
LEGACY_RELEASE_IMAGES = tuple(
    component["name"] for component in COMPONENTS if not component["live_canary_only"]
)
DEFAULT_PRODUCTION_COMPONENTS = tuple(
    sorted(
        (
            component
            for component in COMPONENTS
            if component["production_compose"] and not component["live_canary_only"]
        ),
        key=lambda component: component["production_order"],
    )
)
REQUIRED_CI = REGISTRY["required_ci"]
REQUIRED_CI_JOBS = tuple(REQUIRED_CI["jobs"])
REGISTRY_SHA256 = "sha256:" + hashlib.sha256(REGISTRY_PATH.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "build-matrix"))
    args = parser.parse_args()
    if args.command == "build-matrix":
        print(json.dumps(build_matrix(), sort_keys=True, separators=(",", ":")))
    else:
        print("RELEASE_COMPONENTS_OK")


if __name__ == "__main__":
    main()
