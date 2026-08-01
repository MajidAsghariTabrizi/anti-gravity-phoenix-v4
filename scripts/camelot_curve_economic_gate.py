#!/usr/bin/env python3
"""Validate the immutable Camelot -> Curve B-B economic proof."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


class GateError(RuntimeError):
    pass


def ceil_mul_div(value: int, numerator: int, denominator: int) -> int:
    if value < 0 or numerator < 0 or denominator <= 0:
        raise GateError("invalid integer cost input")
    return (value * numerator + denominator - 1) // denominator


def validate(document: dict[str, object]) -> dict[str, object]:
    if document.get("schema") != "phoenix.camelot-curve-revenue-proof.v1":
        raise GateError("unexpected proof schema")
    if document.get("chain_id") != 42161 or document.get("classification") != "B-B":
        raise GateError("proof is not the reviewed Arbitrum B-B classification")
    agreement = document.get("provider_agreement")
    conclusion = document.get("conclusion")
    policy = document.get("policy")
    ladder = document.get("seven_size_ladder")
    if not isinstance(agreement, dict) or not isinstance(conclusion, dict) or not isinstance(policy, dict):
        raise GateError("proof metadata is malformed")
    if not isinstance(ladder, list) or len(ladder) != 7:
        raise GateError("proof must contain the exact seven-size ladder")
    if agreement.get("reviewed_provider_count") != 2 or not all(
        agreement.get(key) is True
        for key in ("same_block_hash", "same_identity", "same_state", "same_quotes")
    ):
        raise GateError("independent provider agreement is incomplete")

    flash_bps = int(policy["flash_premium_bps"])
    conservative_bps = int(policy["conservative_cost_multiplier_bps"])
    minimum_retained = int(policy["minimum_retained_profit_wei"])
    expected_values: list[int] = []
    for row in ladder:
        if not isinstance(row, dict):
            raise GateError("ladder row is malformed")
        amount = int(row["amount_in_wei"])
        output = int(row["curve_weth_out_wei"])
        gross = int(row["gross_profit_wei"])
        premium = int(row["flash_premium_wei"])
        expected = int(row["expected_net_upper_bound_wei"])
        conservative = int(row["conservative_net_upper_bound_wei"])
        severe = int(row["severe_net_upper_bound_wei"])
        if output - amount != gross:
            raise GateError("gross-profit arithmetic is inconsistent")
        if premium != ceil_mul_div(amount, flash_bps, 10_000):
            raise GateError("flash-premium arithmetic is inconsistent")
        if expected != gross - premium:
            raise GateError("expected upper-bound arithmetic is inconsistent")
        if conservative != gross - ceil_mul_div(premium, conservative_bps, 10_000):
            raise GateError("conservative upper-bound arithmetic is inconsistent")
        if severe != gross - 2 * premium:
            raise GateError("severe upper-bound arithmetic is inconsistent")
        if gross >= 0 or expected >= minimum_retained or conservative >= minimum_retained or severe >= minimum_retained:
            raise GateError("B-B proof contains a non-negative or floor-clearing row")
        expected_values.append(expected)

    best = max(expected_values)
    if int(conclusion.get("highest_expected_net_upper_bound_wei", 0)) != best:
        raise GateError("conclusion does not bind the best ladder row")
    if conclusion.get("decision") != "B-B" or conclusion.get("retained_profit_floor_satisfied") is not False:
        raise GateError("conclusion contradicts the economic proof")
    return {
        "classification": "B-B",
        "block_number": document["block_number"],
        "block_hash": document["block_hash"],
        "best_expected_net_upper_bound_wei": str(best),
        "reviewed_provider_count": 2,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("proof", type=Path)
    args = parser.parse_args()
    try:
        with args.proof.open(encoding="utf-8") as handle:
            result = validate(json.load(handle))
        print(json.dumps(result, separators=(",", ":"), sort_keys=True))
        return 0
    except Exception as error:
        print(f"Camelot-Curve economic gate failed: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
