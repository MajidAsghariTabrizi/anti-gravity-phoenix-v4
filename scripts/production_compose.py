#!/usr/bin/env python3
"""Build and execute the one canonical Phoenix Production Compose command."""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path
from typing import Mapping, Sequence


MODES = ("SHADOW", "LIVE")


class ProductionComposeError(ValueError):
    """The requested Production Compose context is unsafe or incomplete."""


def _is_absolute(path: Path) -> bool:
    # Tests run on Windows while the installed executor is Linux-only.
    return path.is_absolute() or path.as_posix().startswith("/")


def build_compose_command(
    *,
    mode: str,
    env_file: Path,
    release_env: Path,
    compose_file: Path,
    overlay_file: Path | None,
    arguments: Sequence[str],
    docker_binary: Path = Path("/usr/bin/docker"),
    compose_binary: Path | None = None,
    project_directory: Path | None = None,
) -> list[str]:
    """Return the deterministic, duplicate-free Docker Compose argv."""
    if mode not in MODES:
        raise ProductionComposeError("mode must be SHADOW or LIVE")
    required = (env_file, release_env, compose_file)
    if any(not _is_absolute(path) for path in required):
        raise ProductionComposeError("Compose inputs must be absolute paths")
    command_binary = compose_binary or docker_binary
    if not _is_absolute(command_binary):
        raise ProductionComposeError("Compose executable must be an absolute path")
    effective_project = project_directory or compose_file.parent
    if not _is_absolute(effective_project):
        raise ProductionComposeError("project directory must be an absolute path")
    if mode == "LIVE":
        if overlay_file is None or not _is_absolute(overlay_file):
            raise ProductionComposeError("LIVE mode requires an absolute overlay")
    elif overlay_file is not None:
        raise ProductionComposeError("SHADOW mode must not include an overlay")
    if not arguments or arguments[0] == "--":
        raise ProductionComposeError("a Docker Compose operation is required")

    command = [
        command_binary.as_posix(),
    ]
    if compose_binary is None:
        command.append("compose")
    command.extend(
        [
            "--env-file",
            env_file.as_posix(),
            "--env-file",
            release_env.as_posix(),
            "-f",
            compose_file.as_posix(),
        ]
    )
    if mode == "LIVE":
        command.extend(
            [
                "-f",
                overlay_file.as_posix(),
                "--profile",
                "live-autonomous",
            ]
        )
    command.extend(["--project-directory", effective_project.as_posix()])
    command.extend(arguments)
    return command


def compose_environment(
    env_file: Path,
    release_env: Path,
    source: Mapping[str, str] | None = None,
) -> dict[str, str]:
    environment = dict(os.environ if source is None else source)
    environment["PHOENIX_ENV_FILE"] = str(env_file)
    environment["PHOENIX_RELEASE_ENV"] = str(release_env)
    return environment


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", required=True, choices=MODES)
    parser.add_argument("--env-file", required=True, type=Path)
    parser.add_argument("--release-env", required=True, type=Path)
    parser.add_argument("--compose-file", required=True, type=Path)
    parser.add_argument("--overlay-file", type=Path)
    parser.add_argument("--project-directory", type=Path)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    return parser


def main(argv: list[str] | None = None) -> int:
    parsed = _parser().parse_args(argv)
    arguments = parsed.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    compose_override = os.environ.get("PHOENIX_COMPOSE_BIN")
    compose_binary = Path(compose_override) if compose_override else None
    docker_binary = Path(
        os.environ.get("PHOENIX_DOCKER_BIN")
        or shutil.which("docker")
        or "/usr/bin/docker"
    )
    try:
        command = build_compose_command(
            mode=parsed.mode,
            env_file=parsed.env_file,
            release_env=parsed.release_env,
            compose_file=parsed.compose_file,
            overlay_file=parsed.overlay_file,
            arguments=arguments,
            docker_binary=docker_binary,
            compose_binary=compose_binary,
            project_directory=parsed.project_directory,
        )
    except ProductionComposeError as exc:
        print(f"PRODUCTION_COMPOSE_FAILED: {exc}", file=sys.stderr)
        return 64
    os.execvpe(
        command[0],
        command,
        compose_environment(parsed.env_file, parsed.release_env),
    )
    return 127


if __name__ == "__main__":
    raise SystemExit(main())
