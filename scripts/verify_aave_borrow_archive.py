#!/usr/bin/env python3
"""Verify and manifest a deterministic Aave V3 Borrow archive.

The verifier never accepts provider inclusion as completeness. It validates
every cached range, rejects duplicate canonical identities, optionally
re-fetches every range from one reviewed authority, and binds range boundary
headers. Endpoint values are consumed only through protected environment
references or the credential-redacting SSH container bridge.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    from scripts.export_aave_borrow_discovery import (
        BORROW_TOPIC,
        CHAIN_ID,
        POOL,
        Provider,
        SSHContainerProvider,
        canonical_hash,
        get_logs,
        header,
        load_cached_chunk,
        provider_urls,
        sanitize_log,
        write_json_atomic,
    )
except ModuleNotFoundError:
    from export_aave_borrow_discovery import (  # type: ignore[no-redef]
        BORROW_TOPIC,
        CHAIN_ID,
        POOL,
        Provider,
        SSHContainerProvider,
        canonical_hash,
        get_logs,
        header,
        load_cached_chunk,
        provider_urls,
        sanitize_log,
        write_json_atomic,
    )


STATE_SCHEMA = "phoenix.atlas.aave-borrow-archive-state.v1"
MANIFEST_SCHEMA = "phoenix.atlas.aave-borrow-archive-manifest.v1"
DEFAULT_SECONDARY_ENV = "PHOENIX_ATLAS_ARCHIVE_SECONDARY_RPC_URL"


class VerificationError(RuntimeError):
    pass


def validate_sha256(value: object, name: str) -> str:
    if not isinstance(value, str) or len(value) != 64:
        raise VerificationError(f"{name} must be a SHA-256 digest")
    try:
        int(value, 16)
    except ValueError as error:
        raise VerificationError(f"{name} must be hexadecimal") from error
    return value.lower()


def load_hash_bound(path: Path, schema: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise VerificationError(f"{path.name} is unreadable") from error
    if not isinstance(value, dict) or value.get("schema") != schema:
        raise VerificationError(f"{path.name} schema mismatch")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise VerificationError(f"{path.name} content hash mismatch")
    return value


def expected_ranges(start: int, end: int, chunk_size: int) -> list[tuple[int, int]]:
    if start < 0 or end < start or chunk_size < 1:
        raise VerificationError("archive bounds are invalid")
    ranges = []
    cursor = start
    while cursor <= end:
        chunk_end = min(cursor + chunk_size - 1, end)
        ranges.append((cursor, chunk_end))
        cursor = chunk_end + 1
    return ranges


def validate_state(value: dict[str, Any], require_complete: bool) -> list[tuple[int, int]]:
    if (
        value.get("chain_id") != CHAIN_ID
        or str(value.get("pool", "")).lower() != POOL
        or str(value.get("borrow_topic", "")).lower() != BORROW_TOPIC
    ):
        raise VerificationError("archive state identity mismatch")
    ranges = expected_ranges(
        int(value["start_block"]),
        int(value["checkpoint_block"]),
        int(value["chunk_size"]),
    )
    if value.get("expected_chunk_count") != len(ranges):
        raise VerificationError("expected chunk count mismatch")
    chunks = value.get("chunks")
    if not isinstance(chunks, list):
        raise VerificationError("state chunks are invalid")
    if value.get("completed_chunk_count") != len(chunks):
        raise VerificationError("completed chunk count mismatch")
    for index, chunk in enumerate(chunks):
        if not isinstance(chunk, dict):
            raise VerificationError("state chunk entry is invalid")
        expected_start, expected_end = ranges[index]
        if (
            chunk.get("start_block") != expected_start
            or chunk.get("end_block") != expected_end
        ):
            raise VerificationError("state chunk coverage is non-contiguous")
        validate_sha256(chunk.get("content_sha256"), "chunk content hash")
    if require_complete:
        if value.get("archive_complete") is not True or len(chunks) != len(ranges):
            raise VerificationError("archive state is incomplete")
        if value.get("next_start_block") != int(value["checkpoint_block"]) + 1:
            raise VerificationError("complete archive next cursor mismatch")
    return ranges


def log_identity(log: dict[str, object]) -> tuple[object, ...]:
    block_hash = str(log.get("block_hash", "")).lower()
    transaction_hash = str(log.get("transaction_hash", "")).lower()
    log_index = log.get("log_index")
    if (
        len(block_hash) != 66
        or len(transaction_hash) != 66
        or not isinstance(log_index, int)
        or log_index < 0
    ):
        raise VerificationError("canonical log identity is invalid")
    return (
        CHAIN_ID,
        block_hash,
        transaction_hash,
        log_index,
        POOL,
        BORROW_TOPIC,
    )


def log_set_hash(logs: list[dict[str, object]]) -> str:
    return canonical_hash([list(log_identity(log)) for log in logs])


def borrower_set_hash(logs: list[dict[str, object]]) -> str:
    borrowers = sorted({str(log["borrower"]).lower() for log in logs})
    return canonical_hash(borrowers)


def request_profile_hash(start: int, end: int) -> str:
    return canonical_hash(
        {
            "method": "eth_getLogs",
            "chain_id": CHAIN_ID,
            "address": POOL,
            "topics": [BORROW_TOPIC],
            "from_block": start,
            "to_block": end,
            "sort": ["block_number", "transaction_index", "log_index"],
        }
    )


def mtime_utc(path: Path) -> str:
    return datetime.fromtimestamp(path.stat().st_mtime, timezone.utc).isoformat()


def configured_provider(args: argparse.Namespace) -> object | None:
    ssh_selected = any(
        value is not None
        for value in (
            args.ssh_provider_host,
            args.ssh_provider_identity,
            args.ssh_provider_container,
        )
    )
    if ssh_selected:
        if (
            not args.ssh_provider_host
            or args.ssh_provider_identity is None
            or not args.ssh_provider_container
            or args.provider_env
        ):
            raise VerificationError("SSH provider arguments are incomplete")
        return SSHContainerProvider(
            args.provider_id,
            args.ssh_executable,
            args.ssh_provider_host,
            args.ssh_provider_port,
            args.ssh_provider_identity,
            args.ssh_provider_known_hosts,
            args.ssh_provider_container,
            args.ssh_provider_index,
        )
    if not args.provider_env:
        return None
    urls = provider_urls(None, [args.provider_env])
    return Provider(args.provider_id, urls[0])


def verify_discovery(
    discovery_path: Path | None,
    state: dict[str, Any],
    total_logs: int,
    borrowers: set[str],
) -> str | None:
    if discovery_path is None:
        return None
    discovery = load_hash_bound(
        discovery_path, "phoenix.atlas.aave-borrow-discovery.v1"
    )
    if (
        discovery.get("archive_complete") is not True
        or discovery.get("start_block") != state["start_block"]
        or discovery.get("checkpoint_block") != state["checkpoint_block"]
        or discovery.get("log_count") != total_logs
        or discovery.get("borrower_count") != len(borrowers)
        or discovery.get("borrowers") != sorted(borrowers)
    ):
        raise VerificationError("final discovery artifact disagrees with chunks")
    return str(discovery["content_sha256"])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--state-file", required=True, type=Path)
    parser.add_argument("--chunk-cache-dir", required=True, type=Path)
    parser.add_argument("--discovery-file", type=Path)
    parser.add_argument("--output-manifest", required=True, type=Path)
    parser.add_argument("--build-code-sha256", required=True)
    parser.add_argument("--require-complete", action="store_true")
    parser.add_argument("--revalidate-rpc", action="store_true")
    parser.add_argument("--require-boundary-headers", action="store_true")
    parser.add_argument("--provider-env")
    parser.add_argument("--provider-id", default="reviewed-authority")
    parser.add_argument("--ssh-executable", default="ssh")
    parser.add_argument("--ssh-provider-host")
    parser.add_argument("--ssh-provider-port", type=int, default=22)
    parser.add_argument("--ssh-provider-identity", type=Path)
    parser.add_argument("--ssh-provider-known-hosts", type=Path)
    parser.add_argument("--ssh-provider-container")
    parser.add_argument("--ssh-provider-index", type=int, default=0)
    args = parser.parse_args()

    provider = None
    try:
        build_code_sha = validate_sha256(
            args.build_code_sha256, "build code SHA-256"
        )
        state = load_hash_bound(args.state_file, STATE_SCHEMA)
        ranges = validate_state(state, args.require_complete)
        completed = int(state["completed_chunk_count"])
        provider = configured_provider(args)
        if (args.revalidate_rpc or args.require_boundary_headers) and provider is None:
            raise VerificationError("reviewed provider is required")
        if provider is not None:
            chain = provider.call("eth_chainId", [])
            if int(str(chain), 16) != CHAIN_ID:
                raise VerificationError("provider chain disagreement")

        seen: set[tuple[object, ...]] = set()
        borrowers: set[str] = set()
        chunk_rows = []
        total_logs = 0
        total_duplicates = 0
        for index, (start, end) in enumerate(ranges[:completed]):
            path = args.chunk_cache_dir / f"{start}-{end}.json"
            logs = load_cached_chunk(path, start, end)
            logs.sort(
                key=lambda item: (
                    item["block_number"],
                    item["transaction_index"],
                    item["log_index"],
                )
            )
            local_identities = [log_identity(log) for log in logs]
            local_duplicate_count = len(local_identities) - len(set(local_identities))
            if local_duplicate_count:
                raise VerificationError("duplicate identity inside chunk")
            overlap = seen.intersection(local_identities)
            if overlap:
                raise VerificationError("duplicate identity across chunks")
            seen.update(local_identities)
            borrowers.update(str(log["borrower"]).lower() for log in logs)

            authority = "cache_only"
            retry_count = 0
            if args.revalidate_rpc:
                before_retries = int(getattr(provider, "retry_count", 0))
                remote_logs = [
                    sanitize_log(log) for log in get_logs(provider, start, end)
                ]
                remote_logs.sort(
                    key=lambda item: (
                        item["block_number"],
                        item["transaction_index"],
                        item["log_index"],
                    )
                )
                retry_count = int(getattr(provider, "retry_count", 0)) - before_retries
                if log_set_hash(remote_logs) != log_set_hash(logs):
                    raise VerificationError(
                        f"provider log disagreement for range {start}-{end}"
                    )
                authority = "provider_revalidated"

            boundary_headers = None
            if provider is not None:
                boundary_headers = {
                    "start": header(provider, start),
                    "end": header(provider, end),
                }
            elif args.require_boundary_headers:
                raise VerificationError("range boundary headers are required")

            first_log = None
            last_log = None
            if logs:
                first_log = {
                    "block_number": logs[0]["block_number"],
                    "block_hash": logs[0]["block_hash"],
                }
                last_log = {
                    "block_number": logs[-1]["block_number"],
                    "block_hash": logs[-1]["block_hash"],
                }
            chunk_rows.append(
                {
                    "index": index,
                    "range_start": start,
                    "range_end": end,
                    "provider_id": args.provider_id if provider is not None else state["provider_bindings"][0]["provider_id"],
                    "authority": authority,
                    "request_profile_sha256": request_profile_hash(start, end),
                    "response_count": len(logs),
                    "range_boundary_headers": boundary_headers,
                    "first_log_block": first_log,
                    "last_log_block": last_log,
                    "sorted_log_sha256": log_set_hash(logs),
                    "borrower_set_sha256": borrower_set_hash(logs),
                    "duplicate_count": local_duplicate_count,
                    "retry_count": retry_count,
                    "completion_timestamp": mtime_utc(path),
                    "chunk_content_sha256": state["chunks"][index]["content_sha256"],
                }
            )
            total_logs += len(logs)
            total_duplicates += local_duplicate_count

        coverage_gaps = [
            {"start_block": start, "end_block": end}
            for start, end in ranges[completed:]
        ]
        final_archive_sha = verify_discovery(
            args.discovery_file, state, total_logs, borrowers
        )
        manifest: dict[str, object] = {
            "schema": MANIFEST_SCHEMA,
            "chain_id": CHAIN_ID,
            "contract_address": POOL,
            "event_topic0": BORROW_TOPIC,
            "verified_interval": {
                "start_block": state["start_block"],
                "end_block": state["checkpoint_block"],
                "chunk_size": state["chunk_size"],
                "expected_chunk_count": len(ranges),
            },
            "archive_complete": not coverage_gaps and state["archive_complete"] is True,
            "independent_validation": bool(args.revalidate_rpc),
            "provider_id": args.provider_id if provider is not None else None,
            "chunks": chunk_rows,
            "total_logs": total_logs,
            "total_unique_borrowers": len(borrowers),
            "total_duplicates_rejected": total_duplicates,
            "coverage_gaps": coverage_gaps,
            "build_code_sha256": build_code_sha,
            "final_archive_sha256": final_archive_sha,
        }
        manifest["content_sha256"] = canonical_hash(manifest)
        write_json_atomic(args.output_manifest, manifest)
        print(
            "archive_manifest_complete="
            + str(manifest["archive_complete"]).lower()
            + f" chunks={len(chunk_rows)}/{len(ranges)} logs={total_logs} "
            + f"borrowers={len(borrowers)} manifest_sha256={manifest['content_sha256']}",
            file=sys.stderr,
        )
        return 0
    except Exception as error:
        print(f"Aave archive verification failed: {error}", file=sys.stderr)
        return 1
    finally:
        close = getattr(provider, "close", None)
        if close is not None:
            close()


if __name__ == "__main__":
    raise SystemExit(main())
