"""Pure GitHub-side release eligibility checks."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import PurePosixPath
from typing import Any, Iterable


REQUIRED_JOBS = (
    "hygiene",
    "go",
    "rust-phoenix",
    "rust-rpc-gateway",
    "rust-recorder",
    "rust-replay",
    "rust-fork-sandbox",
    "solidity",
    "python-dashboard",
    "docker-validation",
    "integration-fixtures",
    "jetstream-integration",
)

DOCS_ROOTS = ("docs/",)
DOCS_FILES = (
    "README.md",
    "LICENSE",
    "SECURITY.md",
    "CODE_OF_CONDUCT.md",
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")


class ControllerError(ValueError):
    """Release eligibility evidence was invalid."""


def _load(path: str) -> Any:
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ControllerError(f"invalid JSON evidence: {path}") from exc


def _safe_path(value: str) -> str:
    if not value or "\\" in value or "\x00" in value:
        raise ControllerError("changed path is invalid")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise ControllerError("changed path is invalid")
    return value


def is_docs_only(paths: Iterable[str]) -> bool:
    checked = [_safe_path(path.strip()) for path in paths if path.strip()]
    if not checked:
        return False
    return all(
        path in DOCS_FILES or any(path.startswith(root) for root in DOCS_ROOTS)
        for path in checked
    )


def validate_source_ci(
    run: object,
    jobs: object,
    *,
    repository: str,
    expected_sha: str,
    expected_run_id: int,
    expected_attempt: int,
    current_main_sha: str,
) -> dict[str, Any]:
    if not SHA_RE.fullmatch(expected_sha) or current_main_sha != expected_sha:
        raise ControllerError("source SHA is not the exact current main tip")
    if not isinstance(run, dict):
        raise ControllerError("source CI run evidence is invalid")
    expected = {
        "id": expected_run_id,
        "run_attempt": expected_attempt,
        "head_sha": expected_sha,
        "head_branch": "main",
        "event": "push",
        "status": "completed",
        "conclusion": "success",
        "name": "Phoenix CI",
    }
    for key, value in expected.items():
        if run.get(key) != value:
            raise ControllerError(f"source CI run field is invalid: {key}")
    repository_value = run.get("repository")
    if not isinstance(repository_value, dict) or repository_value.get("full_name") != repository:
        raise ControllerError("source CI repository identity is invalid")
    if not isinstance(jobs, dict) or not isinstance(jobs.get("jobs"), list):
        raise ControllerError("source CI jobs evidence is invalid")
    job_results: dict[str, str] = {}
    for job in jobs["jobs"]:
        if not isinstance(job, dict) or not isinstance(job.get("name"), str):
            raise ControllerError("source CI job evidence is invalid")
        name = job["name"]
        if name in REQUIRED_JOBS:
            if name in job_results:
                raise ControllerError(f"duplicate source CI job: {name}")
            if job.get("status") != "completed" or job.get("conclusion") != "success":
                raise ControllerError(f"required source CI job did not pass: {name}")
            job_results[name] = "success"
    missing = sorted(set(REQUIRED_JOBS) - set(job_results))
    if missing:
        raise ControllerError(f"required source CI jobs are missing: {','.join(missing)}")
    return {
        "schema": "phoenix.release-source-ci.v1",
        "repository": repository,
        "release_sha": expected_sha,
        "run_id": expected_run_id,
        "run_attempt": expected_attempt,
        "jobs": job_results,
    }


def _command_verify(args: argparse.Namespace) -> int:
    evidence = validate_source_ci(
        _load(args.run),
        _load(args.jobs),
        repository=args.repository,
        expected_sha=args.expected_sha,
        expected_run_id=args.run_id,
        expected_attempt=args.run_attempt,
        current_main_sha=args.current_main_sha,
    )
    print(json.dumps(evidence, sort_keys=True, separators=(",", ":")))
    return 0


def _command_impact(args: argparse.Namespace) -> int:
    with open(args.paths, encoding="utf-8") as handle:
        paths = [line.rstrip("\n") for line in handle]
    print("docs-only" if is_docs_only(paths) else "release")
    return 0


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    subparsers = value.add_subparsers(dest="command", required=True)
    verify = subparsers.add_parser("verify-source-ci")
    verify.add_argument("--run", required=True)
    verify.add_argument("--jobs", required=True)
    verify.add_argument("--repository", required=True)
    verify.add_argument("--expected-sha", required=True)
    verify.add_argument("--run-id", required=True, type=int)
    verify.add_argument("--run-attempt", required=True, type=int)
    verify.add_argument("--current-main-sha", required=True)
    verify.set_defaults(handler=_command_verify)
    impact = subparsers.add_parser("classify-impact")
    impact.add_argument("--paths", required=True)
    impact.set_defaults(handler=_command_impact)
    return value


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        return int(args.handler(args))
    except ControllerError as exc:
        print(
            json.dumps(
                {
                    "status": "error",
                    "phase": "SOURCE_CI_VERIFIED",
                    "code": "SOURCE_CI_INVALID",
                    "evidence": {"message": str(exc)},
                },
                sort_keys=True,
            ),
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
