#!/usr/bin/env python3
"""Deterministic, signerless Aave borrower inventory and auction evaluator.

The module consumes a sanitized, hash-bound archive transcript.  It never makes
network calls and it refuses to turn partial evidence into apparent coverage.
Pool events establish intent and borrower discovery; scaled token events are the
accounting source of truth.  Every primary token movement therefore needs the
exact scaled amount used by the reviewed Aave implementation.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import re
from pathlib import Path
from typing import Any, Iterable


RAY = 10**27
WAD = 10**18
PERCENTAGE_FACTOR = 10_000
HALF_PERCENTAGE_FACTOR = 5_000
DEFAULT_LIQUIDATION_CLOSE_FACTOR_BPS = 5_000
CLOSE_FACTOR_HF_THRESHOLD_WAD = 950_000_000_000_000_000
MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD = 2_000 * 10**8
MIN_LEFTOVER_BASE = MIN_BASE_MAX_CLOSE_FACTOR_THRESHOLD // 2

POOL_EVIDENCE_KINDS = {
    "supply",
    "withdraw",
    "borrow",
    "repay",
    "liquidation_call",
}
CONFIG_KINDS = {
    "account_configuration_snapshot",
    "collateral_enabled",
    "collateral_disabled",
    "user_emode_set",
    "reserve_configuration",
    "reserve_data_updated",
}
TOKEN_KINDS = {
    "atoken_mint",
    "atoken_burn",
    "atoken_transfer",
    "variable_debt_mint",
    "variable_debt_burn",
    "variable_debt_transfer",
    "stable_debt_mint",
    "stable_debt_burn",
    "stable_debt_transfer",
}
SUPPORTED_EVENT_KINDS = POOL_EVIDENCE_KINDS | CONFIG_KINDS | TOKEN_KINDS


class EvidenceError(ValueError):
    """Raised when evidence cannot support an exact deterministic result."""


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("utf-8")


def content_hash(value: dict[str, Any], field: str = "content_sha256") -> str:
    body = {key: item for key, item in value.items() if key != field}
    return hashlib.sha256(canonical_json(body)).hexdigest()


def bind_hash(value: dict[str, Any], field: str = "content_sha256") -> dict[str, Any]:
    result = copy.deepcopy(value)
    result[field] = content_hash(result, field)
    return result


def verify_hash(value: dict[str, Any], field: str = "content_sha256") -> None:
    observed = value.get(field)
    if not isinstance(observed, str) or observed != content_hash(value, field):
        raise EvidenceError(f"{field} mismatch")


def read_json(path: str | Path) -> dict[str, Any]:
    with Path(path).open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise EvidenceError("top-level JSON must be an object")
    return value


def write_json(path: str | Path, value: dict[str, Any]) -> None:
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def _require_int(value: Any, name: str, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise EvidenceError(f"{name} must be an integer >= {minimum}")
    return value


def _require_address(value: Any, name: str) -> str:
    if not isinstance(value, str) or len(value) != 42 or not value.startswith("0x"):
        raise EvidenceError(f"{name} must be a canonical address")
    try:
        int(value[2:], 16)
    except ValueError as exc:
        raise EvidenceError(f"{name} must be hexadecimal") from exc
    return value.lower()


def _require_hash(value: Any, name: str) -> str:
    if not isinstance(value, str) or len(value) != 66 or not value.startswith("0x"):
        raise EvidenceError(f"{name} must be a bytes32 hash")
    try:
        int(value[2:], 16)
    except ValueError as exc:
        raise EvidenceError(f"{name} must be hexadecimal") from exc
    return value.lower()


def percent_mul(value: int, percentage: int) -> int:
    return (value * percentage + HALF_PERCENTAGE_FACTOR) // PERCENTAGE_FACTOR


def percent_mul_floor(value: int, percentage: int) -> int:
    return value * percentage // PERCENTAGE_FACTOR


def percent_mul_ceil(value: int, percentage: int) -> int:
    return (value * percentage + PERCENTAGE_FACTOR - 1) // PERCENTAGE_FACTOR


def percent_div_floor(value: int, percentage: int) -> int:
    if percentage == 0:
        raise EvidenceError("percentage division by zero")
    return value * PERCENTAGE_FACTOR // percentage


def percent_div_ceil(value: int, percentage: int) -> int:
    if percentage == 0:
        raise EvidenceError("percentage division by zero")
    return (value * PERCENTAGE_FACTOR + percentage - 1) // percentage


def ray_mul_floor(value: int, index: int) -> int:
    return value * index // RAY


def ray_mul_ceil(value: int, index: int) -> int:
    product = value * index
    return (product + RAY - 1) // RAY


def mul_div_ceil(value: int, multiplier: int, divisor: int) -> int:
    if divisor == 0:
        raise EvidenceError("integer division by zero")
    return (value * multiplier + divisor - 1) // divisor


def wad_div(value: int, divisor: int) -> int:
    if divisor == 0:
        raise EvidenceError("wad division by zero")
    return (value * WAD + divisor // 2) // divisor


def validate_market(market: dict[str, Any]) -> dict[str, dict[str, Any]]:
    verify_hash(market)
    if market.get("schema") != "phoenix.atlas.aave-market.v1":
        raise EvidenceError("unsupported market schema")
    if market.get("chain_id") != 42161:
        raise EvidenceError("market must be Arbitrum One")
    protocol = market.get("protocol")
    if not isinstance(protocol, dict):
        raise EvidenceError("protocol identity is missing")
    for name in ("pool", "pool_addresses_provider", "oracle", "data_provider"):
        _require_address(protocol.get(name), f"protocol.{name}")
    if not isinstance(market.get("liquidation_logic"), dict):
        raise EvidenceError("deployment-bound liquidation logic is missing")
    if not isinstance(market.get("sources"), dict):
        raise EvidenceError("official source identities are missing")
    liquidation_logic = market["liquidation_logic"]
    if liquidation_logic.get("pool_implementation") is not None:
        _require_address(
            liquidation_logic["pool_implementation"],
            "liquidation_logic.pool_implementation",
        )
    if liquidation_logic.get("pool_implementation_code_hash") is not None:
        _require_hash(
            liquidation_logic["pool_implementation_code_hash"],
            "liquidation_logic.pool_implementation_code_hash",
        )
    for field in (
        "default_close_factor_bps",
        "close_factor_hf_threshold_wad",
        "minimum_reserve_value_base",
        "minimum_leftover_base",
    ):
        if liquidation_logic.get(field) is not None:
            _require_int(liquidation_logic[field], f"liquidation_logic.{field}")
    reserves: dict[str, dict[str, Any]] = {}
    raw_reserves = market.get("reserves")
    if not isinstance(raw_reserves, list) or not raw_reserves:
        raise EvidenceError("market reserves are missing")
    token_owners: dict[str, tuple[str, str]] = {}
    for reserve in raw_reserves:
        if not isinstance(reserve, dict):
            raise EvidenceError("reserve entry must be an object")
        asset = _require_address(reserve.get("asset"), "reserve.asset")
        if asset in reserves:
            raise EvidenceError(f"duplicate reserve {asset}")
        _require_int(reserve.get("decimals"), "reserve.decimals", 0)
        _require_address(reserve.get("price_feed"), "reserve.price_feed")
        for flag in ("active", "paused"):
            if reserve.get(flag) is not None and not isinstance(reserve[flag], bool):
                raise EvidenceError(f"reserve.{flag} must be boolean or null")
        for field, token_type in (
            ("atoken", "atoken"),
            ("variable_debt_token", "variable_debt"),
        ):
            token = _require_address(reserve.get(field), f"reserve.{field}")
            if token in token_owners:
                raise EvidenceError(f"token identity reused: {token}")
            token_owners[token] = (asset, token_type)
        stable = reserve.get("stable_debt_token")
        if stable is not None:
            stable_address = _require_address(stable, "reserve.stable_debt_token")
            if stable_address in token_owners:
                raise EvidenceError(f"token identity reused: {stable_address}")
            token_owners[stable_address] = (asset, "stable_debt")
        reserves[asset] = copy.deepcopy(reserve)
    return reserves


def _market_exact(market: dict[str, Any]) -> tuple[bool, list[str]]:
    reasons: list[str] = []
    if market.get("evidence_status") != "complete":
        reasons.append("market_identity_or_configuration_incomplete")
    logic = market.get("liquidation_logic", {})
    sources = market.get("sources", {})
    for source in ("aave_address_book", "aave_v3_origin"):
        if not isinstance(sources.get(source), dict) or not sources[source].get("commit"):
            reasons.append(f"official_source_{source}_commit_missing")
    for field in (
        "pool_implementation",
        "pool_implementation_code_hash",
        "default_close_factor_bps",
        "close_factor_hf_threshold_wad",
        "minimum_reserve_value_base",
        "minimum_leftover_base",
    ):
        if logic.get(field) is None:
            reasons.append(f"liquidation_logic_{field}_missing")
    for reserve in market.get("reserves", []):
        for field in (
            "reserve_id",
            "active",
            "paused",
            "liquidation_grace_period_until",
            "liquidation_threshold_bps",
            "liquidation_bonus_bps",
            "liquidation_protocol_fee_bps",
            "liquidity_index_ray",
            "variable_borrow_index_ray",
        ):
            if reserve.get(field) is None:
                reasons.append(
                    f"reserve_{reserve.get('symbol', reserve.get('asset'))}_{field}_missing"
                )
    return not reasons, sorted(set(reasons))


def validate_transcript(transcript: dict[str, Any]) -> list[dict[str, Any]]:
    verify_hash(transcript)
    if transcript.get("schema") != "phoenix.atlas.aave-archive-transcript.v1":
        raise EvidenceError("unsupported transcript schema")
    blocks = transcript.get("blocks")
    if not isinstance(blocks, list):
        raise EvidenceError("transcript blocks must be an array")
    previous_number: int | None = None
    previous_hash: str | None = None
    seen: dict[str, str] = {}
    normalized: list[dict[str, Any]] = []
    for raw_block in blocks:
        if not isinstance(raw_block, dict):
            raise EvidenceError("block entry must be an object")
        number = _require_int(raw_block.get("number"), "block.number")
        block_hash = _require_hash(raw_block.get("hash"), "block.hash")
        parent_hash = _require_hash(raw_block.get("parent_hash"), "block.parent_hash")
        if previous_number is not None:
            if number != previous_number + 1:
                raise EvidenceError("archive block range is not contiguous")
            if parent_hash != previous_hash:
                raise EvidenceError("archive block hash chain reorged")
        previous_number = number
        previous_hash = block_hash
        logs = raw_block.get("logs")
        if not isinstance(logs, list):
            raise EvidenceError("block logs must be an array")
        prior_log_index = -1
        for raw_event in logs:
            if not isinstance(raw_event, dict):
                raise EvidenceError("event must be an object")
            event = copy.deepcopy(raw_event)
            event["block_number"] = number
            event["block_hash"] = block_hash
            event["transaction_hash"] = _require_hash(
                event.get("transaction_hash"), "event.transaction_hash"
            )
            log_index = _require_int(event.get("log_index"), "event.log_index")
            if log_index <= prior_log_index:
                raise EvidenceError("event log indexes must be strictly increasing per block")
            prior_log_index = log_index
            kind = event.get("event_kind")
            if kind not in SUPPORTED_EVENT_KINDS:
                raise EvidenceError(f"unsupported event kind: {kind}")
            event_key = f"{block_hash}/{event['transaction_hash']}/{log_index}"
            event_digest = hashlib.sha256(canonical_json(event)).hexdigest()
            if event_key in seen:
                if seen[event_key] != event_digest:
                    raise EvidenceError("conflicting duplicate event identity")
                continue
            seen[event_key] = event_digest
            event["evidence_sha256"] = event_digest
            normalized.append(event)
    if blocks:
        first = blocks[0]
        last = blocks[-1]
        if transcript.get("start_block") != first.get("number"):
            raise EvidenceError("transcript start block mismatch")
        if str(transcript.get("start_hash", "")).lower() != str(first.get("hash", "")).lower():
            raise EvidenceError("transcript start hash mismatch")
        if transcript.get("end_block") != last.get("number"):
            raise EvidenceError("transcript end block mismatch")
        if str(transcript.get("end_hash", "")).lower() != str(last.get("hash", "")).lower():
            raise EvidenceError("transcript end hash mismatch")
    return normalized


def _empty_borrower(address: str) -> dict[str, Any]:
    return {
        "address": address,
        "account_configuration_bitmap": None,
        "account_configuration_block_hash": None,
        "collateral_enabled": {},
        "emode_category": 0,
        "scaled_supply": {},
        "scaled_variable_debt": {},
        "stable_debt": {},
        "evidence_sha256": [],
        "event_kinds": [],
        "last_block": None,
        "last_block_hash": None,
        "incomplete_reasons": [],
    }


def _borrower(state: dict[str, dict[str, Any]], address: Any, field: str) -> dict[str, Any]:
    canonical = _require_address(address, field)
    return state.setdefault(canonical, _empty_borrower(canonical))


def _touch(item: dict[str, Any], event: dict[str, Any]) -> None:
    digest = event["evidence_sha256"]
    if digest not in item["evidence_sha256"]:
        item["evidence_sha256"].append(digest)
    kind = event["event_kind"]
    if kind not in item["event_kinds"]:
        item["event_kinds"].append(kind)
    item["last_block"] = event["block_number"]
    item["last_block_hash"] = event["block_hash"]


def _asset(event: dict[str, Any], reserves: dict[str, dict[str, Any]]) -> str:
    asset = _require_address(event.get("asset"), "event.asset")
    if asset not in reserves:
        raise EvidenceError(f"event references unknown reserve {asset}")
    return asset


def _adjust(mapping: dict[str, int], asset: str, delta: int, context: str) -> None:
    updated = mapping.get(asset, 0) + delta
    if updated < 0:
        raise EvidenceError(f"negative balance after {context}")
    if updated == 0:
        mapping.pop(asset, None)
    else:
        mapping[asset] = updated


def _scaled_amount(event: dict[str, Any]) -> int:
    if event.get("accounting_role") != "primary":
        raise EvidenceError("token event must declare accounting_role=primary or mirror")
    return _require_int(event.get("scaled_amount"), "event.scaled_amount", 1)


def _apply_event(
    event: dict[str, Any],
    borrowers: dict[str, dict[str, Any]],
    reserves: dict[str, dict[str, Any]],
) -> None:
    kind = event["event_kind"]
    if kind in POOL_EVIDENCE_KINDS:
        for field in ("user", "on_behalf_of", "borrower"):
            if event.get(field) is not None:
                item = _borrower(borrowers, event[field], f"event.{field}")
                _touch(item, event)
        if event.get("asset") is not None:
            _asset(event, reserves)
        if event.get("collateral_asset") is not None:
            _asset({"asset": event["collateral_asset"]}, reserves)
        if event.get("debt_asset") is not None:
            _asset({"asset": event["debt_asset"]}, reserves)
        return

    if kind == "reserve_configuration":
        asset = _asset(event, reserves)
        for field in (
            "reserve_id",
            "liquidation_threshold_bps",
            "liquidation_bonus_bps",
            "liquidation_protocol_fee_bps",
        ):
            reserves[asset][field] = _require_int(event.get(field), f"event.{field}")
        return

    if kind == "reserve_data_updated":
        asset = _asset(event, reserves)
        reserves[asset]["liquidity_index_ray"] = _require_int(
            event.get("liquidity_index_ray"), "event.liquidity_index_ray", 1
        )
        reserves[asset]["variable_borrow_index_ray"] = _require_int(
            event.get("variable_borrow_index_ray"),
            "event.variable_borrow_index_ray",
            1,
        )
        return

    if kind in {"collateral_enabled", "collateral_disabled"}:
        asset = _asset(event, reserves)
        item = _borrower(borrowers, event.get("user"), "event.user")
        item["collateral_enabled"][asset] = kind == "collateral_enabled"
        _touch(item, event)
        return

    if kind == "user_emode_set":
        item = _borrower(borrowers, event.get("user"), "event.user")
        item["emode_category"] = _require_int(
            event.get("category_id"), "event.category_id"
        )
        _touch(item, event)
        return

    if kind == "account_configuration_snapshot":
        item = _borrower(borrowers, event.get("user"), "event.user")
        item["account_configuration_bitmap"] = _require_int(
            event.get("configuration_bitmap"), "event.configuration_bitmap"
        )
        item["account_configuration_block_hash"] = event["block_hash"]
        _touch(item, event)
        return

    asset = _asset(event, reserves)
    role = event.get("accounting_role")
    if role == "mirror":
        for field in ("from", "to", "user", "on_behalf_of"):
            if event.get(field) is not None and event[field] != "0x0000000000000000000000000000000000000000":
                _touch(_borrower(borrowers, event[field], f"event.{field}"), event)
        return
    amount = _scaled_amount(event)

    if kind.endswith("_mint"):
        target_field = "on_behalf_of" if event.get("on_behalf_of") is not None else "user"
        item = _borrower(borrowers, event.get(target_field), f"event.{target_field}")
        if kind == "atoken_mint":
            _adjust(item["scaled_supply"], asset, amount, kind)
        elif kind == "variable_debt_mint":
            _adjust(item["scaled_variable_debt"], asset, amount, kind)
        else:
            raw_amount = _require_int(event.get("balance_increase_adjusted_amount"), "event.balance_increase_adjusted_amount", 1)
            _adjust(item["stable_debt"], asset, raw_amount, kind)
        _touch(item, event)
        return

    if kind.endswith("_burn"):
        item = _borrower(borrowers, event.get("user"), "event.user")
        if kind == "atoken_burn":
            _adjust(item["scaled_supply"], asset, -amount, kind)
        elif kind == "variable_debt_burn":
            _adjust(item["scaled_variable_debt"], asset, -amount, kind)
        else:
            raw_amount = _require_int(event.get("balance_decrease_adjusted_amount"), "event.balance_decrease_adjusted_amount", 1)
            _adjust(item["stable_debt"], asset, -raw_amount, kind)
        _touch(item, event)
        return

    if kind.endswith("_transfer"):
        source = _borrower(borrowers, event.get("from"), "event.from")
        target = _borrower(borrowers, event.get("to"), "event.to")
        if kind == "atoken_transfer":
            source_map = source["scaled_supply"]
            target_map = target["scaled_supply"]
        elif kind == "variable_debt_transfer":
            source_map = source["scaled_variable_debt"]
            target_map = target["scaled_variable_debt"]
        else:
            source_map = source["stable_debt"]
            target_map = target["stable_debt"]
        _adjust(source_map, asset, -amount, kind)
        _adjust(target_map, asset, amount, kind)
        _touch(source, event)
        _touch(target, event)
        return
    raise EvidenceError(f"unhandled event kind: {kind}")


def build_inventory(
    market: dict[str, Any], transcript: dict[str, Any]
) -> dict[str, Any]:
    reserves = validate_market(market)
    events = validate_transcript(transcript)
    if transcript.get("market_content_sha256") != market.get("content_sha256"):
        raise EvidenceError("transcript is bound to a different market identity")

    borrowers: dict[str, dict[str, Any]] = {}
    mutable_reserves = copy.deepcopy(reserves)
    for event in events:
        _apply_event(event, borrowers, mutable_reserves)

    exact_market, reasons = _market_exact(
        {**market, "reserves": list(mutable_reserves.values())}
    )
    if not transcript.get("archive_complete"):
        reasons.append("archive_log_range_incomplete")
    if not transcript.get("canonical_head_confirmed"):
        reasons.append("canonical_end_hash_not_independently_confirmed")
    if not transcript.get("reviewed_start_state_zero"):
        reasons.append("reviewed_zero_state_start_not_proven")
    required_sources = {"eth_getBlockByNumber", "eth_getLogs"}
    if not required_sources.issubset(set(transcript.get("source_methods", []))):
        reasons.append("required_archive_source_methods_missing")

    inventory_borrowers: list[dict[str, Any]] = []
    feed_index: dict[str, list[str]] = {}
    for address in sorted(borrowers):
        item = borrowers[address]
        positions: list[dict[str, Any]] = []
        active_assets = sorted(
            set(item["scaled_supply"])
            | set(item["scaled_variable_debt"])
            | set(item["stable_debt"])
        )
        item_reasons = list(item["incomplete_reasons"])
        derived_configuration_bitmap = 0
        for asset in active_assets:
            reserve = mutable_reserves[asset]
            liquidity_index = reserve.get("liquidity_index_ray")
            borrow_index = reserve.get("variable_borrow_index_ray")
            scaled_supply = item["scaled_supply"].get(asset, 0)
            scaled_variable = item["scaled_variable_debt"].get(asset, 0)
            if scaled_supply and liquidity_index is None:
                item_reasons.append(f"{asset}:liquidity_index_missing")
            if scaled_variable and borrow_index is None:
                item_reasons.append(f"{asset}:variable_borrow_index_missing")
            supplied = (
                ray_mul_floor(scaled_supply, liquidity_index)
                if scaled_supply and liquidity_index is not None
                else None if scaled_supply else 0
            )
            variable_debt = (
                ray_mul_ceil(scaled_variable, borrow_index)
                if scaled_variable and borrow_index is not None
                else None if scaled_variable else 0
            )
            stable_debt = item["stable_debt"].get(asset, 0)
            positions.append(
                {
                    "asset": asset,
                    "symbol": reserve.get("symbol"),
                    "scaled_supply": scaled_supply,
                    "supplied": supplied,
                    "collateral_enabled": bool(
                        item["collateral_enabled"].get(asset, False)
                    ),
                    "scaled_variable_debt": scaled_variable,
                    "variable_debt": variable_debt,
                    "stable_debt": stable_debt,
                    "debt_type": (
                        "variable_and_stable"
                        if scaled_variable and stable_debt
                        else "variable" if scaled_variable
                        else "stable" if stable_debt
                        else "none"
                    ),
                    "price_feed": reserve["price_feed"].lower(),
                }
            )
            feed = reserve["price_feed"].lower()
            feed_index.setdefault(feed, []).append(address)
            reserve_id = reserve.get("reserve_id")
            if reserve_id is None:
                item_reasons.append(f"{asset}:reserve_id_missing")
            else:
                reserve_id = _require_int(reserve_id, "reserve.reserve_id")
                if item["collateral_enabled"].get(asset, False) and scaled_supply:
                    derived_configuration_bitmap |= 1 << (reserve_id * 2 + 1)
                if scaled_variable or stable_debt:
                    derived_configuration_bitmap |= 1 << (reserve_id * 2)
        if item["account_configuration_bitmap"] is None:
            item_reasons.append("checkpoint_account_configuration_missing")
        elif item["account_configuration_bitmap"] != derived_configuration_bitmap:
            item_reasons.append("checkpoint_account_configuration_mismatch")
        if item["account_configuration_block_hash"] != transcript.get("end_hash"):
            item_reasons.append("checkpoint_account_configuration_not_end_hash_bound")
        borrower_complete = exact_market and not reasons and not item_reasons
        inventory_borrowers.append(
            {
                "address": address,
                "account_configuration_bitmap": item["account_configuration_bitmap"],
                "derived_account_configuration_bitmap": derived_configuration_bitmap,
                "emode_category": item["emode_category"],
                "positions": positions,
                "feed_dependencies": sorted(
                    {position["price_feed"] for position in positions}
                ),
                "last_block": item["last_block"],
                "last_block_hash": item["last_block_hash"],
                "evidence_sha256": sorted(item["evidence_sha256"]),
                "event_kinds": sorted(item["event_kinds"]),
                "completeness_status": "complete" if borrower_complete else "incomplete",
                "incomplete_reasons": sorted(set(item_reasons + ([] if exact_market else reasons))),
            }
        )

    incomplete_borrower_count = sum(
        1
        for item in inventory_borrowers
        if item["completeness_status"] != "complete"
    )
    if incomplete_borrower_count:
        reasons.append(f"incomplete_borrower_records:{incomplete_borrower_count}")

    reasons = sorted(set(reasons))
    inventory = {
        "schema": "phoenix.atlas.borrower-inventory.v1",
        "chain_id": 42161,
        "market_content_sha256": market["content_sha256"],
        "transcript_content_sha256": transcript["content_sha256"],
        "checkpoint_block": transcript.get("end_block"),
        "checkpoint_hash": transcript.get("end_hash"),
        "reserves": [mutable_reserves[key] for key in sorted(mutable_reserves)],
        "emode_categories": copy.deepcopy(market.get("emode_categories", [])),
        "liquidation_logic": copy.deepcopy(market.get("liquidation_logic")),
        "borrowers": inventory_borrowers,
        "feed_index": {
            feed: sorted(set(addresses)) for feed, addresses in sorted(feed_index.items())
        },
        "evidence_event_count": len(events),
        "unique_borrower_count": len(inventory_borrowers),
        "completeness_status": "complete" if not reasons else "incomplete",
        "incomplete_reasons": reasons,
        "execution_authority": {
            "signer": False,
            "bond": False,
            "bid": False,
            "solver": False,
            "submission": False,
            "production_write": False,
        },
    }
    return bind_hash(inventory, "snapshot_sha256")


def verify_inventory(inventory: dict[str, Any]) -> None:
    verify_hash(inventory, "snapshot_sha256")
    if inventory.get("schema") != "phoenix.atlas.borrower-inventory.v1":
        raise EvidenceError("unsupported inventory schema")


def _checkpoint_hash(checkpoint: dict[str, Any]) -> None:
    observed = checkpoint.get("content_sha256")
    body = {key: value for key, value in checkpoint.items() if key != "content_sha256"}
    if not isinstance(observed, str) or observed != hashlib.sha256(canonical_json(body)).hexdigest():
        raise EvidenceError("checkpoint content_sha256 mismatch")


def build_inventory_from_checkpoint(
    market: dict[str, Any], checkpoint: dict[str, Any]
) -> dict[str, Any]:
    """Import a reviewed full-state checkpoint without inventing event history.

    Borrow history proves the discovery set from Pool deployment, while the
    independently agreed checkpoint is the accounting source of truth.  This
    is the explicit hash-bound snapshot bootstrap permitted by the Phase 2
    inventory contract; it is not interchangeable with a partial log replay.
    """

    market_reserves = validate_market(market)
    _checkpoint_hash(checkpoint)
    if checkpoint.get("schema") != "phoenix.atlas.aave-checkpoint.v1":
        raise EvidenceError("unsupported checkpoint schema")
    if checkpoint.get("chain_id") != 42161:
        raise EvidenceError("checkpoint must be Arbitrum One")
    if checkpoint.get("archive_complete") is not True:
        raise EvidenceError("checkpoint discovery archive is incomplete")
    if checkpoint.get("independent_state_agreement") is not True:
        raise EvidenceError("checkpoint state lacks independent agreement")
    discovery_hash = checkpoint.get("discovery_content_sha256")
    if not isinstance(discovery_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", discovery_hash):
        raise EvidenceError("checkpoint discovery content hash is invalid")
    required_methods = {
        "eth_chainId",
        "eth_getBlockByNumber",
        "eth_getCode",
        "eth_getStorageAt",
        "eth_call",
        "eth_getLogs",
    }
    if not required_methods.issubset(set(checkpoint.get("source_methods", []))):
        raise EvidenceError("checkpoint source methods are incomplete")
    authority = checkpoint.get("execution_authority")
    if not isinstance(authority, dict) or any(authority.values()):
        raise EvidenceError("checkpoint must not carry execution authority")

    protocol = checkpoint.get("protocol")
    if not isinstance(protocol, dict):
        raise EvidenceError("checkpoint protocol identity is missing")
    for field in ("pool", "data_provider", "oracle"):
        if _require_address(protocol.get(field), f"checkpoint.protocol.{field}") != str(
            market["protocol"][field]
        ).lower():
            raise EvidenceError(f"checkpoint protocol {field} disagrees with market")
    pool_implementation = _require_address(
        protocol.get("pool_implementation"), "checkpoint.protocol.pool_implementation"
    )

    checkpoint_block = _require_int(checkpoint.get("checkpoint_block"), "checkpoint_block", 1)
    checkpoint_hash = _require_hash(checkpoint.get("checkpoint_hash"), "checkpoint_hash")
    headers = checkpoint.get("provider_headers")
    if not isinstance(headers, list) or len(headers) < 2:
        raise EvidenceError("checkpoint needs two independent provider headers")
    provider_ids = set()
    for item in headers:
        if not isinstance(item, dict) or not isinstance(item.get("provider_id"), str):
            raise EvidenceError("checkpoint provider header is malformed")
        provider_ids.add(item["provider_id"])
        header = item.get("checkpoint")
        if not isinstance(header, dict):
            raise EvidenceError("checkpoint provider block is missing")
        if _require_int(header.get("number"), "provider checkpoint number", 1) != checkpoint_block:
            raise EvidenceError("provider checkpoint number disagreement")
        if _require_hash(header.get("hash"), "provider checkpoint hash") != checkpoint_hash:
            raise EvidenceError("provider checkpoint hash disagreement")
    if len(provider_ids) != len(headers):
        raise EvidenceError("checkpoint provider identities are duplicated")
    finalized_heads = checkpoint.get("finalized_heads")
    if not isinstance(finalized_heads, list) or len(finalized_heads) != len(headers):
        raise EvidenceError("checkpoint finalized-head evidence is incomplete")
    if {item.get("provider_id") for item in finalized_heads if isinstance(item, dict)} != provider_ids:
        raise EvidenceError("checkpoint finalized-head provider set disagreement")
    for item in finalized_heads:
        if not isinstance(item, dict):
            raise EvidenceError("checkpoint finalized-head evidence is malformed")
        if _require_int(item.get("number"), "finalized head number", 1) < checkpoint_block:
            raise EvidenceError("checkpoint is newer than a provider finalized head")
        _require_hash(item.get("hash"), "finalized head hash")

    archive_checkpoint = _require_int(
        checkpoint.get("archive_checkpoint_block"), "archive_checkpoint_block", 1
    )
    if archive_checkpoint > checkpoint_block:
        raise EvidenceError("archive checkpoint is newer than current checkpoint")
    tail = checkpoint.get("tail_discovery")
    collection_provider_id = (
        tail.get("collection_provider_id") if isinstance(tail, dict) else None
    )
    if (
        collection_provider_id not in provider_ids
        or tail.get("independent_log_verification") is not True
    ):
        raise EvidenceError("checkpoint Borrow continuity evidence is missing")
    expected_agreement_scope = {
        "checkpoint_block_hash",
        "reserve_state",
        "retained_borrower_configuration",
        "retained_borrower_state",
        "emode_state",
    }
    if set(checkpoint.get("independent_state_agreement_scope", [])) != expected_agreement_scope:
        raise EvidenceError("checkpoint independent state agreement scope is incomplete")
    if _require_int(tail.get("start_block"), "tail start block", 1) != archive_checkpoint + 1:
        raise EvidenceError("checkpoint Borrow continuity start is invalid")
    if _require_int(tail.get("end_block"), "tail end block", 1) != checkpoint_block:
        raise EvidenceError("checkpoint Borrow continuity end is invalid")
    tail_logs = tail.get("logs")
    if not isinstance(tail_logs, list) or len(tail_logs) != _require_int(
        tail.get("log_count"), "tail log count"
    ):
        raise EvidenceError("checkpoint Borrow continuity logs are incomplete")
    if tail.get("logs_content_sha256") != hashlib.sha256(canonical_json(tail_logs)).hexdigest():
        raise EvidenceError("checkpoint Borrow continuity hash mismatch")
    tail_provider_bindings = tail.get("provider_bindings")
    if (
        not isinstance(tail_provider_bindings, list)
        or len(tail_provider_bindings) != len(provider_ids)
        or {item.get("provider_id") for item in tail_provider_bindings if isinstance(item, dict)}
        != provider_ids
    ):
        raise EvidenceError("checkpoint Borrow continuity provider bindings are incomplete")
    for item in tail_provider_bindings:
        if (
            not isinstance(item, dict)
            or _require_int(item.get("log_count"), "tail provider log count") != len(tail_logs)
            or item.get("logs_content_sha256") != tail.get("logs_content_sha256")
        ):
            raise EvidenceError("checkpoint Borrow continuity provider disagreement")
    tail_borrowers = set()
    tail_identities = set()
    for log in tail_logs:
        if not isinstance(log, dict):
            raise EvidenceError("checkpoint Borrow continuity log is malformed")
        number = _require_int(log.get("block_number"), "tail log block", archive_checkpoint + 1)
        if number > checkpoint_block:
            raise EvidenceError("checkpoint Borrow continuity log is out of range")
        block_hash = _require_hash(log.get("block_hash"), "tail block hash")
        tx_hash = _require_hash(log.get("transaction_hash"), "tail transaction hash")
        _require_int(log.get("transaction_index"), "tail transaction index")
        log_index = _require_int(log.get("log_index"), "tail log index")
        tail_identities.add((block_hash, tx_hash, log_index))
        tail_borrowers.add(_require_address(log.get("borrower"), "tail borrower"))
        _require_address(log.get("reserve"), "tail reserve")
        data_sha256 = log.get("data_sha256")
        if not isinstance(data_sha256, str) or not re.fullmatch(r"[0-9a-f]{64}", data_sha256):
            raise EvidenceError("checkpoint Borrow continuity data hash is invalid")
    if len(tail_identities) != len(tail_logs):
        raise EvidenceError("checkpoint Borrow continuity identity is duplicated")
    if len(tail_borrowers) != _require_int(tail.get("borrower_count"), "tail borrower count"):
        raise EvidenceError("checkpoint Borrow continuity borrower count mismatch")

    code_bindings = checkpoint.get("protocol_code_bindings")
    if not isinstance(code_bindings, list) or len(code_bindings) != len(provider_ids):
        raise EvidenceError("checkpoint independent protocol code bindings are missing")
    if checkpoint.get("protocol_code_independent_agreement") is not True:
        raise EvidenceError("checkpoint protocol code agreement is absent")
    implementation_hashes = set()
    code_provider_ids = set()
    for item in code_bindings:
        if not isinstance(item, dict) or item.get("pool_implementation") != pool_implementation:
            raise EvidenceError("checkpoint implementation binding disagreement")
        code_provider_ids.add(item.get("provider_id"))
        hashes = item.get("code_sha256")
        if not isinstance(hashes, dict):
            raise EvidenceError("checkpoint protocol code hashes are missing")
        for code_name in ("pool", "data_provider", "oracle", "pool_implementation"):
            digest = hashes.get(code_name)
            if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
                raise EvidenceError(f"checkpoint {code_name} code hash is invalid")
        implementation_hashes.add(hashlib.sha256(canonical_json(hashes)).hexdigest())
    if (
        len(code_provider_ids) != len(code_bindings)
        or code_provider_ids != provider_ids
        or len(implementation_hashes) != 1
    ):
        raise EvidenceError("checkpoint implementation code binding is invalid")

    source_bindings = checkpoint.get("source_bindings")
    if not isinstance(source_bindings, dict):
        raise EvidenceError("checkpoint official source bindings are missing")
    for source_name in ("aave_address_book", "aave_v3_origin"):
        expected = market.get("sources", {}).get(source_name, {}).get("commit")
        observed = source_bindings.get(source_name, {}).get("commit")
        if not expected or observed != expected:
            raise EvidenceError(f"checkpoint source binding mismatch: {source_name}")
    if source_bindings["aave_address_book"].get("pool_implementation") != pool_implementation:
        raise EvidenceError("address-book Pool implementation mismatch")

    state_bindings = checkpoint.get("state_bindings")
    if not isinstance(state_bindings, list):
        raise EvidenceError("checkpoint state bindings are missing")
    by_context: dict[str, list[dict[str, Any]]] = {}
    for item in state_bindings:
        if not isinstance(item, dict) or not isinstance(item.get("context"), str):
            raise EvidenceError("checkpoint state binding is malformed")
        by_context.setdefault(item["context"], []).append(item)
    for context in (
        "reserve_list",
        "reserve_state",
        "borrower_activity_retained",
        "borrower_state",
        "emode_state",
    ):
        rows = by_context.get(context, [])
        if len(rows) != len(headers):
            raise EvidenceError(f"checkpoint {context} lacks independent bindings")
        if {row.get("provider_id") for row in rows} != provider_ids:
            raise EvidenceError(f"checkpoint {context} provider set disagreement")
        if len({row.get("result_sha256") for row in rows}) != 1:
            raise EvidenceError(f"checkpoint {context} state disagreement")
        if len({row.get("call_count") for row in rows}) != 1:
            raise EvidenceError(f"checkpoint {context} call-count disagreement")
    broad_rows = by_context.get("borrower_activity_primary", [])
    if len(broad_rows) != 1 or broad_rows[0].get("provider_id") not in provider_ids:
        raise EvidenceError("checkpoint primary borrower screen binding is invalid")
    if not isinstance(broad_rows[0].get("result_sha256"), str) or not re.fullmatch(
        r"[0-9a-f]{64}", broad_rows[0]["result_sha256"]
    ):
        raise EvidenceError("checkpoint primary borrower screen hash is invalid")

    raw_reserves = checkpoint.get("reserves")
    if not isinstance(raw_reserves, list) or not 0 < len(raw_reserves) <= 128:
        raise EvidenceError("checkpoint reserves are missing")
    reserves: dict[str, dict[str, Any]] = {}
    for raw in raw_reserves:
        if not isinstance(raw, dict):
            raise EvidenceError("checkpoint reserve is malformed")
        asset = _require_address(raw.get("asset"), "checkpoint.reserve.asset")
        if asset in reserves:
            raise EvidenceError("checkpoint reserve is duplicated")
        stable = _require_address(raw.get("stable_debt_token"), "checkpoint.reserve.stable_debt_token")
        reserve = {
            **copy.deepcopy(raw),
            "asset": asset,
            "atoken": _require_address(raw.get("atoken"), "checkpoint.reserve.atoken"),
            "variable_debt_token": _require_address(
                raw.get("variable_debt_token"), "checkpoint.reserve.variable_debt_token"
            ),
            "stable_debt_token": None if stable == "0x" + "0" * 40 else stable,
            "price_feed": _require_address(raw.get("price_feed"), "checkpoint.reserve.price_feed"),
        }
        if not isinstance(reserve.get("symbol"), str) or not reserve["symbol"]:
            raise EvidenceError("checkpoint reserve symbol is invalid")
        for field in (
            "reserve_id",
            "decimals",
            "liquidation_threshold_bps",
            "liquidation_bonus_bps",
            "liquidation_protocol_fee_bps",
            "liquidity_index_ray",
            "variable_borrow_index_ray",
            "liquidation_grace_period_until",
            "price_base_units",
        ):
            _require_int(reserve.get(field), f"checkpoint.reserve.{field}")
        for field in ("active", "paused"):
            if not isinstance(reserve.get(field), bool):
                raise EvidenceError(f"checkpoint.reserve.{field} must be boolean")
        if asset in market_reserves:
            expected = market_reserves[asset]
            for field in ("atoken", "variable_debt_token", "price_feed"):
                if reserve[field] != str(expected[field]).lower():
                    raise EvidenceError(f"checkpoint reserve {asset} {field} disagrees with market")
        reserves[asset] = reserve

    logic = checkpoint.get("liquidation_logic")
    if not isinstance(logic, dict):
        raise EvidenceError("checkpoint liquidation logic is missing")
    liquidation_logic = {
        "pool_implementation": pool_implementation,
        "pool_implementation_code_hash": "0x" + next(iter(implementation_hashes)),
    }
    for field in (
        "default_close_factor_bps",
        "close_factor_hf_threshold_wad",
        "minimum_reserve_value_base",
        "minimum_leftover_base",
    ):
        liquidation_logic[field] = _require_int(logic.get(field), f"checkpoint.liquidation_logic.{field}")

    raw_emodes = checkpoint.get("emode_categories", [])
    if not isinstance(raw_emodes, list):
        raise EvidenceError("checkpoint eMode categories are invalid")
    emode_ids = set()
    for category in raw_emodes:
        if not isinstance(category, dict):
            raise EvidenceError("checkpoint eMode category is malformed")
        category_id = _require_int(category.get("category_id"), "checkpoint.emode.category_id", 1)
        if category_id in emode_ids:
            raise EvidenceError("checkpoint eMode category is duplicated")
        emode_ids.add(category_id)
        for field in (
            "ltv_bps",
            "liquidation_threshold_bps",
            "liquidation_bonus_bps",
            "collateral_bitmap",
            "borrowable_bitmap",
        ):
            _require_int(category.get(field), f"checkpoint.emode.{field}")

    raw_borrowers = checkpoint.get("borrowers")
    if not isinstance(raw_borrowers, list) or len(raw_borrowers) > 250_000:
        raise EvidenceError("checkpoint borrower state is missing")
    if _require_int(checkpoint.get("active_borrower_count"), "checkpoint.active_borrower_count") != len(
        raw_borrowers
    ):
        raise EvidenceError("checkpoint active borrower count mismatch")
    discovered_borrower_count = _require_int(
        checkpoint.get("discovered_borrower_count"), "checkpoint.discovered_borrower_count"
    )
    screened_borrower_count = _require_int(
        checkpoint.get("screened_borrower_count"), "checkpoint.screened_borrower_count"
    )
    if screened_borrower_count != discovered_borrower_count:
        raise EvidenceError("checkpoint borrower discovery was not completely screened")
    historical_discovered = _require_int(
        checkpoint.get("historical_discovered_borrower_count"),
        "checkpoint.historical_discovered_borrower_count",
    )
    if historical_discovered > discovered_borrower_count:
        raise EvidenceError("checkpoint historical discovery exceeds current screen")
    if _require_int(
        checkpoint.get("tail_discovered_borrower_count"),
        "checkpoint.tail_discovered_borrower_count",
    ) != len(tail_borrowers):
        raise EvidenceError("checkpoint tail borrower count mismatch")
    if _require_int(
        checkpoint.get("debt_bearing_borrower_count"),
        "checkpoint.debt_bearing_borrower_count",
    ) != len(raw_borrowers):
        raise EvidenceError("checkpoint debt-bearing borrower count mismatch")
    if broad_rows[0].get("call_count") != screened_borrower_count:
        raise EvidenceError("checkpoint primary borrower screen is incomplete")
    if discovered_borrower_count < len(raw_borrowers):
        raise EvidenceError("checkpoint discovered borrower count is below active count")
    inventory_borrowers = []
    feed_index: dict[str, list[str]] = {}
    for raw in raw_borrowers:
        if not isinstance(raw, dict):
            raise EvidenceError("checkpoint borrower is malformed")
        address = _require_address(raw.get("address"), "checkpoint.borrower.address")
        configuration = _require_int(
            raw.get("account_configuration_bitmap"), "checkpoint borrower configuration"
        )
        emode = _require_int(raw.get("emode_category"), "checkpoint borrower emode")
        positions = []
        derived_configuration = 0
        for raw_position in raw.get("positions", []):
            asset = _require_address(raw_position.get("asset"), "checkpoint.position.asset")
            if asset not in reserves:
                raise EvidenceError("checkpoint position references an unknown reserve")
            reserve = reserves[asset]
            supplied = _require_int(raw_position.get("current_supply"), "checkpoint.position.current_supply")
            scaled_supply = _require_int(raw_position.get("scaled_supply"), "checkpoint.position.scaled_supply")
            variable_debt = _require_int(
                raw_position.get("current_variable_debt"), "checkpoint.position.current_variable_debt"
            )
            scaled_variable = _require_int(
                raw_position.get("scaled_variable_debt"), "checkpoint.position.scaled_variable_debt"
            )
            stable_debt = _require_int(
                raw_position.get("current_stable_debt"), "checkpoint.position.current_stable_debt"
            )
            collateral_enabled = raw_position.get("usage_as_collateral_enabled")
            if not isinstance(collateral_enabled, bool):
                raise EvidenceError("checkpoint collateral flag is invalid")
            if supplied or variable_debt or stable_debt:
                positions.append(
                    {
                        "asset": asset,
                        "symbol": reserve.get("symbol"),
                        "scaled_supply": scaled_supply,
                        "supplied": supplied,
                        "collateral_enabled": collateral_enabled,
                        "scaled_variable_debt": scaled_variable,
                        "variable_debt": variable_debt,
                        "stable_debt": stable_debt,
                        "debt_type": (
                            "variable_and_stable"
                            if variable_debt and stable_debt
                            else "variable" if variable_debt
                            else "stable" if stable_debt
                            else "none"
                        ),
                        "price_feed": reserve["price_feed"],
                    }
                )
                feed_index.setdefault(reserve["price_feed"], []).append(address)
            reserve_id = reserve["reserve_id"]
            if collateral_enabled and supplied:
                derived_configuration |= 1 << (reserve_id * 2 + 1)
            if variable_debt or stable_debt:
                derived_configuration |= 1 << (reserve_id * 2)
        if configuration != derived_configuration:
            raise EvidenceError(f"checkpoint borrower configuration mismatch: {address}")
        if not any(position["variable_debt"] or position["stable_debt"] for position in positions):
            raise EvidenceError("checkpoint output contains a borrower without active debt")
        inventory_borrowers.append(
            {
                "address": address,
                "account_configuration_bitmap": configuration,
                "derived_account_configuration_bitmap": derived_configuration,
                "emode_category": emode,
                "positions": positions,
                "feed_dependencies": sorted({position["price_feed"] for position in positions}),
                "last_block": checkpoint_block,
                "last_block_hash": checkpoint_hash,
                "evidence_sha256": sorted(
                    {checkpoint["content_sha256"], checkpoint["discovery_content_sha256"]}
                ),
                "event_kinds": ["hash_bound_checkpoint"],
                "completeness_status": "complete",
                "incomplete_reasons": [],
            }
        )

    inventory = {
        "schema": "phoenix.atlas.borrower-inventory.v1",
        "chain_id": 42161,
        "market_content_sha256": market["content_sha256"],
        "checkpoint_content_sha256": checkpoint["content_sha256"],
        "discovery_content_sha256": checkpoint["discovery_content_sha256"],
        "bootstrap_mode": "hash_bound_independently_agreed_checkpoint",
        "checkpoint_block": checkpoint_block,
        "checkpoint_hash": checkpoint_hash,
        "reserves": [reserves[key] for key in sorted(reserves)],
        "emode_categories": copy.deepcopy(raw_emodes),
        "liquidation_logic": liquidation_logic,
        "borrowers": sorted(inventory_borrowers, key=lambda item: item["address"]),
        "feed_index": {
            feed: sorted(set(addresses)) for feed, addresses in sorted(feed_index.items())
        },
        "evidence_event_count": _require_int(
            checkpoint.get("discovery_log_count"), "checkpoint.discovery_log_count"
        ),
        "discovered_borrower_count": discovered_borrower_count,
        "unique_borrower_count": len(inventory_borrowers),
        "completeness_status": "complete",
        "incomplete_reasons": [],
        "execution_authority": {
            "signer": False,
            "bond": False,
            "bid": False,
            "solver": False,
            "submission": False,
            "production_write": False,
        },
    }
    return bind_hash(inventory, "snapshot_sha256")


def _reserve_map(inventory: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {reserve["asset"].lower(): reserve for reserve in inventory["reserves"]}


def _emode_map(inventory: dict[str, Any]) -> dict[int, dict[str, Any]]:
    return {category["category_id"]: category for category in inventory.get("emode_categories", [])}


def _price(prices: dict[str, Any], asset: str) -> int:
    if asset not in prices:
        raise EvidenceError(f"price missing for {asset}")
    return _require_int(prices[asset], f"price.{asset}", 1)


def _position_value(amount: int, price: int, decimals: int) -> int:
    return amount * price // (10**decimals)


def _debt_position_value(amount: int, price: int, decimals: int) -> int:
    return mul_div_ceil(amount, price, 10**decimals)


def calculate_account(
    borrower: dict[str, Any],
    reserves: dict[str, dict[str, Any]],
    emodes: dict[int, dict[str, Any]],
    prices: dict[str, Any],
) -> dict[str, int]:
    collateral_base = 0
    debt_base = 0
    weighted_threshold_numerator = 0
    category = emodes.get(borrower.get("emode_category", 0))
    category_collateral = set(category.get("collateral_assets", [])) if category else set()
    for position in borrower["positions"]:
        asset = position["asset"].lower()
        reserve = reserves[asset]
        price = _price(prices, asset)
        decimals = _require_int(reserve["decimals"], "reserve.decimals")
        supplied = _require_int(position.get("supplied"), "position.supplied")
        variable_debt = _require_int(position.get("variable_debt"), "position.variable_debt")
        stable_debt = _require_int(position.get("stable_debt"), "position.stable_debt")
        if position.get("collateral_enabled") and supplied:
            value = _position_value(supplied, price, decimals)
            threshold = reserve.get("liquidation_threshold_bps")
            if category and asset in category_collateral:
                threshold = category.get("liquidation_threshold_bps")
            threshold = _require_int(threshold, "liquidation_threshold_bps")
            collateral_base += value
            weighted_threshold_numerator += value * threshold
        debt_base += _debt_position_value(
            variable_debt + stable_debt, price, decimals
        )
    health_factor = (
        wad_div(weighted_threshold_numerator, debt_base) // PERCENTAGE_FACTOR
        if debt_base
        else 2**256 - 1
    )
    return {
        "total_collateral_base": collateral_base,
        "total_debt_base": debt_base,
        "weighted_liquidation_threshold_numerator": weighted_threshold_numerator,
        "health_factor_wad": health_factor,
    }


def _scenario_costs(quote: dict[str, Any], scenario: str) -> int:
    values = quote.get("scenario_costs_base", {}).get(scenario)
    if not isinstance(values, dict):
        raise EvidenceError(f"quote.scenario_costs_base.{scenario} is missing")
    total = 0
    for attribution in ("dex_fee_base", "price_impact_base"):
        _require_int(values.get(attribution), f"quote.{scenario}.{attribution}")
    for field in (
        "gas_and_l1_data_base",
        "ordering_cost_base",
        "failure_reserve_base",
    ):
        total += _require_int(values.get(field), f"quote.{scenario}.{field}")
    return total


def _pair_quote(
    auction: dict[str, Any], debt_asset: str, collateral_asset: str
) -> dict[str, Any] | None:
    matches = [
        quote
        for quote in auction.get("pair_quotes", [])
        if str(quote.get("debt_asset", "")).lower() == debt_asset
        and str(quote.get("collateral_asset", "")).lower() == collateral_asset
    ]
    if len(matches) > 1:
        raise EvidenceError("duplicate quote for debt/collateral pair")
    return matches[0] if matches else None


def _pair_economics(
    borrower: dict[str, Any],
    debt_position: dict[str, Any],
    collateral_position: dict[str, Any],
    account_after: dict[str, int],
    reserves: dict[str, dict[str, Any]],
    emodes: dict[int, dict[str, Any]],
    prices: dict[str, Any],
    quote: dict[str, Any] | None,
    liquidation_logic: dict[str, Any],
    checkpoint_block: int,
    checkpoint_hash: str,
    block_timestamp: int,
) -> dict[str, Any]:
    debt_asset = debt_position["asset"].lower()
    collateral_asset = collateral_position["asset"].lower()
    debt_reserve = reserves[debt_asset]
    collateral_reserve = reserves[collateral_asset]
    debt_amount = _require_int(debt_position["variable_debt"], "variable_debt") + _require_int(debt_position["stable_debt"], "stable_debt")
    collateral_amount = _require_int(collateral_position["supplied"], "supplied")
    debt_price = _price(prices, debt_asset)
    collateral_price = _price(prices, collateral_asset)
    debt_unit = 10 ** _require_int(debt_reserve["decimals"], "debt.decimals")
    collateral_unit = 10 ** _require_int(collateral_reserve["decimals"], "collateral.decimals")
    debt_reserve_base = (debt_amount * debt_price + debt_unit - 1) // debt_unit
    collateral_reserve_base = collateral_amount * collateral_price // collateral_unit
    default_close_factor_bps = _require_int(
        liquidation_logic.get("default_close_factor_bps"),
        "liquidation_logic.default_close_factor_bps",
    )
    close_factor_hf_threshold_wad = _require_int(
        liquidation_logic.get("close_factor_hf_threshold_wad"),
        "liquidation_logic.close_factor_hf_threshold_wad",
    )
    minimum_reserve_value_base = _require_int(
        liquidation_logic.get("minimum_reserve_value_base"),
        "liquidation_logic.minimum_reserve_value_base",
    )
    minimum_leftover_base = _require_int(
        liquidation_logic.get("minimum_leftover_base"),
        "liquidation_logic.minimum_leftover_base",
    )

    max_debt = debt_amount
    if (
        collateral_reserve_base >= minimum_reserve_value_base
        and debt_reserve_base >= minimum_reserve_value_base
        and account_after["health_factor_wad"] > close_factor_hf_threshold_wad
    ):
        default_base = percent_mul(
            account_after["total_debt_base"], default_close_factor_bps
        )
        if debt_reserve_base > default_base:
            max_debt = default_base * debt_unit // debt_price
    repay = min(debt_amount, max_debt)

    category = emodes.get(borrower.get("emode_category", 0))
    category_collateral = set(category.get("collateral_assets", [])) if category else set()
    bonus_bps = collateral_reserve.get("liquidation_bonus_bps")
    if category and collateral_asset in category_collateral:
        bonus_bps = category.get("liquidation_bonus_bps")
    bonus_bps = _require_int(bonus_bps, "liquidation_bonus_bps", PERCENTAGE_FACTOR)
    fee_bps = _require_int(
        collateral_reserve.get("liquidation_protocol_fee_bps"),
        "liquidation_protocol_fee_bps",
    )

    base_collateral = debt_price * repay * collateral_unit // (collateral_price * debt_unit)
    max_collateral = percent_mul_floor(base_collateral, bonus_bps)
    if max_collateral > collateral_amount:
        seized_before_fee = collateral_amount
        repay = percent_div_ceil(
            collateral_price * seized_before_fee * debt_unit // (debt_price * collateral_unit),
            bonus_bps,
        )
    else:
        seized_before_fee = max_collateral
    bonus_collateral = seized_before_fee - percent_div_floor(seized_before_fee, bonus_bps)
    protocol_fee = percent_mul_ceil(bonus_collateral, fee_bps) if fee_bps else 0
    liquidator_collateral = seized_before_fee - protocol_fee
    repay_base = _debt_position_value(
        repay, debt_price, debt_reserve["decimals"]
    )
    collateral_base = _position_value(
        liquidator_collateral, collateral_price, collateral_reserve["decimals"]
    )
    remaining_debt = debt_amount - repay
    remaining_collateral = collateral_amount - seized_before_fee
    dust_ok = True
    if remaining_debt and remaining_collateral:
        remaining_debt_base = (
            remaining_debt * debt_price + debt_unit - 1
        ) // debt_unit
        remaining_collateral_base = (
            remaining_collateral * collateral_price // collateral_unit
        )
        dust_ok = (
            remaining_debt_base >= minimum_leftover_base
            and remaining_collateral_base >= minimum_leftover_base
        )
    grace_ok = (
        _require_int(
            collateral_reserve.get("liquidation_grace_period_until"),
            "collateral.liquidation_grace_period_until",
        )
        < block_timestamp
        and _require_int(
            debt_reserve.get("liquidation_grace_period_until"),
            "debt.liquidation_grace_period_until",
        )
        < block_timestamp
    )
    reserve_flags_ok = (
        collateral_reserve.get("active") is True
        and debt_reserve.get("active") is True
        and collateral_reserve.get("paused") is False
        and debt_reserve.get("paused") is False
    )
    result = {
        "debt_asset": debt_asset,
        "collateral_asset": collateral_asset,
        "close_factor_rules": {
            "default_bps": default_close_factor_bps,
            "health_factor_threshold_wad": close_factor_hf_threshold_wad,
            "minimum_reserve_value_base": minimum_reserve_value_base,
            "minimum_leftover_base": minimum_leftover_base,
        },
        "repay": repay,
        "seized_before_protocol_fee": seized_before_fee,
        "protocol_fee_collateral": protocol_fee,
        "liquidator_collateral": liquidator_collateral,
        "liquidation_bonus_bps": bonus_bps,
        "liquidation_protocol_fee_bps": fee_bps,
        "repay_base": repay_base,
        "liquidator_collateral_base": collateral_base,
        "required_unwind_input_collateral": liquidator_collateral,
        "validity": {
            "reserve_flags": reserve_flags_ok,
            "liquidation_grace_period_elapsed": grace_ok,
            "dust_rule": dust_ok,
        },
    }
    if not (reserve_flags_ok and grace_ok and dust_ok):
        result.update(
            {
                "economics_status": "INVALID_PROTOCOL_CONSTRAINT",
                "pnl_base": None,
                "max_rational_atlas_bid_base": None,
            }
        )
        return result
    if quote is None:
        result.update(
            {
                "economics_status": "INCOMPLETE_PAIR_QUOTE",
                "pnl_base": None,
                "max_rational_atlas_bid_base": None,
                "incomplete_reason": "exact_block_bound_flash_and_unwind_quote_missing",
            }
        )
        return result

    if _require_int(quote.get("block_number"), "quote.block_number") != checkpoint_block:
        raise EvidenceError("pair quote block does not match inventory checkpoint")
    if _require_hash(quote.get("block_hash"), "quote.block_hash") != checkpoint_hash:
        raise EvidenceError("pair quote hash does not match inventory checkpoint")
    if _require_address(quote.get("debt_asset"), "quote.debt_asset") != debt_asset:
        raise EvidenceError("pair quote debt asset mismatch")
    if _require_address(quote.get("collateral_asset"), "quote.collateral_asset") != collateral_asset:
        raise EvidenceError("pair quote collateral asset mismatch")
    _require_address(quote.get("flash_provider"), "quote.flash_provider")
    _require_address(quote.get("unwind_venue"), "quote.unwind_venue")
    if _require_int(quote.get("flash_max_amount"), "quote.flash_max_amount") < repay:
        result.update(
            {
                "economics_status": "INCOMPLETE_FLASH_CAPACITY",
                "pnl_base": None,
                "max_rational_atlas_bid_base": None,
            }
        )
        return result
    if _require_int(quote.get("unwind_input_collateral"), "quote.unwind_input_collateral") != liquidator_collateral:
        raise EvidenceError("unwind quote input is not the exact seized collateral")
    flash_premium_bps = _require_int(
        quote.get("flash_premium_bps"), "quote.flash_premium_bps"
    )
    flash_premium_amount = percent_mul_ceil(repay, flash_premium_bps)
    flash_premium_base = _debt_position_value(
        flash_premium_amount, debt_price, debt_reserve["decimals"]
    )
    outputs = quote.get("unwind_outputs_debt")
    if not isinstance(outputs, dict):
        raise EvidenceError("quote.unwind_outputs_debt is missing")
    pnl: dict[str, int] = {}
    output_base: dict[str, int] = {}
    gross_unwind_margin_base: dict[str, int] = {}
    for scenario in ("expected", "conservative", "severe"):
        output_amount = _require_int(outputs.get(scenario), f"quote.output.{scenario}")
        output_base[scenario] = _position_value(
            output_amount, debt_price, debt_reserve["decimals"]
        )
        gross_unwind_margin_base[scenario] = output_base[scenario] - repay_base
        pnl[scenario] = (
            output_base[scenario]
            - repay_base
            - flash_premium_base
            - _scenario_costs(quote, scenario)
        )
    retained_floor = _require_int(
        quote.get("retained_profit_floor_base"),
        "quote.retained_profit_floor_base",
    )
    result.update(
        {
            "economics_status": "EXACT_FULL_COST",
            "quote_identity": {
                "block_number": checkpoint_block,
                "block_hash": checkpoint_hash,
                "flash_provider": quote["flash_provider"].lower(),
                "unwind_venue": quote["unwind_venue"].lower(),
            },
            "flash_premium_base": flash_premium_base,
            "flash_premium_amount": flash_premium_amount,
            "unwind_outputs_debt": {
                scenario: _require_int(outputs[scenario], f"quote.output.{scenario}")
                for scenario in ("expected", "conservative", "severe")
            },
            "unwind_output_base": output_base,
            "gross_unwind_margin_base": gross_unwind_margin_base,
            "cost_attribution_base": copy.deepcopy(quote["scenario_costs_base"]),
            "pnl_base": pnl,
            "max_rational_atlas_bid_base": max(
                0, pnl["conservative"] - retained_floor
            ),
            "retained_profit_floor_base": retained_floor,
        }
    )
    return result


def evaluate_auction(
    inventory: dict[str, Any], auction: dict[str, Any]
) -> dict[str, Any]:
    verify_inventory(inventory)
    verify_hash(auction)
    if auction.get("schema") != "phoenix.atlas.aave-auction-evaluation.v1":
        raise EvidenceError("unsupported auction schema")
    affected_asset = _require_address(auction.get("affected_asset"), "affected_asset")
    reserves_at_checkpoint = _reserve_map(inventory)
    if affected_asset not in reserves_at_checkpoint:
        raise EvidenceError("auction affected asset is not an indexed reserve")
    affected_feed = _require_address(auction.get("affected_feed"), "affected_feed")
    if affected_feed != reserves_at_checkpoint[affected_asset]["price_feed"].lower():
        raise EvidenceError("auction feed does not match the affected reserve")
    before_price = _require_int(auction.get("price_before"), "price_before", 1)
    after_price = _require_int(auction.get("price_after"), "price_after", 1)
    base = {
        "schema": "phoenix.atlas.aave-auction-result.v1",
        "auction_content_sha256": auction["content_sha256"],
        "inventory_snapshot_sha256": inventory["snapshot_sha256"],
        "affected_asset": affected_asset,
        "affected_feed": affected_feed,
        "price_before": before_price,
        "price_after": after_price,
        "price_delta": after_price - before_price,
        "execution_authority": inventory["execution_authority"],
    }
    if before_price == after_price:
        return bind_hash(
            {
                **base,
                "status": "ZERO_DELTA_NO_RISK_CHANGE",
                "borrowers_evaluated": 0,
                "newly_liquidatable": [],
                "already_liquidatable": [],
                "pairs": [],
                "fast_path": True,
            },
            "result_sha256",
        )
    if inventory.get("completeness_status") != "complete":
        return bind_hash(
            {
                **base,
                "status": "INCOMPLETE_INVENTORY",
                "borrowers_evaluated": 0,
                "newly_liquidatable": [],
                "already_liquidatable": [],
                "pairs": [],
                "fast_path": False,
                "incomplete_reasons": inventory.get("incomplete_reasons", []),
            },
            "result_sha256",
        )

    reserves = reserves_at_checkpoint
    prices_after = {
        _require_address(key, "prices_after key"): _require_int(value, "prices_after value", 1)
        for key, value in auction.get("prices_after", {}).items()
    }
    prices_before = dict(prices_after)
    prices_before[affected_asset] = before_price
    prices_after[affected_asset] = after_price
    emodes = _emode_map(inventory)
    feed = reserves[affected_asset]["price_feed"].lower()
    affected_addresses = set(inventory.get("feed_index", {}).get(feed, []))
    borrower_map = {item["address"]: item for item in inventory["borrowers"]}
    newly: list[dict[str, Any]] = []
    already: list[dict[str, Any]] = []
    pairs: list[dict[str, Any]] = []
    evaluated = 0
    all_pair_economics_exact = True
    for address in sorted(affected_addresses):
        borrower = borrower_map[address]
        if borrower.get("completeness_status") != "complete":
            raise EvidenceError("complete inventory contains incomplete borrower")
        account_before = calculate_account(borrower, reserves, emodes, prices_before)
        account_after = calculate_account(borrower, reserves, emodes, prices_after)
        evaluated += 1
        record = {
            "borrower": address,
            "health_factor_before_wad": account_before["health_factor_wad"],
            "health_factor_after_wad": account_after["health_factor_wad"],
        }
        if account_after["health_factor_wad"] >= WAD:
            continue
        if account_before["health_factor_wad"] >= WAD:
            newly.append(record)
        else:
            already.append(record)
        debts = [
            position
            for position in borrower["positions"]
            if (position["variable_debt"] or position["stable_debt"])
        ]
        collaterals = [
            position
            for position in borrower["positions"]
            if position["collateral_enabled"] and position["supplied"]
        ]
        for debt in debts:
            for collateral in collaterals:
                economics = _pair_economics(
                    borrower,
                    debt,
                    collateral,
                    account_after,
                    reserves,
                    emodes,
                    prices_after,
                    _pair_quote(auction, debt["asset"].lower(), collateral["asset"].lower()),
                    inventory["liquidation_logic"],
                    _require_int(inventory.get("checkpoint_block"), "checkpoint_block"),
                    _require_hash(inventory.get("checkpoint_hash"), "checkpoint_hash"),
                    _require_int(auction.get("block_timestamp"), "block_timestamp", 1),
                )
                if economics["economics_status"] != "EXACT_FULL_COST":
                    all_pair_economics_exact = False
                pairs.append({"borrower": address, **economics})

    return bind_hash(
        {
            **base,
            "status": "COMPLETE" if all_pair_economics_exact else "INCOMPLETE_PAIR_ECONOMICS",
            "borrowers_evaluated": evaluated,
            "newly_liquidatable": newly,
            "already_liquidatable": already,
            "pairs": pairs,
            "fast_path": False,
        },
        "result_sha256",
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate = subparsers.add_parser("validate-market")
    validate.add_argument("--input", required=True)
    build = subparsers.add_parser("build")
    build.add_argument("--market", required=True)
    build.add_argument("--transcript", required=True)
    build.add_argument("--output", required=True)
    import_checkpoint = subparsers.add_parser("import-checkpoint")
    import_checkpoint.add_argument("--market", required=True)
    import_checkpoint.add_argument("--checkpoint", required=True)
    import_checkpoint.add_argument("--output", required=True)
    evaluate = subparsers.add_parser("evaluate")
    evaluate.add_argument("--inventory", required=True)
    evaluate.add_argument("--auction", required=True)
    evaluate.add_argument("--output", required=True)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if args.command == "validate-market":
        market = read_json(args.input)
        validate_market(market)
        print(json.dumps({"status": "valid", "content_sha256": market["content_sha256"]}, sort_keys=True))
        return 0
    if args.command == "build":
        inventory = build_inventory(read_json(args.market), read_json(args.transcript))
        write_json(args.output, inventory)
        return 0
    if args.command == "import-checkpoint":
        inventory = build_inventory_from_checkpoint(
            read_json(args.market), read_json(args.checkpoint)
        )
        write_json(args.output, inventory)
        return 0
    if args.command == "evaluate":
        result = evaluate_auction(read_json(args.inventory), read_json(args.auction))
        write_json(args.output, result)
        return 0
    raise AssertionError(args.command)


if __name__ == "__main__":
    raise SystemExit(main())
