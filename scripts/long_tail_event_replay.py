#!/usr/bin/env python3
"""Fail-closed evidence gate for the ten reviewed Phoenix long-tail events.

This is deliberately not a generic replay engine.  The admissible event set is
compiled into this module and every external proof must bind to one of those
identities.  A proof may use a reviewed prestate/diff trace or a bounded replay
of canonical transactions from the parent block through the initiating
transaction.  Current, later, and end-of-block state are rejected.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any


CHAIN_ID = 42161
ALLOWLIST_SCHEMA = "phoenix.long-tail.allowlist.v1"
BOUNDARY_SCHEMA = "phoenix.long-tail.transaction-boundary.v1"
REPORT_SCHEMA = "phoenix.long-tail.event-report.v1"
LINK_BACKFILL_SCHEMA = "phoenix.long-tail.link-backfill.v1"
ATLAS_LINK_SCHEMA = "phoenix.long-tail.atlas-link.v1"

WETH = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
USDC = "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
AAVE = "0xba5ddd1f9d7f570dc94a51479a000e3bce967196"
UNI = "0xfa7f8980b0f1e64a2062791cc3b0871572f1f7f0"
LINK = "0xf97f4df75117a78c1a5a0dbb814af92458539fb4"
UNISWAP_V3_FACTORY = "0x1f98431c8ad98523631ae4a59f267346ea31f984"

EXPECTED_EVENT_BINDINGS = frozenset(
    {
        ("467484826", "0x089f78f2c6582bca6721a05efb6ea49b6cdcc5d7ac98e45ed3b2f1b4bff60c72"),
        ("467608634", "0x1f8ecad783494ababd17fb217133d7e331538238dcd743121c22db38e4106d39"),
        ("467608770", "0x2d8c3b1e585ba717c393d6efb62a8f6c807572fbd488c128ba5ce0410a40737e"),
        ("467608839", "0x671a7aae11248eb19f5984e33dd0016dc19c340a7d032acc85cdeb0c23e82702"),
        ("467609448", "0x1d30e4fb2437fe61e4809899d758585955d6c207cdfb1ab15c5ed155144207ff"),
        ("467609455", "0x64dd49ec80d7afdebd77eead05e5300535c07e7415fe1f4f9824803f00f07ac0"),
        ("467609502", "0x5081dcb64cdd283851338d51c3779766a73491112ef76c98221c63a7f8a65cd1"),
        ("467610316", "0xc3e06072b384b9b34ec455922c199bba20b6edd4a30f5b088166d99d5cc205bd"),
        ("467610363", "0x1afdd9348ffa0a48b9d495854ff21f8d5da0a87abfd32fd09587c417e599f7cd"),
        ("467610934", "0xb0043b0d52fccbf0ec7f84bca112ea98aaf90fa5bcc4504df75d9769e1717bf9"),
    }
)
EXPECTED_EVENT_IDENTITIES = frozenset(
    f"phoenix.engine.input.v1:{sequence}:{transaction_hash}"
    for sequence, transaction_hash in EXPECTED_EVENT_BINDINGS
)

HASH_RE = re.compile(r"^0x[0-9a-f]{64}$")
ADDRESS_RE = re.compile(r"^0x[0-9a-f]{40}$")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


class EvidenceError(ValueError):
    """Raised when evidence cannot satisfy the exact boundary contract."""


def _require(condition: bool, reason: str) -> None:
    if not condition:
        raise EvidenceError(reason)


def _integer(value: Any, field: str, *, minimum: int = 0) -> int:
    _require(type(value) in (int, str), f"{field}_not_integer")
    try:
        parsed = int(value)
    except (TypeError, ValueError) as exc:
        raise EvidenceError(f"{field}_not_integer") from exc
    _require(parsed >= minimum, f"{field}_below_minimum")
    return parsed


def canonical_hash(value: Any) -> str:
    encoded = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def event_identity(event: dict[str, Any]) -> str:
    identity = event.get("source_event_identity")
    _require(isinstance(identity, dict), "source_event_identity_invalid")
    _require(
        set(identity) == {"schema_version", "source_feed_sequence", "transaction_hash"},
        "source_event_identity_fields_invalid",
    )
    _require(identity["schema_version"] == "phoenix.engine.input.v1", "source_event_identity_schema_invalid")
    sequence = str(identity["source_feed_sequence"])
    _integer(sequence, "source_event_identity_sequence", minimum=1)
    transaction_hash = identity["transaction_hash"]
    _require(HASH_RE.fullmatch(transaction_hash) is not None, "source_event_identity_transaction_invalid")
    _require(sequence == str(event.get("source_feed_sequence")), "source_event_sequence_mismatch")
    _require(transaction_hash == event.get("transaction_hash"), "source_event_transaction_mismatch")
    return f"phoenix.engine.input.v1:{sequence}:{transaction_hash}"


def _validate_pool_path(event: dict[str, Any]) -> None:
    tokens = event["token_path"]
    fees = event["fee_path"]
    pools = event["pool_path"]
    _require(len(tokens) == len(fees) + 1, "token_fee_cardinality_mismatch")
    _require(len(pools) == len(fees), "pool_fee_cardinality_mismatch")
    for index, pool in enumerate(pools):
        parts = pool.split(":")
        _require(len(parts) == 3, "pool_identity_malformed")
        token0, token1, fee = parts
        _require(ADDRESS_RE.fullmatch(token0) is not None, "pool_token0_invalid")
        _require(ADDRESS_RE.fullmatch(token1) is not None, "pool_token1_invalid")
        _require(int(fee) == int(fees[index]), "pool_fee_identity_mismatch")
        _require(
            {token0, token1} == {tokens[index], tokens[index + 1]},
            "pool_token_identity_mismatch",
        )


def validate_event(event: dict[str, Any]) -> None:
    required = {
        "source_event_identity",
        "source_identity_hash",
        "transaction_hash",
        "block_number",
        "block_hash",
        "transaction_index",
        "command_index",
        "event_index",
        "token_path",
        "fee_path",
        "pool_path",
        "pool_addresses",
        "initiating_amount",
        "parent_block_number",
        "parent_block_hash",
        "optimistic_upper_bound",
        "boundary_evidence",
    }
    _require(required <= event.keys(), "event_required_field_missing")
    _require(
        event_identity(event) in EXPECTED_EVENT_IDENTITIES,
        "event_not_in_immutable_allowlist",
    )
    _require(HEX64_RE.fullmatch(event["source_identity_hash"]) is not None, "identity_hash_invalid")
    _require(HASH_RE.fullmatch(event["transaction_hash"]) is not None, "transaction_hash_invalid")
    _require(HASH_RE.fullmatch(event["block_hash"]) is not None, "block_hash_invalid")
    _require(HASH_RE.fullmatch(event["parent_block_hash"]) is not None, "parent_block_hash_invalid")
    block_number = _integer(event["block_number"], "block_number", minimum=1)
    parent_block_number = _integer(event["parent_block_number"], "parent_block_number")
    _require(parent_block_number + 1 == block_number, "parent_block_not_adjacent")
    _integer(event["transaction_index"], "transaction_index")
    _integer(event["command_index"], "command_index")
    _integer(event["event_index"], "event_index")
    _integer(event["initiating_amount"], "initiating_amount", minimum=1)
    _require(all(ADDRESS_RE.fullmatch(token) for token in event["token_path"]), "token_path_invalid")
    _require(all(ADDRESS_RE.fullmatch(pool) for pool in event["pool_addresses"]), "pool_address_invalid")
    _require(event["optimistic_upper_bound"] == "positive_notional_ceiling", "upper_bound_not_reviewed")
    _validate_pool_path(event)

    boundary = event["boundary_evidence"]
    _require(boundary["status"] in {"complete", "incomplete"}, "boundary_status_invalid")
    if boundary["status"] == "incomplete":
        _require(boundary["method"] == "unavailable", "incomplete_boundary_method_invalid")
        _require(bool(boundary.get("failure_reason")), "incomplete_boundary_reason_missing")


def load_allowlist(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    _require(payload.get("schema_version") == ALLOWLIST_SCHEMA, "allowlist_schema_invalid")
    _require(payload.get("chain_id") == CHAIN_ID, "allowlist_chain_invalid")
    events = payload.get("events")
    _require(isinstance(events, list), "allowlist_events_invalid")
    _require(len(events) == 10, "immutable_event_count_changed")
    identities = {event_identity(event) for event in events}
    _require(identities == EXPECTED_EVENT_IDENTITIES, "immutable_event_set_changed")
    for event in events:
        validate_event(event)
    aave_count = sum(event["token_path"] == [AAVE, WETH, USDC] for event in events)
    uni_count = sum(event["token_path"] == [USDC, WETH, UNI] for event in events)
    _require((aave_count, uni_count) == (9, 1), "immutable_surface_distribution_changed")
    return payload


def _validate_provider_agreement(event: dict[str, Any], proof: dict[str, Any]) -> None:
    providers = proof.get("provider_bindings")
    _require(isinstance(providers, list) and len(providers) >= 2, "independent_provider_evidence_missing")
    provider_ids = {item.get("provider_id") for item in providers}
    _require(len(provider_ids) >= 2 and None not in provider_ids, "providers_not_independent")
    for item in providers:
        _require(item.get("chain_id") == CHAIN_ID, "provider_chain_disagreement")
        _require(item.get("block_number") == event["block_number"], "provider_block_number_disagreement")
        _require(item.get("block_hash") == event["block_hash"], "provider_block_hash_disagreement")
        _require(item.get("transaction_hash") == event["transaction_hash"], "provider_transaction_disagreement")
        _require(item.get("transaction_index") == event["transaction_index"], "provider_transaction_index_disagreement")
    _require(HEX64_RE.fullmatch(proof.get("provider_agreement_hash", "")) is not None, "provider_agreement_hash_missing")


def _validate_parent_replay(event: dict[str, Any], proof: dict[str, Any]) -> None:
    transactions = proof.get("canonical_transactions")
    boundary_index = _integer(event["transaction_index"], "transaction_index")
    _require(isinstance(transactions, list), "canonical_transactions_missing")
    _require(len(transactions) == boundary_index + 1, "parent_replay_did_not_stop_at_boundary")
    for expected_index, transaction in enumerate(transactions):
        _require(transaction.get("transaction_index") == str(expected_index), "canonical_transaction_order_gap")
        _require(HASH_RE.fullmatch(transaction.get("transaction_hash", "")) is not None, "canonical_transaction_hash_invalid")
        _require(transaction.get("receipt_status") in {"success", "reverted"}, "canonical_receipt_status_missing")
        _require(HEX64_RE.fullmatch(transaction.get("receipt_hash", "")) is not None, "canonical_receipt_hash_missing")
    last = transactions[-1]
    _require(last["transaction_hash"] == event["transaction_hash"], "replay_boundary_transaction_mismatch")
    _require(proof.get("last_replayed_transaction_index") == event["transaction_index"], "replay_last_index_mismatch")
    _require(proof.get("last_replayed_transaction_hash") == event["transaction_hash"], "replay_last_hash_mismatch")


def validate_boundary_proof(event: dict[str, Any], proof: dict[str, Any]) -> None:
    _require(proof.get("schema_version") == BOUNDARY_SCHEMA, "boundary_schema_invalid")
    exact_fields = {
        "transaction_hash": "transaction_hash",
        "block_number": "block_number",
        "block_hash": "block_hash",
        "transaction_index": "transaction_index",
        "parent_block_number": "parent_block_number",
        "parent_block_hash": "parent_block_hash",
    }
    _require(proof.get("source_event_identity") == event_identity(event), "boundary_source_event_identity_mismatch")
    for proof_field, event_field in exact_fields.items():
        _require(proof.get(proof_field) == event[event_field], f"boundary_{proof_field}_mismatch")
    _require(
        proof.get("boundary_scope") == "post_initiating_transaction_pre_next_canonical_transaction",
        "state_boundary_substitution_forbidden",
    )
    _require(proof.get("state_block_tag") not in {"latest", "pending", "safe", "finalized"}, "dynamic_state_tag_forbidden")
    _require(proof.get("end_of_block_state") is False, "end_of_block_state_forbidden")
    method = proof.get("method")
    _require(method in {"debug_trace_transaction_prestate_diff", "bounded_parent_block_replay"}, "boundary_method_unsupported")
    if method == "bounded_parent_block_replay":
        _validate_parent_replay(event, proof)
    else:
        _require(HEX64_RE.fullmatch(proof.get("prestate_hash", "")) is not None, "prestate_hash_missing")
        _require(HEX64_RE.fullmatch(proof.get("state_diff_hash", "")) is not None, "state_diff_hash_missing")
    _require(HEX64_RE.fullmatch(proof.get("post_state_hash", "")) is not None, "post_state_hash_missing")

    state_reads = proof.get("state_reads")
    _require(isinstance(state_reads, list) and state_reads, "same_boundary_state_reads_missing")
    roles = {read.get("role") for read in state_reads}
    _require({"initiating", "alternative"} <= roles, "initiating_or_alternative_state_missing")
    for read in state_reads:
        _require(ADDRESS_RE.fullmatch(read.get("pool", "")) is not None, "state_read_pool_invalid")
        _require(read.get("block_number") == event["block_number"], "state_read_block_number_mismatch")
        _require(read.get("block_hash") == event["block_hash"], "state_read_block_hash_mismatch")
        _require(read.get("transaction_index") == event["transaction_index"], "state_read_boundary_index_mismatch")
        _require(read.get("boundary_scope") == proof["boundary_scope"], "state_read_scope_mismatch")
        _require(HEX64_RE.fullmatch(read.get("state_hash", "")) is not None, "state_read_hash_missing")
    _validate_provider_agreement(event, proof)


FULL_COST_FIELDS = (
    "gross_profit_wei",
    "dex_fee_wei",
    "price_impact_wei",
    "flash_premium_wei",
    "execution_gas_wei",
    "l1_data_fee_wei",
    "ordering_cost_wei",
    "failure_reserve_wei",
    "retained_profit_floor_wei",
    "expected_pnl_wei",
    "conservative_pnl_wei",
    "severe_pnl_wei",
)


def validate_economics(economics: dict[str, Any]) -> None:
    for field in FULL_COST_FIELDS:
        _integer(economics.get(field), field, minimum=0 if field.endswith("_cost_wei") or field in {
            "dex_fee_wei", "price_impact_wei", "flash_premium_wei", "execution_gas_wei",
            "l1_data_fee_wei", "ordering_cost_wei", "failure_reserve_wei", "retained_profit_floor_wei",
            "gross_profit_wei"
        } else -10**100)
    fork = economics.get("fork")
    _require(isinstance(fork, dict) and fork.get("passed") is True, "exact_fork_not_passed")
    _require(fork.get("public_broadcast") is False, "public_broadcast_forbidden")
    _require(fork.get("signer_used") is False, "signer_use_forbidden")
    _require(HEX64_RE.fullmatch(fork.get("result_hash", "")) is not None, "fork_result_hash_missing")
    lifetime = _integer(economics.get("opportunity_lifetime_ms"), "opportunity_lifetime_ms", minimum=1)
    latency = _integer(economics.get("end_to_end_latency_ms"), "end_to_end_latency_ms", minimum=1)
    _require(lifetime > latency, "opportunity_lifetime_not_above_latency")
    prediction_error = _integer(economics.get("prediction_error_bps"), "prediction_error_bps")
    prediction_limit = _integer(economics.get("prediction_error_limit_bps"), "prediction_error_limit_bps")
    _require(prediction_error <= prediction_limit, "prediction_error_above_limit")
    _require(
        economics.get("severe_loss_within_reviewed_limit") is True,
        "severe_loss_outside_reviewed_limit",
    )
    _require(
        economics.get("security_accounting_defects") == [],
        "security_or_accounting_defect_unresolved",
    )


def evaluate(allowlist: dict[str, Any], proofs: dict[str, Any] | None = None) -> dict[str, Any]:
    proof_map = {} if proofs is None else proofs.get("proofs", {})
    rows = []
    for event in allowlist["events"]:
        canonical_identity = event_identity(event)
        proof = proof_map.get(canonical_identity)
        if proof is None:
            boundary = event["boundary_evidence"]
            rows.append(
                {
                    "source_event_identity": canonical_identity,
                    "transaction_hash": event["transaction_hash"],
                    "surface": event["surface"],
                    "optimistic_upper_bound": event["optimistic_upper_bound"],
                    "replay_required": True,
                    "boundary_complete": False,
                    "boundary_failure_reason": boundary["failure_reason"],
                    "expected_pnl_wei": None,
                    "conservative_pnl_wei": None,
                    "severe_pnl_wei": None,
                    "recommendation": "STATE_INCOMPLETE",
                }
            )
            continue
        validate_boundary_proof(event, proof)
        economics = proof.get("economics")
        if economics is None:
            recommendation = "BOUNDARY_COMPLETE_ECONOMICS_INCOMPLETE"
            expected = conservative = severe = None
        else:
            validate_economics(economics)
            expected = str(economics["expected_pnl_wei"])
            conservative = str(economics["conservative_pnl_wei"])
            severe = str(economics["severe_pnl_wei"])
            floor = int(economics["retained_profit_floor_wei"])
            recommendation = (
                "CANDIDATE"
                if int(economics["expected_pnl_wei"]) > floor
                and int(economics["conservative_pnl_wei"]) > floor
                else "FULL_COST_NON_POSITIVE"
            )
        rows.append(
            {
                "source_event_identity": canonical_identity,
                "transaction_hash": event["transaction_hash"],
                "surface": event["surface"],
                "optimistic_upper_bound": event["optimistic_upper_bound"],
                "replay_required": True,
                "boundary_complete": True,
                "boundary_failure_reason": None,
                "expected_pnl_wei": expected,
                "conservative_pnl_wei": conservative,
                "severe_pnl_wei": severe,
                "recommendation": recommendation,
            }
        )
    return {
        "schema_version": REPORT_SCHEMA,
        "chain_id": CHAIN_ID,
        "allowlist_hash": canonical_hash(allowlist),
        "event_count": len(rows),
        "complete_boundaries": sum(row["boundary_complete"] for row in rows),
        "incomplete_boundaries": sum(not row["boundary_complete"] for row in rows),
        "candidate_count": sum(row["recommendation"] == "CANDIDATE" for row in rows),
        "events": rows,
    }


def exact_link_adjacent_hops(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    matches = []
    for record in records:
        tokens = record.get("token_path")
        fees = record.get("fee_path")
        pools = record.get("pool_path")
        pool_addresses = record.get("pool_addresses")
        if (
            not isinstance(tokens, list)
            or not isinstance(fees, list)
            or not isinstance(pools, list)
            or not isinstance(pool_addresses, list)
        ):
            continue
        if (
            len(tokens) != len(fees) + 1
            or len(pools) != len(fees)
            or len(pool_addresses) != len(fees)
            or record.get("source_factory") != UNISWAP_V3_FACTORY
        ):
            continue
        for index in range(len(tokens) - 1):
            if {tokens[index], tokens[index + 1]} != {LINK, WETH}:
                continue
            parts = pools[index].split(":")
            if len(parts) != 3:
                continue
            if {parts[0], parts[1]} != {LINK, WETH} or parts[2] != str(fees[index]):
                continue
            if ADDRESS_RE.fullmatch(pool_addresses[index]) is None:
                continue
            try:
                command_index = int(record.get("command_index"))
            except (TypeError, ValueError):
                continue
            if command_index < 0:
                continue
            matches.append(record)
            break
    return matches


def validate_link_backfill(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    _require(payload.get("schema_version") == LINK_BACKFILL_SCHEMA, "link_backfill_schema_invalid")
    _require(payload.get("chain_id") == CHAIN_ID, "link_backfill_chain_invalid")
    records = payload.get("records")
    _require(isinstance(records, list), "link_backfill_records_invalid")
    matches = exact_link_adjacent_hops(records)
    _require(payload.get("exact_match_count") == len(matches), "link_backfill_count_mismatch")
    return {
        "records_scanned": payload.get("records_scanned"),
        "exact_match_count": len(matches),
        "link_status": "LINK-D" if not matches else "LINK_EVIDENCE_REQUIRES_UPPER_BOUND_SCREEN",
    }


def validate_atlas_link(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    _require(payload.get("schema_version") == ATLAS_LINK_SCHEMA, "atlas_link_schema_invalid")
    _require(payload.get("chain_id") == CHAIN_ID, "atlas_link_chain_invalid")
    _require(payload.get("asset") == "LINK", "atlas_link_asset_invalid")
    _require(HASH_RE.fullmatch(payload.get("settlement_transaction_hash", "")) is not None, "atlas_link_transaction_invalid")
    _integer(payload.get("settlement_block"), "atlas_link_settlement_block", minimum=1)
    parent_price = _integer(payload.get("parent_price"), "atlas_link_parent_price", minimum=1)
    settlement_price = _integer(payload.get("settlement_price"), "atlas_link_settlement_price", minimum=1)
    _require(parent_price == settlement_price, "atlas_link_price_delta_nonzero")
    _require(payload.get("price_delta") == "0", "atlas_link_price_delta_invalid")
    _require(payload.get("newly_induced_hf_crossings") == 0, "atlas_link_crossing_count_invalid")
    _require(payload.get("public_liquidation_count") == 0, "atlas_link_liquidation_count_invalid")
    _require(payload.get("liquidation_opportunity") is False, "atlas_link_opportunity_misclassified")
    return {
        "auction_id": payload.get("auction_id"),
        "price_delta": "0",
        "newly_induced_hf_crossings": 0,
        "result": "ZERO_DELTA_NOT_A_LIQUIDATION_OPPORTUNITY",
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--allowlist", type=Path, required=True)
    parser.add_argument("--proofs", type=Path)
    parser.add_argument("--link-backfill", type=Path)
    parser.add_argument("--atlas-link", type=Path)
    args = parser.parse_args()
    try:
        allowlist = load_allowlist(args.allowlist)
        proofs = json.loads(args.proofs.read_text(encoding="utf-8")) if args.proofs else None
        report = evaluate(allowlist, proofs)
        if args.link_backfill:
            report["link"] = validate_link_backfill(args.link_backfill)
        if args.atlas_link:
            report["atlas_link"] = validate_atlas_link(args.atlas_link)
        print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    except (EvidenceError, json.JSONDecodeError, OSError) as exc:
        print(f"long_tail_event_replay: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
