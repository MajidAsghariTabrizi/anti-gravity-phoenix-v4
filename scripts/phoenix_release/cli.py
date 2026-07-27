#!/usr/bin/env python3
"""Versioned command-line entry point for the root release gateway."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Support explicit isolated-mode execution from a noexec release tree.
PACKAGE_PARENT = Path(__file__).resolve().parent.parent
if str(PACKAGE_PARENT) not in sys.path:
    sys.path.insert(0, str(PACKAGE_PARENT))

from phoenix_release.gateway import (  # noqa: E402
    GatewayError,
    evidence,
    emergency_pause,
    history,
    host_paths,
    json_error,
    json_result,
    receive_package,
    reconcile_active_context,
    rollback_release,
    resume,
    status,
)
from phoenix_release.model import PROTOCOL_VERSION, SHA_RE  # noqa: E402


def _sha(value: str) -> str:
    if not SHA_RE.fullmatch(value):
        raise argparse.ArgumentTypeError("expected lowercase 40-character SHA")
    return value


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser(prog="phoenix-release")
    value.add_argument("--protocol", default=PROTOCOL_VERSION)
    subparsers = value.add_subparsers(dest="command", required=True)
    subparsers.add_parser("status")
    subparsers.add_parser("history")
    receive = subparsers.add_parser("receive")
    receive.add_argument("release_sha", type=_sha)
    plan = subparsers.add_parser("plan")
    plan.add_argument("release_sha", type=_sha)
    resume_parser = subparsers.add_parser("resume")
    resume_parser.add_argument("release_sha", type=_sha)
    rollback = subparsers.add_parser("rollback")
    rollback.add_argument("release_sha", type=_sha)
    subparsers.add_parser("emergency-pause")
    evidence_parser = subparsers.add_parser("evidence")
    evidence_parser.add_argument("release_sha", type=_sha)
    subparsers.add_parser("reconcile-active-context")
    return value


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    if arguments.protocol != PROTOCOL_VERSION:
        print(
            json_error(GatewayError("PROTOCOL_VERSION_UNSUPPORTED")),
            file=sys.stderr,
        )
        return 2
    paths = host_paths()
    try:
        if arguments.command == "status":
            result = status(paths)
        elif arguments.command == "history":
            result = {
                "schema": "phoenix.release-history.v1",
                "releases": history(paths),
            }
        elif arguments.command == "receive":
            request = receive_package(
                sys.stdin.buffer,
                paths,
                expected_release_sha=arguments.release_sha,
            )
            result = {
                "schema": "phoenix.release-receive.v1",
                "status": "accepted",
                "release_sha": arguments.release_sha,
            }
        elif arguments.command == "plan":
            result = evidence(paths, arguments.release_sha)
        elif arguments.command == "resume":
            result = resume(paths, arguments.release_sha)
        elif arguments.command == "evidence":
            result = evidence(paths, arguments.release_sha)
        elif arguments.command == "reconcile-active-context":
            result = reconcile_active_context(paths)
        elif arguments.command == "rollback":
            result = rollback_release(paths, arguments.release_sha)
        elif arguments.command == "emergency-pause":
            result = emergency_pause(paths)
        else:
            raise GatewayError("COMMAND_UNSUPPORTED")
        print(json_result(result))
        return 0
    except GatewayError as exc:
        print(json_error(exc), file=sys.stderr)
        return 1
    except Exception:
        # Never leak paths, environment data, or arbitrary exception text.
        print(json_error(GatewayError("GATEWAY_INTERNAL_ERROR")), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
