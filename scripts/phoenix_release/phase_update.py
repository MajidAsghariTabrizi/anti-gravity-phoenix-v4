#!/usr/bin/env python3
"""Narrow state callback used by the reviewed deployment shell."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

PACKAGE_PARENT = Path(__file__).resolve().parent.parent
if str(PACKAGE_PARENT) not in sys.path:
    sys.path.insert(0, str(PACKAGE_PARENT))

from phoenix_release.gateway import (  # noqa: E402
    GatewayError,
    host_paths,
    json_error,
    json_result,
    mark_release_failure,
    mark_release_phase,
    mark_mutation_started,
    mark_owner_transaction,
    mark_engine_baseline,
    mark_rollback,
)
from phoenix_release.model import FAILURE_PHASES, PHASES, SHA_RE, StateError  # noqa: E402


def _sha(value: str) -> str:
    if not SHA_RE.fullmatch(value):
        raise argparse.ArgumentTypeError("invalid release SHA")
    return value


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    value.add_argument("release_sha", type=_sha)
    value.add_argument(
        "operation",
        choices=("phase", "engine-baseline", "mutation", "failure", "rollback"),
    )
    value.add_argument("value")
    value.add_argument("--code")
    value.add_argument("--result", choices=("ok", "failed"))
    value.add_argument("--transaction-hash")
    value.add_argument("--container-id")
    value.add_argument("--restart-count", type=int)
    value.add_argument("--terminal-integrity", type=int)
    value.add_argument("--process-fatal-integrity", type=int)
    return value


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        if arguments.operation == "phase":
            if arguments.value not in PHASES:
                raise GatewayError("PHASE_INVALID")
            state = mark_release_phase(
                host_paths(), arguments.release_sha, arguments.value
            )
            if arguments.transaction_hash:
                state = mark_owner_transaction(
                    host_paths(),
                    arguments.release_sha,
                    arguments.transaction_hash,
                )
        elif arguments.operation == "engine-baseline":
            if arguments.value != "ENGINE_BURN_IN_STARTED":
                raise GatewayError("PHASE_INVALID")
            state = mark_engine_baseline(
                host_paths(),
                arguments.release_sha,
                container_id=arguments.container_id or "",
                restart_count=arguments.restart_count
                if arguments.restart_count is not None
                else -1,
                terminal_integrity=arguments.terminal_integrity
                if arguments.terminal_integrity is not None
                else -1,
                process_fatal_integrity=arguments.process_fatal_integrity
                if arguments.process_fatal_integrity is not None
                else -1,
            )
        elif arguments.operation == "mutation":
            state = mark_mutation_started(host_paths(), arguments.release_sha)
        elif arguments.operation == "failure":
            state = mark_release_failure(
                host_paths(),
                arguments.release_sha,
                arguments.code or "DEPLOYMENT_FAILED",
                {"source": "deploy-release", "detail": arguments.value[:256]},
            )
        else:
            if arguments.value not in FAILURE_PHASES:
                raise GatewayError("ROLLBACK_PHASE_INVALID")
            result = None
            if arguments.result:
                result = {"status": arguments.result}
            state = mark_rollback(
                host_paths(), arguments.release_sha, arguments.value, result
            )
        print(json_result({"status": "ok", "phase": state["current_phase"]}))
        return 0
    except (GatewayError, StateError) as exc:
        error = exc if isinstance(exc, GatewayError) else GatewayError("STATE_UPDATE_FAILED")
        print(json_error(error, "STATE_UPDATE"), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
