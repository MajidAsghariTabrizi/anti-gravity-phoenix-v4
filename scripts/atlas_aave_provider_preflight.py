#!/usr/bin/env python3
"""Sanitized, bounded NOWNodes/Slot 0 current-state preflight."""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from pathlib import Path
from typing import Any

from scripts.export_aave_borrow_discovery import (
    BridgeRequestError,
    ProviderDiagnosticError,
    SSHContainerProvider,
)
from scripts.export_aave_checkpoint import (
    CHAIN_ID,
    POOL,
    POOL_IMPLEMENTATION,
    POOL_IMPLEMENTATION_SLOT,
    SELECTORS,
    ExportError,
    canonical_hash,
    current_state_proof_policy,
    finalized_checkpoint,
    provider_request_usage,
)


def _digest_hex(value: Any) -> str:
    if not isinstance(value, str) or not value.startswith("0x"):
        raise ExportError("preflight state response is invalid")
    try:
        payload = bytes.fromhex(value[2:])
    except ValueError as error:
        raise ExportError("preflight state response is invalid") from error
    return hashlib.sha256(payload).hexdigest()


def _state_sample(provider: SSHContainerProvider, block: int) -> dict[str, str]:
    code = provider.call("eth_getCode", [POOL, hex(block)], attempts=1)
    storage = provider.call(
        "eth_getStorageAt", [POOL, POOL_IMPLEMENTATION_SLOT, hex(block)], attempts=1
    )
    reserves = provider.call(
        "eth_call",
        [{"to": POOL, "data": "0x" + SELECTORS["get_reserves_list"]}, hex(block)],
        attempts=1,
    )
    if (
        not isinstance(code, str)
        or code == "0x"
        or not isinstance(storage, str)
        or len(storage) != 66
        or not isinstance(reserves, str)
        or reserves == "0x"
        or "0x" + storage[-40:].lower() != POOL_IMPLEMENTATION
    ):
        raise ExportError("preflight protocol state is incomplete")
    return {
        "code_sha256": _digest_hex(code),
        "implementation_storage_sha256": _digest_hex(storage),
        "reserves_call_sha256": _digest_hex(reserves),
        "implementation_storage_word": str(storage).lower(),
    }


def run(args: argparse.Namespace) -> dict[str, Any]:
    providers: list[SSHContainerProvider] = []
    try:
        primary = SSHContainerProvider(
            "production-nownodes-arbitrum",
            args.ssh_executable,
            args.ssh_provider_host,
            args.ssh_provider_port,
            args.ssh_provider_identity,
            args.ssh_provider_known_hosts,
            args.ssh_provider_container,
            0,
            authenticated=True,
        )
        providers.append(primary)
        peer = SSHContainerProvider(
            "production-slot-0",
            args.ssh_executable,
            args.ssh_provider_host,
            args.ssh_provider_port,
            args.ssh_provider_identity,
            args.ssh_provider_known_hosts,
            args.ssh_provider_container,
            0,
        )
        providers.append(peer)
        for provider in providers:
            provider.set_diagnostic_context("chain_identity")
            if int(str(provider.call("eth_chainId", [], attempts=1)), 16) != CHAIN_ID:
                raise ExportError("provider chain identity disagreement")
        block, finalized_heads, headers = finalized_checkpoint(providers)
        if len({item["checkpoint"]["hash"] for item in headers}) != 1:
            raise ExportError("preflight exact block hash disagreement")
        if len({item["provider_reference_sha256"] for item in headers}) != 2:
            raise ExportError("preflight provider independence is absent")
        peer.set_diagnostic_context("peer_state_sample")
        peer_sample = _state_sample(peer, block)
        round_hashes: list[str] = []
        round_durations_ms: list[int] = []
        primary_words: list[str] = []
        for stability_round in range(1, 11):
            primary.set_diagnostic_context(
                f"nownodes_stability_round_{stability_round}", stability_round
            )
            started = time.monotonic_ns()
            exact_header = primary.call(
                "eth_getBlockByNumber", [hex(block), False], attempts=1
            )
            if (
                not isinstance(exact_header, dict)
                or str(exact_header.get("hash", "")).lower()
                != headers[0]["checkpoint"]["hash"]
            ):
                raise ExportError("NOWNodes exact block stability failed")
            sample = _state_sample(primary, block)
            primary_words.append(sample.pop("implementation_storage_word"))
            round_hashes.append(canonical_hash(sample))
            round_durations_ms.append((time.monotonic_ns() - started) // 1_000_000)
        peer_word = peer_sample.pop("implementation_storage_word")
        if len(set(round_hashes)) != 1 or canonical_hash(peer_sample) != round_hashes[0]:
            raise ExportError("independent direct state agreement failed")
        if len(set(primary_words + [peer_word])) != 1:
            raise ExportError("implementation storage agreement failed")
        proof_policy = current_state_proof_policy(
            providers, headers, block, [primary_words[0], peer_word]
        )
        output = {
            "schema": "phoenix.atlas.aave-provider-preflight.v1",
            "chain_id": CHAIN_ID,
            "checkpoint_block": block,
            "checkpoint_hash": headers[0]["checkpoint"]["hash"],
            "finalized_heads": finalized_heads,
            "provider_headers": headers,
            "authenticated_provider_secret_present": True,
            "authenticated_provider_secret_value_observed": False,
            "nownodes_stability_rounds_required": 10,
            "nownodes_stability_rounds_passed": 10,
            "nownodes_state_sample_sha256": round_hashes[0],
            "nownodes_round_duration_ms": round_durations_ms,
            "direct_state_independent_agreement": True,
            "proof_policy": proof_policy,
            "provider_request_usage": provider_request_usage(providers),
            "execution_authority": False,
        }
        output["content_sha256"] = canonical_hash(output)
        return output
    finally:
        for provider in providers:
            provider.close()


def failure_artifact(error: Exception) -> dict[str, Any]:
    artifact: dict[str, Any] = {
        "schema": "phoenix.atlas.aave-provider-preflight-error.v1",
        "status": "failed_closed",
        "error_class": type(error).__name__,
        "execution_authority": False,
    }
    if isinstance(error, ProviderDiagnosticError):
        artifact.update(error.sanitized_evidence())
    else:
        artifact["failure_class"] = "preflight_invariant_failed"
    return artifact


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ssh-executable", default="ssh")
    parser.add_argument("--ssh-provider-host", required=True)
    parser.add_argument("--ssh-provider-port", type=int, default=22)
    parser.add_argument("--ssh-provider-identity", required=True, type=Path)
    parser.add_argument("--ssh-provider-known-hosts", type=Path)
    parser.add_argument("--ssh-provider-container", required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(run(args), sort_keys=True, separators=(",", ":")))
        return 0
    except Exception as error:
        print(
            json.dumps(
                failure_artifact(error),
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
