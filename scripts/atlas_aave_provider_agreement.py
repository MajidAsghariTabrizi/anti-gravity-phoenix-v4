#!/usr/bin/env python3
"""Derive a fail-closed provider-agreement artifact from an exact checkpoint."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

try:
    from scripts.atlas_borrower_index import (
        EvidenceError,
        canonical_json,
        read_json,
        verify_inventory,
    )
except ModuleNotFoundError:
    from atlas_borrower_index import (  # type: ignore[no-redef]
        EvidenceError,
        canonical_json,
        read_json,
        verify_inventory,
    )


SCHEMA = "phoenix.atlas.aave-provider-agreement.v1"
CHECKPOINT_SCHEMA = "phoenix.atlas.aave-checkpoint.v1"
REQUIRED_CONTEXTS = (
    "reserve_list",
    "reserve_state",
    "borrower_activity_retained",
    "borrower_state",
    "emode_state",
)


def canonical_hash(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def sha256(value: object, name: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise EvidenceError(f"{name} is not a SHA-256 digest")
    return value


def verify_checkpoint(checkpoint: dict[str, Any]) -> None:
    if checkpoint.get("schema") != CHECKPOINT_SCHEMA:
        raise EvidenceError("checkpoint schema mismatch")
    observed = checkpoint.get("content_sha256")
    body = {key: value for key, value in checkpoint.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise EvidenceError("checkpoint content hash mismatch")
    if (
        checkpoint.get("chain_id") != 42161
        or checkpoint.get("archive_complete") is not True
        or checkpoint.get("independent_state_agreement") is not True
        or checkpoint.get("protocol_code_independent_agreement") is not True
    ):
        raise EvidenceError("checkpoint independent agreement is incomplete")
    authority = checkpoint.get("execution_authority")
    if not isinstance(authority, dict) or any(authority.values()):
        raise EvidenceError("checkpoint carries execution authority")


def build_agreement(
    inventory: dict[str, Any], checkpoint: dict[str, Any]
) -> dict[str, Any]:
    verify_inventory(inventory)
    verify_checkpoint(checkpoint)
    if inventory.get("checkpoint_content_sha256") != checkpoint["content_sha256"]:
        raise EvidenceError("inventory checkpoint binding mismatch")
    if (
        inventory.get("checkpoint_block") != checkpoint.get("checkpoint_block")
        or inventory.get("checkpoint_hash") != checkpoint.get("checkpoint_hash")
    ):
        raise EvidenceError("inventory checkpoint identity mismatch")

    headers = checkpoint.get("provider_headers")
    if not isinstance(headers, list) or len(headers) < 2:
        raise EvidenceError("two independent provider headers are required")
    providers = [item.get("provider_id") for item in headers if isinstance(item, dict)]
    if (
        len(providers) != len(headers)
        or not all(isinstance(item, str) and item for item in providers)
        or len(set(providers)) != len(providers)
    ):
        raise EvidenceError("provider header identities are invalid")

    bindings = checkpoint.get("state_bindings")
    if not isinstance(bindings, list):
        raise EvidenceError("checkpoint state bindings are missing")
    code_bindings = checkpoint.get("protocol_code_bindings")
    if not isinstance(code_bindings, list):
        raise EvidenceError("checkpoint protocol code bindings are missing")
    tail = checkpoint.get("tail_discovery")
    tail_bindings = tail.get("provider_bindings") if isinstance(tail, dict) else None
    if not isinstance(tail_bindings, list):
        raise EvidenceError("checkpoint tail provider bindings are missing")

    provider_evidence = []
    for provider in providers:
        contexts: dict[str, dict[str, Any]] = {}
        for context in REQUIRED_CONTEXTS:
            rows = [
                row
                for row in bindings
                if isinstance(row, dict)
                and row.get("provider_id") == provider
                and row.get("context") == context
            ]
            if len(rows) != 1:
                raise EvidenceError(f"provider {context} binding is not unique")
            contexts[context] = {
                "call_count": rows[0].get("call_count"),
                "result_sha256": sha256(
                    rows[0].get("result_sha256"), f"{context} result"
                ),
            }
        codes = [
            row
            for row in code_bindings
            if isinstance(row, dict) and row.get("provider_id") == provider
        ]
        tails = [
            row
            for row in tail_bindings
            if isinstance(row, dict) and row.get("provider_id") == provider
        ]
        if len(codes) != 1 or len(tails) != 1:
            raise EvidenceError("provider code or tail binding is not unique")
        code_hashes = codes[0].get("code_sha256")
        if not isinstance(code_hashes, dict):
            raise EvidenceError("provider code hashes are missing")
        for name in ("pool", "data_provider", "oracle", "pool_implementation"):
            sha256(code_hashes.get(name), f"{name} code")
        tail_hash = sha256(tails[0].get("logs_content_sha256"), "tail logs")
        evidence = {
            "block_number": checkpoint["checkpoint_block"],
            "block_hash": checkpoint["checkpoint_hash"],
            "inventory_snapshot_sha256": inventory["snapshot_sha256"],
            "checkpoint_content_sha256": checkpoint["content_sha256"],
            "contexts": contexts,
            "protocol_code_sha256": code_hashes,
            "tail_log_count": tails[0].get("log_count"),
            "tail_logs_content_sha256": tail_hash,
        }
        provider_evidence.append(
            {"provider_id": provider, "state_sha256": canonical_hash(evidence)}
        )

    state_hashes = [item["state_sha256"] for item in provider_evidence]
    if len(set(state_hashes)) != 1:
        raise EvidenceError("independent provider state hashes disagree")
    result: dict[str, Any] = {
        "schema": SCHEMA,
        "status": "agreed",
        "chain_id": 42161,
        "block_number": checkpoint["checkpoint_block"],
        "block_hash": checkpoint["checkpoint_hash"],
        "inventory_snapshot_sha256": inventory["snapshot_sha256"],
        "checkpoint_content_sha256": checkpoint["content_sha256"],
        "provider_ids": providers,
        "state_hashes": state_hashes,
        "provider_evidence": provider_evidence,
        "agreement_scope": [
            "checkpoint_header",
            "borrow_tail",
            "protocol_code",
            *REQUIRED_CONTEXTS,
        ],
        "execution_authority": {
            "signer": False,
            "bond": False,
            "bid": False,
            "submission": False,
            "capital": False,
        },
    }
    result["content_sha256"] = canonical_hash(result)
    return result


def write_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.parent / f".{path.name}.{os.getpid()}.tmp"
    temporary.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    os.replace(temporary, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--inventory", required=True)
    parser.add_argument("--checkpoint", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        agreement = build_agreement(
            read_json(args.inventory), read_json(args.checkpoint)
        )
        write_atomic(args.output, agreement)
        print(
            f"provider_agreement_status=agreed content_sha256={agreement['content_sha256']}"
        )
        return 0
    except Exception as error:
        print(f"Atlas/Aave provider agreement failed: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
