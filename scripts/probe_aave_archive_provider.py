#!/usr/bin/env python3
"""Credential-redacting capability probe for canonical Aave archive logs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request


CHAIN_ID = 42161
POOL = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
MAX_PROVIDERS = 8
MAX_SPAN = 50_000_000
DEFAULT_PROVIDER_ENV = "PHOENIX_ATLAS_ARCHIVE_SECONDARY_RPC_URL"
GET_RESERVES_LIST_CALL = "0xd1946dbc"


def provider_urls(container: str | None, provider_envs: list[str]) -> list[str]:
    if provider_envs:
        if container is not None:
            raise ValueError("select either provider environment references or container")
        urls = []
        for name in provider_envs:
            if not name or not name.replace("_", "").isalnum() or name.upper() != name:
                raise ValueError("provider environment reference is invalid")
            value = os.environ.get(name)
            if not value:
                raise ValueError(f"provider environment reference is unset:{name}")
            urls.append(value)
        if len(set(urls)) != len(urls):
            raise ValueError("reviewed provider set contains duplicates")
        if not all(url.startswith(("http://", "https://")) for url in urls):
            raise ValueError("reviewed provider configuration is invalid")
        return urls
    if container is None:
        raise ValueError("provider configuration is required")
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
        raise ValueError("reviewed provider set is unavailable")
    if len(set(urls)) != len(urls):
        raise ValueError("reviewed provider set contains duplicates")
    if not all(isinstance(url, str) and url.startswith(("http://", "https://")) for url in urls):
        raise ValueError("reviewed provider configuration is invalid")
    return urls


class Provider:
    def __init__(self, label: str, url: str) -> None:
        self.label = label
        self._url = url
        self._request_id = 0

    def call(self, method: str, params: list[object]) -> tuple[bool, object]:
        if method not in {
            "eth_chainId",
            "eth_blockNumber",
            "eth_getBlockByNumber",
            "eth_call",
            "eth_getLogs",
        }:
            raise ValueError("method outside read-only allowlist")
        self._request_id += 1
        body = json.dumps(
            {"jsonrpc": "2.0", "id": self._request_id, "method": method, "params": params},
            separators=(",", ":"),
        ).encode()
        request = urllib.request.Request(
            self._url,
            data=body,
            headers={"Content-Type": "application/json", "User-Agent": "phoenix-atlas-archive-probe/1"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=90) as response:
                payload = json.load(response)
        except urllib.error.HTTPError as error:
            return False, f"http_error:{error.code}"
        except Exception:
            return False, "transport_error"
        if payload.get("error") is not None:
            error = payload["error"]
            code = error.get("code") if isinstance(error, dict) else None
            return False, f"rpc_error:{code}" if isinstance(code, int) else "rpc_error"
        if "result" not in payload:
            return False, "result_missing"
        return True, payload["result"]


def canonical_log_hash(logs: list[dict[str, object]]) -> str:
    identities = sorted(
        (
            str(log.get("blockHash", "")).lower(),
            str(log.get("transactionHash", "")).lower(),
            str(log.get("logIndex", "")).lower(),
        )
        for log in logs
    )
    return hashlib.sha256(
        json.dumps(identities, separators=(",", ":")).encode()
    ).hexdigest()


def bytecode_hash(value: object) -> str | None:
    if not isinstance(value, str) or not value.startswith("0x") or value == "0x":
        return None
    try:
        return hashlib.sha256(bytes.fromhex(value[2:])).hexdigest()
    except ValueError:
        return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container")
    parser.add_argument("--provider-env", action="append", default=[])
    parser.add_argument("--provider-id", action="append", default=[])
    parser.add_argument("--from-block", type=int, required=True)
    parser.add_argument("--to-block", type=int, required=True)
    args = parser.parse_args()
    if args.from_block < 0 or args.to_block < args.from_block:
        raise SystemExit("invalid probe bounds")
    if args.to_block - args.from_block + 1 > MAX_SPAN:
        raise SystemExit("probe span exceeds bound")

    output = {"schema": "phoenix.atlas.aave-archive-capability.v1", "chain_id": CHAIN_ID, "providers": []}
    try:
        provider_envs = args.provider_env or (
            [DEFAULT_PROVIDER_ENV] if args.container is None else []
        )
        urls = provider_urls(args.container, provider_envs)
        if args.provider_id and len(args.provider_id) != len(urls):
            raise ValueError("provider identity count mismatch")
        labels = args.provider_id or [
            f"reviewed-provider-{index}" for index in range(1, len(urls) + 1)
        ]
        for label, url in zip(labels, urls):
            provider = Provider(label, url)
            ok_chain, chain = provider.call("eth_chainId", [])
            ok_head, head = provider.call("eth_blockNumber", [])
            ok_finalized, finalized = provider.call(
                "eth_getBlockByNumber", ["finalized", False]
            )
            finalized_number = (
                int(str(finalized.get("number")), 16)
                if ok_finalized and isinstance(finalized, dict)
                else None
            )
            ok_state, state = (
                provider.call(
                    "eth_call",
                    [{"to": POOL, "data": GET_RESERVES_LIST_CALL}, hex(finalized_number)],
                )
                if finalized_number is not None
                else (False, "finalized_unavailable")
            )
            ok_prior_header, prior_header = (
                provider.call(
                    "eth_getBlockByNumber", [hex(args.from_block - 1), False]
                )
                if args.from_block > 0
                else (False, "prior_block_unavailable")
            )
            ok_start_header, start_header = provider.call(
                "eth_getBlockByNumber", [hex(args.from_block), False]
            )
            ok_prior_code, prior_code = (
                provider.call("eth_getCode", [POOL, hex(args.from_block - 1)])
                if args.from_block > 0
                else (False, "prior_block_unavailable")
            )
            ok_start_code, start_code = provider.call(
                "eth_getCode", [POOL, hex(args.from_block)]
            )
            ok_boundary_state, boundary_state = provider.call(
                "eth_call",
                [{"to": POOL, "data": GET_RESERVES_LIST_CALL}, hex(args.from_block)],
            )
            ok_logs, logs = provider.call(
                "eth_getLogs",
                [{"address": POOL, "fromBlock": hex(args.from_block), "toBlock": hex(args.to_block), "topics": [BORROW_TOPIC]}],
            )
            row = {
                "provider_id": provider.label,
                "chain_status": "agrees" if ok_chain and int(str(chain), 16) == CHAIN_ID else str(chain),
                "head_status": "available" if ok_head else str(head),
                "from_block": args.from_block,
                "to_block": args.to_block,
                "span": args.to_block - args.from_block + 1,
                "finalized_status": "available" if finalized_number is not None else str(finalized),
                "finalized_block": finalized_number,
                "finalized_hash": (
                    str(finalized.get("hash", "")).lower()
                    if isinstance(finalized, dict)
                    else None
                ),
                "exact_aave_call_status": "available" if ok_state and isinstance(state, str) else str(state),
                "exact_aave_call_sha256": (
                    hashlib.sha256(str(state).lower().encode()).hexdigest()
                    if ok_state and isinstance(state, str)
                    else None
                ),
                "deployment_boundary_status": (
                    "verified_exact_creation"
                    if ok_prior_header
                    and isinstance(prior_header, dict)
                    and ok_start_header
                    and isinstance(start_header, dict)
                    and ok_prior_code
                    and prior_code == "0x"
                    and ok_start_code
                    and isinstance(start_code, str)
                    and start_code != "0x"
                    and bytecode_hash(start_code) is not None
                    and ok_boundary_state
                    and isinstance(boundary_state, str)
                    else "unavailable_or_disagreed"
                ),
                "deployment_block_hash": (
                    str(start_header.get("hash", "")).lower()
                    if isinstance(start_header, dict)
                    else None
                ),
                "deployment_code_sha256": bytecode_hash(start_code),
                "deployment_state_sha256": (
                    hashlib.sha256(str(boundary_state).lower().encode()).hexdigest()
                    if ok_boundary_state and isinstance(boundary_state, str)
                    else None
                ),
            }
            if ok_logs and isinstance(logs, list):
                row.update(log_status="available", log_count=len(logs), log_identity_sha256=canonical_log_hash(logs))
            else:
                row.update(log_status="unavailable", failure=str(logs))
            output["providers"].append(row)
        output["content_sha256"] = hashlib.sha256(
            json.dumps(output, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        json.dump(output, sys.stdout, sort_keys=True, separators=(",", ":"))
        sys.stdout.write("\n")
        return 0
    except Exception as error:
        print(f"bounded Aave archive probe failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
