#!/usr/bin/env python3
"""Export a bounded, credential-free Atlas public-chain evidence transcript.

This helper performs read-only JSON-RPC calls through a reviewed provider
configuration already present in a running Phoenix RPC gateway container. It
mirrors the reviewed rpc-gateway provider contract:

  - Authenticated production contract (precedence 1): when the container
    declares RPC_AUTHORITY_MODE=single_primary, the exporter builds exactly
    one provider from RPC_AUTH_PROVIDER_ID / RPC_AUTH_PROVIDER_URL /
    RPC_AUTH_PROVIDER_PRIORITY / RPC_AUTH_PROVIDER_HEADER_NAME /
    RPC_AUTH_PROVIDER_HEADER_FILE. The header secret is read from the
    container-internal secret file through a single pipe (docker exec), kept
    in memory only, and never printed, logged, or embedded in errors.
  - Legacy contract (precedence 2): RPC_PROVIDER_URLS, unchanged.
  - Anything else fails closed.

Stdout is only the sanitized transcript consumed by atlas-reconciler. Provider
URLs and credentials are never printed.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import urllib.request
from dataclasses import dataclass

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
RPC_TIMEOUT_SECONDS = 30

SINGLE_PRIMARY_MODE = "single_primary"
AUTH_PROVIDER_IDENTITY = "production-nownodes-arbitrum"
MAX_SECRET_BYTES = 4096
MAX_PRIORITY = 2**32 - 1
HEADER_NAME_PATTERN = re.compile(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]{1,200}$")


@dataclass(frozen=True)
class ReviewedProvider:
    """A reviewed provider identity. label/url are configuration-safe: the
    label is a validated provider identity or an index, never a URL."""

    label: str
    url: str
    header_name: str | None = None
    header_value: str | None = None


def is_http_url(url: str) -> bool:
    """Mirror rpc-gateway providers::is_http_url (http/https only, non-empty host)."""
    if not url.startswith(("http://", "https://")):
        return False
    rest = url[len("https://") :] if url.startswith("https://") else url[len("http://") :]
    authority = re.split(r"[/?#]", rest, maxsplit=1)[0]
    host_port = authority.rsplit("@", 1)[-1]
    host = host_port.split(":", 1)[0]
    return bool(host)


def container_environment(container: str) -> dict[str, str]:
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
    return values


def read_container_secret(container: str, path: str) -> str:
    """Read a container-internal secret file through a single pipe.

    Mirrors rpc-gateway main::read_header_secret: regular file only (the
    container-side check rejects symlinks), 1..4096 bytes, strict UTF-8, and
    no CR/LF. The secret value is returned to the caller only; this function
    raises redacted error classes and never includes file contents.
    """
    result = subprocess.run(
        [
            "sudo",
            "-n",
            "docker",
            "exec",
            container,
            "sh",
            "-c",
            'test -f "$1" && ! test -L "$1" && cat "$1"',
            "--",
            path,
        ],
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        raise RuntimeError("auth provider secret file is unavailable")
    value = result.stdout
    if not value or len(value) > MAX_SECRET_BYTES:
        raise RuntimeError("auth provider secret file is invalid")
    try:
        secret = value.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise RuntimeError("auth provider secret file is invalid") from error
    if "\r" in secret or "\n" in secret:
        raise RuntimeError("auth provider secret file is invalid")
    return secret


def _authenticated_provider(container: str, environment: dict[str, str]) -> ReviewedProvider:
    identity = environment.get("RPC_AUTH_PROVIDER_ID", "")
    if identity != AUTH_PROVIDER_IDENTITY:
        raise RuntimeError("auth RPC provider identity is invalid")
    url = environment.get("RPC_AUTH_PROVIDER_URL", "")
    if not is_http_url(url):
        raise RuntimeError("auth RPC provider URL is invalid")
    header_name = environment.get("RPC_AUTH_PROVIDER_HEADER_NAME", "")
    if not HEADER_NAME_PATTERN.fullmatch(header_name):
        raise RuntimeError("auth RPC provider header name is invalid")
    priority_raw = environment.get("RPC_AUTH_PROVIDER_PRIORITY", "100")
    try:
        priority = int(priority_raw)
    except ValueError as error:
        raise RuntimeError("auth RPC provider priority is invalid") from error
    if priority <= 0 or priority > MAX_PRIORITY:
        raise RuntimeError("auth RPC provider priority is invalid")
    secret_file = environment.get("RPC_AUTH_PROVIDER_HEADER_FILE", "")
    if not secret_file.startswith("/"):
        raise RuntimeError("auth RPC provider secret file is unsafe")
    auth_value = read_container_secret(container, secret_file)
    return ReviewedProvider(
        label=identity,
        url=url,
        header_name=header_name,
        header_value=auth_value,
    )


def _legacy_providers(environment: dict[str, str]) -> list[ReviewedProvider]:
    raw = environment.get("RPC_PROVIDER_URLS", "")
    if raw.startswith("["):
        providers = json.loads(raw)
    else:
        providers = [part.strip() for part in raw.split(",") if part.strip()]
    if not providers or not all(
        isinstance(provider, str) and is_http_url(provider) for provider in providers
    ):
        raise RuntimeError("reviewed RPC provider configuration has an invalid shape")
    return [
        ReviewedProvider(label=f"provider_{index}", url=provider)
        for index, provider in enumerate(providers)
    ]


def load_reviewed_providers(container: str) -> list[ReviewedProvider]:
    """Deterministic precedence: authenticated production contract first,
    legacy RPC_PROVIDER_URLS second, otherwise fail closed. Ambiguity is
    resolved by RPC_AUTHORITY_MODE: single_primary always selects the
    authenticated contract."""
    environment = container_environment(container)
    if environment.get("RPC_AUTHORITY_MODE") == SINGLE_PRIMARY_MODE:
        return [_authenticated_provider(container, environment)]
    if not environment.get("RPC_PROVIDER_URLS", "").strip():
        raise RuntimeError("no reviewed RPC provider is configured")
    return _legacy_providers(environment)


class BoundedRPC:
    def __init__(self, providers: list[ReviewedProvider]) -> None:
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
        for provider in self._providers:
            headers = {
                "Content-Type": "application/json",
                "User-Agent": "anti-gravity-phoenix-rpc-gateway/4",
            }
            if provider.header_name and provider.header_value:
                headers[provider.header_name] = provider.header_value
            request = urllib.request.Request(
                provider.url,
                data=body,
                headers=headers,
                method="POST",
            )
            try:
                with urllib.request.urlopen(request, timeout=RPC_TIMEOUT_SECONDS) as response:
                    payload = json.load(response)
                if payload.get("error") is not None or "result" not in payload:
                    failures.append(f"provider[{provider.label}]=rpc_error")
                    continue
                return payload["result"]
            except Exception:  # Deliberately redact provider URLs and credentials.
                failures.append(f"provider[{provider.label}]=transport_error")
        raise RuntimeError("all reviewed RPC providers failed: " + ",".join(failures))


def quantity(value: str) -> int:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise RuntimeError("RPC returned an invalid hexadecimal quantity")
    return int(value, 16)


def plan_block_bounds(from_block: int, to_block: int | str, latest: int) -> int:
    """Resolve the effective to-block and enforce the bounded span."""
    resolved = latest if to_block == "latest" else int(to_block)
    if from_block < 0 or resolved < from_block or resolved > latest:
        raise RuntimeError("invalid transcript block bounds")
    if resolved - from_block + 1 > MAX_BLOCK_SPAN:
        raise RuntimeError("requested transcript exceeds the bounded block span")
    return resolved


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
        rpc = BoundedRPC(load_reviewed_providers(args.container))
        chain_id = rpc.call("eth_chainId", [])
        if not isinstance(chain_id, str) or chain_id.lower() != ARBITRUM_CHAIN_ID:
            raise RuntimeError("reviewed RPC provider returned the wrong chain ID")
        latest_hex = rpc.call("eth_blockNumber", [])
        if not isinstance(latest_hex, str):
            raise RuntimeError("RPC returned an invalid latest block")
        latest = quantity(latest_hex)
        to_block = plan_block_bounds(args.from_block, args.to_block, latest)

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
