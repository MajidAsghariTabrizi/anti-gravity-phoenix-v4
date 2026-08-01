#!/usr/bin/env python3
"""Export a bounded, credential-free Atlas public-chain evidence transcript.

This helper performs read-only JSON-RPC calls through URLs already present in a
running Phoenix RPC gateway container. It never prints those URLs or the
container environment. Its stdout is only the sanitized transcript consumed by
atlas-reconciler.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import urllib.request


ATLAS = "0x8ad1aE9D97C79aA68A0a151E83ff3942f68F86C1"
ARBITRUM_CHAIN_ID = "0xa4b1"
SOLVER_RESULT_TOPIC = (
    "0x94e79da376f3bc5202c947c2466a329832d3e9af2f4e094a18c160868453273c"
)
METACALL_RESULT_TOPIC = (
    "0x1c8af9222013876e762969f616bf76d9bd3a356e39ce598256dd515b6cb7f82b"
)
TRANSCRIPT_SCHEMA = "phoenix.atlas-rpc-transcript.v1"
MAX_BLOCK_SPAN = 20_000
LOG_CHUNK_SIZE = 512


class BoundedRPC:
    def __init__(self, providers: list[str]) -> None:
        if not providers:
            raise RuntimeError("no reviewed RPC provider is configured")
        self._providers = providers
        self._request_id = 0

    def call(self, method: str, params: list[object]) -> object:
        allowed = {
            "eth_chainId",
            "eth_blockNumber",
            "eth_getLogs",
            "eth_getTransactionByHash",
            "eth_getTransactionReceipt",
        }
        if method not in allowed:
            raise RuntimeError("RPC method is outside the read-only allowlist")
        self._request_id += 1
        body = json.dumps(
            {"jsonrpc": "2.0", "id": self._request_id, "method": method, "params": params},
            separators=(",", ":"),
        ).encode()
        failures: list[str] = []
        for index, provider in enumerate(self._providers):
            request = urllib.request.Request(
                provider,
                data=body,
                headers={
                    "Content-Type": "application/json",
                    "User-Agent": "anti-gravity-phoenix-rpc-gateway/4",
                },
                method="POST",
            )
            try:
                with urllib.request.urlopen(request, timeout=30) as response:
                    payload = json.load(response)
                if payload.get("error") is not None or "result" not in payload:
                    failures.append(f"provider[{index}]=rpc_error")
                    continue
                return payload["result"]
            except Exception:  # Deliberately redact provider URLs and credentials.
                failures.append(f"provider[{index}]=transport_error")
        raise RuntimeError("all reviewed RPC providers failed: " + ",".join(failures))


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
    environment = json.loads(result.stdout)
    values: dict[str, str] = {}
    for item in environment:
        if "=" in item:
            key, value = item.split("=", 1)
            values[key] = value
    raw = values.get("RPC_PROVIDER_URLS", "")
    if raw.startswith("["):
        providers = json.loads(raw)
    else:
        providers = [part.strip() for part in raw.split(",") if part.strip()]
    if not all(isinstance(provider, str) and provider.startswith(("http://", "https://")) for provider in providers):
        raise RuntimeError("reviewed RPC provider configuration has an invalid shape")
    return providers


def quantity(value: str) -> int:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise RuntimeError("RPC returned an invalid hexadecimal quantity")
    return int(value, 16)


def sanitized_log(log: dict[str, object]) -> dict[str, object]:
    return {
        "address": log["address"],
        "topics": log["topics"],
        "data": log["data"],
        "logIndex": log["logIndex"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", required=True)
    parser.add_argument("--from-block", required=True, type=int)
    parser.add_argument("--to-block", default="latest")
    args = parser.parse_args()

    try:
        rpc = BoundedRPC(load_provider_urls(args.container))
        chain_id = rpc.call("eth_chainId", [])
        if not isinstance(chain_id, str) or chain_id.lower() != ARBITRUM_CHAIN_ID:
            raise RuntimeError("reviewed RPC provider returned the wrong chain ID")
        latest_hex = rpc.call("eth_blockNumber", [])
        if not isinstance(latest_hex, str):
            raise RuntimeError("RPC returned an invalid latest block")
        latest = quantity(latest_hex)
        to_block = latest if args.to_block == "latest" else int(args.to_block)
        if args.from_block < 0 or to_block < args.from_block or to_block > latest:
            raise RuntimeError("invalid transcript block bounds")
        if to_block - args.from_block + 1 > MAX_BLOCK_SPAN:
            raise RuntimeError("requested transcript exceeds the bounded block span")

        transaction_hashes: set[str] = set()
        cursor = args.from_block
        while cursor <= to_block:
            chunk_end = min(cursor + LOG_CHUNK_SIZE - 1, to_block)
            logs = rpc.call(
                "eth_getLogs",
                [
                    {
                        "address": ATLAS,
                        "fromBlock": hex(cursor),
                        "toBlock": hex(chunk_end),
                        "topics": [[SOLVER_RESULT_TOPIC, METACALL_RESULT_TOPIC]],
                    }
                ],
            )
            if not isinstance(logs, list):
                raise RuntimeError("RPC returned invalid Atlas logs")
            for log in logs:
                transaction_hashes.add(log["transactionHash"].lower())
            cursor = chunk_end + 1

        transactions: list[dict[str, object]] = []
        for tx_hash in sorted(transaction_hashes):
            tx = rpc.call("eth_getTransactionByHash", [tx_hash])
            receipt = rpc.call("eth_getTransactionReceipt", [tx_hash])
            if not isinstance(tx, dict) or not isinstance(receipt, dict):
                raise RuntimeError("RPC returned an incomplete public transaction")
            if tx.get("to", "").lower() != ATLAS.lower():
                raise RuntimeError("Atlas result log transaction has an unexpected target")
            sanitized_receipt: dict[str, object] = {
                "transactionHash": receipt["transactionHash"],
                "blockNumber": receipt["blockNumber"],
                "status": receipt["status"],
                "gasUsed": receipt["gasUsed"],
                "effectiveGasPrice": receipt["effectiveGasPrice"],
                "logs": [sanitized_log(log) for log in receipt["logs"]],
            }
            if "gasUsedForL1" in receipt:
                sanitized_receipt["gasUsedForL1"] = receipt["gasUsedForL1"]
            transactions.append(
                {
                    "hash": tx["hash"],
                    "to": tx["to"],
                    "input": tx["input"],
                    "blockNumber": tx["blockNumber"],
                    "transactionIndex": tx["transactionIndex"],
                    "receipt": sanitized_receipt,
                }
            )

        transcript = {
            "schema": TRANSCRIPT_SCHEMA,
            "chain_id": chain_id,
            "atlas": ATLAS,
            "from_block": hex(args.from_block),
            "to_block": hex(to_block),
            "latest_block": latest_hex,
            "transactions": transactions,
        }
        json.dump(transcript, sys.stdout, separators=(",", ":"), sort_keys=True)
        sys.stdout.write("\n")
        return 0
    except Exception as error:
        print(f"bounded transcript export failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
