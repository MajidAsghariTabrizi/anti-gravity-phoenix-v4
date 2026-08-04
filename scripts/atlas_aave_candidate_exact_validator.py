#!/usr/bin/env python3
"""Candidate-first exact Aave validation for a retained prefilter cohort.

The validator deliberately issues only individual, exact-block JSON-RPC
requests.  It refreshes retained account data before resolving any reserve and
persists only sanitized, hash-bound derived evidence.  It grants no execution
authority.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from pathlib import Path
from typing import Any

try:
    from scripts.atlas_borrower_index import calculate_account
    from scripts.export_aave_borrow_discovery import (
        ProviderDiagnosticError,
        SSHContainerProvider,
    )
    from scripts.export_aave_checkpoint import (
        AAVE_ADDRESS_BOOK_COMMIT,
        AAVE_V3_ORIGIN_COMMIT,
        CHAIN_ID,
        DATA_PROVIDER,
        ORACLE,
        POOL,
        POOL_IMPLEMENTATION,
        POOL_IMPLEMENTATION_SLOT,
        SELECTORS,
        ExportError,
        bind_hash,
        call_data,
        canonical_hash,
        current_state_proof_policy,
        encode_address,
        provider_request_usage,
        uint_word,
        word_address,
        word_int,
        word_uint,
        words,
        write_private_json,
    )
except ModuleNotFoundError:
    from atlas_borrower_index import calculate_account  # type: ignore[no-redef]
    from export_aave_borrow_discovery import (  # type: ignore[no-redef]
        ProviderDiagnosticError,
        SSHContainerProvider,
    )
    from export_aave_checkpoint import (  # type: ignore[no-redef]
        AAVE_ADDRESS_BOOK_COMMIT,
        AAVE_V3_ORIGIN_COMMIT,
        CHAIN_ID,
        DATA_PROVIDER,
        ORACLE,
        POOL,
        POOL_IMPLEMENTATION,
        POOL_IMPLEMENTATION_SLOT,
        SELECTORS,
        ExportError,
        bind_hash,
        call_data,
        canonical_hash,
        current_state_proof_policy,
        encode_address,
        provider_request_usage,
        uint_word,
        word_address,
        word_int,
        word_uint,
        words,
        write_private_json,
    )


WAD = 10**18
MAX_BORROWERS = 2
MAX_RELEVANT_RESERVES = 20
MAX_RESERVE_ID = 127
MAX_RUNTIME_SECONDS = 300
MAX_ITEMS_PER_PROVIDER = 250
REFRESH_MAX_SECONDS = 60
REFRESH_MAX_ITEMS_PER_PROVIDER = 10
SCHEMA = "phoenix.atlas.aave-candidate-exact-validation.v1"
MATRIX_SCHEMA = "phoenix.atlas.aave-reserve-configuration-matrix.v1"
COHORT_SCHEMA = "phoenix.atlas.aave-screen-cohort.v1"
ORACLE_CAPABILITY_SCHEMA = "phoenix.atlas.aave-oracle-capability-matrix.v1"
ORACLE_CAPABILITY_MAX_RUNTIME_SECONDS = 90
ORACLE_CAPABILITY_MAX_ITEMS_PER_PROVIDER = 30
ORACLE_CAPABILITY_ASSETS = (
    "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8",  # USDC.e
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",  # WETH
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831",  # USDC
)

ORACLE_SELECTORS = {
    "latest_answer": "50d25bcd",
    "latest_timestamp": "8205bf6a",
}

ROUND_CAPABILITY_FAILURES = {"rpc_error:3"}

# AaveProtocolDataProvider selectors from the source-bound V3 Origin ABI.
DATA_PROVIDER_SELECTORS = {
    "get_paused": "b55d9904",
    "get_liquidation_protocol_fee": "3cb8a622",
    "get_debt_ceiling": "3c798109",
    "get_siloed_borrowing": "fcf40a62",
}


class CandidateEvidenceError(ExportError):
    """Fail-closed Candidate error carrying sanitized partial evidence."""

    def __init__(
        self,
        message: str,
        evidence: dict[str, Any] | None = None,
        stage: str | None = None,
        method: str | None = None,
    ) -> None:
        super().__init__(message)
        self.evidence = evidence or {}
        self.stage = stage
        self.method = method


def _address(value: Any, name: str) -> str:
    if not isinstance(value, str) or len(value) != 42 or not value.startswith("0x"):
        raise ExportError(f"{name} is not a canonical address")
    try:
        int(value[2:], 16)
    except ValueError as error:
        raise ExportError(f"{name} is not hexadecimal") from error
    return value.lower()


def _digest(value: Any, name: str) -> str:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise ExportError(f"{name} is not a SHA-256 digest")
    return value


def load_cohort(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ExportError("retained cohort is not an object")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("retained cohort content hash mismatch")
    addresses = value.get("addresses")
    if (
        value.get("schema") != COHORT_SCHEMA
        or value.get("chain_id") != CHAIN_ID
        or str(value.get("pool", "")).lower() != POOL
        or value.get("candidate_authority") is not False
        or value.get("execution_authority") is not False
        or not isinstance(addresses, list)
        or not 1 <= len(addresses) <= MAX_BORROWERS
    ):
        raise ExportError("retained cohort authority or identity is invalid")
    canonical = [_address(item, "retained borrower") for item in addresses]
    if canonical != addresses or len(set(canonical)) != len(canonical):
        raise ExportError("retained cohort addresses are not canonical and unique")
    _digest(value.get("source_discovery_content_sha256"), "discovery binding")
    _digest(value.get("source_prefilter_content_sha256"), "prefilter binding")
    return value


def _result_hash(value: str) -> str:
    try:
        payload = bytes.fromhex(value[2:])
    except (TypeError, ValueError) as error:
        raise ExportError("RPC result is not canonical hex") from error
    return hashlib.sha256(payload).hexdigest()


def _header(value: Any, expected: int | None = None) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExportError("exact finalized header is unavailable")
    try:
        number = int(str(value.get("number")), 16)
        timestamp = int(str(value.get("timestamp")), 16)
    except (TypeError, ValueError) as error:
        raise ExportError("exact finalized header is malformed") from error
    block_hash = str(value.get("hash", "")).lower()
    parent_hash = str(value.get("parentHash", "")).lower()
    state_root = str(value.get("stateRoot", "")).lower()
    if (
        number < 1
        or timestamp < 1
        or (expected is not None and number != expected)
        or any(len(item) != 66 for item in (block_hash, parent_hash, state_root))
    ):
        raise ExportError("exact finalized header is invalid")
    return {
        "number": number,
        "hash": block_hash,
        "parent_hash": parent_hash,
        "state_root": state_root,
        "timestamp": timestamp,
    }


def _context(provider: Any, stage: str) -> None:
    setter = getattr(provider, "set_diagnostic_context", None)
    if callable(setter):
        setter(stage)


def _call(provider: Any, method: str, params: list[object], stage: str) -> Any:
    _context(provider, stage)
    return provider.call(method, params, attempts=1)


def _eth_call(
    provider: Any, target: str, data: str, block: int, semantic: str
) -> str:
    result = _call(
        provider,
        "eth_call",
        [{"to": target, "data": data}, hex(block)],
        semantic,
    )
    if not isinstance(result, str) or not result.startswith("0x"):
        raise ExportError(f"{semantic} returned an invalid result")
    words(result)
    return result.lower()


def _account_data(result: str) -> dict[str, int]:
    values = words(result, 6)
    if len(values) != 6:
        raise ExportError("getUserAccountData result length is invalid")
    return {
        "total_collateral_base": word_uint(values[0]),
        "total_debt_base": word_uint(values[1]),
        "available_borrows_base": word_uint(values[2]),
        "current_liquidation_threshold_bps": word_uint(values[3]),
        "ltv_bps": word_uint(values[4]),
        "health_factor_wad": word_uint(values[5]),
    }


def _provider_binding(provider: Any) -> dict[str, Any]:
    return {
        "provider_id": provider.label,
        "provider_reference_sha256": provider.provider_reference_sha256,
        "header_name": provider.header_name,
        "authenticated": provider.authenticated,
    }


def _sanitized_request_usage(providers: list[Any]) -> list[dict[str, Any]]:
    return [
        {
            key: value
            for key, value in item.items()
            if key != "endpoint_identity"
        }
        for item in provider_request_usage(providers)
    ]


def _capability_probe(
    provider: Any,
    method: str,
    params: list[object],
    semantic: str,
    asset: str,
    source: str | None,
) -> tuple[Any | None, dict[str, Any]]:
    started = time.monotonic_ns()
    try:
        result = _call(provider, method, params, f"oracle_capability_{semantic}")
        if not isinstance(result, str) or not result.startswith("0x"):
            raise ExportError("oracle capability result is invalid")
        response_sha256 = _result_hash(result.lower())
        evidence = {
            "asset": asset,
            "source": source,
            "provider_id": provider.label,
            "provider_reference_sha256": provider.provider_reference_sha256,
            "semantic_method_name": semantic,
            "success": True,
            "failure_class": None,
            "return_length_bytes": (len(result) - 2) // 2,
            "response_sha256": response_sha256,
            "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
        }
        return result.lower(), evidence
    except ProviderDiagnosticError as error:
        failure = error.failure_class
    except ExportError:
        failure = "invalid_result"
    return None, {
        "asset": asset,
        "source": source,
        "provider_id": provider.label,
        "provider_reference_sha256": provider.provider_reference_sha256,
        "semantic_method_name": semantic,
        "success": False,
        "failure_class": failure,
        "return_length_bytes": None,
        "response_sha256": None,
        "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
    }


def oracle_capability_matrix(providers: list[Any], started_ns: int) -> dict[str, Any]:
    if len(providers) != 2:
        raise ExportError("oracle capability matrix requires exactly two providers")
    chains: list[int] = []
    finalized: list[dict[str, Any]] = []
    for provider in providers:
        chains.append(
            int(str(_call(provider, "eth_chainId", [], "oracle_capability_chain")), 16)
        )
        finalized.append(
            _header(
                _call(
                    provider,
                    "eth_getBlockByNumber",
                    ["finalized", False],
                    "oracle_capability_finalized",
                )
            )
        )
    if set(chains) != {CHAIN_ID}:
        raise ExportError("oracle capability provider chain identity disagreement")
    block = min(item["number"] for item in finalized)
    exact_headers = [
        _header(
            _call(
                provider,
                "eth_getBlockByNumber",
                [hex(block), False],
                "oracle_capability_exact_header",
            ),
            block,
        )
        for provider in providers
    ]
    if (
        len({item["hash"] for item in exact_headers}) != 1
        or len({item["state_root"] for item in exact_headers}) != 1
    ):
        raise ExportError("oracle capability exact header disagreement")

    calls: list[dict[str, Any]] = []
    for asset in ORACLE_CAPABILITY_ASSETS:
        asset_word = encode_address(asset)
        for provider in providers:
            source_result, source_evidence = _capability_probe(
                provider,
                "eth_call",
                [
                    {"to": ORACLE, "data": call_data(SELECTORS["get_source_of_asset"], asset_word)},
                    hex(block),
                ],
                "get_source_of_asset",
                asset,
                None,
            )
            source = None
            if source_result is not None:
                try:
                    source = word_address(words(source_result)[0])
                    source_evidence["source"] = source
                except ExportError:
                    source_evidence["success"] = False
                    source_evidence["failure_class"] = "invalid_source_address"
            calls.append(source_evidence)
            target = source or "0x" + "0" * 40
            methods = (
                ("source_code", "eth_getCode", [target, hex(block)]),
                (
                    "get_asset_price",
                    "eth_call",
                    [
                        {"to": ORACLE, "data": call_data(SELECTORS["get_asset_price"], asset_word)},
                        hex(block),
                    ],
                ),
                (
                    "latest_answer",
                    "eth_call",
                    [{"to": target, "data": call_data(ORACLE_SELECTORS["latest_answer"])}, hex(block)],
                ),
                (
                    "latest_round_data",
                    "eth_call",
                    [{"to": target, "data": call_data(SELECTORS["latest_round_data"])}, hex(block)],
                ),
                (
                    "latest_timestamp",
                    "eth_call",
                    [{"to": target, "data": call_data(ORACLE_SELECTORS["latest_timestamp"])}, hex(block)],
                ),
                (
                    "decimals",
                    "eth_call",
                    [{"to": target, "data": call_data(SELECTORS["decimals"])}, hex(block)],
                ),
            )
            for semantic, method, params in methods:
                result, evidence = _capability_probe(
                    provider, method, params, semantic, asset, source
                )
                if result is not None and semantic in {
                    "get_asset_price",
                    "latest_answer",
                    "latest_timestamp",
                    "decimals",
                }:
                    try:
                        value = (
                            word_int(words(result)[0])
                            if semantic == "latest_answer"
                            else word_uint(words(result)[0])
                        )
                        evidence["integer_value"] = value
                    except ExportError:
                        evidence["success"] = False
                        evidence["failure_class"] = "invalid_integer_result"
                if result is not None and semantic == "source_code":
                    evidence["source_code_sha256"] = _result_hash(result)
                calls.append(evidence)

    usage = _sanitized_request_usage(providers)
    runtime_ms = (time.monotonic_ns() - started_ns) // 1_000_000
    if (
        runtime_ms > ORACLE_CAPABILITY_MAX_RUNTIME_SECONDS * 1_000
        or any(
            item["json_rpc_item_count"] > ORACLE_CAPABILITY_MAX_ITEMS_PER_PROVIDER
            or item["retry_count"] != 0
            for item in usage
        )
    ):
        raise ExportError("oracle capability matrix exceeded a hard bound")
    return bind_hash(
        {
            "schema": ORACLE_CAPABILITY_SCHEMA,
            "status": "oracle_capability_matrix",
            "chain_id": CHAIN_ID,
            "checkpoint_block": block,
            "checkpoint_hash": exact_headers[0]["hash"],
            "checkpoint_state_root": exact_headers[0]["state_root"],
            "checkpoint_timestamp": exact_headers[0]["timestamp"],
            "provider_headers": [
                {
                    **_provider_binding(provider),
                    "checkpoint": header,
                }
                for provider, header in zip(providers, exact_headers)
            ],
            "assets": list(ORACLE_CAPABILITY_ASSETS),
            "calls": calls,
            "provider_request_usage": usage,
            "individual_json_rpc_calls_only": True,
            "automatic_retries": 0,
            "raw_rpc_responses_persisted": False,
            "candidate_authority": False,
            "execution_authority": False,
            "runtime_ms": runtime_ms,
        }
    )


def refresh_signals(
    providers: list[Any], cohort: dict[str, Any], started_ns: int
) -> dict[str, Any]:
    if len(providers) != 2:
        raise ExportError("candidate refresh requires exactly two providers")
    chains = []
    finalized = []
    for provider in providers:
        chain = _call(provider, "eth_chainId", [], "candidate_chain_identity")
        chains.append(int(str(chain), 16))
        finalized.append(
            _header(
                _call(
                    provider,
                    "eth_getBlockByNumber",
                    ["finalized", False],
                    "candidate_finalized_head",
                )
            )
        )
    if set(chains) != {CHAIN_ID}:
        raise ExportError("candidate provider chain identity disagreement")
    block = min(item["number"] for item in finalized)
    exact_headers = [
        _header(
            _call(
                provider,
                "eth_getBlockByNumber",
                [hex(block), False],
                "candidate_exact_header",
            ),
            block,
        )
        for provider in providers
    ]
    if (
        len({item["hash"] for item in exact_headers}) != 1
        or len({item["state_root"] for item in exact_headers}) != 1
    ):
        raise ExportError("candidate exact header or state root disagreement")
    pool_codes: list[str] = []
    implementation_words: list[str] = []
    for provider in providers:
        code = _call(
            provider,
            "eth_getCode",
            [POOL, hex(block)],
            "candidate_pool_code",
        )
        implementation = _call(
            provider,
            "eth_getStorageAt",
            [POOL, POOL_IMPLEMENTATION_SLOT, hex(block)],
            "candidate_pool_implementation",
        )
        if not isinstance(code, str) or code == "0x":
            raise ExportError("candidate Pool code is unavailable")
        if not isinstance(implementation, str) or len(implementation) != 66:
            raise ExportError("candidate Pool implementation storage is unavailable")
        if word_address(words(implementation)[0]) != POOL_IMPLEMENTATION:
            raise ExportError("candidate Pool implementation source binding mismatch")
        pool_codes.append(_result_hash(code.lower()))
        implementation_words.append(implementation.lower())
    if len(set(pool_codes)) != 1 or len(set(implementation_words)) != 1:
        raise ExportError("candidate Pool code or implementation disagreement")

    rows = []
    for borrower in cohort["addresses"]:
        results: list[dict[str, int] | None] = []
        failures: list[dict[str, Any] | None] = []
        for provider in providers:
            try:
                result = _eth_call(
                    provider,
                    POOL,
                    call_data(
                        SELECTORS["get_user_account_data"], encode_address(borrower)
                    ),
                    block,
                    "candidate_account_refresh",
                )
                results.append(_account_data(result))
                failures.append(None)
            except ProviderDiagnosticError as error:
                results.append(None)
                failures.append(
                    {
                        "provider_id": error.provider_id,
                        "failure_class": error.failure_class,
                        "method": error.method,
                    }
                )
            except ExportError:
                results.append(None)
                failures.append(
                    {
                        "provider_id": provider.label,
                        "failure_class": "invalid_account_data",
                        "method": "eth_call",
                    }
                )
        if any(result is None for result in results):
            classification = "incomplete"
            agreed = None
        elif results[0] != results[1]:
            classification = "provider_disagreement"
            agreed = None
        else:
            agreed = results[0]
            assert agreed is not None
            classification = (
                "exact_liquidatable_signal"
                if agreed["health_factor_wad"] < WAD
                else "stale_not_liquidatable"
            )
        rows.append(
            {
                "borrower": borrower,
                "classification": classification,
                "agreed_account_data": agreed,
                "sanitized_failures": [item for item in failures if item is not None],
            }
        )

    elapsed_ms = (time.monotonic_ns() - started_ns) // 1_000_000
    usage = _sanitized_request_usage(providers)
    if (
        elapsed_ms > REFRESH_MAX_SECONDS * 1_000
        or any(
            item["json_rpc_item_count"] > REFRESH_MAX_ITEMS_PER_PROVIDER
            or item["retry_count"] != 0
            for item in usage
        )
    ):
        raise ExportError("candidate refresh exceeded its hard bound")
    provider_headers = [
        {
            **_provider_binding(provider),
            "checkpoint": exact_header,
        }
        for provider, exact_header in zip(providers, exact_headers)
    ]
    return {
        "checkpoint_block": block,
        "checkpoint_hash": exact_headers[0]["hash"],
        "checkpoint_state_root": exact_headers[0]["state_root"],
        "checkpoint_timestamp": exact_headers[0]["timestamp"],
        "finalized_heads": finalized,
        "provider_headers": provider_headers,
        "pool_code_sha256": pool_codes[0],
        "implementation_words": implementation_words,
        "rows": rows,
        "request_usage": usage,
        "elapsed_ms": elapsed_ms,
    }


def _probe(
    provider: Any,
    target: str,
    data: str,
    block: int,
    semantic: str,
    asset: str,
) -> tuple[str | None, dict[str, Any]]:
    started = time.monotonic_ns()
    try:
        result = _eth_call(provider, target, data, block, f"matrix_{semantic}")
        evidence = {
            "provider_id": provider.label,
            "method_semantic_name": semantic,
            "target_logical_id": (
                "pool" if target == POOL else "protocol_data_provider"
            ),
            "asset": asset,
            "block_number": block,
            "success": True,
            "failure_class": None,
            "return_length_bytes": (len(result) - 2) // 2,
            "response_sha256": _result_hash(result),
            "request_count": 1,
            "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
        }
        return result, evidence
    except ProviderDiagnosticError as error:
        failure = error.failure_class
    except ExportError:
        failure = "invalid_result"
    return None, {
        "provider_id": provider.label,
        "method_semantic_name": semantic,
        "target_logical_id": "pool" if target == POOL else "protocol_data_provider",
        "asset": asset,
        "block_number": block,
        "success": False,
        "failure_class": failure,
        "return_length_bytes": None,
        "response_sha256": None,
        "request_count": 1,
        "elapsed_ms": (time.monotonic_ns() - started) // 1_000_000,
    }


def _configuration_from_bitmap(bitmap: int) -> dict[str, Any]:
    return {
        "configuration_bitmap": bitmap,
        "ltv_bps": bitmap & 0xFFFF,
        "liquidation_threshold_bps": (bitmap >> 16) & 0xFFFF,
        "liquidation_bonus_bps": (bitmap >> 32) & 0xFFFF,
        "decimals": (bitmap >> 48) & 0xFF,
        "active": bool((bitmap >> 56) & 1),
        "frozen": bool((bitmap >> 57) & 1),
        "borrowing_enabled": bool((bitmap >> 58) & 1),
        "paused": bool((bitmap >> 60) & 1),
        "reserve_factor_bps": (bitmap >> 64) & 0xFFFF,
        "liquidation_protocol_fee_bps": (bitmap >> 152) & 0xFFFF,
        "siloed_borrowing": False,
        "isolation_mode_debt_ceiling": 0,
        "legacy_isolation_fields_removed": True,
    }


def _configuration_base_from_data_provider(result: str) -> dict[str, Any]:
    values = words(result, 10)
    if len(values) != 10:
        raise ExportError("ProtocolDataProvider configuration result length is invalid")
    booleans = [word_uint(item) for item in values[5:10]]
    if any(item not in (0, 1) for item in booleans):
        raise ExportError("ProtocolDataProvider returned an invalid boolean")
    return {
        "decimals": word_uint(values[0]),
        "ltv_bps": word_uint(values[1]),
        "liquidation_threshold_bps": word_uint(values[2]),
        "liquidation_bonus_bps": word_uint(values[3]),
        "reserve_factor_bps": word_uint(values[4]),
        "usage_as_collateral_enabled": bool(booleans[0]),
        "borrowing_enabled": bool(booleans[1]),
        "stable_borrowing_enabled": bool(booleans[2]),
        "active": bool(booleans[3]),
        "frozen": bool(booleans[4]),
    }


def _configuration_from_data_provider(results: dict[str, str]) -> dict[str, Any]:
    required = {
        "data_provider_configuration",
        "data_provider_paused",
        "data_provider_liquidation_protocol_fee",
        "data_provider_siloed",
        "data_provider_debt_ceiling",
    }
    if not required.issubset(results):
        raise ExportError("ProtocolDataProvider field-complete result is missing")
    base = _configuration_base_from_data_provider(
        results["data_provider_configuration"]
    )
    paused = word_uint(words(results["data_provider_paused"])[0])
    siloed = word_uint(words(results["data_provider_siloed"])[0])
    if any(item not in (0, 1) for item in [paused, siloed]):
        raise ExportError("ProtocolDataProvider returned an invalid boolean")
    return {
        **base,
        "configuration_bitmap": None,
        "paused": bool(paused),
        "liquidation_protocol_fee_bps": word_uint(
            words(results["data_provider_liquidation_protocol_fee"])[0]
        ),
        "siloed_borrowing": bool(siloed),
        "isolation_mode_debt_ceiling": word_uint(
            words(results["data_provider_debt_ceiling"])[0]
        ),
        "legacy_isolation_fields_removed": True,
    }


def _configuration_equivalent(
    bitmap: dict[str, Any], data_provider: dict[str, Any]
) -> bool:
    fields = (
        "decimals",
        "ltv_bps",
        "liquidation_threshold_bps",
        "liquidation_bonus_bps",
        "reserve_factor_bps",
        "borrowing_enabled",
        "active",
        "frozen",
        "paused",
        "liquidation_protocol_fee_bps",
        "siloed_borrowing",
        "isolation_mode_debt_ceiling",
    )
    return all(bitmap[field] == data_provider[field] for field in fields)


def _configuration_base_equivalent(
    bitmap: dict[str, Any], data_provider: dict[str, Any]
) -> bool:
    fields = (
        "decimals",
        "ltv_bps",
        "liquidation_threshold_bps",
        "liquidation_bonus_bps",
        "reserve_factor_bps",
        "borrowing_enabled",
        "active",
        "frozen",
    )
    return all(bitmap[field] == data_provider[field] for field in fields)


def configuration_matrix(
    providers: list[Any], block: int, assets: list[str]
) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]], str]:
    evidence: list[dict[str, Any]] = []
    raw: dict[str, dict[str, str | None]] = {asset: {} for asset in assets}
    base_probes = (
        ("pool_configuration", POOL, SELECTORS["get_configuration"]),
        (
            "data_provider_configuration",
            DATA_PROVIDER,
            SELECTORS["get_reserve_configuration"],
        ),
    )
    supplemental_probes = (
        ("data_provider_paused", DATA_PROVIDER, DATA_PROVIDER_SELECTORS["get_paused"]),
        (
            "data_provider_liquidation_protocol_fee",
            DATA_PROVIDER,
            DATA_PROVIDER_SELECTORS["get_liquidation_protocol_fee"],
        ),
        (
            "data_provider_siloed",
            DATA_PROVIDER,
            DATA_PROVIDER_SELECTORS["get_siloed_borrowing"],
        ),
        (
            "data_provider_debt_ceiling",
            DATA_PROVIDER,
            DATA_PROVIDER_SELECTORS["get_debt_ceiling"],
        ),
    )
    for asset in assets:
        asset_word = encode_address(asset)
        for semantic, target, selector in base_probes:
            for provider in providers:
                result, item = _probe(
                    provider,
                    target,
                    call_data(selector, asset_word),
                    block,
                    semantic,
                    asset,
                )
                raw[asset][f"{provider.label}:{semantic}"] = result
                evidence.append(item)

    fallback_assets = []
    for asset in assets:
        values = raw[asset]
        primary_direct = values[
            f"production-nownodes-arbitrum:pool_configuration"
        ]
        peer_direct = values[f"production-slot-0:pool_configuration"]
        if primary_direct is None and peer_direct is not None:
            primary_evidence = next(
                item
                for item in evidence
                if item["asset"] == asset
                and item["provider_id"] == "production-nownodes-arbitrum"
                and item["method_semantic_name"] == "pool_configuration"
            )
            if primary_evidence["failure_class"] != "rpc_error:3":
                raise ExportError(f"unreviewed direct Pool failure for {asset}")
            fallback_assets.append(asset)
        elif primary_direct is None or peer_direct is None:
            raise ExportError(f"direct Pool configuration path unavailable for {asset}")
    for asset in fallback_assets:
        asset_word = encode_address(asset)
        for semantic, target, selector in supplemental_probes:
            for provider in providers:
                result, item = _probe(
                    provider,
                    target,
                    call_data(selector, asset_word),
                    block,
                    semantic,
                    asset,
                )
                raw[asset][f"{provider.label}:{semantic}"] = result
                evidence.append(item)

    configurations: dict[str, dict[str, Any]] = {}
    sources = set()
    for asset in assets:
        values = raw[asset]
        primary_direct = values[f"production-nownodes-arbitrum:pool_configuration"]
        peer_direct = values[f"production-slot-0:pool_configuration"]
        primary_base = values[
            f"production-nownodes-arbitrum:data_provider_configuration"
        ]
        peer_base = values[f"production-slot-0:data_provider_configuration"]
        if primary_base is None or peer_base is None or primary_base != peer_base:
            raise ExportError(f"configuration provider disagreement for {asset}")
        data_base = _configuration_base_from_data_provider(primary_base)
        if asset not in fallback_assets:
            assert primary_direct is not None and peer_direct is not None
            if primary_direct != peer_direct:
                raise ExportError(f"direct Pool configuration disagreement for {asset}")
            direct = _configuration_from_bitmap(word_uint(words(primary_direct)[0]))
            if not _configuration_base_equivalent(direct, data_base):
                raise ExportError(f"Pool/DataProvider configuration disagreement for {asset}")
            configuration = {
                **direct,
                "usage_as_collateral_enabled": data_base[
                    "usage_as_collateral_enabled"
                ],
                "stable_borrowing_enabled": data_base["stable_borrowing_enabled"],
            }
            source = "pool_configuration_bitmap"
        else:
            assert peer_direct is not None
            data_results: list[dict[str, str]] = []
            for provider in providers:
                provider_values = {
                    "data_provider_configuration": primary_base
                    if provider is providers[0]
                    else peer_base
                }
                for semantic, _target, _selector in supplemental_probes:
                    value = values[f"{provider.label}:{semantic}"]
                    if value is None:
                        raise ExportError(
                            f"field-complete configuration unavailable for {asset}"
                        )
                    provider_values[semantic] = value
                data_results.append(provider_values)
            if data_results[0] != data_results[1]:
                raise ExportError(f"configuration provider disagreement for {asset}")
            data_configuration = _configuration_from_data_provider(data_results[0])
            peer = _configuration_from_bitmap(word_uint(words(peer_direct)[0]))
            if not _configuration_equivalent(peer, data_configuration):
                raise ExportError(f"fallback semantic disagreement for {asset}")
            configuration = data_configuration
            source = "protocol_data_provider_field_set"
        if (
            configuration.get("liquidation_protocol_fee_bps") is None
            or configuration.get("paused") is None
            or configuration.get("active") is None
            or configuration.get("frozen") is None
        ):
            raise ExportError(f"field-complete configuration missing for {asset}")
        configuration["configuration_source"] = source
        configurations[asset] = configuration
        sources.add(source)
    selected = sources.pop() if len(sources) == 1 else "per_reserve_explicit"
    return evidence, configurations, selected


def _agreed_eth_call(
    providers: list[Any], target: str, data: str, block: int, semantic: str
) -> str:
    results = [
        _eth_call(provider, target, data, block, semantic) for provider in providers
    ]
    if results[0] != results[1]:
        raise ExportError(f"independent provider disagreement: {semantic}")
    return results[0]


def _code_binding(providers: list[Any], address: str, block: int, semantic: str) -> str:
    hashes = []
    for provider in providers:
        code = _call(provider, "eth_getCode", [address, hex(block)], semantic)
        if not isinstance(code, str) or code == "0x":
            raise ExportError(f"{semantic} code is missing")
        hashes.append(_result_hash(code.lower()))
    if hashes[0] != hashes[1]:
        raise ExportError(f"independent provider disagreement: {semantic} code")
    return hashes[0]


def _relevant_reserves(
    providers: list[Any], block: int, liquidatable_rows: list[dict[str, Any]], maximum: int
) -> tuple[dict[str, int], dict[str, int]]:
    configurations: dict[str, int] = {}
    reserve_ids: set[int] = set()
    for row in liquidatable_rows:
        borrower = row["borrower"]
        result = _agreed_eth_call(
            providers,
            POOL,
            call_data(SELECTORS["get_user_configuration"], encode_address(borrower)),
            block,
            "candidate_user_configuration",
        )
        bitmap = word_uint(words(result)[0])
        configurations[borrower] = bitmap
        for reserve_id in range(MAX_RESERVE_ID + 1):
            if ((bitmap >> (reserve_id * 2)) & 3) != 0:
                reserve_ids.add(reserve_id)
    if not reserve_ids or len(reserve_ids) > maximum:
        raise ExportError("candidate relevant-reserve bound is invalid or exceeded")
    assets: dict[str, int] = {}
    for reserve_id in sorted(reserve_ids):
        result = _agreed_eth_call(
            providers,
            POOL,
            call_data(
                SELECTORS["get_reserve_address_by_id"], uint_word(reserve_id)
            ),
            block,
            "candidate_reserve_id_mapping",
        )
        asset = word_address(words(result)[0])
        if asset in assets:
            raise ExportError("candidate reserve ID mapping is duplicated")
        assets[asset] = reserve_id
    return assets, configurations


def _oracle_provider_record(
    provider: Any, asset: str, block: int
) -> dict[str, Any]:
    asset_word = encode_address(asset)
    source_result, source_call = _capability_probe(
        provider,
        "eth_call",
        [
            {
                "to": ORACLE,
                "data": call_data(SELECTORS["get_source_of_asset"], asset_word),
            },
            hex(block),
        ],
        "get_source_of_asset",
        asset,
        None,
    )
    source = None
    if source_result is not None:
        try:
            source = word_address(words(source_result)[0])
            source_call["source"] = source
        except ExportError:
            source_call["success"] = False
            source_call["failure_class"] = "invalid_source_address"
    target = source or "0x" + "0" * 40
    probes = {
        "source_code": _capability_probe(
            provider,
            "eth_getCode",
            [target, hex(block)],
            "source_code",
            asset,
            source,
        ),
        "get_asset_price": _capability_probe(
            provider,
            "eth_call",
            [
                {
                    "to": ORACLE,
                    "data": call_data(SELECTORS["get_asset_price"], asset_word),
                },
                hex(block),
            ],
            "get_asset_price",
            asset,
            source,
        ),
        "latest_answer": _capability_probe(
            provider,
            "eth_call",
            [
                {
                    "to": target,
                    "data": call_data(ORACLE_SELECTORS["latest_answer"]),
                },
                hex(block),
            ],
            "latest_answer",
            asset,
            source,
        ),
        "latest_round_data": _capability_probe(
            provider,
            "eth_call",
            [
                {
                    "to": target,
                    "data": call_data(SELECTORS["latest_round_data"]),
                },
                hex(block),
            ],
            "latest_round_data",
            asset,
            source,
        ),
        "decimals": _capability_probe(
            provider,
            "eth_call",
            [
                {"to": target, "data": call_data(SELECTORS["decimals"])},
                hex(block),
            ],
            "decimals",
            asset,
            source,
        ),
    }
    record: dict[str, Any] = {
        "asset": asset,
        "source": source,
        "provider_id": provider.label,
        "provider_reference_sha256": provider.provider_reference_sha256,
        "calls": {
            "get_source_of_asset": source_call,
            **{name: evidence for name, (_result, evidence) in probes.items()},
        },
    }
    code_result = probes["source_code"][0]
    if code_result is not None:
        record["source_code_sha256"] = _result_hash(code_result)
        record["source_code_length_bytes"] = (len(code_result) - 2) // 2
    price_result = probes["get_asset_price"][0]
    answer_result = probes["latest_answer"][0]
    decimals_result = probes["decimals"][0]
    if price_result is not None:
        try:
            record["aave_oracle_price"] = word_uint(words(price_result)[0])
        except ExportError:
            record["calls"]["get_asset_price"]["success"] = False
            record["calls"]["get_asset_price"]["failure_class"] = (
                "invalid_integer_result"
            )
    if answer_result is not None:
        try:
            record["source_latest_answer"] = word_int(words(answer_result)[0])
        except ExportError:
            record["calls"]["latest_answer"]["success"] = False
            record["calls"]["latest_answer"]["failure_class"] = (
                "invalid_integer_result"
            )
    if decimals_result is not None:
        try:
            record["source_decimals"] = word_uint(words(decimals_result)[0])
        except ExportError:
            record["calls"]["decimals"]["success"] = False
            record["calls"]["decimals"]["failure_class"] = (
                "invalid_integer_result"
            )
    round_result = probes["latest_round_data"][0]
    if round_result is None:
        record["round_metadata"] = {
            "supported": False,
            "failure_class": probes["latest_round_data"][1]["failure_class"],
        }
    else:
        try:
            values = words(round_result, 5)
            if len(values) != 5:
                raise ExportError("oracle round result length is invalid")
            record["round_metadata"] = {
                "supported": True,
                "round_id": word_uint(values[0]),
                "answer": word_int(values[1]),
                "started_at": word_uint(values[2]),
                "updated_at": word_uint(values[3]),
                "answered_in_round": word_uint(values[4]),
                "response_sha256": _result_hash(round_result),
            }
        except ExportError:
            record["round_metadata"] = {
                "supported": False,
                "failure_class": "invalid_round_result",
            }
    return record


def _oracle_failure(message: str, records: list[dict[str, Any]]) -> None:
    raise CandidateEvidenceError(
        message,
        {"oracle_provider_evidence": records},
        stage="candidate_oracle_semantics",
        method="eth_call",
    )


def _select_oracle_policy(
    asset: str, records: list[dict[str, Any]]
) -> dict[str, Any]:
    if len(records) != 2:
        _oracle_failure("candidate oracle requires exactly two Providers", records)
    required_calls = (
        "get_source_of_asset",
        "source_code",
        "get_asset_price",
        "latest_answer",
    )
    if any(
        not record["calls"][method]["success"]
        for record in records
        for method in required_calls
    ):
        _oracle_failure("candidate oracle required capability failed", records)
    sources = {record.get("source") for record in records}
    code_hashes = {record.get("source_code_sha256") for record in records}
    prices = {record.get("aave_oracle_price") for record in records}
    answers = {record.get("source_latest_answer") for record in records}
    if sources == {None} or "0x" + "0" * 40 in sources:
        _oracle_failure("candidate oracle hidden fallback source rejected", records)
    if len(sources) != 1:
        _oracle_failure("candidate oracle source disagreement", records)
    if len(code_hashes) != 1 or None in code_hashes:
        _oracle_failure("candidate oracle source code disagreement", records)
    if any(record.get("source_code_length_bytes", 0) <= 0 for record in records):
        _oracle_failure("candidate oracle source code is missing", records)
    if len(prices) != 1:
        _oracle_failure("candidate AaveOracle price disagreement", records)
    if len(answers) != 1:
        _oracle_failure("candidate oracle latestAnswer disagreement", records)
    price = next(iter(prices))
    answer = next(iter(answers))
    if not isinstance(price, int) or not isinstance(answer, int) or price <= 0 or answer <= 0:
        _oracle_failure("candidate oracle price is nonpositive", records)
    if answer != price:
        _oracle_failure("candidate latestAnswer/AaveOracle price mismatch", records)

    rounds = [record["round_metadata"] for record in records]
    successful_rounds = [item for item in rounds if item.get("supported") is True]
    for item in successful_rounds:
        if (
            item["round_id"] == 0
            or item["answer"] <= 0
            or item["updated_at"] == 0
            or item["answered_in_round"] < item["round_id"]
            or item["answer"] != answer
        ):
            _oracle_failure("candidate oracle round metadata is invalid", records)
    if len(successful_rounds) == 2:
        comparable = [
            {
                key: item[key]
                for key in (
                    "round_id",
                    "answer",
                    "started_at",
                    "updated_at",
                    "answered_in_round",
                )
            }
            for item in successful_rounds
        ]
        if comparable[0] != comparable[1]:
            _oracle_failure("candidate oracle round metadata disagreement", records)
        semantics = "aggregator_v3_round_data"
        round_metadata_supported = True
        round_metadata = comparable[0]
    else:
        failures = [
            item.get("failure_class")
            for item in rounds
            if item.get("supported") is not True
        ]
        if any(failure not in ROUND_CAPABILITY_FAILURES for failure in failures):
            _oracle_failure("candidate oracle unexpected Provider error", records)
        semantics = "aave_v3_latest_answer"
        round_metadata_supported = False
        round_metadata = None
    return {
        "asset": asset,
        "source": next(iter(sources)),
        "source_code_sha256": next(iter(code_hashes)),
        "aave_oracle_price": price,
        "source_latest_answer": answer,
        "oracle_semantics": semantics,
        "round_metadata_supported": round_metadata_supported,
        "round_metadata": round_metadata,
        "fallback_path_active": False,
        "fallback_path_proof": (
            "positive_source_latest_answer_equals_aave_oracle_price"
        ),
        "provider_evidence": records,
    }


def _oracle_state(providers: list[Any], block: int, asset: str) -> dict[str, Any]:
    return _select_oracle_policy(
        asset,
        [_oracle_provider_record(provider, asset, block) for provider in providers],
    )


def _reserve_state(
    providers: list[Any],
    block: int,
    asset_ids: dict[str, int],
    configurations: dict[str, dict[str, Any]],
) -> list[dict[str, Any]]:
    reserves = []
    for asset, reserve_id in sorted(asset_ids.items(), key=lambda item: item[1]):
        asset_word = encode_address(asset)
        token_words = words(
            _agreed_eth_call(
                providers,
                DATA_PROVIDER,
                call_data(SELECTORS["get_reserve_tokens"], asset_word),
                block,
                "candidate_reserve_tokens",
            ),
            3,
        )
        if len(token_words) != 3:
            raise ExportError("candidate reserve token result is invalid")
        oracle = _oracle_state(providers, block, asset)
        configuration = configurations[asset]
        reserves.append(
            {
                "reserve_id": reserve_id,
                "asset": asset,
                **configuration,
                "atoken": word_address(token_words[0]),
                "stable_debt_token": word_address(token_words[1]),
                "variable_debt_token": word_address(token_words[2]),
                "liquidity_index_ray": word_uint(
                    words(
                        _agreed_eth_call(
                            providers,
                            POOL,
                            call_data(SELECTORS["get_normalized_income"], asset_word),
                            block,
                            "candidate_liquidity_index",
                        )
                    )[0]
                ),
                "variable_borrow_index_ray": word_uint(
                    words(
                        _agreed_eth_call(
                            providers,
                            POOL,
                            call_data(
                                SELECTORS["get_normalized_variable_debt"], asset_word
                            ),
                            block,
                            "candidate_variable_borrow_index",
                        )
                    )[0]
                ),
                "liquidation_grace_period_until": word_uint(
                    words(
                        _agreed_eth_call(
                            providers,
                            POOL,
                            call_data(
                                SELECTORS["get_liquidation_grace_period"], asset_word
                            ),
                            block,
                            "candidate_liquidation_grace_period",
                        )
                    )[0]
                ),
                "price_feed": oracle["source"],
                "oracle_source_code_sha256": oracle["source_code_sha256"],
                "oracle_semantics": oracle["oracle_semantics"],
                "round_metadata_supported": oracle["round_metadata_supported"],
                "round_metadata": oracle["round_metadata"],
                "fallback_path_active": oracle["fallback_path_active"],
                "fallback_path_proof": oracle["fallback_path_proof"],
                "oracle_provider_evidence": oracle["provider_evidence"],
                "price_base_units": oracle["aave_oracle_price"],
                "price_base_decimals": 8,
                "price_feed_answer": oracle["source_latest_answer"],
            }
        )
    return reserves


def _emode_state(
    providers: list[Any], block: int, categories: set[int], reserves: list[dict[str, Any]]
) -> dict[int, dict[str, Any]]:
    output: dict[int, dict[str, Any]] = {}
    by_id = {int(reserve["reserve_id"]): reserve["asset"] for reserve in reserves}
    for category in sorted(categories):
        if category == 0:
            continue
        word = uint_word(category)
        collateral = words(
            _agreed_eth_call(
                providers,
                POOL,
                call_data(SELECTORS["get_emode_collateral_config"], word),
                block,
                "candidate_emode_collateral",
            ),
            3,
        )
        collateral_bitmap = word_uint(
            words(
                _agreed_eth_call(
                    providers,
                    POOL,
                    call_data(SELECTORS["get_emode_collateral_bitmap"], word),
                    block,
                    "candidate_emode_collateral_bitmap",
                )
            )[0]
        )
        borrowable_bitmap = word_uint(
            words(
                _agreed_eth_call(
                    providers,
                    POOL,
                    call_data(SELECTORS["get_emode_borrowable_bitmap"], word),
                    block,
                    "candidate_emode_borrowable_bitmap",
                )
            )[0]
        )
        output[category] = {
            "category_id": category,
            "ltv_bps": word_uint(collateral[0]),
            "liquidation_threshold_bps": word_uint(collateral[1]),
            "liquidation_bonus_bps": word_uint(collateral[2]),
            "collateral_bitmap": collateral_bitmap,
            "borrowable_bitmap": borrowable_bitmap,
            "collateral_assets": sorted(
                asset for reserve_id, asset in by_id.items() if (collateral_bitmap >> reserve_id) & 1
            ),
        }
    return output


def _require_protocol_derived_agreement(
    protocol: dict[str, int] | None, derived: dict[str, int]
) -> None:
    if (
        protocol is None
        or derived["total_collateral_base"] != protocol["total_collateral_base"]
        or derived["total_debt_base"] != protocol["total_debt_base"]
        or derived["health_factor_wad"] != protocol["health_factor_wad"]
    ):
        raise ExportError("candidate protocol/derived Health Factor disagreement")


def _borrowers(
    providers: list[Any],
    block: int,
    rows: list[dict[str, Any]],
    reserves: list[dict[str, Any]],
    user_configurations: dict[str, int],
) -> tuple[list[dict[str, Any]], dict[int, dict[str, Any]]]:
    categories: dict[str, int] = {}
    for row in rows:
        borrower = row["borrower"]
        categories[borrower] = word_uint(
            words(
                _agreed_eth_call(
                    providers,
                    POOL,
                    call_data(SELECTORS["get_user_emode"], encode_address(borrower)),
                    block,
                    "candidate_user_emode",
                )
            )[0]
        )
    emodes = _emode_state(providers, block, set(categories.values()), reserves)
    reserve_map = {reserve["asset"]: reserve for reserve in reserves}
    output = []
    for row in rows:
        borrower = row["borrower"]
        borrower_word = encode_address(borrower)
        positions = []
        configuration = user_configurations[borrower]
        for reserve in reserves:
            reserve_id = int(reserve["reserve_id"])
            if ((configuration >> (reserve_id * 2)) & 3) == 0:
                continue
            values = words(
                _agreed_eth_call(
                    providers,
                    DATA_PROVIDER,
                    call_data(
                        SELECTORS["get_user_reserve_data"],
                        encode_address(reserve["asset"]),
                        borrower_word,
                    ),
                    block,
                    "candidate_user_reserve_data",
                ),
                9,
            )
            if len(values) != 9:
                raise ExportError("candidate user reserve result is invalid")
            position = {
                "asset": reserve["asset"],
                "current_supply": word_uint(values[0]),
                "scaled_supply": None,
                "current_stable_debt": word_uint(values[1]),
                "current_variable_debt": word_uint(values[2]),
                "principal_stable_debt": word_uint(values[3]),
                "scaled_variable_debt": word_uint(values[4]),
                "usage_as_collateral_enabled": bool(word_uint(values[8])),
                "scaled_supply_required_for_hf_derivation": False,
            }
            if word_uint(values[8]) not in (0, 1):
                raise ExportError("candidate collateral flag is invalid")
            positions.append(position)
        borrower_record = {
            "address": borrower,
            "account_configuration_bitmap": configuration,
            "emode_category": categories[borrower],
            "protocol_account_data": row["agreed_account_data"],
            "positions": positions,
        }
        derived = calculate_account(
            {
                "emode_category": categories[borrower],
                "positions": [
                    {
                        "asset": item["asset"],
                        "supplied": item["current_supply"],
                        "variable_debt": item["current_variable_debt"],
                        "stable_debt": item["current_stable_debt"],
                        "collateral_enabled": item["usage_as_collateral_enabled"],
                    }
                    for item in positions
                ],
            },
            reserve_map,
            emodes,
            {asset: reserve["price_base_units"] for asset, reserve in reserve_map.items()},
        )
        protocol = row["agreed_account_data"]
        _require_protocol_derived_agreement(protocol, derived)
        borrower_record["derived_account_data"] = derived
        output.append(borrower_record)
    return output, emodes


def _eligible_pairs(
    borrowers: list[dict[str, Any]], reserves: list[dict[str, Any]], timestamp: int
) -> list[dict[str, Any]]:
    reserve_map = {reserve["asset"]: reserve for reserve in reserves}
    candidates = []
    for borrower in borrowers:
        debts = [
            position
            for position in borrower["positions"]
            if position["current_variable_debt"] or position["current_stable_debt"]
        ]
        collaterals = [
            position
            for position in borrower["positions"]
            if position["current_supply"] and position["usage_as_collateral_enabled"]
        ]
        pairs = []
        for debt in debts:
            debt_reserve = reserve_map[debt["asset"]]
            for collateral in collaterals:
                collateral_reserve = reserve_map[collateral["asset"]]
                if (
                    not debt_reserve["active"]
                    or debt_reserve["paused"]
                    or not collateral_reserve["active"]
                    or collateral_reserve["paused"]
                    or collateral_reserve["liquidation_threshold_bps"] == 0
                    or collateral_reserve["liquidation_grace_period_until"] > timestamp
                ):
                    continue
                pairs.append(
                    {
                        "debt_asset": debt["asset"],
                        "collateral_asset": collateral["asset"],
                    }
                )
        if pairs:
            candidates.append(
                {
                    "borrower": borrower["address"],
                    "health_factor_wad": borrower["protocol_account_data"]["health_factor_wad"],
                    "valid_pair_count": len(pairs),
                    "pairs": pairs,
                    "candidate_authority": True,
                    "execution_authority": False,
                }
            )
    return candidates


def _artifact_path(output_dir: Path, prefix: str, artifact: dict[str, Any]) -> Path:
    return output_dir / f"{prefix}-{artifact['content_sha256']}.json"


def run(args: argparse.Namespace) -> dict[str, Any]:
    cohort = load_cohort(args.cohort)
    providers: list[Any] = []
    started_ns = time.monotonic_ns()
    try:
        providers = [
            SSHContainerProvider(
                "production-nownodes-arbitrum",
                args.ssh_executable,
                args.ssh_provider_host,
                args.ssh_provider_port,
                args.ssh_provider_identity,
                args.ssh_provider_known_hosts,
                args.ssh_provider_container,
                0,
                authenticated=True,
            ),
            SSHContainerProvider(
                "production-slot-0",
                args.ssh_executable,
                args.ssh_provider_host,
                args.ssh_provider_port,
                args.ssh_provider_identity,
                args.ssh_provider_known_hosts,
                args.ssh_provider_container,
                0,
            ),
        ]
        if len({provider.provider_reference_sha256 for provider in providers}) != 2:
            raise ExportError("candidate provider independence is absent")
        refresh = refresh_signals(providers, cohort, started_ns)
        classifications = {
            row["classification"] for row in refresh["rows"]
        }
        base = {
            "schema": SCHEMA,
            "chain_id": CHAIN_ID,
            "pool": POOL,
            "source_cohort_content_sha256": cohort["content_sha256"],
            "source_discovery_content_sha256": cohort[
                "source_discovery_content_sha256"
            ],
            "source_prefilter_content_sha256": cohort[
                "source_prefilter_content_sha256"
            ],
            "checkpoint_block": refresh["checkpoint_block"],
            "checkpoint_hash": refresh["checkpoint_hash"],
            "checkpoint_state_root": refresh["checkpoint_state_root"],
            "checkpoint_timestamp": refresh["checkpoint_timestamp"],
            "provider_headers": [
                {
                    "provider_id": item["provider_id"],
                    "provider_reference_sha256": item[
                        "provider_reference_sha256"
                    ],
                    "authenticated": item["authenticated"],
                    "header_name": item["header_name"],
                    "checkpoint": item["checkpoint"],
                }
                for item in refresh["provider_headers"]
            ],
            "pool_code_sha256": refresh["pool_code_sha256"],
            "signal_rows": refresh["rows"],
            "individual_json_rpc_calls_only": True,
            "automatic_retries": 0,
            "raw_rpc_responses_persisted": False,
            "source_bindings": {
                "aave_address_book_commit": AAVE_ADDRESS_BOOK_COMMIT,
                "aave_v3_origin_commit": AAVE_V3_ORIGIN_COMMIT,
                "aave_protocol_data_provider_path": "src/contracts/helpers/AaveProtocolDataProvider.sol",
                "reserve_configuration_path": "src/contracts/protocol/libraries/configuration/ReserveConfiguration.sol",
            },
            "execution_authority": False,
        }
        if "provider_disagreement" in classifications or "incomplete" in classifications:
            artifact = bind_hash(
                {
                    **base,
                    "status": "blocked",
                    "terminal": "LIVE_PLATFORM_BLOCKER",
                    "relevant_reserve_count": 0,
                    "candidate_count": 0,
                    "candidate_authority": False,
                    "provider_request_usage": _sanitized_request_usage(providers),
                }
            )
            write_private_json(_artifact_path(args.output_dir, "blocked", artifact), artifact)
            return artifact
        liquidatable_rows = [
            row
            for row in refresh["rows"]
            if row["classification"] == "exact_liquidatable_signal"
        ]
        if not liquidatable_rows:
            artifact = bind_hash(
                {
                    **base,
                    "status": "stale_signals",
                    "terminal": None,
                    "relevant_reserve_count": 0,
                    "candidate_count": 0,
                    "candidate_authority": False,
                    "reserve_reconstruction_skipped": True,
                    "economics_skipped": True,
                    "fork_skipped": True,
                    "provider_request_usage": _sanitized_request_usage(providers),
                    "runtime_ms": (time.monotonic_ns() - started_ns) // 1_000_000,
                }
            )
            write_private_json(_artifact_path(args.output_dir, "stale", artifact), artifact)
            return artifact

        asset_ids, user_configurations = _relevant_reserves(
            providers,
            refresh["checkpoint_block"],
            liquidatable_rows,
            args.max_relevant_reserves,
        )
        matrix_evidence, configurations, selected_source = configuration_matrix(
            providers, refresh["checkpoint_block"], list(asset_ids)
        )
        matrix = bind_hash(
            {
                "schema": MATRIX_SCHEMA,
                "chain_id": CHAIN_ID,
                "checkpoint_block": refresh["checkpoint_block"],
                "checkpoint_hash": refresh["checkpoint_hash"],
                "relevant_reserve_count": len(asset_ids),
                "selected_configuration_source": selected_source,
                "calls": matrix_evidence,
                "provider_request_usage": _sanitized_request_usage(providers),
                "raw_rpc_responses_persisted": False,
                "candidate_authority": False,
                "execution_authority": False,
            }
        )
        write_private_json(_artifact_path(args.output_dir, "matrix", matrix), matrix)
        implementation_code_hash = _code_binding(
            providers,
            POOL_IMPLEMENTATION,
            refresh["checkpoint_block"],
            "candidate_pool_implementation",
        )
        data_provider_code_hash = _code_binding(
            providers,
            DATA_PROVIDER,
            refresh["checkpoint_block"],
            "candidate_data_provider",
        )
        oracle_code_hash = _code_binding(
            providers,
            ORACLE,
            refresh["checkpoint_block"],
            "candidate_oracle",
        )
        proof_policy = current_state_proof_policy(
            providers,
            refresh["provider_headers"],
            refresh["checkpoint_block"],
            refresh["implementation_words"],
        )
        try:
            reserves = _reserve_state(
                providers,
                refresh["checkpoint_block"],
                asset_ids,
                configurations,
            )
            borrowers, emodes = _borrowers(
                providers,
                refresh["checkpoint_block"],
                liquidatable_rows,
                reserves,
                user_configurations,
            )
            candidates = _eligible_pairs(
                borrowers, reserves, refresh["checkpoint_timestamp"]
            )
        except Exception as error:
            partial = {
                **base,
                "status": "failed_closed",
                "terminal": "LIVE_PLATFORM_BLOCKER",
                "relevant_reserve_count": len(asset_ids),
                "relevant_reserves": [
                    {"asset": asset, "reserve_id": reserve_id}
                    for asset, reserve_id in sorted(
                        asset_ids.items(), key=lambda item: item[1]
                    )
                ],
                "configuration_matrix_content_sha256": matrix["content_sha256"],
                "selected_reserve_configuration_source": selected_source,
                "candidate_count": 0,
                "candidate_authority": False,
                "provider_request_usage": _sanitized_request_usage(providers),
            }
            stage = "candidate_exact_validation"
            method = None
            if isinstance(error, CandidateEvidenceError):
                partial.update(error.evidence)
                stage = error.stage or stage
                method = error.method
            elif isinstance(error, ProviderDiagnosticError):
                partial["provider_failure"] = error.sanitized_evidence()
                stage = error.stage
                method = error.method
            raise CandidateEvidenceError(
                "candidate exact validation failed closed",
                partial,
                stage=stage,
                method=method,
            ) from error
        usage = _sanitized_request_usage(providers)
        runtime_ms = (time.monotonic_ns() - started_ns) // 1_000_000
        if (
            runtime_ms > args.max_runtime_seconds * 1_000
            or any(
                item["json_rpc_item_count"] > args.max_items_per_provider
                or item["retry_count"] != 0
                for item in usage
            )
        ):
            raise ExportError("candidate exact validation exceeded a hard bound")
        artifact = bind_hash(
            {
                **base,
                "status": "exact_candidates" if candidates else "no_valid_pair",
                "terminal": None,
                "proof_policy": proof_policy,
                "protocol_code_bindings": {
                    "pool_implementation_sha256": implementation_code_hash,
                    "data_provider_sha256": data_provider_code_hash,
                    "oracle_sha256": oracle_code_hash,
                },
                "configuration_matrix_content_sha256": matrix["content_sha256"],
                "selected_reserve_configuration_source": selected_source,
                "relevant_reserve_count": len(reserves),
                "reserves": reserves,
                "emode_categories": [emodes[key] for key in sorted(emodes)],
                "borrowers": borrowers,
                "candidate_count": len(candidates),
                "candidates": candidates,
                "candidate_authority": bool(candidates),
                "provider_request_usage": usage,
                "runtime_ms": runtime_ms,
            }
        )
        write_private_json(_artifact_path(args.output_dir, "exact", artifact), artifact)
        return artifact
    except Exception as error:
        if (
            isinstance(error, CandidateEvidenceError)
            and "signal_rows" in error.evidence
        ):
            raise
        if "base" not in locals():
            raise
        observed_assets = locals().get("asset_ids", {})
        partial = {
            **base,
            "status": "failed_closed",
            "terminal": "LIVE_PLATFORM_BLOCKER",
            "relevant_reserve_count": len(observed_assets),
            "relevant_reserves": [
                {"asset": asset, "reserve_id": reserve_id}
                for asset, reserve_id in sorted(
                    observed_assets.items(), key=lambda item: item[1]
                )
            ],
            "candidate_count": 0,
            "candidate_authority": False,
            "provider_request_usage": _sanitized_request_usage(providers),
        }
        observed_matrix = locals().get("matrix")
        if isinstance(observed_matrix, dict):
            partial["configuration_matrix_content_sha256"] = observed_matrix.get(
                "content_sha256"
            )
        if "selected_source" in locals():
            partial["selected_reserve_configuration_source"] = selected_source
        stage = "candidate_exact_validation"
        method = None
        if isinstance(error, CandidateEvidenceError):
            partial.update(error.evidence)
            stage = error.stage or stage
            method = error.method
        elif isinstance(error, ProviderDiagnosticError):
            partial["provider_failure"] = error.sanitized_evidence()
            stage = error.stage
            method = error.method
        raise CandidateEvidenceError(
            "candidate exact validation failed closed",
            partial,
            stage=stage,
            method=method,
        ) from error
    finally:
        for provider in providers:
            provider.close()


def run_oracle_capability(args: argparse.Namespace) -> dict[str, Any]:
    providers: list[Any] = []
    started_ns = time.monotonic_ns()
    try:
        providers = [
            SSHContainerProvider(
                "production-nownodes-arbitrum",
                args.ssh_executable,
                args.ssh_provider_host,
                args.ssh_provider_port,
                args.ssh_provider_identity,
                args.ssh_provider_known_hosts,
                args.ssh_provider_container,
                0,
                authenticated=True,
            ),
            SSHContainerProvider(
                "production-slot-0",
                args.ssh_executable,
                args.ssh_provider_host,
                args.ssh_provider_port,
                args.ssh_provider_identity,
                args.ssh_provider_known_hosts,
                args.ssh_provider_container,
                0,
            ),
        ]
        if len({provider.provider_reference_sha256 for provider in providers}) != 2:
            raise ExportError("oracle capability provider independence is absent")
        artifact = oracle_capability_matrix(providers, started_ns)
        write_private_json(
            _artifact_path(args.output_dir, "oracle-capability", artifact), artifact
        )
        return artifact
    finally:
        for provider in providers:
            provider.close()


def failure_artifact(error: Exception) -> dict[str, Any]:
    evidence = error.evidence if isinstance(error, CandidateEvidenceError) else {}
    artifact: dict[str, Any] = {
        **evidence,
        "schema": "phoenix.atlas.aave-candidate-exact-validation-error.v1",
        "status": "failed_closed",
        "terminal": "LIVE_PLATFORM_BLOCKER",
        "error_class": type(error).__name__,
        "candidate_authority": False,
        "execution_authority": False,
    }
    if isinstance(error, CandidateEvidenceError):
        artifact["failure_class"] = "candidate_exact_invariant_failed"
        artifact["stage"] = error.stage
        artifact["method"] = error.method
    elif isinstance(error, ProviderDiagnosticError):
        artifact.update(error.sanitized_evidence())
    else:
        artifact["failure_class"] = "candidate_exact_invariant_failed"
    return bind_hash(artifact)


def _summary(artifact: dict[str, Any]) -> dict[str, Any]:
    oracle_evidence = [
        {
            "asset": reserve.get("asset"),
            "source": reserve.get("price_feed"),
            "oracle_semantics": reserve.get("oracle_semantics"),
            "round_metadata_supported": reserve.get(
                "round_metadata_supported"
            ),
        }
        for reserve in artifact.get("reserves", [])
    ]
    if not oracle_evidence:
        seen: set[tuple[Any, Any]] = set()
        for record in artifact.get("oracle_provider_evidence", []):
            key = (record.get("asset"), record.get("source"))
            if key in seen:
                continue
            seen.add(key)
            oracle_evidence.append(
                {
                    "asset": key[0],
                    "source": key[1],
                    "oracle_semantics": None,
                    "round_metadata_supported": None,
                }
            )
    return {
        "schema": artifact["schema"],
        "status": artifact.get("status"),
        "terminal": artifact.get("terminal"),
        "content_sha256": artifact.get("content_sha256"),
        "checkpoint_block": artifact.get("checkpoint_block"),
        "checkpoint_hash": artifact.get("checkpoint_hash"),
        "hf_classifications": [
            {
                "borrower": row.get("borrower"),
                "classification": row.get("classification"),
                "health_factor_wad": (row.get("agreed_account_data") or {}).get(
                    "health_factor_wad"
                ),
            }
            for row in artifact.get("signal_rows", [])
        ],
        "relevant_reserve_count": artifact.get("relevant_reserve_count", 0),
        "selected_reserve_configuration_source": artifact.get(
            "selected_reserve_configuration_source"
        ),
        "oracle_evidence": oracle_evidence,
        "candidate_count": artifact.get("candidate_count", 0),
        "candidate_authority": artifact.get("candidate_authority", False),
        "execution_authority": False,
        "provider_request_usage": artifact.get("provider_request_usage", []),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cohort", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--ssh-executable", default="ssh")
    parser.add_argument("--ssh-provider-host", required=True)
    parser.add_argument("--ssh-provider-port", type=int, default=22)
    parser.add_argument("--ssh-provider-identity", required=True, type=Path)
    parser.add_argument("--ssh-provider-known-hosts", type=Path)
    parser.add_argument("--ssh-provider-container", required=True)
    parser.add_argument(
        "--max-relevant-reserves", type=int, default=MAX_RELEVANT_RESERVES
    )
    parser.add_argument("--max-runtime-seconds", type=int, default=MAX_RUNTIME_SECONDS)
    parser.add_argument(
        "--max-items-per-provider", type=int, default=MAX_ITEMS_PER_PROVIDER
    )
    parser.add_argument("--oracle-capability-only", action="store_true")
    args = parser.parse_args()
    if (
        not 1 <= args.max_relevant_reserves <= MAX_RELEVANT_RESERVES
        or not 1 <= args.max_runtime_seconds <= MAX_RUNTIME_SECONDS
        or not 1 <= args.max_items_per_provider <= MAX_ITEMS_PER_PROVIDER
    ):
        print(json.dumps(_summary(failure_artifact(ExportError("invalid bound"))), sort_keys=True))
        return 1
    try:
        artifact = run_oracle_capability(args) if args.oracle_capability_only else run(args)
    except Exception as error:
        artifact = failure_artifact(error)
        write_private_json(_artifact_path(args.output_dir, "error", artifact), artifact)
        print(json.dumps(_summary(artifact), sort_keys=True, separators=(",", ":")))
        return 1
    print(json.dumps(_summary(artifact), sort_keys=True, separators=(",", ":")))
    return 0 if artifact.get("status") not in {"blocked", "failed_closed"} else 1


if __name__ == "__main__":
    raise SystemExit(main())
