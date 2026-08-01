#!/usr/bin/env python3
"""Credential-redacting capability probe for canonical Aave archive logs."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import urllib.error
import urllib.request


CHAIN_ID = 42161
POOL = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
MAX_PROVIDERS = 8
MAX_SPAN = 50_000_000


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
        if method not in {"eth_chainId", "eth_blockNumber", "eth_getLogs"}:
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", required=True)
    parser.add_argument("--from-block", type=int, required=True)
    parser.add_argument("--to-block", type=int, required=True)
    args = parser.parse_args()
    if args.from_block < 0 or args.to_block < args.from_block:
        raise SystemExit("invalid probe bounds")
    if args.to_block - args.from_block + 1 > MAX_SPAN:
        raise SystemExit("probe span exceeds bound")

    output = {"schema": "phoenix.atlas.aave-archive-capability.v1", "chain_id": CHAIN_ID, "providers": []}
    try:
        for index, url in enumerate(provider_urls(args.container), 1):
            provider = Provider(f"reviewed-provider-{index}", url)
            ok_chain, chain = provider.call("eth_chainId", [])
            ok_head, head = provider.call("eth_blockNumber", [])
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
