#!/usr/bin/env python3
"""Probe reviewed RPC providers for exact long-tail boundary trace evidence.

The helper is intentionally credential-redacting.  It reads provider URLs from
the already-running Phoenix RPC gateway container, but stdout contains only
bounded provider labels, canonical chain identities, trace capability status,
and hashes of successful public-chain responses.  Raw traces, provider URLs,
container environment values, and key material are never printed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import urllib.request
from pathlib import Path
from typing import Any


CHAIN_ID = 42161
ALLOWLIST_SCHEMA = "phoenix.long-tail.allowlist.v1"
OUTPUT_SCHEMA = "phoenix.long-tail.trace-capability.v1"
MAX_EVENTS = 10
MAX_PROVIDERS = 8


class ProbeError(RuntimeError):
    """Raised when the bounded probe contract is invalid."""


def canonical_hash(value: Any) -> str:
    encoded = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def load_provider_urls(container: str) -> list[str]:
    result = subprocess.run(
        [
            "sudo",
            "-n",
            "docker",
            "inspect",
            "--format",
            "{{json .Config.Env}}",
            container,
        ],
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
    providers = json.loads(raw) if raw.startswith("[") else [
        part.strip() for part in raw.split(",") if part.strip()
    ]
    if not isinstance(providers, list) or not (2 <= len(providers) <= MAX_PROVIDERS):
        raise ProbeError("independent reviewed provider set is unavailable")
    if len(set(providers)) != len(providers):
        raise ProbeError("reviewed provider configuration contains duplicates")
    if not all(
        isinstance(provider, str) and provider.startswith(("http://", "https://"))
        for provider in providers
    ):
        raise ProbeError("reviewed provider configuration has an invalid shape")
    return providers


class Provider:
    def __init__(self, label: str, url: str) -> None:
        self.label = label
        self._url = url
        self._request_id = 0

    def call(self, method: str, params: list[object]) -> tuple[bool, object]:
        allowed = {
            "eth_chainId",
            "eth_getBlockByNumber",
            "eth_getTransactionByHash",
            "eth_getTransactionReceipt",
            "debug_traceTransaction",
            "trace_replayTransaction",
            "arbtrace_replayTransaction",
        }
        if method not in allowed:
            raise ProbeError("RPC method is outside the read-only probe allowlist")
        self._request_id += 1
        body = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": self._request_id,
                "method": method,
                "params": params,
            },
            separators=(",", ":"),
        ).encode()
        request = urllib.request.Request(
            self._url,
            data=body,
            headers={
                "Content-Type": "application/json",
                "User-Agent": "anti-gravity-phoenix-trace-probe/1",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=45) as response:
                payload = json.load(response)
        except Exception:
            return False, "transport_error"
        if payload.get("error") is not None:
            error = payload["error"]
            code = error.get("code") if isinstance(error, dict) else None
            return False, f"rpc_error:{code}" if isinstance(code, int) else "rpc_error"
        if "result" not in payload:
            return False, "result_missing"
        return True, payload["result"]


def require_result(provider: Provider, method: str, params: list[object]) -> object:
    ok, result = provider.call(method, params)
    if not ok:
        raise ProbeError(f"{provider.label}:{method}:{result}")
    return result


def load_events(path: Path) -> list[dict[str, Any]]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if payload.get("schema_version") != ALLOWLIST_SCHEMA:
        raise ProbeError("unsupported allowlist schema")
    if payload.get("chain_id") != CHAIN_ID:
        raise ProbeError("allowlist chain mismatch")
    events = payload.get("events")
    if not isinstance(events, list) or len(events) != MAX_EVENTS:
        raise ProbeError("immutable event count changed")
    return events


def trace_event(provider: Provider, event: dict[str, Any]) -> dict[str, Any]:
    tx_hash = event["transaction_hash"]
    block_number = int(event["block_number"])
    block = require_result(provider, "eth_getBlockByNumber", [hex(block_number), False])
    tx = require_result(provider, "eth_getTransactionByHash", [tx_hash])
    receipt = require_result(provider, "eth_getTransactionReceipt", [tx_hash])
    if not isinstance(block, dict) or not isinstance(tx, dict) or not isinstance(receipt, dict):
        raise ProbeError(f"{provider.label}:canonical identity response is incomplete")
    if str(block.get("hash", "")).lower() != event["block_hash"]:
        raise ProbeError(f"{provider.label}:block hash disagreement")
    if str(tx.get("hash", "")).lower() != tx_hash:
        raise ProbeError(f"{provider.label}:transaction hash disagreement")
    if int(tx.get("transactionIndex", "0x-1"), 16) != int(event["transaction_index"]):
        raise ProbeError(f"{provider.label}:transaction index disagreement")
    if str(receipt.get("blockHash", "")).lower() != event["block_hash"]:
        raise ProbeError(f"{provider.label}:receipt block disagreement")

    ok, trace = provider.call(
        "debug_traceTransaction",
        [
            tx_hash,
            {
                "tracer": "prestateTracer",
                "tracerConfig": {"diffMode": True},
                "timeout": "30s",
            },
        ],
    )
    method = "debug_traceTransaction_prestate_diff"
    if not ok:
        debug_failure = str(trace)
        ok, trace = provider.call("trace_replayTransaction", [tx_hash, ["stateDiff"]])
        method = "trace_replayTransaction_stateDiff"
        if not ok:
            replay_failure = str(trace)
            ok, trace = provider.call(
                "arbtrace_replayTransaction", [tx_hash, ["stateDiff"]]
            )
            method = "arbtrace_replayTransaction_stateDiff"
        if not ok:
            return {
                "transaction_hash": tx_hash,
                "block_number": str(block_number),
                "block_hash": event["block_hash"],
                "transaction_index": str(event["transaction_index"]),
                "trace_status": "unsupported",
                "debug_failure": debug_failure,
                "replay_failure": replay_failure,
                "arbtrace_failure": str(trace),
            }

    if trace is None:
        raise ProbeError(f"{provider.label}:trace result is null")
    return {
        "transaction_hash": tx_hash,
        "block_number": str(block_number),
        "block_hash": event["block_hash"],
        "transaction_index": str(event["transaction_index"]),
        "trace_status": "available",
        "trace_method": method,
        "trace_response_sha256": canonical_hash(trace),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", required=True)
    parser.add_argument("--allowlist", required=True, type=Path)
    args = parser.parse_args()
    try:
        events = load_events(args.allowlist)
        providers = [
            Provider(f"reviewed-provider-{index + 1}", url)
            for index, url in enumerate(load_provider_urls(args.container))
        ]
        output: dict[str, Any] = {
            "schema": OUTPUT_SCHEMA,
            "chain_id": CHAIN_ID,
            "allowlist_sha256": canonical_hash(
                json.loads(args.allowlist.read_text(encoding="utf-8"))
            ),
            "providers": [],
        }
        for provider in providers:
            try:
                chain_id = require_result(provider, "eth_chainId", [])
                if int(str(chain_id), 16) != CHAIN_ID:
                    raise ProbeError(f"{provider.label}:chain mismatch")
            except Exception as error:
                output["providers"].append(
                    {
                        "provider_id": provider.label,
                        "provider_status": "unavailable",
                        "failure": str(error),
                        "event_count": 0,
                        "trace_available_count": 0,
                        "events": [],
                    }
                )
                continue

            rows = []
            for event in events:
                try:
                    rows.append(trace_event(provider, event))
                except Exception as error:
                    rows.append(
                        {
                            "transaction_hash": event["transaction_hash"],
                            "block_number": str(event["block_number"]),
                            "block_hash": event["block_hash"],
                            "transaction_index": str(event["transaction_index"]),
                            "trace_status": "verification_failed",
                            "failure": str(error),
                        }
                    )
            output["providers"].append(
                {
                    "provider_id": provider.label,
                    "provider_status": "available",
                    "event_count": len(rows),
                    "trace_available_count": sum(
                        row["trace_status"] == "available" for row in rows
                    ),
                    "events": rows,
                }
            )
        output["content_sha256"] = canonical_hash(output)
        json.dump(output, sys.stdout, sort_keys=True, separators=(",", ":"))
        sys.stdout.write("\n")
        return 0
    except Exception as error:
        print(f"bounded trace probe failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
