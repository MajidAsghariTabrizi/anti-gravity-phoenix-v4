#!/usr/bin/env python3
"""Analyze and persist SHADOW Atlas validation evidence (read-only).

Reads the single-row JSON emitted by scripts/sql/shadow-atlas-validation.sql
on stdin, validates its shape, computes integer basis-point ratios, enforces
the zero-invariants (any violation fails the report), and either prints the
normalized report or writes it atomically into an output directory.

This tool never writes to any database and never authorizes execution.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

REPORT_SCHEMA = "phoenix.atlas-shadow-validation.v1"
MAX_INPUT_BYTES = 1 * 1024 * 1024
ISO_PATTERN = re.compile(
    r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z\Z"
)
SIGNED_INTEGER_PATTERN = re.compile(r"-?(?:0|[1-9][0-9]*)\Z")
OUTPUT_NAME_PATTERN = re.compile(
    r"phoenix-atlas-shadow-validation-[0-9]{4}-[0-9]{2}-[0-9]{2}\.json\Z"
)

COUNT_KEYS = {
    "coverage": ("relevant_ingress", "shadow_evaluated"),
    "callback_simulation": ("attempted", "passed"),
    "bid_ability": (
        "evaluated_rows",
        "eligible_rows",
        "eligible_with_maximum_bid",
        "rejected_rows",
    ),
    "zero_invariants": (
        "atlas_solver_requests_total",
        "execution_requests_total",
        "active_attempts",
        "unresolved_submissions",
        "eligible_rows_with_rejection_reason",
        "eligible_rows_without_maximum_bid",
    ),
}
SUM_KEYS = ("expected_net_after_bid_sum", "conservative_net_after_bid_sum")


class ValidationError(ValueError):
    pass


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, child in pairs:
        if key in value:
            raise ValidationError(f"duplicate JSON key: {key}")
        value[key] = child
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Analyze and persist SHADOW Atlas validation evidence."
    )
    parser.add_argument(
        "--format",
        choices=("json", "text"),
        default="text",
        help="Report rendering format",
    )
    parser.add_argument(
        "--output-dir",
        help="Directory for the atomic JSON report artifact",
    )
    parser.add_argument(
        "--window-start",
        help="Expected window start; when set it must match the payload",
    )
    parser.add_argument(
        "--window-end",
        help="Expected window end; when set it must match the payload",
    )
    return parser.parse_args()


def require_iso(value: Any, field: str) -> str:
    if not isinstance(value, str) or ISO_PATTERN.fullmatch(value) is None:
        raise ValidationError(f"window_{field}_invalid")
    return value


def require_count(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValidationError(f"count_{field}_invalid")
    return value


def require_sum(value: Any, field: str) -> str:
    if not isinstance(value, str) or SIGNED_INTEGER_PATTERN.fullmatch(value) is None:
        raise ValidationError(f"sum_{field}_invalid")
    return value


def basis_points(numerator: int, denominator: int) -> int | None:
    if denominator == 0:
        return None
    return min((numerator * 10000 + denominator // 2) // denominator, 10000)


def analyze(raw: dict[str, Any]) -> dict[str, Any]:
    if raw.get("schema") != REPORT_SCHEMA:
        raise ValidationError("schema_identity_invalid")
    window = raw.get("window")
    if not isinstance(window, dict):
        raise ValidationError("window_shape_invalid")
    window_start = require_iso(window.get("start"), "start")
    window_end = require_iso(window.get("end"), "end")
    if window_start >= window_end:
        raise ValidationError("window_order_invalid")

    sections: dict[str, dict[str, int]] = {}
    for section, keys in COUNT_KEYS.items():
        child = raw.get(section)
        if not isinstance(child, dict):
            raise ValidationError(f"section_{section}_invalid")
        sections[section] = {
            key: require_count(child.get(key), f"{section}_{key}") for key in keys
        }

    value_proxy = raw.get("value_proxy")
    if not isinstance(value_proxy, dict):
        raise ValidationError("value_proxy_shape_invalid")
    sums = {
        key: require_sum(value_proxy.get(key), key) for key in SUM_KEYS
    }

    coverage = sections["coverage"]
    callback = sections["callback_simulation"]
    bid_ability = sections["bid_ability"]
    zero = sections["zero_invariants"]

    violations = [key for key, value in zero.items() if value != 0]
    if violations:
        raise ValidationError(
            "zero_invariants_violated:" + ",".join(sorted(violations))
        )

    warnings: list[str] = []
    if coverage["shadow_evaluated"] > coverage["relevant_ingress"]:
        warnings.append("shadow_evaluated_exceeds_relevant_ingress")
    if callback["passed"] > callback["attempted"]:
        warnings.append("callback_passed_exceeds_attempted")
    if bid_ability["eligible_with_maximum_bid"] > bid_ability["eligible_rows"]:
        warnings.append("eligible_with_maximum_bid_exceeds_eligible_rows")
    if (
        bid_ability["eligible_rows"] + bid_ability["rejected_rows"]
        != bid_ability["evaluated_rows"]
    ):
        warnings.append("eligible_plus_rejected_does_not_match_evaluated")

    return {
        "schema": REPORT_SCHEMA,
        "window": {"start": window_start, "end": window_end},
        "coverage": {
            **coverage,
            "svr_coverage_bp": basis_points(
                coverage["shadow_evaluated"], coverage["relevant_ingress"]
            ),
        },
        "callback_simulation": {
            **callback,
            "success_bp": basis_points(callback["passed"], callback["attempted"]),
        },
        "bid_ability": bid_ability,
        "value_proxy": sums,
        "zero_invariants": zero,
        "warnings": warnings,
        "mode": "SHADOW",
        "financial_authority": "CLOSED",
        "realization_status": "not realized; SHADOW evidence only",
    }


def atomic_write(output: Path, payload: bytes) -> None:
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            prefix=".atlas-shadow-validation-",
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


def render_text(report: dict[str, Any]) -> str:
    coverage = report["coverage"]
    callback = report["callback_simulation"]
    bid_ability = report["bid_ability"]
    lines = [
        "Shadow Atlas validation report",
        f"Window: {report['window']['start']} .. {report['window']['end']}",
        f"Mode: {report['mode']}; financial authority: {report['financial_authority']}",
        f"SVR coverage: {coverage['svr_coverage_bp']} bp "
        f"({coverage['shadow_evaluated']}/{coverage['relevant_ingress']})",
        f"Callback success: {callback['success_bp']} bp "
        f"({callback['passed']}/{callback['attempted']})",
        f"Bid ability: {bid_ability['eligible_with_maximum_bid']} eligible "
        f"rows with maximum bid ({bid_ability['eligible_rows']} eligible, "
        f"{bid_ability['rejected_rows']} rejected, "
        f"{bid_ability['evaluated_rows']} evaluated)",
        "Value proxy (wei, SHADOW only): "
        f"expected_net_after_bid_sum={report['value_proxy']['expected_net_after_bid_sum']} "
        "conservative_net_after_bid_sum="
        f"{report['value_proxy']['conservative_net_after_bid_sum']}",
        "Zero invariants: all zero",
        "Realization status: not realized; SHADOW evidence only",
    ]
    if report["warnings"]:
        lines.append("Warnings: " + ", ".join(report["warnings"]))
    return "\n".join(lines) + "\n"


def main() -> int:
    args = parse_args()
    if args.window_start is not None:
        require_iso(args.window_start, "start")
    if args.window_end is not None:
        require_iso(args.window_end, "end")

    raw_bytes = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw_bytes) > MAX_INPUT_BYTES:
        print("atlas shadow validation failed: input exceeds size bound", file=sys.stderr)
        return 1
    try:
        raw = json.loads(
            raw_bytes.decode("utf-8"), object_pairs_hook=reject_duplicate_keys
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        print(f"atlas shadow validation failed: malformed input: {exc}", file=sys.stderr)
        return 1
    if not isinstance(raw, dict):
        print("atlas shadow validation failed: input is not an object", file=sys.stderr)
        return 1

    try:
        report = analyze(raw)
    except ValidationError as exc:
        print(f"atlas shadow validation failed: {exc}", file=sys.stderr)
        return 1

    if args.window_start is not None and report["window"]["start"] != args.window_start:
        print("atlas shadow validation failed: window start mismatch", file=sys.stderr)
        return 1
    if args.window_end is not None and report["window"]["end"] != args.window_end:
        print("atlas shadow validation failed: window end mismatch", file=sys.stderr)
        return 1

    payload = json.dumps(
        report, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("utf-8")

    if args.output_dir is not None:
        output_dir = Path(args.output_dir)
        if not output_dir.is_dir():
            print("atlas shadow validation failed: output dir is unavailable", file=sys.stderr)
            return 1
        date = report["window"]["start"][:10]
        output = output_dir / f"phoenix-atlas-shadow-validation-{date}.json"
        if OUTPUT_NAME_PATTERN.fullmatch(output.name) is None:
            print("atlas shadow validation failed: output name is invalid", file=sys.stderr)
            return 1
        atomic_write(output, payload)

    if args.format == "json":
        sys.stdout.buffer.write(payload + b"\n")
    else:
        sys.stdout.write(render_text(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
