#!/usr/bin/env python3
"""Build a fail-closed Atlas/Aave external-fork qualification package."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from typing import Any

try:
    from scripts.atlas_borrower_index import (
        EvidenceError,
        canonical_json,
        read_json,
        verify_hash,
        verify_inventory,
    )
except ModuleNotFoundError:
    from atlas_borrower_index import (  # type: ignore[no-redef]
        EvidenceError,
        canonical_json,
        read_json,
        verify_hash,
        verify_inventory,
    )


CHAIN_ID = 42161
PACKAGE_SCHEMA = "phoenix.atlas.aave-external-fork-package.v1"
AGREEMENT_SCHEMA = "phoenix.atlas.aave-provider-agreement.v1"
PLAN_SCHEMA = "phoenix.atlas.aave-execution-plan.v1"


def canonical_hash(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def address(value: object, name: str) -> str:
    if not isinstance(value, str) or len(value) != 42 or not value.startswith("0x"):
        raise EvidenceError(f"{name} must be a canonical address")
    try:
        int(value[2:], 16)
    except ValueError as error:
        raise EvidenceError(f"{name} must be hexadecimal") from error
    return value.lower()


def integer(value: object, name: str, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise EvidenceError(f"{name} must be an integer >= {minimum}")
    return value


def digest(value: object, name: str) -> str:
    if not isinstance(value, str) or len(value) != 64:
        raise EvidenceError(f"{name} must be a SHA-256 digest")
    try:
        int(value, 16)
    except ValueError as error:
        raise EvidenceError(f"{name} must be hexadecimal") from error
    return value.lower()


def hash_bound(value: Any, schema: str, name: str) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("schema") != schema:
        raise EvidenceError(f"{name} schema mismatch")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise EvidenceError(f"{name} content hash mismatch")
    return value


def select_pair(
    result: dict[str, Any], borrower: str, debt_asset: str, collateral_asset: str
) -> dict[str, Any]:
    matches = [
        pair
        for pair in result.get("pairs", [])
        if isinstance(pair, dict)
        and str(pair.get("borrower", "")).lower() == borrower
        and str(pair.get("debt_asset", "")).lower() == debt_asset
        and str(pair.get("collateral_asset", "")).lower() == collateral_asset
    ]
    if len(matches) != 1:
        raise EvidenceError("exact borrower/debt/collateral pair is not unique")
    return matches[0]


def verify_agreement(
    agreement: dict[str, Any], inventory: dict[str, Any], block_number: int, block_hash: str
) -> None:
    hash_bound(agreement, AGREEMENT_SCHEMA, "provider agreement")
    if agreement.get("status") != "agreed":
        raise EvidenceError("independent provider agreement is absent")
    if (
        agreement.get("chain_id") != CHAIN_ID
        or agreement.get("inventory_snapshot_sha256") != inventory["snapshot_sha256"]
        or agreement.get("checkpoint_content_sha256")
        != inventory.get("checkpoint_content_sha256")
    ):
        raise EvidenceError("provider agreement inventory binding mismatch")
    providers = agreement.get("provider_ids")
    if (
        not isinstance(providers, list)
        or len(providers) < 2
        or len(set(providers)) != len(providers)
        or not all(isinstance(item, str) and item for item in providers)
    ):
        raise EvidenceError("provider identities are not independently bound")
    if (
        agreement.get("block_number") != block_number
        or str(agreement.get("block_hash", "")).lower() != block_hash
    ):
        raise EvidenceError("provider agreement block mismatch")
    state_hashes = agreement.get("state_hashes")
    if (
        not isinstance(state_hashes, list)
        or len(state_hashes) != len(providers)
        or len(set(state_hashes)) != 1
    ):
        raise EvidenceError("provider state hashes disagree")
    digest(state_hashes[0], "provider state hash")


def verify_plan(
    plan: dict[str, Any],
    pair: dict[str, Any],
    borrower: str,
    debt_asset: str,
    collateral_asset: str,
    block_number: int,
    block_hash: str,
) -> None:
    hash_bound(plan, PLAN_SCHEMA, "execution plan")
    if plan.get("chain_id") != CHAIN_ID:
        raise EvidenceError("execution plan chain mismatch")
    if (
        plan.get("block_number") != block_number
        or str(plan.get("block_hash", "")).lower() != block_hash
        or address(plan.get("borrower"), "plan borrower") != borrower
        or address(plan.get("debt_asset"), "plan debt asset") != debt_asset
        or address(plan.get("collateral_asset"), "plan collateral asset")
        != collateral_asset
    ):
        raise EvidenceError("execution plan identity mismatch")
    if integer(plan.get("repay"), "plan repay", 1) != pair["repay"]:
        raise EvidenceError("execution plan repay mismatch")
    if integer(plan.get("max_repay"), "plan max repay", 1) != pair["repay"]:
        raise EvidenceError("execution plan max repay mismatch")
    if (
        integer(plan.get("seized_collateral"), "plan seized collateral", 1)
        != pair["liquidator_collateral"]
    ):
        raise EvidenceError("execution plan seize mismatch")
    atlas_bid = integer(plan.get("atlas_bid_base"), "plan Atlas bid")
    max_atlas_bid = integer(plan.get("max_atlas_bid_base"), "plan max Atlas bid")
    if (
        atlas_bid != integer(pair.get("atlas_bid_base"), "pair Atlas bid")
        or max_atlas_bid > pair["max_rational_atlas_bid_base"]
        or atlas_bid > max_atlas_bid
    ):
        raise EvidenceError("execution plan Atlas bid exceeds rational maximum")
    minimum_profit_base = integer(
        plan.get("minimum_final_realized_profit_base"),
        "plan minimum final realized profit base",
        1,
    )
    if minimum_profit_base < pair["retained_profit_floor_base"]:
        raise EvidenceError("execution plan minimum profit is below the retained floor")
    integer(
        plan.get("minimum_final_realized_profit"),
        "plan minimum final realized profit",
        1,
    )
    if plan.get("atomic_bounds_enforced") is not True:
        raise EvidenceError("execution plan atomic bounds are not enforced")
    calldata = plan.get("calldata")
    if (
        not isinstance(calldata, str)
        or not calldata.startswith("0x")
        or hashlib.sha256(calldata.lower().encode()).hexdigest()
        != plan.get("calldata_sha256")
    ):
        raise EvidenceError("execution plan calldata hash mismatch")
    for name in (
        "flash_amount",
        "flash_premium",
        "unwind_min_out",
        "gas_limit",
        "max_fee_per_gas_wei",
        "l1_data_fee_wei",
        "deadline",
    ):
        integer(plan.get(name), f"plan {name}", 1)
    if not isinstance(plan.get("nonce_assumption"), str) or not plan[
        "nonce_assumption"
    ]:
        raise EvidenceError("execution plan nonce assumption is missing")
    balances = plan.get("balance_reconciliation")
    if not isinstance(balances, dict) or balances.get("all_costs_included") is not True:
        raise EvidenceError("execution plan balance reconciliation is incomplete")
    prediction_error = integer(
        plan.get("prediction_error_bps"), "plan prediction error"
    )
    maximum_error = integer(
        plan.get("maximum_prediction_error_bps"), "plan maximum prediction error"
    )
    if prediction_error > maximum_error:
        raise EvidenceError("prediction error exceeds policy")
    detected = integer(plan.get("detected_at_ms"), "plan detected time", 1)
    expires = integer(plan.get("expires_at_ms"), "plan expiry time", detected + 1)
    latency = integer(
        plan.get("end_to_end_latency_p95_ms"), "plan end-to-end latency", 1
    )
    if expires - detected <= latency:
        raise EvidenceError("opportunity lifetime does not exceed latency")
    authority = plan.get("execution_authority")
    if not isinstance(authority, dict) or any(authority.values()):
        raise EvidenceError("fork plan must not grant execution authority")


def build_package(
    inventory: dict[str, Any],
    auction: dict[str, Any],
    result: dict[str, Any],
    agreement: dict[str, Any],
    plan: dict[str, Any],
) -> dict[str, Any]:
    verify_inventory(inventory)
    verify_hash(auction)
    if result.get("schema") != "phoenix.atlas.aave-auction-result.v1":
        raise EvidenceError("auction result schema mismatch")
    observed_result = result.get("result_sha256")
    result_body = {
        key: item for key, item in result.items() if key != "result_sha256"
    }
    if observed_result != canonical_hash(result_body):
        raise EvidenceError("auction result hash mismatch")
    if result.get("status") != "COMPLETE":
        raise EvidenceError("auction result is not complete")
    if result.get("inventory_snapshot_sha256") != inventory["snapshot_sha256"]:
        raise EvidenceError("auction result inventory mismatch")
    if result.get("auction_content_sha256") != auction["content_sha256"]:
        raise EvidenceError("auction result input mismatch")

    borrower = address(plan.get("borrower"), "plan borrower")
    debt_asset = address(plan.get("debt_asset"), "plan debt asset")
    collateral_asset = address(
        plan.get("collateral_asset"), "plan collateral asset"
    )
    pair = select_pair(result, borrower, debt_asset, collateral_asset)
    if pair.get("economics_status") != "EXACT_FULL_COST":
        raise EvidenceError("pair economics are not exact full-cost")
    if not all(pair.get("validity", {}).values()):
        raise EvidenceError("pair violates a protocol constraint")
    pnl = pair.get("pnl_base")
    floor = integer(pair.get("retained_profit_floor_base"), "retained floor")
    if (
        not isinstance(pnl, dict)
        or integer(pnl.get("expected"), "expected PnL") <= floor
        or integer(pnl.get("conservative"), "conservative PnL") <= floor
        or integer(pair.get("margin_to_gate_base"), "margin to gate", 1)
        != integer(pnl.get("conservative"), "conservative PnL") - floor
    ):
        raise EvidenceError("expected and conservative PnL do not clear the floor")

    block_number = integer(inventory.get("checkpoint_block"), "checkpoint block", 1)
    block_hash = str(inventory.get("checkpoint_hash", "")).lower()
    verify_agreement(agreement, inventory, block_number, block_hash)
    verify_plan(
        plan,
        pair,
        borrower,
        debt_asset,
        collateral_asset,
        block_number,
        block_hash,
    )
    package: dict[str, Any] = {
        "schema": PACKAGE_SCHEMA,
        "status": "READY_FOR_EXTERNAL_FORK",
        "chain_id": CHAIN_ID,
        "block_number": block_number,
        "block_hash": block_hash,
        "borrower": borrower,
        "debt_asset": debt_asset,
        "collateral_asset": collateral_asset,
        "inventory_snapshot_sha256": inventory["snapshot_sha256"],
        "reserve_state_sha256": canonical_hash(inventory["reserves"]),
        "emode_state_sha256": canonical_hash(inventory["emode_categories"]),
        "auction_content_sha256": auction["content_sha256"],
        "auction_result_sha256": result["result_sha256"],
        "provider_agreement_sha256": agreement["content_sha256"],
        "execution_plan_sha256": plan["content_sha256"],
        "pair": pair,
        "execution_plan": plan,
        "fork_status": "not_run",
        "fork_request_created": False,
        "execution_authority": {
            "signer": False,
            "bond": False,
            "bid": False,
            "submission": False,
            "capital": False,
        },
    }
    package["content_sha256"] = canonical_hash(package)
    return package


def write_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.parent / f".{path.name}.{os.getpid()}.tmp"
    temporary.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--inventory", required=True)
    parser.add_argument("--auction", required=True)
    parser.add_argument("--auction-result", required=True)
    parser.add_argument("--provider-agreement", required=True)
    parser.add_argument("--execution-plan", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        package = build_package(
            read_json(args.inventory),
            read_json(args.auction),
            read_json(args.auction_result),
            read_json(args.provider_agreement),
            read_json(args.execution_plan),
        )
        write_atomic(args.output, package)
        print(
            f"fork_package_status={package['status']} content_sha256={package['content_sha256']}"
        )
        return 0
    except Exception as error:
        print(f"Atlas/Aave fork package failed: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
