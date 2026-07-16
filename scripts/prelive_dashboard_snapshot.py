#!/usr/bin/env python3
"""Validate and atomically promote a bounded PRE-LIVE Dashboard snapshot."""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_MODEL_DIR = SCRIPT_DIR.parent / "dashboard"
sys.path.insert(0, str(REPO_MODEL_DIR if REPO_MODEL_DIR.is_dir() else SCRIPT_DIR))

from snapshot_model import (  # noqa: E402
    SnapshotError,
    canonical_snapshot_bytes,
    load_snapshot,
    read_artifact,
)


SAFE_OUTPUT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate and atomically promote a redacted PRE-LIVE Dashboard snapshot."
    )
    parser.add_argument(
        "--input", required=True, help="Candidate snapshot in the evidence directory"
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Promoted snapshot in the same evidence directory",
    )
    parser.add_argument(
        "--check", action="store_true", help="Validate without writing the output"
    )
    return parser.parse_args()


def validate_paths(source: Path, output: Path) -> None:
    if SAFE_OUTPUT_RE.fullmatch(output.name) is None:
        raise SnapshotError("snapshot_output_path_invalid")
    try:
        source_parent = source.parent.resolve(strict=True)
        output_parent = output.parent.resolve(strict=True)
    except OSError as exc:
        raise SnapshotError("snapshot_output_path_invalid") from exc
    if source_parent != output_parent:
        raise SnapshotError("snapshot_output_path_invalid")


def atomic_write(output: Path, payload: bytes) -> None:
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            prefix=".dashboard-snapshot-",
            suffix=".tmp",
            dir=output.parent,
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, 0o644)
        os.replace(temporary, output)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main() -> int:
    args = parse_args()
    source = Path(args.input)
    output = Path(args.output)
    try:
        validate_paths(source, output)
        snapshot = load_snapshot(source)
        for artifact in snapshot.data["artifacts"]:
            if artifact["available"]:
                read_artifact(snapshot, artifact)
        payload = canonical_snapshot_bytes(snapshot.data)
        if not args.check:
            atomic_write(output, payload)
    except SnapshotError as exc:
        print(f"DASHBOARD_SNAPSHOT_ERROR: {exc.code}", file=sys.stderr)
        return 2
    except OSError:
        print("DASHBOARD_SNAPSHOT_ERROR: snapshot_write_failed", file=sys.stderr)
        return 2
    action = "validated" if args.check else "promoted"
    print(
        f"DASHBOARD_SNAPSHOT_OK: action={action} gate={snapshot.gate_status} "
        f"alerts={len(snapshot.alerts)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
