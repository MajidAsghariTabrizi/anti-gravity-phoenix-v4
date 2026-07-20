#!/usr/bin/env python3
"""Bounded, fail-closed SHADOW route discovery and ranking."""

from __future__ import annotations

import argparse
import copy
import hashlib
import itertools
import json
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path


SCHEMA_VERSION = "phoenix.shadow.route-ranking.v1"
PROOF_SCHEMA_VERSION = "phoenix.route.pool-proofs.v1"
CHAIN_ID = 42161
MODE = "SHADOW"
MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_LINE_BYTES = 2 * 1024 * 1024
MAX_DECODED_ROWS = 100_000
MAX_ENRICHMENT_ROWS = 100_000
MAX_CANDIDATES = 5_000
MAX_U128 = (1 << 128) - 1
MAX_ACCUMULATOR = (1 << 256) - 1
SCORE_SCALE = 10_000
COMPONENT_WEIGHT = 500
MIN_TRANSACTIONS = 20
MIN_UNIQUE_BLOCKS = 5
MIN_COMPLETENESS_BPS = 7_000
MAX_PROVIDER_FAILURE_BPS = 1_000

ADDRESS_RE = re.compile(r"^0x[0-9a-f]{40}$")
HASH_RE = re.compile(r"^0x[0-9a-f]{64}$")
SELECTOR_RE = re.compile(r"^0x[0-9a-f]{8}$")
UNSIGNED_RE = re.compile(r"^(0|[1-9][0-9]*)$")
SIGNED_RE = re.compile(r"^(0|-?[1-9][0-9]*)$")
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")

OFFICIAL_UNISWAP_V3_FACTORY = "0x1f98431c8ad98523631ae4a59f267346ea31f984"
OFFICIAL_UNISWAP_V3_POOL_INIT_CODE_HASH = (
    "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
)
OFFICIAL_UNISWAP_V3_PERIPHERY_REPOSITORY = "https://github.com/Uniswap/v3-periphery"
OFFICIAL_UNISWAP_V3_PERIPHERY_COMMIT = "0682387198a24c7cd63566a2c58398533860a5d1"
OFFICIAL_POOL_ADDRESS_PATH = "contracts/libraries/PoolAddress.sol"
OFFICIAL_DEPLOYMENT_PATH = "deploys.md"
REVIEWED_ROUTERS = {
    "legacy_swap_router": "0xe592427a0aece92de3edee1f18e0157c05861564",
    "swap_router02": "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45",
    "universal_router": "0xa51afafe0263b40edaef0df8781ea9aa03e381a3",
}

COMPONENT_NAMES = (
    "transaction_count",
    "swap_count",
    "unique_blocks",
    "router_distribution",
    "fee_tier_diversity",
    "directional_flow",
    "volume_proxy",
    "liquidity_proxy",
    "pool_impact_frequency",
    "candidate_frequency",
    "rpc_evaluation_availability",
    "expected_net_pnl",
    "near_profitable_frequency",
    "rpc_cost",
    "provider_failure_rate",
    "state_freshness",
    "competition_proxy",
    "decoder_confidence",
    "data_completeness",
    "feed_gap_overlap",
)

DECODED_KEYS = {
    "transaction_hash",
    "source_sequence",
    "recorded_at",
    "source_block_number",
    "source_block_hash",
    "router_address",
    "router_kind",
    "selector",
    "command_family",
    "supported",
    "exact_input",
    "exact_output",
    "input_amount",
    "decoded_token_path",
    "decoded_fee_path",
    "decoded_pool_ids",
    "affected_configured_pool_ids",
    "matched_route_ids",
    "matched_route_fingerprints",
    "route_match_result",
    "rejection_detail_class",
    "candidate_count",
    "candidate_produced",
    "trusted_persisted_source",
    "production_evidence",
    "shadow_only",
    "execution_request_created",
}

PROFITABILITY_KEYS = {
    "record_type",
    "candidate_key",
    "pool_path",
    "token_path",
    "fee_path",
    "pinned_block_number",
    "detected_at_unix_ms",
    "evaluated_at_unix_ms",
    "expected_net_pnl",
    "severe_net_pnl",
    "minimum_required_net_pnl",
    "primary_profitability_status",
    "primary_provider_present",
    "verification_status",
    "agreement_state",
    "rpc_records",
    "rpc_failures",
    "rpc_latency_ns_total",
    "shadow_only",
    "execution_eligible",
    "execution_request_created",
}

POOL_CHECKPOINT_KEYS = {
    "record_type",
    "pool_address",
    "block_number",
    "liquidity",
}

DATA_AVAILABILITY_KEYS = {
    "record_type",
    "feed_gap_overlap_status",
    "feed_gap_overlap_events",
    "feed_gap_observed_events",
}

STRATEGY_KEYS = {
    "min_input_amount",
    "max_input_amount",
    "max_evaluations",
    "candidate_sizes",
    "minimum_net_profit",
    "minimum_net_profit_bps",
    "conservative_cost_multiplier_bps",
    "maximum_pool_depth_utilization_bps",
    "maximum_slippage_bps",
    "maximum_price_impact_bps",
    "maximum_execution_gas",
    "flash_premium_bps",
    "minimum_slippage_bps",
    "protocol_fees",
    "estimated_execution_gas",
    "l1_data_fee",
    "contract_overhead",
    "failed_attempt_gas_cost",
    "failure_probability_bps",
    "stale_state_loss",
    "stale_quote_probability_bps",
    "state_drift_reserve",
    "latency_reserve",
    "uncertainty_reserve",
    "replacement_transaction_cost",
    "probability_of_success_bps",
    "max_gas_price_wei",
    "max_quote_age_ms",
    "max_simulation_age_ms",
    "min_confidence_bps",
}


class DiscoveryError(Exception):
    pass


def reject_float(value: str) -> None:
    raise DiscoveryError("floating-point JSON values are forbidden")


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise DiscoveryError("duplicate JSON keys are forbidden")
        result[key] = value
    return result


def parse_json(raw: str) -> object:
    try:
        return json.loads(
            raw,
            object_pairs_hook=unique_object,
            parse_float=reject_float,
            parse_constant=reject_float,
        )
    except DiscoveryError:
        raise
    except (TypeError, ValueError, json.JSONDecodeError) as error:
        raise DiscoveryError("invalid JSON evidence") from error


def require_object(value: object, label: str) -> dict[str, object]:
    if not isinstance(value, dict):
        raise DiscoveryError(f"{label} must be an object")
    return value


def require_exact_keys(value: dict[str, object], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise DiscoveryError(f"{label} has an invalid schema")


def require_bool(value: object, label: str) -> bool:
    if not isinstance(value, bool):
        raise DiscoveryError(f"{label} must be boolean")
    return value


def require_json_int(value: object, label: str, minimum: int = 0, maximum: int = MAX_ACCUMULATOR) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise DiscoveryError(f"{label} must be a bounded integer")
    return value


def require_string(value: object, label: str, minimum: int = 1, maximum: int = 256) -> str:
    if not isinstance(value, str) or not minimum <= len(value) <= maximum:
        raise DiscoveryError(f"{label} must be a bounded string")
    if any(character in value for character in ("\x00", "\r", "\n")):
        raise DiscoveryError(f"{label} contains control characters")
    return value


def parse_decimal(value: object, label: str, *, signed: bool = False, maximum: int = MAX_ACCUMULATOR) -> int:
    text = require_string(value, label, maximum=80)
    pattern = SIGNED_RE if signed else UNSIGNED_RE
    if pattern.fullmatch(text) is None:
        raise DiscoveryError(f"{label} must be a canonical decimal string")
    parsed = int(text)
    if (not signed and parsed < 0) or abs(parsed) > maximum:
        raise DiscoveryError(f"{label} is out of range")
    return parsed


def canonical_address(value: object, label: str) -> str:
    text = require_string(value, label, minimum=42, maximum=42)
    if ADDRESS_RE.fullmatch(text) is None:
        raise DiscoveryError(f"{label} must be a canonical lowercase address")
    return text


def canonical_hash(value: object, label: str) -> str:
    text = require_string(value, label, minimum=66, maximum=66)
    if HASH_RE.fullmatch(text) is None:
        raise DiscoveryError(f"{label} must be a canonical lowercase hash")
    return text


def bounded_string_list(value: object, label: str, maximum_items: int = 16) -> list[str]:
    if not isinstance(value, list) or len(value) > maximum_items:
        raise DiscoveryError(f"{label} must be a bounded array")
    return [require_string(item, f"{label} item") for item in value]


def validate_timestamp(value: object, label: str) -> str:
    text = require_string(value, label, minimum=10, maximum=64)
    try:
        datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError as error:
        raise DiscoveryError(f"{label} must be an RFC3339 timestamp") from error
    return text


def checked_add(left: int, right: int, label: str) -> int:
    result = left + right
    if result < 0 or result > MAX_ACCUMULATOR:
        raise DiscoveryError(f"{label} overflow")
    return result


def ratio_bps(numerator: int, denominator: int) -> int:
    if numerator < 0 or denominator <= 0:
        raise DiscoveryError("invalid score ratio")
    return min(SCORE_SCALE, numerator * SCORE_SCALE // denominator)


KECCAK_ROUND_CONSTANTS = (
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A,
    0x8000000080008000, 0x000000000000808B, 0x0000000080000001,
    0x8000000080008081, 0x8000000000008009, 0x000000000000008A,
    0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089,
    0x8000000000008003, 0x8000000000008002, 0x8000000000000080,
    0x000000000000800A, 0x800000008000000A, 0x8000000080008081,
    0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
)
KECCAK_ROTATIONS = (1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44)
KECCAK_PI = (10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1)
MASK_64 = (1 << 64) - 1


def rotate_left_64(value: int, shift: int) -> int:
    return ((value << shift) | (value >> (64 - shift))) & MASK_64


def keccak_f1600(state: list[int]) -> None:
    for constant in KECCAK_ROUND_CONSTANTS:
        columns = [state[index] ^ state[index + 5] ^ state[index + 10] ^ state[index + 15] ^ state[index + 20] for index in range(5)]
        for index in range(5):
            delta = columns[(index - 1) % 5] ^ rotate_left_64(columns[(index + 1) % 5], 1)
            for row in range(0, 25, 5):
                state[row + index] ^= delta
        carried = state[1]
        for index, destination in enumerate(KECCAK_PI):
            current = state[destination]
            state[destination] = rotate_left_64(carried, KECCAK_ROTATIONS[index])
            carried = current
        for row in range(0, 25, 5):
            values = state[row:row + 5]
            for index in range(5):
                state[row + index] = values[index] ^ ((~values[(index + 1) % 5]) & values[(index + 2) % 5])
        state[0] ^= constant


def keccak256(payload: bytes) -> bytes:
    state = [0] * 25
    rate = 136
    offset = 0
    while len(payload) - offset >= rate:
        block = payload[offset:offset + rate]
        for lane in range(rate // 8):
            state[lane] ^= int.from_bytes(block[lane * 8:(lane + 1) * 8], "little")
        keccak_f1600(state)
        offset += rate
    final = bytearray(rate)
    remaining = payload[offset:]
    final[:len(remaining)] = remaining
    final[len(remaining)] ^= 0x01
    final[-1] ^= 0x80
    for lane in range(rate // 8):
        state[lane] ^= int.from_bytes(final[lane * 8:(lane + 1) * 8], "little")
    keccak_f1600(state)
    return b"".join(value.to_bytes(8, "little") for value in state)[:32]


def compute_pool_address(factory: str, init_code_hash: str, token0: str, token1: str, fee: int) -> str:
    encoded_key = (
        bytes(12) + bytes.fromhex(token0[2:])
        + bytes(12) + bytes.fromhex(token1[2:])
        + fee.to_bytes(32, "big")
    )
    salt = keccak256(encoded_key)
    digest = keccak256(
        b"\xff"
        + bytes.fromhex(factory[2:])
        + salt
        + bytes.fromhex(init_code_hash[2:])
    )
    return "0x" + digest[-20:].hex()


def load_json_file(path: Path, label: str) -> dict[str, object]:
    try:
        size = path.stat().st_size
    except OSError as error:
        raise DiscoveryError(f"{label} is unavailable") from error
    if size <= 0 or size > MAX_FILE_BYTES:
        raise DiscoveryError(f"{label} exceeds its bound")
    try:
        raw = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise DiscoveryError(f"{label} is unreadable") from error
    return require_object(parse_json(raw), label)


def validate_strategy(value: object) -> dict[str, object]:
    strategy = require_object(value, "strategy template")
    require_exact_keys(strategy, STRATEGY_KEYS, "strategy template")
    decimal_fields = STRATEGY_KEYS - {
        "max_evaluations",
        "candidate_sizes",
        "minimum_net_profit_bps",
        "conservative_cost_multiplier_bps",
        "maximum_pool_depth_utilization_bps",
        "maximum_slippage_bps",
        "maximum_price_impact_bps",
        "maximum_execution_gas",
        "flash_premium_bps",
        "minimum_slippage_bps",
        "estimated_execution_gas",
        "failure_probability_bps",
        "stale_quote_probability_bps",
        "probability_of_success_bps",
        "max_quote_age_ms",
        "max_simulation_age_ms",
        "min_confidence_bps",
    }
    for field in decimal_fields:
        parse_decimal(strategy[field], field, maximum=MAX_U128)
    max_evaluations = require_json_int(
        strategy["max_evaluations"], "max_evaluations", minimum=1, maximum=32
    )
    for field in (
        "estimated_execution_gas",
        "maximum_execution_gas",
        "max_quote_age_ms",
        "max_simulation_age_ms",
    ):
        require_json_int(strategy[field], field, minimum=1, maximum=10_000_000)
    for field in (
        "minimum_net_profit_bps",
        "maximum_pool_depth_utilization_bps",
        "maximum_slippage_bps",
        "maximum_price_impact_bps",
        "flash_premium_bps",
        "minimum_slippage_bps",
        "failure_probability_bps",
        "stale_quote_probability_bps",
        "probability_of_success_bps",
        "min_confidence_bps",
    ):
        require_json_int(strategy[field], field, maximum=SCORE_SCALE)
    conservative_multiplier = require_json_int(
        strategy["conservative_cost_multiplier_bps"],
        "conservative_cost_multiplier_bps",
        minimum=10_000,
        maximum=100_000,
    )
    if conservative_multiplier < SCORE_SCALE:
        raise DiscoveryError("strategy cost multiplier is not conservative")
    min_input = parse_decimal(strategy["min_input_amount"], "min_input_amount", maximum=MAX_U128)
    max_input = parse_decimal(strategy["max_input_amount"], "max_input_amount", maximum=MAX_U128)
    if min_input <= 0 or min_input > max_input:
        raise DiscoveryError("strategy input bounds are inverted")
    candidate_values = strategy["candidate_sizes"]
    if (
        not isinstance(candidate_values, list)
        or not candidate_values
        or len(candidate_values) > max_evaluations
    ):
        raise DiscoveryError("strategy candidate sizes are invalid")
    candidate_sizes = [
        parse_decimal(value, "candidate size", maximum=MAX_U128)
        for value in candidate_values
    ]
    if (
        any(value < min_input or value > max_input for value in candidate_sizes)
        or any(left >= right for left, right in zip(candidate_sizes, candidate_sizes[1:]))
        or require_json_int(strategy["minimum_net_profit_bps"], "minimum_net_profit_bps") == 0
        or require_json_int(
            strategy["maximum_pool_depth_utilization_bps"],
            "maximum_pool_depth_utilization_bps",
        )
        == 0
        or require_json_int(strategy["maximum_slippage_bps"], "maximum_slippage_bps") == 0
        or require_json_int(
            strategy["maximum_price_impact_bps"], "maximum_price_impact_bps"
        )
        == 0
        or require_json_int(strategy["minimum_slippage_bps"], "minimum_slippage_bps") == 0
        or strategy["minimum_slippage_bps"] > strategy["maximum_slippage_bps"]
        or strategy["estimated_execution_gas"] > strategy["maximum_execution_gas"]
        or parse_decimal(strategy["minimum_net_profit"], "minimum_net_profit", maximum=MAX_U128)
        == 0
        or parse_decimal(strategy["max_gas_price_wei"], "max_gas_price_wei", maximum=MAX_U128)
        == 0
        or require_json_int(strategy["probability_of_success_bps"], "probability_of_success_bps")
        == 0
    ):
        raise DiscoveryError("strategy bounded optimizer contract is invalid")
    return copy.deepcopy(strategy)


def load_pool_proofs(path: Path) -> tuple[dict[tuple[str, str, int], dict[str, object]], dict[tuple[str, str, str], dict[str, object]], dict[str, object]]:
    document = load_json_file(path, "pool proof registry")
    require_exact_keys(
        document,
        {"schema_version", "chain_id", "protocol", "factory", "pool_init_code_hash", "source", "pools", "strategy_templates"},
        "pool proof registry",
    )
    if document["schema_version"] != PROOF_SCHEMA_VERSION or document["protocol"] != "UniswapV3":
        raise DiscoveryError("pool proof registry version or protocol is invalid")
    if require_json_int(document["chain_id"], "pool proof chain id") != CHAIN_ID:
        raise DiscoveryError("pool proof registry targets the wrong chain")
    factory = canonical_address(document["factory"], "Uniswap V3 factory")
    init_code_hash = canonical_hash(document["pool_init_code_hash"], "pool init code hash")
    source = require_object(document["source"], "pool proof source")
    require_exact_keys(source, {"kind", "repository", "commit", "pool_address_path", "deployment_path"}, "pool proof source")
    if source["kind"] != "official_uniswap_v3_create2":
        raise DiscoveryError("pool proof source kind is invalid")
    commit = require_string(source["commit"], "pool proof commit", minimum=40, maximum=40)
    if re.fullmatch(r"[0-9a-f]{40}", commit) is None:
        raise DiscoveryError("pool proof source commit is invalid")
    repository = require_string(source["repository"], "pool proof repository", maximum=256)
    pool_address_path = require_string(
        source["pool_address_path"], "pool proof source path", maximum=256
    )
    deployment_path = require_string(
        source["deployment_path"], "pool deployment source path", maximum=256
    )
    if (
        factory != OFFICIAL_UNISWAP_V3_FACTORY
        or init_code_hash != OFFICIAL_UNISWAP_V3_POOL_INIT_CODE_HASH
        or repository != OFFICIAL_UNISWAP_V3_PERIPHERY_REPOSITORY
        or commit != OFFICIAL_UNISWAP_V3_PERIPHERY_COMMIT
        or pool_address_path != OFFICIAL_POOL_ADDRESS_PATH
        or deployment_path != OFFICIAL_DEPLOYMENT_PATH
    ):
        raise DiscoveryError("pool proof registry does not match the pinned official source")

    pools_value = document["pools"]
    if not isinstance(pools_value, list) or not 1 <= len(pools_value) <= 1_000:
        raise DiscoveryError("pool proofs must be a bounded non-empty array")
    pools: dict[tuple[str, str, int], dict[str, object]] = {}
    addresses: set[str] = set()
    token_decimals: dict[str, int] = {}
    for item in pools_value:
        proof = require_object(item, "pool proof")
        require_exact_keys(
            proof,
            {
                "token0",
                "token1",
                "token0_decimals",
                "token1_decimals",
                "fee",
                "tick_spacing",
                "pool_address",
            },
            "pool proof",
        )
        token0 = canonical_address(proof["token0"], "pool token0")
        token1 = canonical_address(proof["token1"], "pool token1")
        token0_decimals = require_json_int(
            proof["token0_decimals"], "pool token0 decimals", minimum=1, maximum=36
        )
        token1_decimals = require_json_int(
            proof["token1_decimals"], "pool token1 decimals", minimum=1, maximum=36
        )
        fee = require_json_int(proof["fee"], "pool fee", minimum=1, maximum=999_999)
        tick_spacing = require_json_int(
            proof["tick_spacing"], "pool tick spacing", minimum=1, maximum=887_272
        )
        address = canonical_address(proof["pool_address"], "pool address")
        if token0 >= token1:
            raise DiscoveryError("pool proof token order is not canonical")
        key = (token0, token1, fee)
        if key in pools or address in addresses:
            raise DiscoveryError("pool proof registry contains duplicates")
        if compute_pool_address(factory, init_code_hash, token0, token1, fee) != address:
            raise DiscoveryError("pool proof does not match official CREATE2 identity")
        for token, decimals in ((token0, token0_decimals), (token1, token1_decimals)):
            if token in token_decimals and token_decimals[token] != decimals:
                raise DiscoveryError("pool proof token decimals are inconsistent")
            token_decimals[token] = decimals
        pools[key] = {
            "token0": token0,
            "token1": token1,
            "token0_decimals": token0_decimals,
            "token1_decimals": token1_decimals,
            "fee": fee,
            "tick_spacing": tick_spacing,
            "pool_address": address,
        }
        addresses.add(address)

    templates_value = document["strategy_templates"]
    if not isinstance(templates_value, list) or len(templates_value) > 100:
        raise DiscoveryError("strategy templates must be a bounded array")
    templates: dict[tuple[str, str, str], dict[str, object]] = {}
    template_ids: set[str] = set()
    for item in templates_value:
        template = require_object(item, "strategy template record")
        require_exact_keys(
            template,
            {
                "template_id",
                "token0",
                "token1",
                "settlement_asset",
                "settlement_asset_decimals",
                "strategy",
            },
            "strategy template record",
        )
        template_id = require_string(template["template_id"], "strategy template id", maximum=128)
        token0 = canonical_address(template["token0"], "strategy token0")
        token1 = canonical_address(template["token1"], "strategy token1")
        settlement_asset = canonical_address(
            template["settlement_asset"], "strategy settlement asset"
        )
        settlement_asset_decimals = require_json_int(
            template["settlement_asset_decimals"],
            "strategy settlement asset decimals",
            minimum=1,
            maximum=36,
        )
        template_key = (token0, token1, settlement_asset)
        if (
            token0 >= token1
            or settlement_asset not in {token0, token1}
            or token_decimals.get(settlement_asset) != settlement_asset_decimals
            or template_key in templates
            or template_id in template_ids
        ):
            raise DiscoveryError("strategy templates contain duplicate or non-canonical identities")
        templates[template_key] = {
            "template_id": template_id,
            "settlement_asset_decimals": settlement_asset_decimals,
            "strategy": validate_strategy(template["strategy"]),
        }
        template_ids.add(template_id)
    metadata = {
        "schema_version": PROOF_SCHEMA_VERSION,
        "factory": factory,
        "pool_init_code_hash": init_code_hash,
        "source_commit": commit,
    }
    return pools, templates, metadata


def validate_pool_id(value: object, token_a: str, token_b: str, fee: int, label: str) -> str:
    text = require_string(value, label, maximum=96)
    expected_tokens = sorted((token_a, token_b))
    expected = f"{expected_tokens[0]}:{expected_tokens[1]}:{fee}"
    if text != expected:
        raise DiscoveryError(f"{label} is not canonical")
    return text


def validate_decoded(value: object) -> dict[str, object]:
    row = require_object(value, "decoded observation")
    require_exact_keys(row, DECODED_KEYS, "decoded observation")
    canonical_hash(row["transaction_hash"], "transaction hash")
    require_json_int(row["source_sequence"], "source sequence", maximum=(1 << 64) - 1)
    validate_timestamp(row["recorded_at"], "recorded_at")
    if row["source_block_number"] is not None:
        require_json_int(row["source_block_number"], "source block number", minimum=1, maximum=(1 << 64) - 1)
    if row["source_block_hash"] is not None:
        canonical_hash(row["source_block_hash"], "source block hash")
    if (row["source_block_number"] is None) != (row["source_block_hash"] is None):
        raise DiscoveryError("source block identity is incomplete")
    router_address = None
    if row["router_address"] is not None:
        router_address = canonical_address(row["router_address"], "router address")
    if row["router_kind"] is not None and row["router_kind"] not in {
        "legacy_swap_router", "swap_router02", "universal_router"
    }:
        raise DiscoveryError("router kind is unsupported")
    if row["router_kind"] is not None and REVIEWED_ROUTERS[row["router_kind"]] != router_address:
        raise DiscoveryError("router kind does not match its reviewed official address")
    if row["selector"] is not None:
        selector = require_string(row["selector"], "selector", minimum=10, maximum=10)
        if SELECTOR_RE.fullmatch(selector) is None:
            raise DiscoveryError("selector is invalid")
    bounded_string_list(row["command_family"], "command family")
    supported = require_bool(row["supported"], "supported")
    if row["exact_input"] is not None:
        require_bool(row["exact_input"], "exact_input")
    exact_output = require_bool(row["exact_output"], "exact_output")
    trusted = require_bool(row["trusted_persisted_source"], "trusted persisted source")
    production = require_bool(row["production_evidence"], "production evidence")
    require_bool(row["candidate_produced"], "candidate produced")
    require_bool(row["shadow_only"], "shadow_only")
    require_bool(row["execution_request_created"], "execution_request_created")
    if row["shadow_only"] is not True or row["execution_request_created"] is not False:
        raise DiscoveryError("decoded observation violates SHADOW safety")
    if production and (not trusted or not row["candidate_produced"]):
        raise DiscoveryError("production evidence lacks trusted candidate provenance")
    require_json_int(row["candidate_count"], "candidate count", maximum=100)
    bounded_string_list(row["affected_configured_pool_ids"], "affected configured pools", maximum_items=32)
    bounded_string_list(row["matched_route_ids"], "matched route ids", maximum_items=32)
    bounded_string_list(row["matched_route_fingerprints"], "matched route fingerprints", maximum_items=32)
    require_string(row["route_match_result"], "route match result", maximum=128)
    if row["rejection_detail_class"] is not None:
        require_string(row["rejection_detail_class"], "rejection detail", maximum=128)

    tokens = bounded_string_list(row["decoded_token_path"], "decoded token path", maximum_items=5)
    fees_value = row["decoded_fee_path"]
    pools_value = row["decoded_pool_ids"]
    if not isinstance(fees_value, list) or not isinstance(pools_value, list) or len(fees_value) > 4 or len(pools_value) > 4:
        raise DiscoveryError("decoded paths exceed their bounds")
    fees = [require_json_int(item, "decoded fee", minimum=1, maximum=999_999) for item in fees_value]
    pools = bounded_string_list(pools_value, "decoded pool path", maximum_items=4)
    if supported:
        if (
            row["router_kind"] is None
            or router_address is None
            or row["exact_input"] is not True
            or exact_output
            or row["input_amount"] is None
        ):
            raise DiscoveryError("supported observation is not exact-input evidence")
        parse_decimal(row["input_amount"], "input amount", maximum=MAX_U128)
        if len(tokens) != len(fees) + 1 or len(fees) != len(pools) or not fees:
            raise DiscoveryError("supported decoded path shape is invalid")
        canonical_tokens = [canonical_address(item, "decoded token") for item in tokens]
        for index, fee in enumerate(fees):
            validate_pool_id(pools[index], canonical_tokens[index], canonical_tokens[index + 1], fee, "decoded pool id")
    else:
        if row["input_amount"] is not None:
            raise DiscoveryError("unsupported observation contains an input amount")
        if tokens or fees or pools:
            raise DiscoveryError("unsupported observation contains decoded route data")
    return row


def read_decoded(path: Path, limit: int) -> tuple[list[dict[str, object]], dict[str, object] | None]:
    if limit < 1 or limit > MAX_DECODED_ROWS:
        raise DiscoveryError("decoded row limit is invalid")
    observations: list[dict[str, object]] = []
    statistics: dict[str, object] | None = None
    total_bytes = 0
    try:
        file = path.open("r", encoding="utf-8")
    except OSError as error:
        raise DiscoveryError("decoded evidence is unavailable") from error
    with file:
        for raw_line in file:
            encoded_bytes = len(raw_line.encode("utf-8"))
            total_bytes += encoded_bytes
            if encoded_bytes > MAX_LINE_BYTES or total_bytes > MAX_FILE_BYTES:
                raise DiscoveryError("decoded evidence exceeds its byte bound")
            line = raw_line.strip()
            if not line:
                continue
            if line.startswith("DISCOVERY_STATISTICS "):
                if statistics is not None:
                    raise DiscoveryError("duplicate discovery statistics")
                statistics = require_object(parse_json(line.removeprefix("DISCOVERY_STATISTICS ")), "discovery statistics")
                continue
            if line in {"POSITIVE_ROUTE_EVIDENCE_FOUND", "POSITIVE_ROUTE_EVIDENCE_NOT_FOUND"}:
                continue
            if len(observations) >= limit:
                raise DiscoveryError("decoded evidence exceeds its row bound")
            observations.append(validate_decoded(parse_json(line)))
    return observations, statistics


def deduplicate_observations(
    observations: list[dict[str, object]],
) -> tuple[list[dict[str, object]], int]:
    by_transaction: dict[str, tuple[str, dict[str, object]]] = {}
    for row in observations:
        transaction_hash = str(row["transaction_hash"])
        semantic = {
            key: value
            for key, value in row.items()
            if key not in {"source_sequence", "recorded_at"}
        }
        fingerprint = json.dumps(semantic, sort_keys=True, separators=(",", ":"))
        existing = by_transaction.get(transaction_hash)
        if existing is not None and existing[0] != fingerprint:
            raise DiscoveryError("duplicate transaction evidence is conflicting")
        if existing is None or (
            int(row["source_sequence"]), str(row["recorded_at"])
        ) < (
            int(existing[1]["source_sequence"]),
            str(existing[1]["recorded_at"]),
        ):
            by_transaction[transaction_hash] = (fingerprint, row)
    deduplicated = [value[1] for _, value in sorted(by_transaction.items())]
    return deduplicated, len(observations) - len(deduplicated)


def validate_profitability(row: dict[str, object]) -> dict[str, object]:
    require_exact_keys(row, PROFITABILITY_KEYS, "profitability enrichment")
    source_candidate_key = require_string(
        row["candidate_key"], "profitability candidate key", maximum=128
    )
    if UUID_RE.fullmatch(source_candidate_key) is None:
        raise DiscoveryError("profitability candidate key is not a canonical UUID")
    tokens = bounded_string_list(row["token_path"], "profitability token path", maximum_items=3)
    pools = bounded_string_list(row["pool_path"], "profitability pool path", maximum_items=2)
    fees_value = row["fee_path"]
    if not isinstance(fees_value, list) or len(fees_value) != 2 or len(tokens) != 3 or len(pools) != 2:
        raise DiscoveryError("profitability route shape is invalid")
    canonical_tokens = [canonical_address(item, "profitability token") for item in tokens]
    if canonical_tokens[0] != canonical_tokens[2] or canonical_tokens[0] == canonical_tokens[1]:
        raise DiscoveryError("profitability route is not a two-pool cycle")
    fees = [require_json_int(item, "profitability fee", minimum=1, maximum=999_999) for item in fees_value]
    if fees[0] == fees[1]:
        raise DiscoveryError("profitability route does not have distinct pools")
    for index, fee in enumerate(fees):
        validate_pool_id(pools[index], canonical_tokens[index], canonical_tokens[index + 1], fee, "profitability pool id")
    block = parse_decimal(row["pinned_block_number"], "pinned block number", maximum=(1 << 64) - 1)
    if block == 0:
        raise DiscoveryError("profitability block must be positive")
    detected = parse_decimal(row["detected_at_unix_ms"], "detected timestamp", maximum=(1 << 63) - 1)
    evaluated = parse_decimal(row["evaluated_at_unix_ms"], "evaluated timestamp", maximum=(1 << 63) - 1)
    if evaluated < detected:
        raise DiscoveryError("profitability timestamps are inverted")
    expected = parse_decimal(row["expected_net_pnl"], "expected net pnl", signed=True)
    severe = parse_decimal(row["severe_net_pnl"], "severe net pnl", signed=True)
    minimum = parse_decimal(row["minimum_required_net_pnl"], "minimum net pnl")
    if severe > expected:
        raise DiscoveryError("profitability scenario ordering is invalid")
    status = require_string(row["primary_profitability_status"], "profitability status", maximum=32)
    if status not in {"meets_minimum", "below_minimum"}:
        raise DiscoveryError("profitability status is incomplete")
    if (status == "meets_minimum") != (expected >= minimum):
        raise DiscoveryError("profitability threshold arithmetic is inconsistent")
    primary_provider = require_bool(row["primary_provider_present"], "primary provider presence")
    verification = require_string(row["verification_status"], "verification status", maximum=32)
    if verification not in {"primary_only", "agreed", "disagreed", "secondary_unavailable", "historical_evidence"}:
        raise DiscoveryError("verification status is invalid")
    agreement = require_string(row["agreement_state"], "agreement state", maximum=32)
    if agreement not in {"not_checked", "agreed", "disagreed", "unavailable"}:
        raise DiscoveryError("agreement state is invalid")
    expected_agreement = {
        "primary_only": "not_checked",
        "agreed": "agreed",
        "disagreed": "disagreed",
        "secondary_unavailable": "unavailable",
        "historical_evidence": "not_checked",
    }[verification]
    if agreement != expected_agreement:
        raise DiscoveryError("verification and agreement states are inconsistent")
    rpc_records = parse_decimal(row["rpc_records"], "RPC record count", maximum=MAX_DECODED_ROWS)
    rpc_failures = parse_decimal(row["rpc_failures"], "RPC failure count", maximum=MAX_DECODED_ROWS)
    rpc_latency = parse_decimal(row["rpc_latency_ns_total"], "RPC latency total")
    if (
        rpc_failures > rpc_records
        or (primary_provider and rpc_records == 0)
        or (rpc_records == 0 and rpc_latency != 0)
    ):
        raise DiscoveryError("RPC enrichment counts are inconsistent")
    if require_bool(row["shadow_only"], "profitability shadow_only") is not True:
        raise DiscoveryError("profitability enrichment is not SHADOW-only")
    if require_bool(row["execution_eligible"], "profitability execution eligibility") is not False:
        raise DiscoveryError("profitability enrichment is executable")
    if require_bool(row["execution_request_created"], "profitability execution request") is not False:
        raise DiscoveryError("profitability enrichment created an execution request")
    token0, token1 = sorted((canonical_tokens[0], canonical_tokens[1]))
    candidate_key = (token0, token1, canonical_tokens[0], fees[0], fees[1])
    return {
        "source_candidate_key": source_candidate_key,
        "candidate_key": candidate_key,
        "settlement_asset": canonical_tokens[0],
        "block": block,
        "delay_ms": evaluated - detected,
        "expected": expected,
        "severe": severe,
        "minimum": minimum,
        "status": status,
        "primary_provider": primary_provider,
        "verification": verification,
        "agreement": agreement,
        "rpc_records": rpc_records,
        "rpc_failures": rpc_failures,
        "rpc_latency_ns": rpc_latency,
    }


def read_enrichment(path: Path) -> tuple[list[dict[str, object]], dict[str, dict[str, int]], dict[str, object]]:
    facts: list[dict[str, object]] = []
    checkpoints: dict[str, dict[str, int]] = {}
    availability: dict[str, object] | None = None
    total_bytes = 0
    rows = 0
    profitability_keys: set[str] = set()
    try:
        file = path.open("r", encoding="utf-8")
    except OSError as error:
        raise DiscoveryError("route enrichment evidence is unavailable") from error
    with file:
        for raw_line in file:
            encoded_bytes = len(raw_line.encode("utf-8"))
            total_bytes += encoded_bytes
            if encoded_bytes > MAX_LINE_BYTES or total_bytes > MAX_FILE_BYTES:
                raise DiscoveryError("route enrichment exceeds its byte bound")
            line = raw_line.strip()
            if not line:
                continue
            rows += 1
            if rows > MAX_ENRICHMENT_ROWS:
                raise DiscoveryError("route enrichment exceeds its row bound")
            row = require_object(parse_json(line), "route enrichment row")
            record_type = require_string(row.get("record_type"), "route enrichment record type", maximum=64)
            if record_type == "overflow":
                raise DiscoveryError("PostgreSQL enrichment exceeded its configured bound")
            if record_type == "profitability":
                fact = validate_profitability(row)
                source_key = str(fact["source_candidate_key"])
                if source_key in profitability_keys:
                    raise DiscoveryError("profitability enrichment is duplicated")
                profitability_keys.add(source_key)
                facts.append(fact)
            elif record_type == "pool_checkpoint":
                require_exact_keys(row, POOL_CHECKPOINT_KEYS, "pool checkpoint enrichment")
                address = canonical_address(row["pool_address"], "checkpoint pool")
                block = parse_decimal(row["block_number"], "checkpoint block", maximum=(1 << 64) - 1)
                liquidity = parse_decimal(row["liquidity"], "checkpoint liquidity", maximum=MAX_U128)
                if block == 0 or address in checkpoints:
                    raise DiscoveryError("pool checkpoint enrichment is duplicate or invalid")
                checkpoints[address] = {"block": block, "liquidity": liquidity}
            elif record_type == "data_availability":
                require_exact_keys(row, DATA_AVAILABILITY_KEYS, "data availability enrichment")
                if availability is not None:
                    raise DiscoveryError("duplicate data availability enrichment")
                status = require_string(row["feed_gap_overlap_status"], "feed gap status", maximum=64)
                if status not in {"available", "unavailable_not_persisted"}:
                    raise DiscoveryError("feed gap status is invalid")
                overlap = row["feed_gap_overlap_events"]
                observed = row["feed_gap_observed_events"]
                if status == "available":
                    overlap_count = parse_decimal(overlap, "feed gap overlap events", maximum=MAX_DECODED_ROWS)
                    observed_count = parse_decimal(observed, "feed gap observed events", maximum=MAX_DECODED_ROWS)
                    if observed_count == 0 or overlap_count > observed_count:
                        raise DiscoveryError("feed gap counts are invalid")
                    availability = {"status": status, "overlap": overlap_count, "observed": observed_count}
                else:
                    if overlap is not None or observed is not None:
                        raise DiscoveryError("unavailable feed gap evidence contains counts")
                    availability = {"status": status, "overlap": None, "observed": None}
            else:
                raise DiscoveryError("route enrichment record type is unsupported")
    if availability is None:
        raise DiscoveryError("feed gap data availability was not declared")
    return facts, checkpoints, availability


def percentile(values: list[int], numerator: int, denominator: int) -> int:
    if not values:
        raise DiscoveryError("percentile input is empty")
    ordered = sorted(values)
    rank = max(1, (len(ordered) * numerator + denominator - 1) // denominator)
    return ordered[rank - 1]


def lower_median(values: list[int]) -> int:
    if not values:
        raise DiscoveryError("median input is empty")
    ordered = sorted(values)
    return ordered[(len(ordered) - 1) // 2]


def component(status: str, raw: object, score: int, basis: str) -> dict[str, object]:
    if status not in {"available", "unavailable", "not_comparable"} or not 0 <= score <= SCORE_SCALE:
        raise DiscoveryError("invalid component score")
    return {
        "status": status,
        "raw": raw,
        "score_bps": score,
        "weight_points": COMPONENT_WEIGHT,
        "weighted_points": score * COMPONENT_WEIGHT // SCORE_SCALE,
        "basis": basis,
    }


def route_identity(candidate_key: tuple[str, str, str, int, int]) -> tuple[str, str]:
    token0, token1, settlement_asset, fee0, fee1 = candidate_key
    material = (
        f"{CHAIN_ID}:{token0}:{token1}:{settlement_asset}:{fee0}:{fee1}:UniswapV3"
    )
    suffix = hashlib.sha256(material.encode("ascii")).hexdigest()[:12]
    route_id = (
        f"arb1-uni-v3-{token0[2:10]}-{token1[2:10]}-"
        f"settle-{settlement_asset[2:10]}-{fee0}-{fee1}-{suffix}"
    )
    return route_id, f"{route_id}-v1"


def suggested_route(
    candidate_key: tuple[str, str, str, int, int],
    proofs: dict[tuple[str, str, int], dict[str, object]],
    templates: dict[tuple[str, str, str], dict[str, object]],
) -> dict[str, object] | None:
    token0, token1, settlement_asset, fee0, fee1 = candidate_key
    proof0 = proofs.get((token0, token1, fee0))
    proof1 = proofs.get((token0, token1, fee1))
    template = templates.get((token0, token1, settlement_asset))
    if proof0 is None or proof1 is None or template is None:
        return None
    other_asset = token1 if settlement_asset == token0 else token0
    settlement_asset_decimals = template["settlement_asset_decimals"]
    other_asset_decimals = (
        proof0["token1_decimals"] if settlement_asset == token0 else proof0["token0_decimals"]
    )
    first_direction = "zero_for_one" if settlement_asset == token0 else "one_for_zero"
    second_direction = "one_for_zero" if settlement_asset == token0 else "zero_for_one"
    route_id, fingerprint = route_identity(candidate_key)
    pool0 = f"{token0}:{token1}:{fee0}"
    pool1 = f"{token0}:{token1}:{fee1}"
    return {
        "route_id": route_id,
        "route_fingerprint": fingerprint,
        "trigger_pool_id": pool0,
        "settlement_asset": settlement_asset,
        "settlement_asset_decimals": settlement_asset_decimals,
        "legs": [
            {
                "pool_id": pool0,
                "state_target": proof0["pool_address"],
                "protocol": "UniswapV3",
                "fee": fee0,
                "token_in": settlement_asset,
                "token_out": other_asset,
                "token_in_decimals": settlement_asset_decimals,
                "token_out_decimals": other_asset_decimals,
                "tick_spacing": proof0["tick_spacing"],
                "direction": first_direction,
            },
            {
                "pool_id": pool1,
                "state_target": proof1["pool_address"],
                "protocol": "UniswapV3",
                "fee": fee1,
                "token_in": other_asset,
                "token_out": settlement_asset,
                "token_in_decimals": other_asset_decimals,
                "token_out_decimals": settlement_asset_decimals,
                "tick_spacing": proof1["tick_spacing"],
                "direction": second_direction,
            },
        ],
        "strategy": copy.deepcopy(template["strategy"]),
    }


def build_report(
    observations: list[dict[str, object]],
    facts: list[dict[str, object]],
    checkpoints: dict[str, dict[str, int]],
    availability: dict[str, object],
    proofs: dict[tuple[str, str, int], dict[str, object]],
    templates: dict[tuple[str, str, str], dict[str, object]],
    proof_metadata: dict[str, object],
    top_limit: int,
    duplicate_rows_removed: int,
) -> dict[str, object]:
    if not 1 <= top_limit <= 10:
        raise DiscoveryError("top route limit is invalid")
    pool_stats: dict[tuple[str, str, int], dict[str, object]] = {}
    pair_fees: dict[tuple[str, str], set[int]] = defaultdict(set)
    supported_transactions: set[str] = set()
    trusted_supported_rows = 0
    for row in observations:
        if not row["supported"]:
            continue
        tx_hash = str(row["transaction_hash"])
        supported_transactions.add(tx_hash)
        if row["trusted_persisted_source"]:
            trusted_supported_rows += 1
        tokens = list(row["decoded_token_path"])
        fees = list(row["decoded_fee_path"])
        amount = parse_decimal(row["input_amount"], "decoded input amount", maximum=MAX_U128)
        for index, fee_value in enumerate(fees):
            fee = int(fee_value)
            token_in = str(tokens[index])
            token_out = str(tokens[index + 1])
            token0, token1 = sorted((token_in, token_out))
            key = (token0, token1, fee)
            pair_fees[(token0, token1)].add(fee)
            stats = pool_stats.setdefault(
                key,
                {
                    "transactions": set(),
                    "trusted_transactions": set(),
                    "swaps": 0,
                    "blocks": set(),
                    "routers": Counter(),
                    "directions": Counter(),
                    "volume": Counter(),
                },
            )
            stats["transactions"].add(tx_hash)
            if row["trusted_persisted_source"]:
                stats["trusted_transactions"].add(tx_hash)
            stats["swaps"] = checked_add(int(stats["swaps"]), 1, "swap count")
            if row["source_block_number"] is not None:
                stats["blocks"].add(int(row["source_block_number"]))
            router = row["router_kind"] or "unavailable"
            stats["routers"][str(router)] += 1
            direction = "token0_to_token1" if token_in == token0 else "token1_to_token0"
            stats["directions"][direction] += 1
            # The decoder exposes the transaction input, not each downstream hop input.
            # Crediting it to every hop would overstate multi-hop volume evidence.
            if len(fees) == 1:
                stats["volume"][token_in] = checked_add(
                    int(stats["volume"][token_in]), amount, "volume proxy"
                )

    candidate_keys: list[tuple[str, str, str, int, int]] = []
    for pair, fees in sorted(pair_fees.items()):
        for fee0, fee1 in itertools.combinations(sorted(fees), 2):
            for settlement_asset in pair:
                for ordered_fees in ((fee0, fee1), (fee1, fee0)):
                    candidate_keys.append(
                        (pair[0], pair[1], settlement_asset, *ordered_fees)
                    )
                    if len(candidate_keys) > MAX_CANDIDATES:
                        raise DiscoveryError("route candidate set exceeds its bound")

    facts_by_candidate: dict[tuple[str, str, str, int, int], list[dict[str, object]]] = defaultdict(list)
    block_candidates: dict[int, set[tuple[str, str, str, int, int]]] = defaultdict(set)
    for fact in facts:
        key = fact["candidate_key"]
        if not isinstance(key, tuple) or len(key) != 5:
            raise DiscoveryError("profitability candidate key is invalid")
        facts_by_candidate[key].append(fact)
        block_candidates[int(fact["block"])].add(key)

    candidates: list[dict[str, object]] = []
    for key in candidate_keys:
        token0, token1, settlement_asset, fee0, fee1 = key
        first = pool_stats[(token0, token1, fee0)]
        second = pool_stats[(token0, token1, fee1)]
        transactions = set(first["transactions"]) | set(second["transactions"])
        trusted_transactions = set(first["trusted_transactions"]) | set(second["trusted_transactions"])
        blocks = set(first["blocks"]) | set(second["blocks"])
        routers = Counter(first["routers"]) + Counter(second["routers"])
        directions = Counter(first["directions"]) + Counter(second["directions"])
        volume = Counter(first["volume"]) + Counter(second["volume"])
        candidate_facts = facts_by_candidate.get(key, [])
        rpc_records = sum(int(item["rpc_records"]) for item in candidate_facts)
        rpc_failures = sum(int(item["rpc_failures"]) for item in candidate_facts)
        primary_facts = sum(1 for item in candidate_facts if item["primary_provider"])
        expected_values = [int(item["expected"]) for item in candidate_facts]
        severe_values = [int(item["severe"]) for item in candidate_facts]
        below = [item for item in candidate_facts if item["status"] == "below_minimum"]
        near = sum(
            1
            for item in below
            if int(item["expected"]) >= 0
            and int(item["expected"]) * SCORE_SCALE >= int(item["minimum"]) * 9_000
        )
        competitive = sum(1 for item in candidate_facts if len(block_candidates[int(item["block"])]) > 1)
        proof0 = proofs.get((token0, token1, fee0))
        proof1 = proofs.get((token0, token1, fee1))
        checkpoint_records = []
        for proof in (proof0, proof1):
            if proof is not None and proof["pool_address"] in checkpoints:
                checkpoint_records.append(checkpoints[str(proof["pool_address"])])
        latest_fact_block = max((int(item["block"]) for item in candidate_facts), default=None)
        candidates.append(
            {
                "key": key,
                "transactions": len(transactions),
                "trusted_transactions": len(trusted_transactions),
                "swaps": int(first["swaps"]) + int(second["swaps"]),
                "blocks": len(blocks),
                "routers": dict(sorted(routers.items())),
                "directions": dict(sorted(directions.items())),
                "volume": {asset: int(value) for asset, value in sorted(volume.items())},
                "fee_tier_diversity": len(pair_fees[(token0, token1)]),
                "facts": candidate_facts,
                "rpc_records": rpc_records,
                "rpc_failures": rpc_failures,
                "primary_facts": primary_facts,
                "expected_lower_median": lower_median(expected_values) if expected_values else None,
                "severe_min": min(severe_values) if severe_values else None,
                "near": near,
                "below": len(below),
                "delay_p90": percentile([int(item["delay_ms"]) for item in candidate_facts], 9, 10) if candidate_facts else None,
                "competitive": competitive,
                "liquidity_min": (
                    min(int(record["liquidity"]) for record in checkpoint_records)
                    if len(checkpoint_records) == 2
                    else None
                ),
                "checkpoint_min_block": (
                    min(int(record["block"]) for record in checkpoint_records)
                    if len(checkpoint_records) == 2
                    else None
                ),
                "latest_fact_block": latest_fact_block,
                "proofs_complete": proof0 is not None and proof1 is not None,
                "template_present": (token0, token1, settlement_asset) in templates,
                "max_quote_age_ms": (
                    int(
                        templates[(token0, token1, settlement_asset)]["strategy"][
                            "max_quote_age_ms"
                        ]
                    )
                    if (token0, token1, settlement_asset) in templates
                    else None
                ),
                "disagreement": any(item["agreement"] == "disagreed" for item in candidate_facts),
            }
        )

    maximum_transactions = max((int(item["transactions"]) for item in candidates), default=1)
    maximum_swaps = max((int(item["swaps"]) for item in candidates), default=1)
    maximum_blocks = max((int(item["blocks"]) for item in candidates), default=1)
    positive_pnl_max: dict[str, int] = defaultdict(int)
    pair_liquidity_max: dict[tuple[str, str], int] = defaultdict(int)
    pair_volume_max: dict[tuple[str, str, str], int] = defaultdict(int)
    rpc_costs = [item["rpc_records"] * SCORE_SCALE // len(item["facts"]) for item in candidates if item["facts"] and item["rpc_records"] > 0]
    for item in candidates:
        token0, token1, settlement_asset, _, _ = item["key"]
        if item["expected_lower_median"] is not None and int(item["expected_lower_median"]) > 0:
            positive_pnl_max[settlement_asset] = max(
                positive_pnl_max[settlement_asset], int(item["expected_lower_median"])
            )
        if item["liquidity_min"] is not None:
            pair_liquidity_max[(token0, token1)] = max(pair_liquidity_max[(token0, token1)], int(item["liquidity_min"]))
        for asset, amount in item["volume"].items():
            pair_volume_max[(token0, token1, asset)] = max(pair_volume_max[(token0, token1, asset)], int(amount))

    minimum_rpc_cost = min(rpc_costs) if rpc_costs else None
    rendered_candidates: list[dict[str, object]] = []
    for item in candidates:
        token0, token1, settlement_asset, fee0, fee1 = item["key"]
        other_asset = token1 if settlement_asset == token0 else token0
        components: dict[str, dict[str, object]] = {}
        components["transaction_count"] = component("available", item["transactions"], ratio_bps(item["transactions"], maximum_transactions), "relative to the largest bounded candidate sample")
        components["swap_count"] = component("available", item["swaps"], ratio_bps(item["swaps"], maximum_swaps), "relative decoded pool touches")
        components["unique_blocks"] = component(
            "available" if item["blocks"] else "unavailable",
            item["blocks"] if item["blocks"] else None,
            ratio_bps(item["blocks"], maximum_blocks) if item["blocks"] else 0,
            "distinct persisted source blocks; absent metadata is not inferred",
        )
        components["router_distribution"] = component("available", item["routers"], ratio_bps(len(item["routers"]), 3), "reviewed official router-family diversity")
        components["fee_tier_diversity"] = component("available", item["fee_tier_diversity"], ratio_bps(min(item["fee_tier_diversity"], 4), 4), "distinct observed fee tiers for the token pair")
        direction_max = max(item["directions"].values(), default=0)
        direction_min = min(
            int(item["directions"].get("token0_to_token1", 0)),
            int(item["directions"].get("token1_to_token0", 0)),
        )
        components["directional_flow"] = component("available", item["directions"], ratio_bps(direction_min, direction_max) if direction_max else 0, "balance between canonical directions")
        volume_scores = []
        for asset, amount in item["volume"].items():
            maximum = pair_volume_max[(token0, token1, asset)]
            if maximum > 0:
                volume_scores.append(ratio_bps(amount, maximum))
        components["volume_proxy"] = component(
            "available" if volume_scores else "unavailable",
            {asset: str(amount) for asset, amount in item["volume"].items()} if volume_scores else None,
            sum(volume_scores) // len(volume_scores) if volume_scores else 0,
            "input amounts normalized only within the same token pair and input asset",
        )
        liquidity_max = pair_liquidity_max.get((token0, token1), 0)
        components["liquidity_proxy"] = component(
            "available" if item["liquidity_min"] is not None and liquidity_max > 0 else "unavailable",
            {
                "minimum_liquidity": str(item["liquidity_min"]),
                "oldest_checkpoint_block": item["checkpoint_min_block"],
                "latest_profitability_block": item["latest_fact_block"],
            }
            if item["liquidity_min"] is not None
            else None,
            ratio_bps(item["liquidity_min"], liquidity_max) if item["liquidity_min"] is not None and liquidity_max > 0 else 0,
            "minimum latest checkpoint liquidity, compared only within the token pair",
        )
        components["pool_impact_frequency"] = component("available", {"route_transactions": item["transactions"], "supported_transactions": len(supported_transactions)}, ratio_bps(item["transactions"], max(1, len(supported_transactions))), "share of bounded supported official-router history touching either pool")
        components["candidate_frequency"] = component("available", {"complete_facts": len(item["facts"]), "route_transactions": item["transactions"]}, ratio_bps(len(item["facts"]), max(1, item["transactions"])), "complete canonical evaluations per observed route transaction")
        components["rpc_evaluation_availability"] = component(
            "available" if item["facts"] else "unavailable",
            {"primary_evaluations": item["primary_facts"], "complete_facts": len(item["facts"])} if item["facts"] else None,
            ratio_bps(item["primary_facts"], len(item["facts"])) if item["facts"] else 0,
            "complete facts with primary provider evidence",
        )
        pnl_max = positive_pnl_max.get(settlement_asset, 0)
        components["expected_net_pnl"] = component(
            "available" if item["expected_lower_median"] is not None else "unavailable",
            {
                "settlement_asset": settlement_asset,
                "conservative_lower_median": str(item["expected_lower_median"]),
            }
            if item["expected_lower_median"] is not None
            else None,
            ratio_bps(max(0, item["expected_lower_median"]), pnl_max)
            if item["expected_lower_median"] is not None and pnl_max > 0
            else 0,
            "conservative lower-median SHADOW expected net PnL normalized only within the settlement asset; not realized",
        )
        components["near_profitable_frequency"] = component(
            "available" if item["below"] else "unavailable",
            {"near": item["near"], "below_minimum": item["below"]} if item["below"] else None,
            ratio_bps(item["near"], item["below"]) if item["below"] else 0,
            "below-minimum facts reaching at least 90 percent of the configured threshold",
        )
        rpc_cost = item["rpc_records"] * SCORE_SCALE // len(item["facts"]) if item["facts"] and item["rpc_records"] > 0 else None
        components["rpc_cost"] = component(
            "available" if rpc_cost is not None and minimum_rpc_cost is not None else "unavailable",
            {
                "rpc_records": item["rpc_records"],
                "rpc_latency_ns_total": str(sum(int(fact["rpc_latency_ns"]) for fact in item["facts"])),
                "complete_facts": len(item["facts"]),
            }
            if rpc_cost is not None
            else None,
            ratio_bps(minimum_rpc_cost, rpc_cost) if rpc_cost is not None and minimum_rpc_cost is not None else 0,
            "inverse RPC quality-record count per complete evaluation; not provider billing units",
        )
        failure_rate = ratio_bps(item["rpc_failures"], item["rpc_records"]) if item["rpc_records"] else None
        components["provider_failure_rate"] = component(
            "available" if failure_rate is not None else "unavailable",
            {"failures": item["rpc_failures"], "records": item["rpc_records"]} if failure_rate is not None else None,
            SCORE_SCALE - failure_rate if failure_rate is not None else 0,
            "inverse observed RPC quality failure share",
        )
        delay = item["delay_p90"]
        quote_age_limit = item["max_quote_age_ms"]
        delay_score = 0
        if delay is not None and quote_age_limit is not None:
            delay_score = SCORE_SCALE - ratio_bps(delay, quote_age_limit)
        components["state_freshness"] = component(
            "available" if delay is not None and quote_age_limit is not None else "unavailable",
            {
                "p90_detection_to_evaluation_ms": delay,
                "reviewed_max_quote_age_ms": quote_age_limit,
            }
            if delay is not None and quote_age_limit is not None
            else None,
            delay_score,
            "absolute p90 detected-to-evaluated delay against the reviewed quote-age policy",
        )
        components["competition_proxy"] = component(
            "available" if item["facts"] else "unavailable",
            {"shared_block_facts": item["competitive"], "complete_facts": len(item["facts"])} if item["facts"] else None,
            SCORE_SCALE - ratio_bps(item["competitive"], len(item["facts"])) if item["facts"] else 0,
            "inverse share of facts on blocks containing another ranked route",
        )
        components["decoder_confidence"] = component(
            "available",
            {"trusted_transactions": item["trusted_transactions"], "route_transactions": item["transactions"]},
            ratio_bps(item["trusted_transactions"], max(1, item["transactions"])),
            "share decoded from trusted PostgreSQL feed-event provenance",
        )
        if availability["status"] == "available":
            overlap = int(availability["overlap"])
            observed = int(availability["observed"])
            components["feed_gap_overlap"] = component("available", {"overlap_events": overlap, "observed_events": observed}, SCORE_SCALE - ratio_bps(overlap, observed), "inverse persisted feed-gap overlap share")
        else:
            components["feed_gap_overlap"] = component("unavailable", None, 0, "feed-gap overlap is not persisted and is never inferred")
        available_count = sum(1 for value in components.values() if value["status"] == "available")
        components["data_completeness"] = component("available", {"available_components": available_count, "evaluated_components": len(COMPONENT_NAMES) - 1}, ratio_bps(available_count, len(COMPONENT_NAMES) - 1), "availability of non-completeness ranking components")
        if set(components) != set(COMPONENT_NAMES):
            raise DiscoveryError("ranking components are incomplete")
        total_score = sum(int(value["weighted_points"]) for value in components.values())
        warnings = [f"{name}:{value['status']}" for name, value in sorted(components.items()) if value["status"] != "available"]
        if item["trusted_transactions"] != item["transactions"]:
            warnings.append("history_contains_untrusted_or_synthetic_rows")
        unsafe = []
        if not item["proofs_complete"]:
            unsafe.append("unverified_pool_address")
        if not item["template_present"]:
            unsafe.append("missing_reviewed_economics_template")
        if item["transactions"] < MIN_TRANSACTIONS:
            unsafe.append("insufficient_transaction_history")
        if item["trusted_transactions"] != item["transactions"] or item["transactions"] == 0:
            unsafe.append("untrusted_or_synthetic_history")
        if item["blocks"] < MIN_UNIQUE_BLOCKS:
            unsafe.append("insufficient_unique_block_provenance")
        if item["liquidity_min"] is None:
            unsafe.append("missing_liquidity_checkpoint")
        elif item["liquidity_min"] == 0:
            unsafe.append("zero_liquidity_checkpoint")
        if (
            item["latest_fact_block"] is not None
            and item["checkpoint_min_block"] is not None
            and item["checkpoint_min_block"] < item["latest_fact_block"]
        ):
            unsafe.append("stale_liquidity_checkpoint")
        if not item["facts"]:
            unsafe.append("missing_complete_profitability_evidence")
        if not item["facts"] or item["primary_facts"] != len(item["facts"]):
            unsafe.append("missing_primary_rpc_evidence")
        if item["severe_min"] is None or item["severe_min"] <= 0:
            unsafe.append("non_positive_severe_pnl_evidence")
        if (
            item["delay_p90"] is None
            or item["max_quote_age_ms"] is None
            or item["delay_p90"] > item["max_quote_age_ms"]
        ):
            unsafe.append("state_freshness_above_policy")
        if item["disagreement"]:
            unsafe.append("provider_disagreement_observed")
        if failure_rate is not None and failure_rate > MAX_PROVIDER_FAILURE_BPS:
            unsafe.append("provider_failure_rate_above_policy")
        if components["data_completeness"]["score_bps"] < MIN_COMPLETENESS_BPS:
            unsafe.append("data_completeness_below_policy")
        suggestion = suggested_route(item["key"], proofs, templates)
        ranking_reasons = [
            name
            for name, _ in sorted(
                components.items(),
                key=lambda pair: (-int(pair[1]["weighted_points"]), pair[0]),
            )[:3]
        ]
        route_id, fingerprint = route_identity(item["key"])
        rendered_candidates.append(
            {
                "route_id": route_id,
                "route_fingerprint": fingerprint,
                "canonical_token_path": [settlement_asset, other_asset, settlement_asset],
                "canonical_pool_ids": [f"{token0}:{token1}:{fee0}", f"{token0}:{token1}:{fee1}"],
                "canonical_pool_addresses": [
                    proofs[(token0, token1, fee0)]["pool_address"] if (token0, token1, fee0) in proofs else None,
                    proofs[(token0, token1, fee1)]["pool_address"] if (token0, token1, fee1) in proofs else None,
                ],
                "fee_path": [fee0, fee1],
                "total_score_points": total_score,
                "component_scores": components,
                "ranking_reasons": ranking_reasons,
                "data_quality_warnings": sorted(set(warnings)),
                "unsafe_or_unsupported_reasons": sorted(set(unsafe)),
                "shadow_activation_eligible": not unsafe and suggestion is not None,
                "suggested_route_json": suggestion,
                "financial_basis": "SHADOW expected",
                "realization_status": "not realized",
            }
        )

    rendered_candidates.sort(
        key=lambda item: (
            -int(item["total_score_points"]),
            -int(item["component_scores"]["transaction_count"]["raw"]),
            str(item["route_id"]),
        )
    )
    top_routes = rendered_candidates[:top_limit]
    for index, item in enumerate(top_routes, start=1):
        item["rank"] = index
    selected = [item["suggested_route_json"] for item in top_routes if item["shadow_activation_eligible"]][:3]
    global_warnings = []
    if not observations:
        global_warnings.append("no_decoded_official_router_history")
    if availability["status"] != "available":
        global_warnings.append("feed_gap_overlap_unavailable_not_persisted")
    if not selected:
        global_warnings.append("no_route_passed_shadow_activation_policy")
    return {
        "schema_version": SCHEMA_VERSION,
        "chain_id": CHAIN_ID,
        "mode": MODE,
        "live_execution": False,
        "execution_eligible": False,
        "execution_request_created": False,
        "production_registry_mutated": False,
        "financial_basis": "SHADOW expected",
        "realization_status": "not realized",
        "ranking_formula": {
            "score_scale_bps": SCORE_SCALE,
            "component_weight_points": COMPONENT_WEIGHT,
            "maximum_score_points": len(COMPONENT_NAMES) * COMPONENT_WEIGHT,
            "components": list(COMPONENT_NAMES),
            "financial_partition": "settlement_asset",
            "volume_partition": "token_pair_and_input_asset",
            "liquidity_partition": "token_pair",
            "unavailable_component_score": 0,
            "tie_break": ["total_score_points_desc", "transaction_count_desc", "route_id_asc"],
        },
        "selection_policy": {
            "maximum_selected_routes": 3,
            "minimum_transactions": MIN_TRANSACTIONS,
            "minimum_unique_blocks": MIN_UNIQUE_BLOCKS,
            "minimum_data_completeness_bps": MIN_COMPLETENESS_BPS,
            "maximum_provider_failure_bps": MAX_PROVIDER_FAILURE_BPS,
            "requires_trusted_postgresql_history": True,
            "requires_create2_pool_proofs": True,
            "requires_positive_severe_pnl": True,
            "severe_pnl_policy": "worst_observed_must_be_positive",
            "state_freshness_policy": "p90_within_reviewed_max_quote_age_ms",
            "registry_update_is_manual_and_out_of_band": True,
        },
        "input_summary": {
            "decoded_rows": len(observations),
            "duplicate_decoded_rows_removed": duplicate_rows_removed,
            "trusted_supported_rows": trusted_supported_rows,
            "supported_transactions": len(supported_transactions),
            "complete_profitability_facts": len(facts),
            "pool_checkpoints": len(checkpoints),
            "candidate_routes": len(rendered_candidates),
            "feed_gap_overlap_status": availability["status"],
        },
        "pool_proof_contract": proof_metadata,
        "top_routes": top_routes,
        "selected_shadow_routes": selected,
        "selected_shadow_route_count": len(selected),
        "global_warnings": sorted(global_warnings),
    }


def render_text(report: dict[str, object]) -> str:
    lines = [
        "SHADOW Route Discovery And Ranking",
        f"Mode: {report['mode']}",
        "Financial basis: SHADOW expected",
        "Realization status: not realized",
        "Production registry mutated: false",
        "",
        "Top 10 Routes",
    ]
    routes = report["top_routes"]
    if not routes:
        lines.append("No bounded route candidates were discovered.")
    for route in routes:
        lines.extend(
            [
                f"{route['rank']}. {route['route_id']} score={route['total_score_points']}",
                f"   fees={route['fee_path']} activation_eligible={str(route['shadow_activation_eligible']).lower()}",
                f"   reasons={','.join(route['ranking_reasons'])}",
                f"   warnings={','.join(route['data_quality_warnings']) or 'none'}",
                f"   unsafe={','.join(route['unsafe_or_unsupported_reasons']) or 'none'}",
            ]
        )
    lines.extend(
        [
            "",
            "Selected SHADOW Routes",
            f"Count: {report['selected_shadow_route_count']}",
            "",
            "Data Quality",
            f"Warnings: {','.join(report['global_warnings']) or 'none'}",
        ]
    )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Rank bounded persisted SHADOW route evidence")
    parser.add_argument("--decoded", required=True, type=Path)
    parser.add_argument("--enrichment", required=True, type=Path)
    parser.add_argument("--pool-proofs", required=True, type=Path)
    parser.add_argument("--format", choices=("json", "text"), default="text")
    parser.add_argument("--limit", type=int, default=10_000)
    parser.add_argument("--top", type=int, default=10)
    return parser.parse_args()


def main() -> int:
    try:
        args = parse_args()
        observations, _statistics = read_decoded(args.decoded, args.limit)
        observations, duplicate_rows_removed = deduplicate_observations(observations)
        facts, checkpoints, availability = read_enrichment(args.enrichment)
        proofs, templates, proof_metadata = load_pool_proofs(args.pool_proofs)
        report = build_report(
            observations,
            facts,
            checkpoints,
            availability,
            proofs,
            templates,
            proof_metadata,
            args.top,
            duplicate_rows_removed,
        )
        if args.format == "json":
            json.dump(report, sys.stdout, sort_keys=True, separators=(",", ":"))
            sys.stdout.write("\n")
        else:
            sys.stdout.write(render_text(report))
        return 0
    except DiscoveryError as error:
        print(f"shadow route discovery failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
