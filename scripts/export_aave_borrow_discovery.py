#!/usr/bin/env python3
"""Export a sanitized, hash-bound Aave borrower-discovery transcript.

Borrow is the canonical discovery event because Aave V3 debt tokens are
non-transferable. Provider URLs and environment values are never emitted.
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


CHAIN_ID = 42161
POOL = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
MAX_PROVIDERS = 8
MAX_TOTAL_SPAN = 500_000_000
INITIAL_CHUNK = 2_000_000
MIN_CHUNK = 512
MAX_RPC_ATTEMPTS = 9
CHUNK_PACING_SECONDS = 2.0


class ExportError(RuntimeError):
    pass


def canonical_hash(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def provider_urls(container: str) -> list[str]:
    result = subprocess.run(
        ["sudo", "-n", "docker", "inspect", "--format", "{{json .Config.Env}}", container],
        check=True,
        capture_output=True,
        text=True,
    )
    values = {}
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


class Provider:
    def __init__(self, label: str, url: str) -> None:
        self.label = label
        self._url = url
        self._request_id = 0

    def call(
        self, method: str, params: list[object], attempts: int = MAX_RPC_ATTEMPTS
    ) -> object:
        if method not in {"eth_chainId", "eth_blockNumber", "eth_getBlockByNumber", "eth_getLogs"}:
            raise ExportError("RPC method outside read-only allowlist")
        failure = "unavailable"
        for attempt in range(attempts):
            self._request_id += 1
            body = json.dumps(
                {"jsonrpc": "2.0", "id": self._request_id, "method": method, "params": params},
                separators=(",", ":"),
            ).encode()
            request = urllib.request.Request(
                self._url,
                data=body,
                headers={"Content-Type": "application/json", "User-Agent": "phoenix-atlas-borrow-discovery/1"},
                method="POST",
            )
            try:
                timeout_seconds = 45 if method == "eth_getLogs" else 90
                with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
                    payload = json.load(response)
                if payload.get("error") is not None:
                    error = payload["error"]
                    code = error.get("code") if isinstance(error, dict) else None
                    failure = f"rpc_error:{code}" if isinstance(code, int) else "rpc_error"
                    if method == "eth_getLogs" and code in {-32000, -32005}:
                        raise ExportError(f"{self.label}:{method}:{failure}")
                elif "result" in payload:
                    return payload["result"]
                else:
                    failure = "result_missing"
            except ExportError:
                raise
            except urllib.error.HTTPError as error:
                failure = f"http_error:{error.code}"
            except Exception:
                failure = "transport_error"
            if attempt + 1 < attempts:
                time.sleep(min(2**attempt, 30))
        raise ExportError(f"{self.label}:{method}:{failure}")


def header(provider: Provider, block_number: int) -> dict[str, object]:
    value = provider.call("eth_getBlockByNumber", [hex(block_number), False])
    if not isinstance(value, dict):
        raise ExportError(f"{provider.label}:block_header_incomplete")
    if int(str(value.get("number")), 16) != block_number:
        raise ExportError(f"{provider.label}:block_number_disagreement")
    block_hash = str(value.get("hash", "")).lower()
    parent_hash = str(value.get("parentHash", "")).lower()
    if len(block_hash) != 66 or len(parent_hash) != 66:
        raise ExportError(f"{provider.label}:block_hash_invalid")
    return {"number": block_number, "hash": block_hash, "parent_hash": parent_hash}


def get_logs(provider: Provider, start: int, end: int) -> list[dict[str, object]]:
    try:
        value = provider.call(
            "eth_getLogs",
            [{"address": POOL, "fromBlock": hex(start), "toBlock": hex(end), "topics": [BORROW_TOPIC]}],
            attempts=MAX_RPC_ATTEMPTS,
        )
    except ExportError as error:
        reason = str(error)
        range_limited = (
            "rpc_error:-32005" in reason
            or "rpc_error:-32000" in reason
            or "http_error:413" in reason
        )
        if not range_limited:
            raise
        if end - start + 1 <= MIN_CHUNK:
            raise
        print(
            f"archive_range_split={start}-{end}",
            file=sys.stderr,
            flush=True,
        )
        midpoint = (start + end) // 2
        return get_logs(provider, start, midpoint) + get_logs(provider, midpoint + 1, end)
    if not isinstance(value, list):
        raise ExportError(f"{provider.label}:log_result_invalid")
    return value


def sanitize_log(log: dict[str, object]) -> dict[str, object]:
    topics = log.get("topics")
    if not isinstance(topics, list) or len(topics) != 4:
        raise ExportError("Borrow log topic shape invalid")
    if str(topics[0]).lower() != BORROW_TOPIC:
        raise ExportError("Borrow log signature mismatch")
    if log.get("removed") is True:
        raise ExportError("removed Borrow log is not canonical")
    borrower = "0x" + str(topics[2]).lower()[-40:]
    return {
        "block_number": int(str(log["blockNumber"]), 16),
        "block_hash": str(log["blockHash"]).lower(),
        "transaction_hash": str(log["transactionHash"]).lower(),
        "transaction_index": int(str(log["transactionIndex"]), 16),
        "log_index": int(str(log["logIndex"]), 16),
        "reserve": "0x" + str(topics[1]).lower()[-40:],
        "borrower": borrower,
        "referral_code": int(str(topics[3]), 16),
        "data_sha256": hashlib.sha256(str(log.get("data", "")).lower().encode()).hexdigest(),
    }


def load_cached_chunk(path: Path, start: int, end: int) -> list[dict[str, object]]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ExportError("cached Borrow chunk is unreadable") from error
    if not isinstance(value, dict) or value.get("schema") != "phoenix.atlas.aave-borrow-chunk.v1":
        raise ExportError("cached Borrow chunk schema mismatch")
    if (
        value.get("chain_id") != CHAIN_ID
        or value.get("pool") != POOL
        or value.get("borrow_topic") != BORROW_TOPIC
    ):
        raise ExportError("cached Borrow chunk identity mismatch")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("cached Borrow chunk content hash mismatch")
    if value.get("start_block") != start or value.get("end_block") != end:
        raise ExportError("cached Borrow chunk range mismatch")
    logs = value.get("logs")
    if not isinstance(logs, list):
        raise ExportError("cached Borrow chunk logs are invalid")
    if any(
        not isinstance(log, dict)
        or not start <= int(log.get("block_number", -1)) <= end
        for log in logs
    ):
        raise ExportError("cached Borrow chunk contains an out-of-range log")
    return logs


def write_cached_chunk(
    directory: Path, start: int, end: int, logs: list[dict[str, object]]
) -> None:
    value = {
        "schema": "phoenix.atlas.aave-borrow-chunk.v1",
        "chain_id": CHAIN_ID,
        "pool": POOL,
        "borrow_topic": BORROW_TOPIC,
        "start_block": start,
        "end_block": end,
        "logs": logs,
    }
    value["content_sha256"] = canonical_hash(value)
    destination = directory / f"{start}-{end}.json"
    temporary = directory / f".{start}-{end}.{os.getpid()}.tmp"
    temporary.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    temporary.chmod(0o600)
    os.replace(temporary, destination)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", required=True)
    parser.add_argument("--start-block", required=True, type=int)
    parser.add_argument("--checkpoint-block", required=True, type=int)
    parser.add_argument("--chunk-cache-dir", type=Path)
    args = parser.parse_args()
    span = args.checkpoint_block - args.start_block + 1
    if args.start_block < 0 or span < 1 or span > MAX_TOTAL_SPAN:
        print("bounded Aave discovery export failed: invalid block bounds", file=sys.stderr)
        return 1
    try:
        providers = [Provider(f"reviewed-provider-{i}", url) for i, url in enumerate(provider_urls(args.container), 1)]
        bindings = []
        for provider in providers:
            chain = provider.call("eth_chainId", [])
            if int(str(chain), 16) != CHAIN_ID:
                raise ExportError(f"{provider.label}:chain_disagreement")
            start_header = header(provider, args.start_block)
            checkpoint_header = header(provider, args.checkpoint_block)
            bindings.append(
                {
                    "provider_id": provider.label,
                    "chain_id": CHAIN_ID,
                    "start_block": start_header,
                    "checkpoint_block": checkpoint_header,
                }
            )
        if len({item["checkpoint_block"]["hash"] for item in bindings}) != 1:
            raise ExportError("independent checkpoint hash disagreement")
        if len({item["start_block"]["hash"] for item in bindings}) != 1:
            raise ExportError("independent start hash disagreement")

        cache_dir = args.chunk_cache_dir
        if cache_dir is not None:
            cache_dir.mkdir(parents=True, exist_ok=True)
            cache_dir.chmod(0o700)
        logs = []
        cursor = args.start_block
        chunks = 0
        reused_chunks = 0
        while cursor <= args.checkpoint_block:
            chunk_end = min(cursor + INITIAL_CHUNK - 1, args.checkpoint_block)
            cache_path = cache_dir / f"{cursor}-{chunk_end}.json" if cache_dir else None
            if cache_path is not None and cache_path.is_file():
                chunk_logs = load_cached_chunk(cache_path, cursor, chunk_end)
                reused_chunks += 1
            else:
                chunk_logs = [
                    sanitize_log(log)
                    for log in get_logs(providers[0], cursor, chunk_end)
                ]
                if cache_dir is not None:
                    write_cached_chunk(cache_dir, cursor, chunk_end, chunk_logs)
            logs.extend(chunk_logs)
            cursor = chunk_end + 1
            chunks += 1
            time.sleep(CHUNK_PACING_SECONDS)
            if chunks % 10 == 0:
                print(
                    f"archive_chunks_completed={chunks} cache_reused={reused_chunks}",
                    file=sys.stderr,
                    flush=True,
                )
        logs.sort(key=lambda item: (item["block_number"], item["transaction_index"], item["log_index"]))
        identities = {(item["block_hash"], item["transaction_hash"], item["log_index"]) for item in logs}
        if len(identities) != len(logs):
            raise ExportError("duplicate canonical Borrow log identity")
        borrowers = sorted({str(item["borrower"]) for item in logs})
        output = {
            "schema": "phoenix.atlas.aave-borrow-discovery.v1",
            "chain_id": CHAIN_ID,
            "pool": POOL,
            "borrow_topic": BORROW_TOPIC,
            "start_block": args.start_block,
            "checkpoint_block": args.checkpoint_block,
            "provider_bindings": bindings,
            "source_methods": ["eth_chainId", "eth_getBlockByNumber", "eth_getLogs"],
            "archive_complete": True,
            "chunk_count": chunks,
            "log_count": len(logs),
            "borrower_count": len(borrowers),
            "borrowers": borrowers,
            "logs": logs,
        }
        output["content_sha256"] = canonical_hash(output)
        json.dump(output, sys.stdout, sort_keys=True, separators=(",", ":"))
        sys.stdout.write("\n")
        return 0
    except Exception as error:
        print(f"bounded Aave discovery export failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
