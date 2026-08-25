#!/usr/bin/env python3
"""Build the canonical Phoenix CI, image, and release impact plan."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Iterable

try:
    from scripts import release_components
except (ImportError, ModuleNotFoundError):  # Direct execution from scripts/.
    import release_components  # type: ignore[no-redef]


SCHEMA = "phoenix.change-impact.v1"
JOBS = release_components.REQUIRED_CI_JOBS
IMAGES = release_components.RELEASE_IMAGES

DOC_FILES = {
    "CODE_OF_CONDUCT.md",
    "LICENSE",
    "README.md",
    "SECURITY.md",
}
RELEASE_ROOTS = (
    ".github/",
    "docs/",
    "schemas/",
    "scripts/",
)
RELEASE_FILES = {
    "compose.fork.yml",
    "compose.live-autonomous.yml",
    "compose.live-canary.yml",
    "compose.prod.yml",
    "docker-compose.yml",
}
DOCKER_STATIC_ROOTS = ("deploy/",)
DOCKER_STATIC_FILES = RELEASE_FILES | {
    ".dockerignore",
    "release-components.json",
}
ALL_IMAGE_CONTRACT_FILES = {
    "release-components.json",
}
DOCKERFILE_IMAGES = {
    "deploy/atlas-observer.Dockerfile": ("atlas-observer",),
    "deploy/atlas-observer.Dockerfile.dockerignore": ("atlas-observer",),
    "deploy/dashboard.Dockerfile": ("dashboard",),
    "deploy/dashboard.Dockerfile.dockerignore": ("dashboard",),
    "deploy/feed-ingestor.Dockerfile": ("feed-ingestor",),
    "deploy/feed-ingestor.Dockerfile.dockerignore": ("feed-ingestor",),
    "deploy/fork-sandbox.Dockerfile": ("fork-sandbox",),
    "deploy/fork-sandbox.Dockerfile.dockerignore": ("fork-sandbox",),
    "deploy/rust.Dockerfile": (
        "live-executor",
        "phoenix-engine",
        "recorder",
        "rpc-gateway",
    ),
    "deploy/rust.Dockerfile.dockerignore": (
        "live-executor",
        "phoenix-engine",
        "recorder",
        "rpc-gateway",
    ),
}


class ImpactError(ValueError):
    """A changed-path or impact-plan contract was invalid."""


def _safe_path(value: str) -> str:
    if not value or value != value.strip() or "\\" in value or "\x00" in value:
        raise ImpactError("changed path is invalid")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise ImpactError("changed path is invalid")
    return path.as_posix()


def _under(path: str, *roots: str) -> bool:
    return any(path.startswith(root) for root in roots)


def _mark(
    jobs: set[str],
    images: set[str],
    *,
    job_names: Iterable[str] = (),
    image_names: Iterable[str] = (),
) -> None:
    jobs.update(job_names)
    images.update(image_names)


def _classify_path(
    path: str,
    jobs: set[str],
    images: set[str],
) -> tuple[bool, bool]:
    """Return (known, docker_static)."""
    docker_static = path in DOCKER_STATIC_FILES or _under(
        path, *DOCKER_STATIC_ROOTS
    )

    if path in DOC_FILES or _under(path, "docs/"):
        return True, False

    if path in ALL_IMAGE_CONTRACT_FILES:
        _mark(
            jobs,
            images,
            job_names=("hygiene", "docker-validation"),
            image_names=IMAGES,
        )
        return True, True

    if path == ".dockerignore":
        _mark(
            jobs,
            images,
            job_names=("hygiene", "docker-validation"),
        )
        return True, True

    if path in DOCKERFILE_IMAGES:
        _mark(
            jobs,
            images,
            job_names=("hygiene", "docker-validation"),
            image_names=DOCKERFILE_IMAGES[path],
        )
        return True, True
    if path == "deploy/nats-server.conf":
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "docker-validation",
                "jetstream-integration",
            ),
            image_names=("phoenix-engine",),
        )
        return True, True
    if _under(path, "deploy/"):
        _mark(jobs, images, job_names=("hygiene", "docker-validation"))
        return True, True

    if _under(path, "dashboard/"):
        _mark(
            jobs,
            images,
            job_names=("hygiene", "python-dashboard", "docker-validation"),
            image_names=("dashboard",),
        )
        return True, True

    if _under(path, "prometheus/"):
        _mark(
            jobs,
            images,
            job_names=("hygiene", "docker-validation"),
        )
        return True, False

    if _under(path, "atlas-observer/"):
        _mark(
            jobs,
            images,
            job_names=("hygiene", "go", "docker-validation"),
            image_names=("atlas-observer",),
        )
        return True, True

    if _under(path, "feed-ingestor/", "migration-runner/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "go",
                "docker-validation",
                "integration-fixtures",
                "jetstream-integration",
            ),
            image_names=("feed-ingestor",),
        )
        return True, True

    if _under(path, "phoenix-telegram-ops/"):
        # The Telegram operations sidecar is compiled into the dashboard
        # image (deploy/dashboard.Dockerfile multi-stage Go builder), so its
        # source changes must rebuild that image or the fix never reaches
        # production.
        _mark(
            jobs,
            images,
            job_names=("hygiene", "go", "docker-validation"),
            image_names=("dashboard",),
        )
        return True, True

    if _under(path, "rpc-gateway/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "rust-rpc-gateway",
                "rust-phoenix",
                "rust-replay",
                "rust-fork-sandbox",
                "docker-validation",
            ),
            image_names=(
                "rpc-gateway",
                "phoenix-engine",
                "fork-sandbox",
                "live-executor",
            ),
        )
        return True, True

    if _under(path, "money-path-classifier/", "recorder/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "rust-recorder",
                "rust-phoenix",
                "rust-replay",
                "jetstream-integration",
                "docker-validation",
            ),
            image_names=("recorder", "phoenix-engine"),
        )
        return True, True

    if _under(path, "phoenix-engine/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "rust-phoenix",
                "rust-replay",
                "integration-fixtures",
                "jetstream-integration",
                "docker-validation",
            ),
            image_names=("phoenix-engine",),
        )
        return True, True

    if _under(path, "replay/"):
        _mark(jobs, images, job_names=("hygiene", "rust-replay"))
        return True, False

    if _under(path, "fork-sandbox/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "rust-fork-sandbox",
                "docker-validation",
            ),
            image_names=("fork-sandbox", "live-executor"),
        )
        return True, True

    if _under(path, "live-executor/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "rust-fork-sandbox",
                "docker-validation",
            ),
            image_names=("live-executor",),
        )
        return True, True

    if _under(path, "autonomous-live-e2e/"):
        _mark(jobs, images, job_names=("hygiene", "rust-fork-sandbox"))
        return True, False

    if _under(path, "contracts/"):
        _mark(
            jobs,
            images,
            job_names=("hygiene", "solidity", "rust-fork-sandbox"),
        )
        return True, False

    if _under(path, "migrations/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "docker-validation",
            ),
            image_names=(
                "feed-ingestor",
                "phoenix-engine",
                "recorder",
                "fork-sandbox",
                "live-executor",
                "rpc-gateway",
            ),
        )
        return True, True

    if _under(path, "fixtures/dashboard/"):
        _mark(jobs, images, job_names=("hygiene", "python-dashboard"))
        return True, False

    if _under(path, "fixtures/", "config/"):
        _mark(
            jobs,
            images,
            job_names=(
                "hygiene",
                "rust-phoenix",
                "rust-fork-sandbox",
                "integration-fixtures",
                "docker-validation",
            ),
            image_names=("phoenix-engine", "fork-sandbox", "live-executor"),
        )
        return True, True

    if path in RELEASE_FILES or _under(path, *RELEASE_ROOTS):
        _mark(jobs, images, job_names=("hygiene",))
        if path in RELEASE_FILES:
            jobs.add("docker-validation")
        return True, docker_static

    if path in {
        ".gitignore",
        ".gitattributes",
        ".github/CODEOWNERS",
        "Makefile",
        "pyproject.toml",
    }:
        _mark(jobs, images, job_names=("hygiene",))
        return True, False

    return False, docker_static


def classify(paths: Iterable[str]) -> dict[str, object]:
    checked = sorted({_safe_path(path) for path in paths if path})
    if not checked:
        raise ImpactError("changed path set is empty")

    jobs: set[str] = set()
    images: set[str] = set()
    docker_static = False
    unknown: list[str] = []
    for path in checked:
        known, static = _classify_path(path, jobs, images)
        docker_static = docker_static or static
        if not known:
            unknown.append(path)

    if unknown:
        jobs.update(JOBS)
        images.update(IMAGES)
        docker_static = True

    docs_only = all(path in DOC_FILES or _under(path, "docs/") for path in checked)
    if docs_only:
        jobs.clear()
        images.clear()
        docker_static = False

    built = sorted(images)
    inherited = sorted(set(IMAGES) - images)
    release_only = (
        not docs_only
        and jobs <= {"hygiene", "docker-validation"}
        and not images
    )
    return {
        "schema": SCHEMA,
        "changed_paths": checked,
        "unknown_paths": unknown,
        "classification": (
            "docs-only"
            if docs_only
            else "release-only"
            if release_only
            else "application"
        ),
        "release_required": not docs_only,
        "docker_static": docker_static,
        "jobs": {name: name in jobs for name in JOBS},
        "built_images": built,
        "inherited_images": inherited,
    }


def _git_paths(base: str, head: str) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", "--diff-filter=ACDMRT", base, head],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise ImpactError("git changed-path resolution failed")
    return result.stdout.splitlines()


def _git_json(ref: str, path: str) -> dict[str, object] | None:
    result = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        return None
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def _exact_atlas_registry_addition(
    base_registry: dict[str, object] | None,
    head_registry: dict[str, object] | None,
) -> bool:
    if base_registry is None or head_registry is None:
        return False
    if (
        base_registry.get("schema") != head_registry.get("schema")
        or base_registry.get("required_ci") != head_registry.get("required_ci")
    ):
        return False
    base_components = base_registry.get("components")
    head_components = head_registry.get("components")
    if not isinstance(base_components, list) or not isinstance(head_components, list):
        return False
    if not all(
        isinstance(item, dict) and isinstance(item.get("name"), str)
        for item in (*base_components, *head_components)
    ):
        return False
    base_by_name = {item["name"]: item for item in base_components}
    head_by_name = {item["name"]: item for item in head_components}
    if len(base_by_name) != len(base_components) or len(head_by_name) != len(
        head_components
    ):
        return False
    if set(head_by_name) - set(base_by_name) != {"atlas-observer"}:
        return False
    if set(base_by_name) - set(head_by_name):
        return False
    if any(
        head_by_name[name] != component
        for name, component in base_by_name.items()
    ):
        return False
    return (
        head_by_name["atlas-observer"]
        == release_components.COMPONENTS_BY_NAME["atlas-observer"]
    )


def classify_git(base: str, head: str) -> dict[str, object]:
    paths = _git_paths(base, head)
    plan = classify(paths)
    if "release-components.json" not in paths:
        return plan
    if not _exact_atlas_registry_addition(
        _git_json(base, "release-components.json"),
        _git_json(head, "release-components.json"),
    ):
        return plan
    other_paths = [path for path in paths if path != "release-components.json"]
    built = {"atlas-observer"} & set(IMAGES)
    if other_paths:
        other_plan = classify(other_paths)
        built.update(set(other_plan["built_images"]) & set(IMAGES))
    plan["built_images"] = sorted(built)
    plan["inherited_images"] = sorted(set(IMAGES) - built)
    return plan


def _canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _write_plan(value: dict[str, object], output: Path | None) -> None:
    rendered = json.dumps(value, indent=2, sort_keys=True) + "\n"
    if output is None:
        print(rendered, end="")
    else:
        output.write_text(rendered, encoding="utf-8")


def _github_output(value: dict[str, object]) -> None:
    jobs = value["jobs"]
    if not isinstance(jobs, dict):
        raise ImpactError("impact job map is invalid")
    print(f"plan={_canonical(value)}")
    print(f"classification={value['classification']}")
    print(f"release_required={str(value['release_required']).lower()}")
    print(f"docker_static={str(value['docker_static']).lower()}")
    print(f"built_images={_canonical(value['built_images'])}")
    print(f"inherited_images={_canonical(value['inherited_images'])}")
    for name in JOBS:
        output_name = f"job_{name.replace('-', '_')}"
        print(f"{output_name}={str(jobs[name]).lower()}")


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(description=__doc__)
    commands = value.add_subparsers(dest="command", required=True)
    paths = commands.add_parser("paths")
    paths.add_argument("--paths", required=True, type=Path)
    paths.add_argument("--output", type=Path)
    git = commands.add_parser("git")
    git.add_argument("--base", required=True)
    git.add_argument("--head", required=True)
    git.add_argument("--output", type=Path)
    github = commands.add_parser("github-output")
    github.add_argument("--base", required=True)
    github.add_argument("--head", required=True)
    return value


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.command == "paths":
            plan = classify(args.paths.read_text(encoding="utf-8").splitlines())
            _write_plan(plan, args.output)
        else:
            plan = classify_git(args.base, args.head)
            if args.command == "github-output":
                _github_output(plan)
            else:
                _write_plan(plan, args.output)
        return 0
    except (ImpactError, OSError, UnicodeError) as exc:
        print(f"CHANGE_IMPACT_FAILED: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
