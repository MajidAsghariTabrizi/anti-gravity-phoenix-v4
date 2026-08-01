#!/usr/bin/env python3
"""Export a bounded, credential-free Camelot -> Curve revenue snapshot.

The script is intended to be streamed to the Phoenix host over BatchMode SSH.
It reads the already-reviewed provider set from the RPC gateway container and
prints only public-chain identity, state, code hashes, and exact integer quotes.
Provider URLs and container environment values are never emitted.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
import urllib.request


CHAIN_ID = "0xa4b1"
SIZES = (
    100_000_000_000_000,
    250_000_000_000_000,
    500_000_000_000_000,
    1_000_000_000_000_000,
    2_500_000_000_000_000,
    5_000_000_000_000_000,
    10_000_000_000_000_000,
)
ADDRESSES = {
    "weth": "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
    "usdc": "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
    "aave_pool": "0x794a61358d6845594f94dc1db02a252b5b4814ad",
    "camelot_factory": "0x1a3c9b1d2f0529d97f2afc5136cc23e58f1fd35b",
    "camelot_quoter": "0x0fc73040b26e9bc8514fa028d998e73a254fa76e",
    "camelot_router": "0x1f721e2e82f6676fce4ea07a5958cf098d339e18",
    "camelot_pool": "0xb1026b8e7276e7ac75410f1fcbbe21796e8f7526",
    "curve_factory": "0x9af14d26075f142eb3f292d5065eb3faa646167b",
    "curve_implementation": "0xf6841c27fe35ed7069189afd5b81513578afd7ff",
    "curve_pool": "0x85bbd07ec4d0fc23c42b6ca4af266eaec65342fb",
}
SELECTORS = {
    "pool_by_pair": "0xd9a641e1",
    "factory": "0xc45a0155",
    "token0": "0x0dfe1681",
    "token1": "0xd21220a7",
    "global_state": "0xe76c01e4",
    "liquidity": "0x1a686502",
    "coins": "0xc6610657",
    "fee": "0xddca3f43",
    "offpeg_fee_multiplier": "0x8edfdd5f",
    "balances": "0x4903b0d1",
    "get_dy": "0x5e0d443f",
    "get_implementation_address": "0x510d98a4",
    "get_coins": "0x9ac90d3d",
    "flash_premium": "0x074b2e43",
    "addresses_provider": "0x0542975c",
    "quote_exact_input_single": "0x2d9ebd1d",
}
OFFICIAL_SOURCES = {
    "camelot_deployments": "https://docs.camelot.exchange/contracts/arbitrum/one-mainnet",
    "curve_repository": "https://github.com/curvefi/stableswap-ng",
    "curve_commit": "2abe778f40206a6c0fd108a0a53ad3266cbedeee",
    "curve_deployments_path": "scripts/deployments.py",
    "curve_pool_source_path": "contracts/main/CurveStableSwapNG.vy",
}


class SnapshotError(RuntimeError):
    pass


def quantity(value: str) -> int:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise SnapshotError("RPC returned a malformed hexadecimal quantity")
    return int(value, 16)


def word(value: int) -> str:
    if value < 0 or value >= 2**256:
        raise SnapshotError("ABI word is out of range")
    return f"{value:064x}"


def address_word(address: str) -> str:
    canonical = address.lower()
    if not canonical.startswith("0x") or len(canonical) != 42:
        raise SnapshotError("address is malformed")
    return canonical[2:].rjust(64, "0")


def decoded_address(value: str) -> str:
    if not isinstance(value, str) or not value.startswith("0x") or len(value) < 66:
        raise SnapshotError("RPC returned a malformed ABI address")
    return "0x" + value[-40:].lower()


def words(value: str) -> list[int]:
    if not isinstance(value, str) or not value.startswith("0x") or (len(value) - 2) % 64:
        raise SnapshotError("RPC returned malformed ABI words")
    return [int(value[index : index + 64], 16) for index in range(2, len(value), 64)]


def signed(value: int, bits: int = 256) -> int:
    return value - 2**bits if value >= 2 ** (bits - 1) else value


def dynamic_addresses(value: str) -> list[str]:
    decoded = words(value)
    if len(decoded) < 2 or decoded[0] != 32 or decoded[1] > len(decoded) - 2:
        raise SnapshotError("RPC returned a malformed dynamic address array")
    return ["0x" + f"{item:064x}"[-40:] for item in decoded[2 : 2 + decoded[1]]]


class Provider:
    def __init__(self, url: str, index: int) -> None:
        self._url = url
        self.index = index
        self._request_id = 0

    def call(self, method: str, params: list[object]) -> object:
        allowed = {
            "eth_blockNumber",
            "eth_call",
            "eth_chainId",
            "eth_getBlockByNumber",
            "eth_getCode",
            "eth_maxPriorityFeePerGas",
            "web3_sha3",
        }
        if method not in allowed:
            raise SnapshotError("RPC method is outside the read-only allowlist")
        self._request_id += 1
        body = json.dumps(
            {"jsonrpc": "2.0", "id": self._request_id, "method": method, "params": params},
            separators=(",", ":"),
        ).encode()
        request = urllib.request.Request(
            self._url,
            data=body,
            headers={
                "Content-Type": "application/json",
                "User-Agent": "anti-gravity-phoenix-rpc-gateway/4",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                payload = json.load(response)
        except Exception as error:
            raise SnapshotError(f"provider[{self.index}] transport failure") from error
        if payload.get("error") is not None or "result" not in payload:
            raise SnapshotError(f"provider[{self.index}] RPC failure for {method}")
        return payload["result"]

    def eth_call(self, to: str, data: str, block_tag: str) -> str:
        result = self.call("eth_call", [{"to": to, "data": data}, block_tag])
        if not isinstance(result, str):
            raise SnapshotError(f"provider[{self.index}] returned a malformed eth_call result")
        return result


def load_providers(container: str) -> list[Provider]:
    result = subprocess.run(
        ["sudo", "-n", "docker", "inspect", "--format", "{{json .Config.Env}}", container],
        check=True,
        capture_output=True,
        text=True,
    )
    environment = json.loads(result.stdout)
    values = dict(item.split("=", 1) for item in environment if "=" in item)
    raw = values.get("RPC_PROVIDER_URLS", "")
    urls = json.loads(raw) if raw.startswith("[") else [part.strip() for part in raw.split(",") if part.strip()]
    if len(urls) < 2 or not all(isinstance(url, str) and url.startswith(("http://", "https://")) for url in urls):
        raise SnapshotError("fewer than two reviewed RPC providers are configured")
    return [Provider(url, index) for index, url in enumerate(urls[:2])]


def code_metadata(provider: Provider, address: str, block_tag: str) -> dict[str, object]:
    code = provider.call("eth_getCode", [address, block_tag])
    if not isinstance(code, str) or not code.startswith("0x") or len(code) <= 2:
        raise SnapshotError(f"provider[{provider.index}] returned empty code for a bound identity")
    code_hash = provider.call("web3_sha3", [code])
    return {"address": address, "code_bytes": (len(code) - 2) // 2, "code_hash": str(code_hash).lower()}


def build_provider_snapshot(provider: Provider, block_number: int) -> dict[str, object]:
    tag = hex(block_number)
    block = provider.call("eth_getBlockByNumber", [tag, False])
    if not isinstance(block, dict) or block.get("number") != tag:
        raise SnapshotError(f"provider[{provider.index}] returned a malformed block")
    if provider.call("eth_chainId", []) != CHAIN_ID:
        raise SnapshotError(f"provider[{provider.index}] returned the wrong chain")

    weth = ADDRESSES["weth"]
    usdc = ADDRESSES["usdc"]
    pair_args = address_word(weth) + address_word(usdc)
    camelot_pool = ADDRESSES["camelot_pool"]
    curve_pool = ADDRESSES["curve_pool"]
    curve_factory = ADDRESSES["curve_factory"]

    global_state = words(provider.eth_call(camelot_pool, SELECTORS["global_state"], tag))
    if len(global_state) < 3:
        raise SnapshotError("Camelot globalState result is incomplete")
    camelot = {
        "factory_pool": decoded_address(
            provider.eth_call(ADDRESSES["camelot_factory"], SELECTORS["pool_by_pair"] + pair_args, tag)
        ),
        "pool_factory": decoded_address(provider.eth_call(camelot_pool, SELECTORS["factory"], tag)),
        "token0": decoded_address(provider.eth_call(camelot_pool, SELECTORS["token0"], tag)),
        "token1": decoded_address(provider.eth_call(camelot_pool, SELECTORS["token1"], tag)),
        "sqrt_price_x96": str(global_state[0]),
        "tick": signed(global_state[1]),
        "current_fee_millionths": global_state[2],
        "liquidity": str(quantity(provider.eth_call(camelot_pool, SELECTORS["liquidity"], tag))),
    }
    factory_coins = dynamic_addresses(
        provider.eth_call(curve_factory, SELECTORS["get_coins"] + address_word(curve_pool), tag)
    )
    if len(factory_coins) < 2:
        raise SnapshotError("Curve factory get_coins result is incomplete")
    curve = {
        "factory_token0": factory_coins[0],
        "factory_token1": factory_coins[1],
        "implementation": decoded_address(
            provider.eth_call(
                curve_factory,
                SELECTORS["get_implementation_address"] + address_word(curve_pool),
                tag,
            )
        ),
        "token0": decoded_address(provider.eth_call(curve_pool, SELECTORS["coins"] + word(0), tag)),
        "token1": decoded_address(provider.eth_call(curve_pool, SELECTORS["coins"] + word(1), tag)),
        "fee_1e10": str(quantity(provider.eth_call(curve_pool, SELECTORS["fee"], tag))),
        "offpeg_fee_multiplier_1e10": str(
            quantity(provider.eth_call(curve_pool, SELECTORS["offpeg_fee_multiplier"], tag))
        ),
        "balance0": str(quantity(provider.eth_call(curve_pool, SELECTORS["balances"] + word(0), tag))),
        "balance1": str(quantity(provider.eth_call(curve_pool, SELECTORS["balances"] + word(1), tag))),
    }
    aave = {
        "flash_premium_total_bps": quantity(
            provider.eth_call(ADDRESSES["aave_pool"], SELECTORS["flash_premium"], tag)
        ),
        "addresses_provider": decoded_address(
            provider.eth_call(ADDRESSES["aave_pool"], SELECTORS["addresses_provider"], tag)
        ),
    }

    ladder: list[dict[str, str | int]] = []
    for amount in SIZES:
        camelot_result = words(
            provider.eth_call(
                ADDRESSES["camelot_quoter"],
                SELECTORS["quote_exact_input_single"]
                + address_word(weth)
                + address_word(usdc)
                + word(amount)
                + word(0),
                tag,
            )
        )
        if len(camelot_result) != 2:
            raise SnapshotError("Camelot quoter returned an unexpected ABI shape")
        usdc_out, fee_millionths = camelot_result
        curve_out = quantity(
            provider.eth_call(
                curve_pool,
                SELECTORS["get_dy"] + word(1) + word(0) + word(usdc_out),
                tag,
            )
        )
        ladder.append(
            {
                "amount_in_wei": str(amount),
                "camelot_usdc_out": str(usdc_out),
                "camelot_fee_millionths": fee_millionths,
                "curve_weth_out_wei": str(curve_out),
                "gross_profit_wei": str(curve_out - amount),
            }
        )

    code = {name: code_metadata(provider, address, tag) for name, address in ADDRESSES.items()}
    return {
        "provider_index": provider.index,
        "head_at_start": quantity(provider.call("eth_blockNumber", [])),
        "block_number": block_number,
        "block_hash": str(block["hash"]).lower(),
        "block_timestamp": dt.datetime.fromtimestamp(quantity(block["timestamp"]), tz=dt.UTC).isoformat(),
        "base_fee_per_gas_wei": str(quantity(block["baseFeePerGas"])),
        "max_priority_fee_per_gas_wei": str(quantity(provider.call("eth_maxPriorityFeePerGas", []))),
        "camelot": camelot,
        "curve": curve,
        "aave": aave,
        "code_identities": code,
        "seven_size_ladder": ladder,
    }


def invariant_projection(snapshot: dict[str, object]) -> dict[str, object]:
    return {
        key: snapshot[key]
        for key in (
            "block_number",
            "block_hash",
            "base_fee_per_gas_wei",
            "camelot",
            "curve",
            "aave",
            "code_identities",
            "seven_size_ladder",
        )
    }


def validate_bound_identity(snapshot: dict[str, object]) -> None:
    camelot = snapshot["camelot"]
    curve = snapshot["curve"]
    aave = snapshot["aave"]
    if not isinstance(camelot, dict) or not isinstance(curve, dict) or not isinstance(aave, dict):
        raise SnapshotError("snapshot identity shape is invalid")
    expected = {
        "camelot_factory_pool": ADDRESSES["camelot_pool"],
        "camelot_pool_factory": ADDRESSES["camelot_factory"],
        "camelot_token0": ADDRESSES["weth"],
        "camelot_token1": ADDRESSES["usdc"],
        "curve_factory_token0": ADDRESSES["weth"],
        "curve_factory_token1": ADDRESSES["usdc"],
        "curve_implementation": ADDRESSES["curve_implementation"],
        "curve_token0": ADDRESSES["weth"],
        "curve_token1": ADDRESSES["usdc"],
    }
    observed = {
        "camelot_factory_pool": camelot["factory_pool"],
        "camelot_pool_factory": camelot["pool_factory"],
        "camelot_token0": camelot["token0"],
        "camelot_token1": camelot["token1"],
        "curve_factory_token0": curve["factory_token0"],
        "curve_factory_token1": curve["factory_token1"],
        "curve_implementation": curve["implementation"],
        "curve_token0": curve["token0"],
        "curve_token1": curve["token1"],
    }
    mismatches = sorted(key for key, value in expected.items() if observed.get(key) != value)
    if aave["flash_premium_total_bps"] != 5:
        mismatches.append("aave_flash_premium_total_bps")
    if mismatches:
        details = ",".join(f"{key}={observed.get(key)}" for key in mismatches)
        raise SnapshotError("bound on-chain identity mismatch: " + details)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--container", default="app-rpc-gateway-1")
    parser.add_argument("--block", type=int)
    args = parser.parse_args()
    try:
        providers = load_providers(args.container)
        heads = [quantity(provider.call("eth_blockNumber", [])) for provider in providers]
        block_number = args.block if args.block is not None else min(heads)
        if block_number <= 0 or block_number > min(heads):
            raise SnapshotError("requested block is outside the common provider range")
        snapshots = [build_provider_snapshot(provider, block_number) for provider in providers]
        for snapshot in snapshots:
            validate_bound_identity(snapshot)
        if invariant_projection(snapshots[0]) != invariant_projection(snapshots[1]):
            raise SnapshotError("reviewed RPC providers disagree on bound state or quotes")
        result = {
            "schema": "phoenix.camelot-curve-revenue-snapshot.v1",
            "captured_at": dt.datetime.now(dt.UTC).isoformat(),
            "chain_id": 42161,
            "common_block_number": block_number,
            "common_block_hash": snapshots[0]["block_hash"],
            "reviewed_provider_count": len(providers),
            "provider_agreement": True,
            "official_sources": OFFICIAL_SOURCES,
            "providers": snapshots,
        }
        json.dump(result, sys.stdout, separators=(",", ":"), sort_keys=True)
        sys.stdout.write("\n")
        return 0
    except Exception as error:
        print(f"bounded Camelot-Curve snapshot failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
