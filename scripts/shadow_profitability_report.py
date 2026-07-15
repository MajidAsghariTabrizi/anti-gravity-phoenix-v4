#!/usr/bin/env python3
"""Build deterministic, bounded SHADOW profitability reports from NDJSON rows."""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import json
import re
import sys
from typing import Any, Iterable


MAX_ROWS = 1_000
MAX_LINE_BYTES = 1_048_576
MAX_INPUT_BYTES = 16 * 1_048_576
INTEGER_PATTERN = re.compile(r"-?(?:0|[1-9][0-9]*)\Z")
ADDRESS_PATTERN = re.compile(r"0x[0-9a-f]{40}\Z")

SIGNED_FINANCIAL_FIELDS = (
    "expected_net_pnl",
    "conservative_net_pnl",
    "severe_net_pnl",
    "gross_spread",
    "gross_profit",
)
UNSIGNED_FINANCIAL_FIELDS = (
    "minimum_required_net_pnl",
    "input_amount",
    "expected_output",
    "execution_gas",
    "gas_price",
    "dex_fees",
    "price_impact",
    "arbitrum_execution_fee",
    "l1_data_fee",
    "flash_loan_premium",
    "protocol_fees",
    "failed_attempt_reserve",
    "ordering_reserve",
    "slippage_reserve",
    "stale_state_reserve",
    "state_drift_reserve",
    "latency_reserve",
    "uncertainty_reserve",
    "contract_overhead",
    "total_cost",
)
FINANCIAL_FIELDS = SIGNED_FINANCIAL_FIELDS + UNSIGNED_FINANCIAL_FIELDS
COST_FIELDS = (
    ("dex_fees", "DEX fees"),
    ("price_impact", "price impact"),
    ("arbitrum_execution_fee", "Arbitrum execution fee"),
    ("l1_data_fee", "L1 data fee"),
    ("flash_loan_premium", "flash-loan premium"),
    ("protocol_fees", "protocol fees"),
    ("failed_attempt_reserve", "failed-attempt reserve"),
    ("ordering_reserve", "ordering reserve"),
    ("slippage_reserve", "slippage reserve"),
    ("stale_state_reserve", "stale-state reserve"),
    ("state_drift_reserve", "state-drift reserve"),
    ("latency_reserve", "latency reserve"),
    ("uncertainty_reserve", "uncertainty reserve"),
    ("contract_overhead", "contract overhead"),
    ("total_cost", "total cost"),
)
REQUIRED_FIELDS = frozenset(
    {
        "candidate_key",
        "source_event_identity",
        "route_fingerprint",
        "settlement_asset",
        "evaluated_at",
        "evidence_completeness_status",
        "disposition",
        "primary_profitability_status",
        "final_rejection_reason",
        "secondary_rejection_reasons",
        "model_version",
        "verification_status",
        "agreement_state",
        "shadow_only",
        "execution_eligible",
        "execution_request_created",
        *FINANCIAL_FIELDS,
    }
)


class ReportError(ValueError):
    pass


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, child in pairs:
        if key in value:
            raise ReportError(f"duplicate JSON key: {key}")
        value[key] = child
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render bounded SHADOW expected profitability evidence from NDJSON."
    )
    parser.add_argument("--format", choices=("json", "text"), default="text")
    parser.add_argument("--limit", type=int, default=100)
    args = parser.parse_args()
    if not 1 <= args.limit <= MAX_ROWS:
        parser.error(f"--limit must be between 1 and {MAX_ROWS}")
    return args


def reject_floats(value: Any) -> None:
    if isinstance(value, float):
        raise ReportError("floating-point input is prohibited")
    if isinstance(value, dict):
        for child in value.values():
            reject_floats(child)
    elif isinstance(value, list):
        for child in value:
            reject_floats(child)


def bounded_text(value: Any, field: str, required: bool = True) -> str | None:
    if value is None and not required:
        return None
    if not isinstance(value, str) or not value or len(value) > 512:
        raise ReportError(f"{field} must be bounded non-empty text")
    if any(ord(character) < 32 or ord(character) == 127 for character in value):
        raise ReportError(f"{field} contains control characters")
    return value


def integer_field(row: dict[str, Any], field: str, required: bool) -> int | None:
    value = row[field]
    if value is None and not required:
        return None
    if not isinstance(value, str) or not INTEGER_PATTERN.fullmatch(value):
        raise ReportError(f"{field} must be a canonical base-10 integer string")
    digits = value.removeprefix("-")
    if len(digits) > 78:
        raise ReportError(f"{field} exceeds NUMERIC(78,0)")
    parsed = int(value)
    if field in UNSIGNED_FINANCIAL_FIELDS and parsed < 0:
        raise ReportError(f"{field} must be non-negative")
    return parsed


def validate_complete_arithmetic(row: dict[str, Any]) -> None:
    protocol_fees = row["_numbers"]["protocol_fees"]
    dex_fees = row["_numbers"]["dex_fees"]
    price_impact = row["_numbers"]["price_impact"]
    gross_spread = row["_numbers"]["gross_spread"]
    gross_profit = row["_numbers"]["gross_profit"]
    total_cost = row["_numbers"]["total_cost"]
    expected_net = row["_numbers"]["expected_net_pnl"]
    cost_total = sum(
        row["_numbers"][field]
        for field, _ in COST_FIELDS
        if field != "total_cost"
    )
    if gross_profit != gross_spread - protocol_fees - dex_fees - price_impact:
        raise ReportError("gross profit arithmetic is inconsistent")
    if (
        row["_numbers"]["arbitrum_execution_fee"]
        != row["_numbers"]["execution_gas"] * row["_numbers"]["gas_price"]
    ):
        raise ReportError("Arbitrum execution fee arithmetic is inconsistent")
    if total_cost != cost_total:
        raise ReportError("total cost arithmetic is inconsistent")
    if expected_net != gross_spread - total_cost:
        raise ReportError("expected net PnL arithmetic is inconsistent")
    if not (
        expected_net >= row["_numbers"]["conservative_net_pnl"]
        >= row["_numbers"]["severe_net_pnl"]
    ):
        raise ReportError("scenario PnL ordering is inconsistent")
    minimum = row["_numbers"]["minimum_required_net_pnl"]
    expected_status = "meets_minimum" if expected_net >= minimum else "below_minimum"
    if row["primary_profitability_status"] != expected_status:
        raise ReportError("primary profitability status is inconsistent")


def validate_row(raw: Any) -> dict[str, Any]:
    reject_floats(raw)
    if not isinstance(raw, dict) or frozenset(raw) != REQUIRED_FIELDS:
        raise ReportError("report row schema does not match the canonical projection")
    row = dict(raw)
    bounded_text(row["candidate_key"], "candidate_key")
    bounded_text(row["evaluated_at"], "evaluated_at")
    bounded_text(row["source_event_identity"], "source_event_identity", required=False)
    bounded_text(row["route_fingerprint"], "route_fingerprint", required=False)
    settlement_asset = bounded_text(
        row["settlement_asset"], "settlement_asset", required=False
    )
    if settlement_asset is not None and not ADDRESS_PATTERN.fullmatch(settlement_asset):
        raise ReportError("settlement_asset must be a canonical address")
    completeness = row["evidence_completeness_status"]
    if completeness not in {"complete", "incomplete"}:
        raise ReportError("invalid evidence completeness status")
    if row["disposition"] not in {None, "accepted", "rejected"}:
        raise ReportError("invalid disposition")
    if row["primary_profitability_status"] not in {
        "incomplete",
        "meets_minimum",
        "below_minimum",
    }:
        raise ReportError("invalid primary profitability status")
    if row["verification_status"] not in {
        "incomplete",
        "primary_only",
        "agreed",
        "disagreed",
        "secondary_unavailable",
        "historical_evidence",
    }:
        raise ReportError("invalid verification status")
    if row["agreement_state"] not in {
        "not_checked",
        "agreed",
        "disagreed",
        "unavailable",
    }:
        raise ReportError("invalid agreement state")
    bounded_text(row["final_rejection_reason"], "final_rejection_reason", required=False)
    bounded_text(row["model_version"], "model_version", required=False)
    if not isinstance(row["secondary_rejection_reasons"], list) or any(
        bounded_text(reason, "secondary_rejection_reason") is None
        for reason in row["secondary_rejection_reasons"]
    ):
        raise ReportError("secondary rejection reasons must be bounded text")
    if (
        row["shadow_only"] is not True
        or row["execution_eligible"] is not False
        or row["execution_request_created"] is not False
    ):
        raise ReportError("unsafe execution lifecycle evidence")

    complete = completeness == "complete"
    numbers = {
        field: integer_field(row, field, required=complete) for field in FINANCIAL_FIELDS
    }
    row["_numbers"] = numbers
    if complete:
        for field in (
            "source_event_identity",
            "route_fingerprint",
            "settlement_asset",
            "model_version",
            "disposition",
        ):
            if row[field] is None:
                raise ReportError(f"complete row is missing {field}")
        if row["primary_profitability_status"] == "incomplete":
            raise ReportError("complete row has incomplete profitability status")
        if row["verification_status"] in {"incomplete", "historical_evidence"}:
            raise ReportError("complete row has incomplete verification status")
        expected_agreement = {
            "primary_only": "not_checked",
            "agreed": "agreed",
            "disagreed": "disagreed",
            "secondary_unavailable": "unavailable",
        }[row["verification_status"]]
        if row["agreement_state"] != expected_agreement:
            raise ReportError("verification and agreement states are inconsistent")
        if row["disposition"] == "rejected" and row["final_rejection_reason"] is None:
            raise ReportError("rejected complete row is missing its final reason")
        validate_complete_arithmetic(row)
    return row


def load_rows(stream: Iterable[str], limit: int) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    total_bytes = 0
    candidate_keys: set[str] = set()
    for line_number, line in enumerate(stream, start=1):
        line_bytes = len(line.encode("utf-8"))
        total_bytes += line_bytes
        if line_bytes > MAX_LINE_BYTES or total_bytes > MAX_INPUT_BYTES:
            raise ReportError("NDJSON input exceeds the bounded byte budget")
        if not line.strip():
            continue
        if len(rows) >= limit:
            raise ReportError("NDJSON row count exceeds --limit")
        try:
            raw = json.loads(line, object_pairs_hook=reject_duplicate_keys)
        except json.JSONDecodeError as error:
            raise ReportError(f"invalid NDJSON at line {line_number}") from error
        row = validate_row(raw)
        if row["candidate_key"] in candidate_keys:
            raise ReportError("duplicate candidate_key in bounded report input")
        candidate_keys.add(row["candidate_key"])
        rows.append(row)
    return rows


def integer_distribution(values: list[int]) -> dict[str, Any]:
    ordered = sorted(values)
    if not ordered:
        return {
            "count": 0,
            "minimum": None,
            "p25_lower": None,
            "median_lower": None,
            "p75_lower": None,
            "maximum": None,
            "sum": None,
            "mean_numerator": None,
            "mean_denominator": 0,
        }

    def lower_quantile(numerator: int, denominator: int) -> int:
        index = ((len(ordered) - 1) * numerator) // denominator
        return ordered[index]

    total = sum(ordered)
    return {
        "count": len(ordered),
        "minimum": str(ordered[0]),
        "p25_lower": str(lower_quantile(1, 4)),
        "median_lower": str(lower_quantile(1, 2)),
        "p75_lower": str(lower_quantile(3, 4)),
        "maximum": str(ordered[-1]),
        "sum": str(total),
        "mean_numerator": str(total),
        "mean_denominator": len(ordered),
    }


def grouped_rows(rows: list[dict[str, Any]], field: str) -> dict[Any, list[dict[str, Any]]]:
    grouped: dict[Any, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        grouped[row[field]].append(row)
    return grouped


def asset_sums(rows: list[dict[str, Any]], field: str) -> list[dict[str, str]]:
    values = []
    for asset, asset_rows in sorted(grouped_rows(rows, "settlement_asset").items()):
        values.append(
            {
                "settlement_asset": asset,
                "sum": str(sum(row["_numbers"][field] for row in asset_rows)),
            }
        )
    return values


def build_report(rows: list[dict[str, Any]], limit: int) -> dict[str, Any]:
    complete = [row for row in rows if row["evidence_completeness_status"] == "complete"]
    incomplete = [row for row in rows if row["evidence_completeness_status"] == "incomplete"]

    route_counts = []
    for route, route_rows in grouped_rows(rows, "route_fingerprint").items():
        route_counts.append(
            {
                "route_fingerprint": route,
                "candidates": len(route_rows),
                "complete": sum(
                    row["evidence_completeness_status"] == "complete" for row in route_rows
                ),
                "incomplete": sum(
                    row["evidence_completeness_status"] == "incomplete" for row in route_rows
                ),
            }
        )
    route_counts.sort(key=lambda item: (item["route_fingerprint"] is None, item["route_fingerprint"] or ""))

    primary_reasons = Counter(
        row["final_rejection_reason"] for row in rows if row["final_rejection_reason"]
    )
    secondary_reasons = Counter(
        reason for row in rows for reason in row["secondary_rejection_reasons"]
    )
    rejection_reasons = [
        {
            "reason": reason,
            "primary_count": primary_reasons[reason],
            "secondary_count": secondary_reasons[reason],
            "total_mentions": primary_reasons[reason] + secondary_reasons[reason],
        }
        for reason in sorted(primary_reasons.keys() | secondary_reasons.keys())
    ]

    profitability_distribution = []
    for asset, asset_rows in sorted(grouped_rows(complete, "settlement_asset").items()):
        expected_values = [row["_numbers"]["expected_net_pnl"] for row in asset_rows]
        profitability_distribution.append(
            {
                "settlement_asset": asset,
                "expected_net_pnl": integer_distribution(expected_values),
                "negative_count": sum(value < 0 for value in expected_values),
                "zero_count": sum(value == 0 for value in expected_values),
                "positive_count": sum(value > 0 for value in expected_values),
            }
        )
    nearest = []
    for asset, asset_rows in sorted(grouped_rows(complete, "settlement_asset").items()):
        candidates = []
        for row in asset_rows:
            gap = (
                row["_numbers"]["minimum_required_net_pnl"]
                - row["_numbers"]["expected_net_pnl"]
            )
            if gap > 0:
                candidates.append(
                    {
                        "candidate_key": row["candidate_key"],
                        "route_fingerprint": row["route_fingerprint"],
                        "expected_net_pnl": str(row["_numbers"]["expected_net_pnl"]),
                        "minimum_required_net_pnl": str(
                            row["_numbers"]["minimum_required_net_pnl"]
                        ),
                        "gap_to_minimum": str(gap),
                    }
                )
        candidates.sort(
            key=lambda item: (int(item["gap_to_minimum"]), item["candidate_key"])
        )
        if candidates:
            nearest.append(
                {
                    "settlement_asset": asset,
                    "candidates": candidates[: min(10, limit)],
                    "financial_basis": "SHADOW expected",
                    "realization_status": "not realized",
                }
            )

    costs_by_asset = []
    for asset, asset_rows in sorted(grouped_rows(complete, "settlement_asset").items()):
        costs_by_asset.append(
            {
                "settlement_asset": asset,
                "complete_rows": len(asset_rows),
                "components": [
                    {
                        "component": label,
                        "field": field,
                        "total": str(sum(row["_numbers"][field] for row in asset_rows)),
                    }
                    for field, label in COST_FIELDS
                ],
                "execution_inputs": {
                    "execution_gas_sum": str(
                        sum(row["_numbers"]["execution_gas"] for row in asset_rows)
                    ),
                    "gas_price_minimum": str(
                        min(row["_numbers"]["gas_price"] for row in asset_rows)
                    ),
                    "gas_price_maximum": str(
                        max(row["_numbers"]["gas_price"] for row in asset_rows)
                    ),
                },
            }
        )

    verification_groups = []
    for status, status_rows in sorted(grouped_rows(rows, "verification_status").items()):
        status_complete = [
            row for row in status_rows if row["evidence_completeness_status"] == "complete"
        ]
        verification_groups.append(
            {
                "verification_status": status,
                "candidates": len(status_rows),
                "complete_candidates": len(status_complete),
                "expected_net_pnl_by_settlement_asset": asset_sums(
                    status_complete, "expected_net_pnl"
                ),
            }
        )
    rpc_failure_statuses = {"disagreed", "secondary_unavailable"}
    rpc_failure_rows = [row for row in rows if row["verification_status"] in rpc_failure_statuses]
    rpc_failure_complete = [
        row for row in rpc_failure_rows if row["evidence_completeness_status"] == "complete"
    ]

    stale_reason_rows = [
        row
        for row in rows
        if row["final_rejection_reason"] == "quote_stale"
        or "quote_stale" in row["secondary_rejection_reasons"]
    ]
    stale_reserve_rows = [
        row for row in complete if row["_numbers"]["stale_state_reserve"] > 0
    ]

    route_groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for row in complete:
        route_groups[(row["route_fingerprint"], row["settlement_asset"])].append(row)
    route_pnl = []
    for (route, asset), route_rows in sorted(route_groups.items()):
        expected = [row["_numbers"]["expected_net_pnl"] for row in route_rows]
        route_pnl.append(
            {
                "route_fingerprint": route,
                "settlement_asset": asset,
                "complete_candidates": len(route_rows),
                "expected_net_pnl_sum": str(sum(expected)),
                "conservative_net_pnl_sum": str(
                    sum(row["_numbers"]["conservative_net_pnl"] for row in route_rows)
                ),
                "severe_net_pnl_sum": str(
                    sum(row["_numbers"]["severe_net_pnl"] for row in route_rows)
                ),
                "expected_net_pnl_minimum": str(min(expected)),
                "expected_net_pnl_maximum": str(max(expected)),
                "financial_basis": "SHADOW expected",
                "realization_status": "not realized",
            }
        )
    model_groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for row in complete:
        model_groups[(row["model_version"], row["settlement_asset"])].append(row)
    model_comparison = []
    for (model, asset), model_rows in sorted(model_groups.items()):
        model_comparison.append(
            {
                "model_version": model,
                "settlement_asset": asset,
                "expected_net_pnl": integer_distribution(
                    [row["_numbers"]["expected_net_pnl"] for row in model_rows]
                ),
                "financial_basis": "SHADOW expected",
                "realization_status": "not realized",
            }
        )
    sensitivity = []
    for asset, asset_rows in sorted(grouped_rows(complete, "settlement_asset").items()):
        sensitivity.append(
            {
                "settlement_asset": asset,
                "expected_minus_conservative": integer_distribution(
                    [
                        row["_numbers"]["expected_net_pnl"]
                        - row["_numbers"]["conservative_net_pnl"]
                        for row in asset_rows
                    ]
                ),
                "expected_minus_severe": integer_distribution(
                    [
                        row["_numbers"]["expected_net_pnl"]
                        - row["_numbers"]["severe_net_pnl"]
                        for row in asset_rows
                    ]
                ),
            }
        )

    missing_counts = Counter()
    for row in incomplete:
        for field in FINANCIAL_FIELDS:
            if row["_numbers"][field] is None:
                missing_counts[field] += 1

    return {
        "report_contract": "phoenix.shadow.profitability.report.v1",
        "financial_basis": "SHADOW expected",
        "realization_status": "not realized",
        "scope": {
            "row_limit": limit,
            "rows_analyzed": len(rows),
            "bounded": True,
        },
        "candidate_funnel": {
            "candidates_observed": len(rows),
            "complete_evaluations": len(complete),
            "incomplete_candidates": len(incomplete),
            "accepted": sum(row["disposition"] == "accepted" for row in rows),
            "rejected": sum(row["disposition"] == "rejected" for row in rows),
            "meets_minimum": sum(
                row["primary_profitability_status"] == "meets_minimum" for row in rows
            ),
            "below_minimum": sum(
                row["primary_profitability_status"] == "below_minimum" for row in rows
            ),
        },
        "counts_by_route": route_counts,
        "rejection_reasons": rejection_reasons,
        "profitability_distribution": {
            "by_settlement_asset": profitability_distribution,
            "financial_basis": "SHADOW expected",
            "realization_status": "not realized",
        },
        "nearest_to_profitable": nearest,
        "cost_breakdown": {
            "by_settlement_asset": costs_by_asset,
            "financial_basis": "SHADOW expected",
            "realization_status": "not realized",
        },
        "rpc_failure_contribution": {
            "causality_claimed": False,
            "candidate_counts_by_verification_status": verification_groups,
            "failure_evidence_candidates": len(rpc_failure_rows),
            "failure_evidence_complete_candidates": len(rpc_failure_complete),
            "failure_evidence_expected_net_pnl_by_settlement_asset": asset_sums(
                rpc_failure_complete, "expected_net_pnl"
            ),
            "financial_basis": "SHADOW expected",
            "realization_status": "not realized",
        },
        "stale_state_contribution": {
            "causality_claimed": False,
            "quote_stale_reason_candidates": len(stale_reason_rows),
            "positive_stale_reserve_candidates": len(stale_reserve_rows),
            "stale_state_reserve_by_settlement_asset": asset_sums(
                stale_reserve_rows, "stale_state_reserve"
            ),
            "financial_basis": "SHADOW expected",
            "realization_status": "not realized",
        },
        "route_expected_pnl": route_pnl,
        "model_comparison": model_comparison,
        "sensitivity": {
            "by_settlement_asset": sensitivity,
            "financial_basis": "SHADOW expected",
            "realization_status": "not realized",
        },
        "data_completeness": {
            "complete": len(complete),
            "incomplete": len(incomplete),
            "financial_aggregates_exclude_incomplete": True,
            "missing_financial_field_counts": [
                {"field": field, "incomplete_rows_missing": missing_counts[field]}
                for field in FINANCIAL_FIELDS
            ],
        },
    }


SECTION_TITLES = (
    ("candidate_funnel", "Candidate Funnel"),
    ("counts_by_route", "Counts By Route"),
    ("rejection_reasons", "Rejection Reasons"),
    ("profitability_distribution", "Profitability Distribution"),
    ("nearest_to_profitable", "Nearest To Profitable"),
    ("cost_breakdown", "Cost Breakdown"),
    ("rpc_failure_contribution", "RPC Failure Contribution"),
    ("stale_state_contribution", "Stale-State Contribution"),
    ("route_expected_pnl", "Route-Level Expected PnL"),
    ("model_comparison", "Model Comparison"),
    ("sensitivity", "Conservative/Severe Sensitivity"),
    ("data_completeness", "Data Completeness"),
)


def render_text(report: dict[str, Any]) -> str:
    lines = [
        "Phoenix SHADOW Profitability Report",
        "Financial basis: SHADOW expected",
        "Realization status: not realized",
        f"Bounded rows: {report['scope']['rows_analyzed']}/{report['scope']['row_limit']}",
    ]
    for key, title in SECTION_TITLES:
        lines.extend(
            (
                "",
                title,
                json.dumps(report[key], sort_keys=True, separators=(",", ":")),
            )
        )
    return "\n".join(lines) + "\n"


def main() -> int:
    args = parse_args()
    try:
        rows = load_rows(sys.stdin, args.limit)
        report = build_report(rows, args.limit)
    except (ReportError, UnicodeError) as error:
        print(f"shadow profitability report failed: {error}", file=sys.stderr)
        return 2
    if args.format == "json":
        json.dump(report, sys.stdout, indent=2, ensure_ascii=True)
        sys.stdout.write("\n")
    else:
        sys.stdout.write(render_text(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
