#!/usr/bin/env python3
"""Export a sanitized, independently agreed Aave V3 checkpoint snapshot.

The exporter consumes the hash-bound Borrow discovery artifact and reads only
canonical view state at its checkpoint block. Provider URLs are obtained from
the reviewed gateway container and are never written to stdout or stderr.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

try:
    from scripts.export_aave_borrow_discovery import SSHContainerProvider
except ModuleNotFoundError:
    from export_aave_borrow_discovery import SSHContainerProvider


CHAIN_ID = 42161
POOL = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
DATA_PROVIDER = "0x243aa95cac2a25651eda86e80bee66114413c43b"
ORACLE = "0xb56c2f0b653b2e0b10c9b928c8580ac5df02c7c7"
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
POOL_IMPLEMENTATION = "0xf05fd3cc911b4c5e36e53c00354f645e22922c9a"
AAVE_ADDRESS_BOOK_COMMIT = "a1770e87fd61db02a7725cd9eed3b1d07c3980af"
AAVE_V3_ORIGIN_COMMIT = "fd1fbd9150426ca8ace9cee45b4acf912ae84f5b"
MAX_PROVIDERS = 8
MAX_BORROWERS = 250_000
MAX_RESERVES = 128
MAX_CHECKPOINT_TAIL_BLOCKS = 2_000_000
MAX_LOG_BLOCK_RANGE = 2_000
BATCH_SIZE = 80
MAX_RPC_ATTEMPTS = 6
BATCH_PACING_SECONDS = 0.20
POOL_IMPLEMENTATION_SLOT = (
    "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc"
)
ARCHIVE_MANIFEST_SCHEMA = "phoenix.atlas.aave-borrow-archive-manifest.v1"
DEFAULT_SECONDARY_PROVIDER_ENV = "PHOENIX_ATLAS_ARCHIVE_SECONDARY_RPC_URL"

SELECTORS = {
    "get_reserves_list": "d1946dbc",
    "get_configuration": "c44b11f7",
    "get_normalized_income": "d15e0053",
    "get_normalized_variable_debt": "386497fd",
    "get_liquidation_grace_period": "5c9a8b18",
    "get_reserve_address_by_id": "52751797",
    "get_reserve_tokens": "d2493b6c",
    "get_reserve_configuration": "3e150141",
    "get_user_configuration": "4417a583",
    "get_user_emode": "eddf1b79",
    "get_user_reserve_data": "28dd2d01",
    "scaled_balance_of": "1da24f3e",
    "get_source_of_asset": "92bf2be0",
    "get_asset_price": "b3596f07",
    "get_emode_collateral_config": "b286f467",
    "get_emode_collateral_bitmap": "b0771dba",
    "get_emode_borrowable_bitmap": "903a2c71",
    "get_emode_label": "2083e183",
    "symbol": "95d89b41",
}


class ExportError(RuntimeError):
    pass


def canonical_hash(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def validate_discovery(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExportError("discovery artifact must be an object")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("discovery content hash mismatch")
    if value.get("schema") != "phoenix.atlas.aave-borrow-discovery.v1":
        raise ExportError("unsupported discovery schema")
    if value.get("chain_id") != CHAIN_ID or str(value.get("pool", "")).lower() != POOL:
        raise ExportError("discovery identity mismatch")
    if value.get("archive_complete") is not True:
        raise ExportError("discovery archive is incomplete")
    borrowers = value.get("borrowers")
    if not isinstance(borrowers, list) or not (0 < len(borrowers) <= MAX_BORROWERS):
        raise ExportError("discovery borrower set is invalid")
    canonical = sorted({_address(item, "borrower") for item in borrowers})
    if canonical != borrowers or len(canonical) != value.get("borrower_count"):
        raise ExportError("discovery borrower set is not canonical")
    return value


def validate_archive_manifest(
    value: Any, discovery: dict[str, Any]
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExportError("archive manifest must be an object")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("archive manifest content hash mismatch")
    if value.get("schema") != ARCHIVE_MANIFEST_SCHEMA:
        raise ExportError("unsupported archive manifest schema")
    if (
        value.get("chain_id") != CHAIN_ID
        or str(value.get("contract_address", "")).lower() != POOL
        or str(value.get("event_topic0", "")).lower() != BORROW_TOPIC
    ):
        raise ExportError("archive manifest identity mismatch")
    if value.get("archive_complete") is not True:
        raise ExportError("archive manifest is incomplete")
    if value.get("independent_validation") is not True:
        raise ExportError("archive manifest lacks independent validation")
    if value.get("coverage_gaps") != []:
        raise ExportError("archive manifest contains coverage gaps")
    if value.get("final_archive_sha256") != discovery["content_sha256"]:
        raise ExportError("archive manifest/discovery hash mismatch")
    return value


def provider_urls(container: str) -> list[str]:
    result = subprocess.run(
        ["sudo", "-n", "docker", "inspect", "--format", "{{json .Config.Env}}", container],
        check=True,
        capture_output=True,
        text=True,
    )
    values: dict[str, str] = {}
    for item in json.loads(result.stdout):
        if "=" in item:
            key, value = item.split("=", 1)
            values[key] = value
    raw = values.get("RPC_PROVIDER_URLS", "")
    urls = json.loads(raw) if raw.startswith("[") else [
        part.strip() for part in raw.split(",") if part.strip()
    ]
    if not isinstance(urls, list) or not (2 <= len(urls) <= MAX_PROVIDERS):
        raise ExportError("independent reviewed provider set is unavailable")
    if len(set(urls)) != len(urls):
        raise ExportError("reviewed provider set contains duplicates")
    if not all(isinstance(url, str) and url.startswith(("http://", "https://")) for url in urls):
        raise ExportError("reviewed provider configuration is invalid")
    return urls


def environment_provider_urls(names: list[str]) -> list[str]:
    if not names or len(names) > MAX_PROVIDERS:
        raise ExportError("protected provider references are unavailable")
    urls = []
    for name in names:
        if not name or not name.replace("_", "").isalnum() or name.upper() != name:
            raise ExportError("provider environment reference is invalid")
        value = os.environ.get(name)
        if not value:
            raise ExportError(f"provider environment reference is unset:{name}")
        urls.append(value)
    if len(set(urls)) != len(urls):
        raise ExportError("reviewed provider set contains duplicates")
    if not all(url.startswith(("http://", "https://")) for url in urls):
        raise ExportError("reviewed provider configuration is invalid")
    return urls


class Provider:
    def __init__(self, label: str, url: str) -> None:
        self.label = label
        self._url = url
        self._request_id = 0

    def _request(self, payload: Any, attempts: int = MAX_RPC_ATTEMPTS) -> Any:
        failure = "unavailable"
        for attempt in range(attempts):
            request = urllib.request.Request(
                self._url,
                data=json.dumps(payload, separators=(",", ":")).encode(),
                headers={
                    "Content-Type": "application/json",
                    "User-Agent": "phoenix-atlas-aave-checkpoint/1",
                },
                method="POST",
            )
            try:
                with urllib.request.urlopen(request, timeout=90) as response:
                    return json.load(response)
            except urllib.error.HTTPError as error:
                failure = f"http_error:{error.code}"
            except Exception:
                failure = "transport_error"
            if attempt + 1 < attempts:
                time.sleep(min(2**attempt, 16))
        raise ExportError(f"{self.label}:{failure}")

    def call(self, method: str, params: list[object]) -> Any:
        if method not in {
            "eth_chainId",
            "eth_getBlockByNumber",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_call",
            "eth_getLogs",
        }:
            raise ExportError("RPC method outside read-only allowlist")
        self._request_id += 1
        response = self._request(
            {"jsonrpc": "2.0", "id": self._request_id, "method": method, "params": params}
        )
        if not isinstance(response, dict):
            raise ExportError(f"{self.label}:{method}:response_invalid")
        if response.get("error") is not None:
            error = response["error"]
            code = error.get("code") if isinstance(error, dict) else None
            suffix = f"rpc_error:{code}" if isinstance(code, int) else "rpc_error"
            raise ExportError(f"{self.label}:{method}:{suffix}")
        if "result" not in response:
            raise ExportError(f"{self.label}:{method}:result_missing")
        return response["result"]

    def eth_calls(self, calls: list[tuple[str, str]], block: int) -> list[str]:
        results: list[str] = []
        batches_completed = 0
        for cursor in range(0, len(calls), BATCH_SIZE):
            batch = calls[cursor : cursor + BATCH_SIZE]
            payload = []
            ids = []
            for target, data in batch:
                self._request_id += 1
                ids.append(self._request_id)
                payload.append(
                    {
                        "jsonrpc": "2.0",
                        "id": self._request_id,
                        "method": "eth_call",
                        "params": [{"to": target, "data": data}, hex(block)],
                    }
                )
            response = self._request(payload)
            if not isinstance(response, list):
                raise ExportError(f"{self.label}:eth_call:batch_invalid")
            mapped = {item.get("id"): item for item in response if isinstance(item, dict)}
            for request_id in ids:
                item = mapped.get(request_id)
                if item is None or item.get("error") is not None or not isinstance(item.get("result"), str):
                    raise ExportError(f"{self.label}:eth_call:batch_item_failed")
                results.append(str(item["result"]).lower())
            batches_completed += 1
            if batches_completed % 50 == 0:
                print(
                    f"{self.label}:eth_call_batches_completed={batches_completed}",
                    file=sys.stderr,
                    flush=True,
                )
            time.sleep(BATCH_PACING_SECONDS)
        return results


def _address(value: Any, name: str) -> str:
    if not isinstance(value, str) or len(value) != 42 or not value.startswith("0x"):
        raise ExportError(f"{name} must be a canonical address")
    try:
        int(value[2:], 16)
    except ValueError as error:
        raise ExportError(f"{name} must be hexadecimal") from error
    return value.lower()


def encode_address(value: str) -> str:
    return "0" * 24 + _address(value, "call address")[2:]


def call_data(selector: str, *words: str) -> str:
    if len(selector) != 8 or any(len(word) != 64 for word in words):
        raise ExportError("invalid ABI call encoding")
    return "0x" + selector + "".join(words)


def uint_word(value: int) -> str:
    if value < 0 or value >= 2**256:
        raise ExportError("ABI integer out of bounds")
    return f"{value:064x}"


def words(value: str, minimum: int = 1) -> list[str]:
    if not isinstance(value, str) or not value.startswith("0x") or (len(value) - 2) % 64:
        raise ExportError("invalid ABI result")
    result = [value[index : index + 64] for index in range(2, len(value), 64)]
    if len(result) < minimum:
        raise ExportError("truncated ABI result")
    return result


def word_uint(value: str) -> int:
    return int(value, 16)


def word_address(value: str) -> str:
    return _address("0x" + value[-40:], "ABI address")


def decode_address_array(value: str) -> list[str]:
    raw = words(value, 2)
    offset = word_uint(raw[0]) // 32
    if offset >= len(raw):
        raise ExportError("address array offset invalid")
    count = word_uint(raw[offset])
    if count > MAX_RESERVES or offset + count >= len(raw):
        raise ExportError("address array length invalid")
    return [word_address(item) for item in raw[offset + 1 : offset + 1 + count]]


def decode_symbol(value: str) -> str:
    raw = words(value)
    if len(raw) == 1:
        data = bytes.fromhex(raw[0]).rstrip(b"\0")
    else:
        offset = word_uint(raw[0]) // 32
        if offset + 1 >= len(raw):
            raise ExportError("symbol offset invalid")
        length = word_uint(raw[offset])
        data = bytes.fromhex("".join(raw[offset + 1 :]))[:length]
    try:
        symbol = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ExportError("reserve symbol is not UTF-8") from error
    if not symbol or len(symbol) > 64:
        raise ExportError("reserve symbol invalid")
    return symbol


def header(provider: Provider, block: int) -> dict[str, Any]:
    value = provider.call("eth_getBlockByNumber", [hex(block), False])
    if not isinstance(value, dict) or int(str(value.get("number")), 16) != block:
        raise ExportError(f"{provider.label}:checkpoint_header_invalid")
    block_hash = str(value.get("hash", "")).lower()
    parent_hash = str(value.get("parentHash", "")).lower()
    if len(block_hash) != 66 or len(parent_hash) != 66:
        raise ExportError(f"{provider.label}:checkpoint_hash_invalid")
    return {"number": block, "hash": block_hash, "parent_hash": parent_hash}


def finalized_checkpoint(
    providers: list[Provider],
) -> tuple[int, list[dict[str, Any]], list[dict[str, Any]]]:
    finalized_heads = []
    for provider in providers:
        value = provider.call("eth_getBlockByNumber", ["finalized", False])
        if not isinstance(value, dict):
            raise ExportError(f"{provider.label}:finalized_header_invalid")
        number = int(str(value.get("number")), 16)
        block_hash = str(value.get("hash", "")).lower()
        if number < 1 or len(block_hash) != 66:
            raise ExportError(f"{provider.label}:finalized_header_invalid")
        finalized_heads.append(
            {"provider_id": provider.label, "number": number, "hash": block_hash}
        )
    selected = min(int(item["number"]) for item in finalized_heads)
    selected_headers = [
        {"provider_id": provider.label, "checkpoint": header(provider, selected)}
        for provider in providers
    ]
    if len({item["checkpoint"]["hash"] for item in selected_headers}) != 1:
        raise ExportError("independent finalized checkpoint hash disagreement")
    return selected, finalized_heads, selected_headers


def sanitized_tail_borrow_logs(
    provider: Provider, start: int, end: int
) -> list[dict[str, Any]]:
    if end < start:
        return []
    if end - start + 1 > MAX_CHECKPOINT_TAIL_BLOCKS:
        raise ExportError("checkpoint Borrow continuity tail is unbounded")
    output = []
    cursor = start
    while cursor <= end:
        chunk_end = min(end, cursor + MAX_LOG_BLOCK_RANGE - 1)
        value = provider.call(
            "eth_getLogs",
            [
                {
                    "address": POOL,
                    "fromBlock": hex(cursor),
                    "toBlock": hex(chunk_end),
                    "topics": [BORROW_TOPIC],
                }
            ],
        )
        if not isinstance(value, list):
            raise ExportError("checkpoint Borrow continuity tail is invalid")
        for log in value:
            if not isinstance(log, dict) or log.get("removed") is True:
                raise ExportError("checkpoint Borrow continuity log is invalid")
            topics = log.get("topics")
            if str(log.get("address", "")).lower() != POOL:
                raise ExportError("checkpoint Borrow continuity source mismatch")
            if not isinstance(topics, list) or len(topics) != 4:
                raise ExportError("checkpoint Borrow continuity topic shape is invalid")
            if str(topics[0]).lower() != BORROW_TOPIC:
                raise ExportError("checkpoint Borrow continuity signature mismatch")
            output.append(
                {
                    "block_number": int(str(log.get("blockNumber")), 16),
                    "block_hash": str(log.get("blockHash", "")).lower(),
                    "transaction_hash": str(log.get("transactionHash", "")).lower(),
                    "transaction_index": int(str(log.get("transactionIndex")), 16),
                    "log_index": int(str(log.get("logIndex")), 16),
                    "reserve": "0x" + str(topics[1]).lower()[-40:],
                    "borrower": "0x" + str(topics[2]).lower()[-40:],
                    "referral_code": int(str(topics[3]), 16),
                    "data_sha256": hashlib.sha256(
                        str(log.get("data", "")).lower().encode()
                    ).hexdigest(),
                }
            )
        cursor = chunk_end + 1
        time.sleep(0.05)
    output.sort(
        key=lambda item: (
            item["block_number"],
            item["transaction_index"],
            item["log_index"],
        )
    )
    identities = {
        (item["block_hash"], item["transaction_hash"], item["log_index"])
        for item in output
    }
    if len(identities) != len(output):
        raise ExportError("duplicate checkpoint Borrow continuity identity")
    if any(not start <= item["block_number"] <= end for item in output):
        raise ExportError("checkpoint Borrow continuity log is out of range")
    return output


def independently_agreed_tail_logs(
    providers: list[Provider], start: int, end: int
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    provider_logs = [
        sanitized_tail_borrow_logs(provider, start, end) for provider in providers
    ]
    hashes = [canonical_hash(logs) for logs in provider_logs]
    if len(set(hashes)) != 1:
        raise ExportError("independent provider disagreement: Borrow tail")
    bindings = [
        {
            "provider_id": provider.label,
            "log_count": len(logs),
            "logs_content_sha256": content_hash,
        }
        for provider, logs, content_hash in zip(providers, provider_logs, hashes)
    ]
    return provider_logs[0], bindings


def independently_agreed_calls(
    providers: list[Provider], calls: list[tuple[str, str]], block: int, context: str
) -> tuple[list[str], list[dict[str, Any]]]:
    provider_results = [provider.eth_calls(calls, block) for provider in providers]
    reference = provider_results[0]
    if any(result != reference for result in provider_results[1:]):
        raise ExportError(f"independent provider disagreement: {context}")
    bindings = [
        {
            "provider_id": provider.label,
            "context": context,
            "call_count": len(result),
            "result_sha256": canonical_hash(result),
        }
        for provider, result in zip(providers, provider_results)
    ]
    return reference, bindings


def reserve_state(providers: list[Provider], block: int) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    reserve_list_results, bindings = independently_agreed_calls(
        providers,
        [(POOL, call_data(SELECTORS["get_reserves_list"]))],
        block,
        "reserve_list",
    )
    reserves = decode_address_array(reserve_list_results[0])
    if not reserves or len(set(reserves)) != len(reserves):
        raise ExportError("canonical reserve list is empty or duplicated")
    calls: list[tuple[str, str]] = []
    for reserve_id, asset in enumerate(reserves):
        asset_word = encode_address(asset)
        calls.extend(
            [
                (POOL, call_data(SELECTORS["get_reserve_address_by_id"], uint_word(reserve_id))),
                (POOL, call_data(SELECTORS["get_configuration"], asset_word)),
                (POOL, call_data(SELECTORS["get_normalized_income"], asset_word)),
                (POOL, call_data(SELECTORS["get_normalized_variable_debt"], asset_word)),
                (POOL, call_data(SELECTORS["get_liquidation_grace_period"], asset_word)),
                (DATA_PROVIDER, call_data(SELECTORS["get_reserve_tokens"], asset_word)),
                (DATA_PROVIDER, call_data(SELECTORS["get_reserve_configuration"], asset_word)),
                (ORACLE, call_data(SELECTORS["get_source_of_asset"], asset_word)),
                (ORACLE, call_data(SELECTORS["get_asset_price"], asset_word)),
                (asset, call_data(SELECTORS["symbol"])),
            ]
        )
    results, reserve_bindings = independently_agreed_calls(providers, calls, block, "reserve_state")
    bindings.extend(reserve_bindings)
    output = []
    cursor = 0
    for reserve_id, asset in enumerate(reserves):
        values = results[cursor : cursor + 10]
        cursor += 10
        if word_address(words(values[0])[0]) != asset:
            raise ExportError("reserve id mapping disagreement")
        configuration_bitmap = word_uint(words(values[1])[0])
        liquidity_index = word_uint(words(values[2])[0])
        variable_borrow_index = word_uint(words(values[3])[0])
        liquidation_grace_period_until = word_uint(words(values[4])[0])
        token_words = words(values[5], 3)
        config_words = words(values[6], 10)
        output.append(
            {
                "reserve_id": reserve_id,
                "asset": asset,
                "configuration_bitmap": configuration_bitmap,
                "decimals": word_uint(config_words[0]),
                "ltv_bps": word_uint(config_words[1]),
                "liquidation_threshold_bps": word_uint(config_words[2]),
                "liquidation_bonus_bps": word_uint(config_words[3]),
                "liquidation_protocol_fee_bps": (configuration_bitmap >> 152)
                & 0xFFFF,
                "reserve_factor_bps": word_uint(config_words[4]),
                "usage_as_collateral_enabled": bool(word_uint(config_words[5])),
                "borrowing_enabled": bool(word_uint(config_words[6])),
                "stable_borrowing_enabled": bool(word_uint(config_words[7])),
                "active": bool(word_uint(config_words[8])),
                "frozen": bool(word_uint(config_words[9])),
                "paused": bool((configuration_bitmap >> 60) & 1),
                "liquidity_index_ray": liquidity_index,
                "variable_borrow_index_ray": variable_borrow_index,
                "liquidation_grace_period_until": liquidation_grace_period_until,
                "atoken": word_address(token_words[0]),
                "stable_debt_token": word_address(token_words[1]),
                "variable_debt_token": word_address(token_words[2]),
                "price_feed": word_address(words(values[7])[0]),
                "price_base_units": word_uint(words(values[8])[0]),
                "price_base_decimals": 8,
                "symbol": decode_symbol(values[9]),
            }
        )
    return output, bindings


def emode_state(
    providers: list[Provider],
    block: int,
    categories: list[int],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    calls: list[tuple[str, str]] = []
    for category_id in categories:
        category_word = uint_word(category_id)
        calls.extend(
            [
                (
                    POOL,
                    call_data(SELECTORS["get_emode_collateral_config"], category_word),
                ),
                (
                    POOL,
                    call_data(SELECTORS["get_emode_collateral_bitmap"], category_word),
                ),
                (
                    POOL,
                    call_data(SELECTORS["get_emode_borrowable_bitmap"], category_word),
                ),
                (POOL, call_data(SELECTORS["get_emode_label"], category_word)),
            ]
        )
    if not calls:
        return [], []
    results, bindings = independently_agreed_calls(providers, calls, block, "emode_state")
    output = []
    cursor = 0
    for category_id in categories:
        collateral = words(results[cursor], 3)
        output.append(
            {
                "category_id": category_id,
                "ltv_bps": word_uint(collateral[0]),
                "liquidation_threshold_bps": word_uint(collateral[1]),
                "liquidation_bonus_bps": word_uint(collateral[2]),
                "collateral_bitmap": word_uint(words(results[cursor + 1])[0]),
                "borrowable_bitmap": word_uint(words(results[cursor + 2])[0]),
                "label": decode_symbol(results[cursor + 3]),
            }
        )
        cursor += 4
    return output, bindings


def active_borrower_state(
    providers: list[Provider],
    block: int,
    borrowers: list[str],
    reserves: list[dict[str, Any]],
) -> tuple[list[str], dict[str, int], list[dict[str, Any]]]:
    """Screen the complete discovery set using exact Aave debt flags.

    UserConfiguration debt bits are current checkpoint state and do not suffer
    from the base-currency rounding that can make tiny debt appear as zero in
    getUserAccountData.  The primary provider performs the permitted broad
    screen. Both providers then independently agree on every retained address.
    """

    calls = [
        (POOL, call_data(SELECTORS["get_user_configuration"], encode_address(borrower)))
        for borrower in borrowers
    ]
    primary_results = providers[0].eth_calls(calls, block)
    if len(primary_results) != len(borrowers):
        raise ExportError("primary borrower activity screen is incomplete")
    bindings = [
        {
            "provider_id": providers[0].label,
            "context": "borrower_activity_primary",
            "call_count": len(primary_results),
            "result_sha256": canonical_hash(primary_results),
        }
    ]
    debt_mask = sum(1 << (int(reserve["reserve_id"]) * 2) for reserve in reserves)
    configurations = {
        borrower: word_uint(words(result)[0])
        for borrower, result in zip(borrowers, primary_results)
    }
    active = [borrower for borrower in borrowers if configurations[borrower] & debt_mask]
    retained_calls = [
        (POOL, call_data(SELECTORS["get_user_configuration"], encode_address(borrower)))
        for borrower in active
    ]
    retained_results, retained_bindings = independently_agreed_calls(
        providers, retained_calls, block, "borrower_activity_retained"
    )
    for borrower, result in zip(active, retained_results):
        if word_uint(words(result)[0]) != configurations[borrower]:
            raise ExportError("retained borrower configuration disagreement")
    bindings.extend(retained_bindings)
    return active, configurations, bindings


def borrower_state(
    providers: list[Provider],
    block: int,
    borrowers: list[str],
    reserves: list[dict[str, Any]],
    configurations: dict[str, int],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    calls: list[tuple[str, str]] = []
    query_plan: dict[str, list[dict[str, Any]]] = {}
    for borrower in borrowers:
        borrower_word = encode_address(borrower)
        calls.append((POOL, call_data(SELECTORS["get_user_emode"], borrower_word)))
        configuration = configurations[borrower]
        flagged = []
        for reserve in reserves:
            reserve_id = int(reserve["reserve_id"])
            if not (
                (configuration >> (reserve_id * 2)) & 1
                or (configuration >> (reserve_id * 2 + 1)) & 1
            ):
                continue
            flagged.append(reserve)
            calls.append(
                (
                    DATA_PROVIDER,
                    call_data(
                        SELECTORS["get_user_reserve_data"],
                        encode_address(reserve["asset"]),
                        borrower_word,
                    ),
                )
            )
            calls.append(
                (
                    reserve["atoken"],
                    call_data(SELECTORS["scaled_balance_of"], borrower_word),
                )
            )
        query_plan[borrower] = flagged
    results, bindings = independently_agreed_calls(providers, calls, block, "borrower_state")
    output = []
    cursor = 0
    for borrower in borrowers:
        configuration_bitmap = configurations[borrower]
        emode_category = word_uint(words(results[cursor])[0])
        cursor += 1
        positions = []
        for reserve in query_plan[borrower]:
            values = words(results[cursor], 9)
            scaled_supply = word_uint(words(results[cursor + 1])[0])
            cursor += 2
            current_supply = word_uint(values[0])
            current_stable_debt = word_uint(values[1])
            current_variable_debt = word_uint(values[2])
            principal_stable_debt = word_uint(values[3])
            scaled_variable_debt = word_uint(values[4])
            usage_as_collateral = bool(word_uint(values[8]))
            if current_supply or current_stable_debt or current_variable_debt or usage_as_collateral:
                positions.append(
                    {
                        "asset": reserve["asset"],
                        "current_supply": current_supply,
                        "scaled_supply": scaled_supply,
                        "current_stable_debt": current_stable_debt,
                        "current_variable_debt": current_variable_debt,
                        "principal_stable_debt": principal_stable_debt,
                        "scaled_variable_debt": scaled_variable_debt,
                        "usage_as_collateral_enabled": usage_as_collateral,
                    }
                )
        output.append(
            {
                "address": borrower,
                "account_configuration_bitmap": configuration_bitmap,
                "emode_category": emode_category,
                "positions": positions,
            }
        )
    return output, bindings


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container")
    parser.add_argument("--provider-env", action="append", default=[])
    parser.add_argument("--provider-id", action="append", default=[])
    parser.add_argument("--ssh-executable", default="ssh")
    parser.add_argument("--ssh-provider-host")
    parser.add_argument("--ssh-provider-port", type=int, default=22)
    parser.add_argument("--ssh-provider-identity", type=Path)
    parser.add_argument("--ssh-provider-known-hosts", type=Path)
    parser.add_argument("--ssh-provider-container")
    parser.add_argument("--ssh-provider-index", type=int, default=0)
    parser.add_argument("--ssh-provider-id", default="phoenix-reviewed-primary")
    parser.add_argument("--discovery", required=True)
    parser.add_argument("--archive-manifest", required=True)
    args = parser.parse_args()
    providers: list[object] = []
    try:
        with Path(args.discovery).open(encoding="utf-8") as handle:
            discovery = validate_discovery(json.load(handle))
        with Path(args.archive_manifest).open(encoding="utf-8") as handle:
            archive_manifest = validate_archive_manifest(
                json.load(handle), discovery
            )
        discovery.pop("logs", None)
        archive_checkpoint = int(discovery["checkpoint_block"])
        ssh_selected = any(
            value is not None
            for value in (
                args.ssh_provider_host,
                args.ssh_provider_identity,
                args.ssh_provider_container,
            )
        )
        if args.container is not None:
            if ssh_selected or args.provider_env:
                raise ExportError("container provider mode cannot be mixed")
            urls = provider_urls(args.container)
            providers = [
                Provider(f"reviewed-provider-{index}", url)
                for index, url in enumerate(urls, 1)
            ]
        else:
            if ssh_selected:
                if (
                    not args.ssh_provider_host
                    or args.ssh_provider_identity is None
                    or not args.ssh_provider_container
                ):
                    raise ExportError("SSH provider arguments are incomplete")
                providers.append(
                    SSHContainerProvider(
                        args.ssh_provider_id,
                        args.ssh_executable,
                        args.ssh_provider_host,
                        args.ssh_provider_port,
                        args.ssh_provider_identity,
                        args.ssh_provider_known_hosts,
                        args.ssh_provider_container,
                        args.ssh_provider_index,
                    )
                )
            urls = environment_provider_urls(args.provider_env) if args.provider_env else []
            if args.provider_id and len(args.provider_id) != len(urls):
                raise ExportError("provider identity count mismatch")
            ids = args.provider_id or [
                f"protected-provider-{index}" for index in range(1, len(urls) + 1)
            ]
            providers.extend(
                Provider(label, url) for label, url in zip(ids, urls)
            )
        if len(providers) < 2:
            raise ExportError("two independent reviewed providers are required")
        labels = [provider.label for provider in providers]
        if len(set(labels)) != len(labels):
            raise ExportError("provider identities contain duplicates")
        for provider in providers:
            chain = provider.call("eth_chainId", [])
            if int(str(chain), 16) != CHAIN_ID:
                raise ExportError(f"{provider.label}:chain disagreement")
        block, finalized_heads, headers = finalized_checkpoint(providers)
        if block < archive_checkpoint:
            raise ExportError("finalized checkpoint regressed behind archive")
        tail_logs, tail_bindings = independently_agreed_tail_logs(
            providers, archive_checkpoint + 1, block
        )
        print(
            f"checkpoint_tail_collection_complete={providers[0].label} "
            f"log_count={len(tail_logs)}",
            file=sys.stderr,
        )
        historical_borrowers = list(discovery["borrowers"])
        tail_borrowers = sorted({_address(item["borrower"], "tail borrower") for item in tail_logs})
        screened_borrowers = sorted(set(historical_borrowers).union(tail_borrowers))
        if len(screened_borrowers) > MAX_BORROWERS:
            raise ExportError("current borrower screen exceeds bound")
        code_bindings = []
        for provider in providers:
            implementation_word = provider.call(
                "eth_getStorageAt", [POOL, POOL_IMPLEMENTATION_SLOT, hex(block)]
            )
            if not isinstance(implementation_word, str):
                raise ExportError(f"{provider.label}:pool implementation unavailable")
            implementation = word_address(words(implementation_word)[0])
            if implementation != POOL_IMPLEMENTATION:
                raise ExportError(f"{provider.label}:pool implementation source binding mismatch")
            code_hashes = {}
            for label, address in (("pool", POOL), ("data_provider", DATA_PROVIDER), ("oracle", ORACLE)):
                code = provider.call("eth_getCode", [address, hex(block)])
                if not isinstance(code, str) or code == "0x":
                    raise ExportError(f"{provider.label}:{label}:code missing")
                code_hashes[label] = hashlib.sha256(bytes.fromhex(code[2:])).hexdigest()
            implementation_code = provider.call(
                "eth_getCode", [implementation, hex(block)]
            )
            if not isinstance(implementation_code, str) or implementation_code == "0x":
                raise ExportError(f"{provider.label}:pool implementation code missing")
            code_hashes["pool_implementation"] = hashlib.sha256(
                bytes.fromhex(implementation_code[2:])
            ).hexdigest()
            code_bindings.append(
                {
                    "provider_id": provider.label,
                    "pool_implementation": implementation,
                    "code_sha256": code_hashes,
                }
            )
        if len({item["pool_implementation"] for item in code_bindings}) != 1:
            raise ExportError("independent provider disagreement: Pool implementation")
        if len({canonical_hash(item["code_sha256"]) for item in code_bindings}) != 1:
            raise ExportError("independent provider disagreement: protocol code")
        reserves, reserve_bindings = reserve_state(providers, block)
        active_addresses, configurations, activity_bindings = active_borrower_state(
            providers, block, screened_borrowers, reserves
        )
        borrowers, borrower_bindings = borrower_state(
            providers, block, active_addresses, reserves, configurations
        )
        emode_categories, emode_bindings = emode_state(
            providers,
            block,
            sorted({int(item["emode_category"]) for item in borrowers if item["emode_category"]}),
        )
        active_borrowers = [
            item
            for item in borrowers
            if any(
                position["current_stable_debt"] or position["current_variable_debt"]
                for position in item["positions"]
            )
        ]
        if len(active_borrowers) != len(borrowers):
            raise ExportError("borrower debt flag/state disagreement")
        output = {
            "schema": "phoenix.atlas.aave-checkpoint.v1",
            "chain_id": CHAIN_ID,
            "checkpoint_block": block,
            "checkpoint_hash": headers[0]["checkpoint"]["hash"],
            "discovery_content_sha256": discovery["content_sha256"],
            "archive_manifest_content_sha256": archive_manifest["content_sha256"],
            "archive_checkpoint_block": archive_checkpoint,
            "finalized_heads": finalized_heads,
            "tail_discovery": {
                "collection_provider_id": providers[0].label,
                "independent_log_verification": True,
                "provider_bindings": tail_bindings,
                "start_block": archive_checkpoint + 1,
                "end_block": block,
                "log_count": len(tail_logs),
                "borrower_count": len(tail_borrowers),
                "logs_content_sha256": canonical_hash(tail_logs),
                "logs": tail_logs,
            },
            "protocol": {
                "pool": POOL,
                "data_provider": DATA_PROVIDER,
                "oracle": ORACLE,
                "pool_implementation": code_bindings[0]["pool_implementation"],
            },
            "source_bindings": {
                "aave_address_book": {
                    "commit": AAVE_ADDRESS_BOOK_COMMIT,
                    "path": "src/AaveV3Arbitrum.sol",
                    "pool_implementation": POOL_IMPLEMENTATION,
                },
                "aave_v3_origin": {
                    "commit": AAVE_V3_ORIGIN_COMMIT,
                    "path": "src/contracts/protocol/libraries/logic/LiquidationLogic.sol",
                },
            },
            "liquidation_logic": {
                "default_close_factor_bps": 5_000,
                "close_factor_hf_threshold_wad": 950_000_000_000_000_000,
                "minimum_reserve_value_base": 2_000 * 10**8,
                "minimum_leftover_base": 1_000 * 10**8,
            },
            "provider_headers": headers,
            "protocol_code_bindings": code_bindings,
            "protocol_code_independent_agreement": True,
            "state_bindings": reserve_bindings
            + activity_bindings
            + borrower_bindings
            + emode_bindings,
            "source_methods": [
                "eth_chainId",
                "eth_getBlockByNumber",
                "eth_getCode",
                "eth_getStorageAt",
                "eth_call",
                "eth_getLogs",
            ],
            "archive_complete": True,
            "independent_state_agreement": True,
            "independent_state_agreement_scope": [
                "checkpoint_block_hash",
                "reserve_state",
                "retained_borrower_configuration",
                "retained_borrower_state",
                "emode_state",
            ],
            "reserves": reserves,
            "emode_categories": emode_categories,
            "historical_discovered_borrower_count": len(historical_borrowers),
            "tail_discovered_borrower_count": len(tail_borrowers),
            "discovered_borrower_count": len(screened_borrowers),
            "screened_borrower_count": len(screened_borrowers),
            "discovery_log_count": discovery["log_count"] + len(tail_logs),
            "active_borrower_count": len(active_borrowers),
            "debt_bearing_borrower_count": len(active_borrowers),
            "borrowers": active_borrowers,
            "execution_authority": {
                "signer": False,
                "bond": False,
                "bid": False,
                "solver": False,
                "submission": False,
                "production_write": False,
            },
        }
        output["content_sha256"] = canonical_hash(output)
        json.dump(output, sys.stdout, sort_keys=True, separators=(",", ":"))
        sys.stdout.write("\n")
        return 0
    except Exception as error:
        print(f"bounded Aave checkpoint export failed: {error}", file=sys.stderr)
        return 1
    finally:
        for provider in providers:
            close = getattr(provider, "close", None)
            if close is not None:
                close()


if __name__ == "__main__":
    raise SystemExit(main())
