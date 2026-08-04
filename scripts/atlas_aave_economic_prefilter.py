#!/usr/bin/env python3
"""Run a cheap, discovery-only Aave borrower economic prefilter.

The prefilter performs one exact-block getUserAccountData call per borrower
through the authenticated operational provider.  It deliberately does not
reconstruct reserves, prove storage, or grant candidate/execution authority.
"""

from __future__ import annotations

import argparse
import json
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    from scripts.export_aave_borrow_discovery import (
        ProviderDiagnosticError,
        SSHContainerProvider,
    )
    from scripts.export_aave_checkpoint import (
        AUTHORITY_CURRENT_STATE,
        CHAIN_ID,
        POOL,
        SELECTORS,
        ExportError,
        bind_hash,
        call_data,
        canonical_hash,
        encode_address,
        validate_discovery,
        word_uint,
        words,
        write_private_json,
    )
except ModuleNotFoundError:
    from export_aave_borrow_discovery import (  # type: ignore[no-redef]
        ProviderDiagnosticError,
        SSHContainerProvider,
    )
    from export_aave_checkpoint import (  # type: ignore[no-redef]
        AUTHORITY_CURRENT_STATE,
        CHAIN_ID,
        POOL,
        SELECTORS,
        ExportError,
        bind_hash,
        call_data,
        canonical_hash,
        encode_address,
        validate_discovery,
        word_uint,
        words,
        write_private_json,
    )


PREFILTER_STATE_SCHEMA = "phoenix.atlas.aave-economic-prefilter-state.v1"
PREFILTER_SCHEMA = "phoenix.atlas.aave-economic-prefilter.v1"
PREFILTER_CONTEXT_SCHEMA = "phoenix.atlas.aave-economic-prefilter-context.v1"
SCREEN_COHORT_SCHEMA = "phoenix.atlas.aave-screen-cohort.v1"
MONITORING_SCHEMA = "phoenix.atlas.aave-prefilter-monitoring.v1"
PREFLIGHT_SCHEMA = "phoenix.atlas.aave-provider-preflight.v1"
BUCKETS = (
    "no_debt",
    "debt_safe",
    "watch",
    "urgent",
    "liquidatable",
    "incomplete",
)
WAD = 10**18
WATCH_HF_WAD = 1_100_000_000_000_000_000
URGENT_HF_WAD = 1_020_000_000_000_000_000
MAX_BATCH_SIZE = 20_000
MAX_DAILY_ADDRESSES = 20_000
MAX_TOTAL_BROAD_ITEMS = 250_000
HARD_ITEMS_PER_ADDRESS_MICROS = 1_250_000
WARNING_ITEMS_PER_ADDRESS_MICROS = 1_200_000
TARGET_ITEMS_PER_ADDRESS_MICROS = 1_100_000


def _digest(value: Any, name: str) -> str:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise ExportError(f"{name} must be a SHA-256 digest")
    return value


def validate_preflight_context(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExportError("provider preflight context must be an object")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("provider preflight context hash mismatch")
    if (
        value.get("schema") != PREFLIGHT_SCHEMA
        or value.get("chain_id") != CHAIN_ID
        or value.get("execution_authority") is not False
        or value.get("direct_state_independent_agreement") is not True
        or value.get("nownodes_stability_rounds_passed") != 10
        or not isinstance(value.get("proof_policy"), dict)
    ):
        raise ExportError("provider preflight context is not authoritative")
    headers = value.get("provider_headers")
    if not isinstance(headers, list) or len(headers) != 2:
        raise ExportError("provider preflight identities are incomplete")
    identities = {
        item.get("provider_id"): item
        for item in headers
        if isinstance(item, dict)
    }
    if set(identities) != {
        "production-nownodes-arbitrum",
        "production-slot-0",
    }:
        raise ExportError("provider preflight identities are invalid")
    _digest(observed, "provider preflight content hash")
    for item in identities.values():
        _digest(item.get("provider_reference_sha256"), "provider reference")
        checkpoint = item.get("checkpoint")
        if not isinstance(checkpoint, dict) or len(str(checkpoint.get("state_root", ""))) != 66:
            raise ExportError("provider preflight checkpoint is incomplete")
    return value


def initial_state(discovery: dict[str, Any]) -> dict[str, Any]:
    return bind_hash(
        {
            "schema": PREFILTER_STATE_SCHEMA,
            "chain_id": CHAIN_ID,
            "pool": POOL,
            "authority_mode": AUTHORITY_CURRENT_STATE,
            "discovery_content_sha256": discovery["content_sha256"],
            "source_address_count": len(discovery["borrowers"]),
            "next_address_index": 0,
            "completed_batches": 0,
            "total_nownodes_json_rpc_items": 0,
            "total_nownodes_transport_requests": 0,
            "total_retries": 0,
            "daily_usage": {},
            "batch_artifacts": [],
            "discovery_only": True,
            "candidate_authority": False,
            "execution_authority": False,
        }
    )


def validate_state(value: Any, discovery: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExportError("prefilter state must be an object")
    observed = value.get("content_sha256")
    body = {key: item for key, item in value.items() if key != "content_sha256"}
    if observed != canonical_hash(body):
        raise ExportError("prefilter state hash mismatch")
    if (
        value.get("schema") != PREFILTER_STATE_SCHEMA
        or value.get("chain_id") != CHAIN_ID
        or str(value.get("pool", "")).lower() != POOL
        or value.get("authority_mode") != AUTHORITY_CURRENT_STATE
        or value.get("discovery_content_sha256") != discovery["content_sha256"]
        or value.get("source_address_count") != len(discovery["borrowers"])
        or value.get("discovery_only") is not True
        or value.get("candidate_authority") is not False
        or value.get("execution_authority") is not False
    ):
        raise ExportError("prefilter state identity mismatch")
    cursor = value.get("next_address_index")
    batches = value.get("completed_batches")
    artifacts = value.get("batch_artifacts")
    if (
        not isinstance(cursor, int)
        or isinstance(cursor, bool)
        or not 0 <= cursor <= len(discovery["borrowers"])
        or not isinstance(batches, int)
        or isinstance(batches, bool)
        or batches < 0
        or not isinstance(artifacts, list)
        or len(artifacts) != batches
        or not isinstance(value.get("daily_usage"), dict)
    ):
        raise ExportError("prefilter state cursor is invalid")
    for field in (
        "total_nownodes_json_rpc_items",
        "total_nownodes_transport_requests",
        "total_retries",
    ):
        item = value.get(field)
        if not isinstance(item, int) or isinstance(item, bool) or item < 0:
            raise ExportError("prefilter state usage is invalid")
    return value


def _parse_header(value: Any, expected_number: int | None = None) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ExportError("prefilter finalized header is unavailable")
    try:
        number = int(str(value.get("number")), 16)
        timestamp = int(str(value.get("timestamp")), 16)
    except (TypeError, ValueError) as error:
        raise ExportError("prefilter finalized header is malformed") from error
    block_hash = str(value.get("hash", "")).lower()
    parent_hash = str(value.get("parentHash", "")).lower()
    state_root = str(value.get("stateRoot", "")).lower()
    if (
        number < 1
        or timestamp < 1
        or (expected_number is not None and number != expected_number)
        or len(block_hash) != 66
        or len(parent_hash) != 66
        or len(state_root) != 66
    ):
        raise ExportError("prefilter finalized header is invalid")
    return {
        "number": number,
        "hash": block_hash,
        "parent_hash": parent_hash,
        "state_root": state_root,
        "timestamp": timestamp,
    }


def exact_primary_context(
    provider: SSHContainerProvider, preflight: dict[str, Any]
) -> dict[str, Any]:
    provider.set_diagnostic_context("economic_prefilter_liveness")
    chain = provider.call("eth_chainId", [], attempts=1)
    if int(str(chain), 16) != CHAIN_ID:
        raise ExportError("prefilter provider chain identity mismatch")
    finalized = _parse_header(
        provider.call("eth_getBlockByNumber", ["finalized", False], attempts=1)
    )
    exact = _parse_header(
        provider.call(
            "eth_getBlockByNumber", [hex(finalized["number"]), False], attempts=1
        ),
        finalized["number"],
    )
    if finalized != exact:
        raise ExportError("prefilter exact finalized header disagreement")
    preflight_primary = next(
        item
        for item in preflight["provider_headers"]
        if item["provider_id"] == "production-nownodes-arbitrum"
    )
    if (
        provider.provider_reference_sha256
        != preflight_primary["provider_reference_sha256"]
    ):
        raise ExportError("prefilter provider identity changed since preflight")
    return exact


def classify_account_data(
    borrower: str,
    result: str,
    context: dict[str, Any],
    provider_reference_sha256: str,
    batch_index: int,
) -> dict[str, Any]:
    base = {
        "borrower": borrower,
        "finalized_block": context["number"],
        "finalized_block_hash": context["hash"],
        "finalized_state_root": context["state_root"],
        "provider_reference_sha256": provider_reference_sha256,
        "batch_index": batch_index,
    }
    try:
        raw = words(result, 6)
        if len(raw) != 6:
            raise ExportError("account data word count is invalid")
        values = [word_uint(item) for item in raw]
    except (ExportError, ValueError):
        return {
            **base,
            "bucket": "incomplete",
            "classification_failure": "account_data_decode_failed",
        }
    total_debt = values[1]
    health_factor = values[5]
    if total_debt == 0:
        bucket = "no_debt"
    elif health_factor < WAD:
        bucket = "liquidatable"
    elif health_factor <= URGENT_HF_WAD:
        bucket = "urgent"
    elif health_factor <= WATCH_HF_WAD:
        bucket = "watch"
    else:
        bucket = "debt_safe"
    return {
        **base,
        "total_collateral_base": values[0],
        "total_debt_base": total_debt,
        "available_borrow_base": values[2],
        "current_liquidation_threshold_bps": values[3],
        "ltv_bps": values[4],
        "health_factor_wad": health_factor,
        "bucket": bucket,
    }


def _monitoring_artifact(
    schema_role: str,
    prefilter: dict[str, Any],
    rows: list[dict[str, Any]],
) -> dict[str, Any]:
    return bind_hash(
        {
            "schema": MONITORING_SCHEMA,
            "role": schema_role,
            "source_prefilter_content_sha256": prefilter["content_sha256"],
            "finalized_block": prefilter["finalized_block"],
            "rows": rows,
            "row_count": len(rows),
            "candidate_authority": False,
            "execution_authority": False,
        }
    )


def build_screen_cohort(
    discovery: dict[str, Any], prefilter: dict[str, Any], rows: list[dict[str, Any]]
) -> dict[str, Any]:
    addresses = sorted(
        row["borrower"]
        for row in rows
        if row.get("bucket") in {"liquidatable", "urgent"}
    )
    return bind_hash(
        {
            "schema": SCREEN_COHORT_SCHEMA,
            "chain_id": CHAIN_ID,
            "pool": POOL,
            "source_discovery_content_sha256": discovery["content_sha256"],
            "addresses": addresses,
            "address_count": len(addresses),
            "cohort_reason": "primary_prefilter_liquidatable_or_urgent",
            "source_prefilter_content_sha256": prefilter["content_sha256"],
            "finalized_prefilter_block": {
                "number": prefilter["finalized_block"],
                "hash": prefilter["finalized_block_hash"],
                "state_root": prefilter["finalized_state_root"],
            },
            "candidate_authority": False,
            "execution_authority": False,
        }
    )


def _today() -> str:
    return datetime.now(timezone.utc).date().isoformat()


def _safe_failure(error: Exception, cursor: int) -> dict[str, Any]:
    artifact: dict[str, Any] = {
        "schema": "phoenix.atlas.aave-economic-prefilter-error.v1",
        "status": "failed_closed",
        "error_class": type(error).__name__,
        "cursor_unchanged": cursor,
        "candidate_authority": False,
        "execution_authority": False,
    }
    if isinstance(error, ProviderDiagnosticError):
        artifact.update(error.sanitized_evidence())
    else:
        artifact["failure_class"] = "prefilter_invariant_failed"
    return bind_hash(artifact)


def run(args: argparse.Namespace) -> dict[str, Any]:
    with args.discovery.open(encoding="utf-8") as handle:
        discovery = validate_discovery(json.load(handle), AUTHORITY_CURRENT_STATE)
    with args.preflight_context.open(encoding="utf-8") as handle:
        preflight = validate_preflight_context(json.load(handle))
    state_path = args.resume_dir / "state.json"
    if state_path.exists():
        with state_path.open(encoding="utf-8") as handle:
            state = validate_state(json.load(handle), discovery)
    else:
        state = initial_state(discovery)
    cursor = int(state["next_address_index"])
    end = min(cursor + args.batch_size, len(discovery["borrowers"]))
    if end == cursor:
        raise ExportError("economic prefilter source cohort is complete")
    addresses = list(discovery["borrowers"][cursor:end])
    day = _today()
    prior_day = state["daily_usage"].get(day, {"addresses": 0, "items": 0})
    if int(prior_day.get("addresses", 0)) + len(addresses) > MAX_DAILY_ADDRESSES:
        raise ExportError("economic prefilter daily address ceiling reached")
    if int(state["total_nownodes_json_rpc_items"]) + len(addresses) + 3 > MAX_TOTAL_BROAD_ITEMS:
        raise ExportError("economic prefilter total request budget reached")

    started = time.monotonic_ns()
    provider: SSHContainerProvider | None = None
    try:
        provider = SSHContainerProvider(
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
        provider_started_ms = (time.monotonic_ns() - started) // 1_000_000
        context_started = time.monotonic_ns()
        context = exact_primary_context(provider, preflight)
        context_ms = (time.monotonic_ns() - context_started) // 1_000_000
        calls = [
            (
                POOL,
                call_data(
                    SELECTORS["get_user_account_data"], encode_address(borrower)
                ),
            )
            for borrower in addresses
        ]
        account_started = time.monotonic_ns()
        provider.set_diagnostic_context("economic_prefilter_account_data")
        results = provider.eth_calls(calls, context["number"], args.rpc_batch_size)
        account_ms = (time.monotonic_ns() - account_started) // 1_000_000
        if len(results) != len(addresses):
            raise ExportError("economic prefilter account-data batch is incomplete")
        classification_started = time.monotonic_ns()
        rows = [
            classify_account_data(
                borrower,
                result,
                context,
                provider.provider_reference_sha256,
                int(state["completed_batches"]),
            )
            for borrower, result in zip(addresses, results)
        ]
        classification_ms = (time.monotonic_ns() - classification_started) // 1_000_000
        total_ms = (time.monotonic_ns() - started) // 1_000_000
        items = int(provider._request_id)
        transports = int(provider.transport_request_count)
        retries = int(provider.retry_count)
        items_per_address_micros = items * 1_000_000 // len(addresses)
        retry_rate_bps = retries * 10_000 // max(items, 1)
        if (
            items > args.max_json_rpc_items
            or transports > args.max_transport_requests
            or retries > args.max_retries
            or total_ms > args.max_runtime_seconds * 1_000
            or items_per_address_micros > HARD_ITEMS_PER_ADDRESS_MICROS
            or retry_rate_bps > 100
        ):
            raise ExportError("economic prefilter exceeded a hard execution bound")
        counts = {bucket: 0 for bucket in BUCKETS}
        for row in rows:
            counts[row["bucket"]] += 1
        remaining = len(discovery["borrowers"]) - end
        projected_remaining_items = (
            remaining * items_per_address_micros + 999_999
        ) // 1_000_000
        artifact = bind_hash(
            {
                "schema": PREFILTER_SCHEMA,
                "chain_id": CHAIN_ID,
                "pool": POOL,
                "discovery_content_sha256": discovery["content_sha256"],
                "preflight_context_content_sha256": preflight["content_sha256"],
                "batch_index": int(state["completed_batches"]),
                "address_offset_start": cursor,
                "address_offset_end_exclusive": end,
                "address_count": len(addresses),
                "finalized_block": context["number"],
                "finalized_block_hash": context["hash"],
                "finalized_state_root": context["state_root"],
                "finalized_block_timestamp": context["timestamp"],
                "provider_id": "production-nownodes-arbitrum",
                "provider_reference_sha256": provider.provider_reference_sha256,
                "primary_provider_only": True,
                "one_account_data_item_per_borrower": True,
                "raw_rpc_responses_persisted": False,
                "rows": rows,
                "rows_content_sha256": canonical_hash(rows),
                "bucket_counts": counts,
                "retained_exact_validation_count": counts["liquidatable"]
                + counts["urgent"],
                "request_usage": {
                    "json_rpc_item_count": items,
                    "transport_request_count": transports,
                    "retry_count": retries,
                    "retry_rate_bps": retry_rate_bps,
                    "items_per_address_micros": items_per_address_micros,
                    "target_items_per_address_micros": TARGET_ITEMS_PER_ADDRESS_MICROS,
                    "warning_items_per_address_micros": WARNING_ITEMS_PER_ADDRESS_MICROS,
                    "hard_items_per_address_micros": HARD_ITEMS_PER_ADDRESS_MICROS,
                    "max_json_rpc_items": args.max_json_rpc_items,
                    "max_transport_requests": args.max_transport_requests,
                    "max_retries": args.max_retries,
                },
                "stage_timing_ms": {
                    "provider_startup": provider_started_ms,
                    "lightweight_header_liveness": context_ms,
                    "account_data_batch": account_ms,
                    "classification": classification_ms,
                    "total": total_ms,
                    "runtime_limit": args.max_runtime_seconds * 1_000,
                },
                "usage_projection": {
                    "processed_after_batch": end,
                    "source_address_count": len(discovery["borrowers"]),
                    "projected_remaining_items_at_observed_rate": projected_remaining_items,
                    "projected_total_items_at_observed_rate": int(
                        state["total_nownodes_json_rpc_items"]
                    )
                    + items
                    + projected_remaining_items,
                    "daily_broad_address_ceiling": MAX_DAILY_ADDRESSES,
                    "total_broad_item_budget": MAX_TOTAL_BROAD_ITEMS,
                    "paid_overage_authorized": False,
                },
                "discovery_only": True,
                "candidate_authority": False,
                "execution_authority": False,
            }
        )
        cohort = build_screen_cohort(discovery, artifact, rows)
        watch = _monitoring_artifact(
            "watch", artifact, [row for row in rows if row["bucket"] == "watch"]
        )
        incomplete = _monitoring_artifact(
            "incomplete_diagnostic_queue",
            artifact,
            [row for row in rows if row["bucket"] == "incomplete"],
        )
        context_artifact = bind_hash(
            {
                "schema": PREFILTER_CONTEXT_SCHEMA,
                "chain_id": CHAIN_ID,
                "provider_id": "production-nownodes-arbitrum",
                "provider_reference_sha256": provider.provider_reference_sha256,
                "finalized_block": context,
                "full_preflight_context_content_sha256": preflight["content_sha256"],
                "proof_policy_refreshed": False,
                "code_storage_sample_refreshed": False,
                "purpose": "ordinary_primary_only_prefilter_liveness",
                "candidate_authority": False,
                "execution_authority": False,
            }
        )
        base_name = f"batch-{int(state['completed_batches']):06d}"
        batch_path = (
            args.resume_dir
            / "batches"
            / f"{base_name}-prefilter-{artifact['content_sha256']}.json"
        )
        cohort_path = (
            args.resume_dir
            / "cohorts"
            / f"{base_name}-cohort-{cohort['content_sha256']}.json"
        )
        watch_path = (
            args.resume_dir
            / "monitoring"
            / f"{base_name}-watch-{watch['content_sha256']}.json"
        )
        incomplete_path = (
            args.resume_dir
            / "diagnostics"
            / f"{base_name}-incomplete-{incomplete['content_sha256']}.json"
        )
        context_path = (
            args.resume_dir
            / "contexts"
            / f"{base_name}-context-{context_artifact['content_sha256']}.json"
        )
        for path, value in (
            (batch_path, artifact),
            (cohort_path, cohort),
            (watch_path, watch),
            (incomplete_path, incomplete),
            (context_path, context_artifact),
        ):
            write_private_json(path, value)
        daily_usage = dict(state["daily_usage"])
        daily_usage[day] = {
            "addresses": int(prior_day.get("addresses", 0)) + len(addresses),
            "items": int(prior_day.get("items", 0)) + items,
        }
        state_body = {key: item for key, item in state.items() if key != "content_sha256"}
        state_body.update(
            {
                "next_address_index": end,
                "completed_batches": int(state["completed_batches"]) + 1,
                "total_nownodes_json_rpc_items": int(
                    state["total_nownodes_json_rpc_items"]
                )
                + items,
                "total_nownodes_transport_requests": int(
                    state["total_nownodes_transport_requests"]
                )
                + transports,
                "total_retries": int(state["total_retries"]) + retries,
                "daily_usage": daily_usage,
                "batch_artifacts": list(state["batch_artifacts"])
                + [
                    {
                        "prefilter_file": str(batch_path.relative_to(args.resume_dir)),
                        "prefilter_content_sha256": artifact["content_sha256"],
                        "cohort_file": str(cohort_path.relative_to(args.resume_dir)),
                        "cohort_content_sha256": cohort["content_sha256"],
                        "watch_file": str(watch_path.relative_to(args.resume_dir)),
                        "watch_content_sha256": watch["content_sha256"],
                        "incomplete_file": str(
                            incomplete_path.relative_to(args.resume_dir)
                        ),
                        "incomplete_content_sha256": incomplete["content_sha256"],
                        "context_file": str(context_path.relative_to(args.resume_dir)),
                        "context_content_sha256": context_artifact["content_sha256"],
                        "address_offset_start": cursor,
                        "address_offset_end_exclusive": end,
                        "finalized_block": context["number"],
                        "bucket_counts": counts,
                        "request_usage": artifact["request_usage"],
                    }
                ],
            }
        )
        next_state = bind_hash(state_body)
        write_private_json(state_path, next_state)
        return {
            "status": "batch_complete",
            "prefilter_cursor_before": cursor,
            "prefilter_cursor_after": end,
            "source_address_count": len(discovery["borrowers"]),
            "finalized_block": context["number"],
            "finalized_block_hash": context["hash"],
            "bucket_counts": counts,
            "exact_validation_cohort_size": cohort["address_count"],
            "request_usage": artifact["request_usage"],
            "stage_timing_ms": artifact["stage_timing_ms"],
            "usage_projection": artifact["usage_projection"],
            "prefilter_content_sha256": artifact["content_sha256"],
            "cohort_content_sha256": cohort["content_sha256"],
            "state_content_sha256": next_state["content_sha256"],
            "candidate_count": 0,
            "economic_result": "not_evaluated_discovery_only",
            "candidate_authority": False,
            "execution_authority": False,
        }
    finally:
        if provider is not None:
            provider.close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--discovery", required=True, type=Path)
    parser.add_argument("--preflight-context", required=True, type=Path)
    parser.add_argument("--resume-dir", required=True, type=Path)
    parser.add_argument("--batch-size", required=True, type=int)
    parser.add_argument("--rpc-batch-size", type=int, default=200)
    parser.add_argument("--max-json-rpc-items", required=True, type=int)
    parser.add_argument("--max-transport-requests", required=True, type=int)
    parser.add_argument("--max-retries", required=True, type=int)
    parser.add_argument("--max-runtime-seconds", required=True, type=int)
    parser.add_argument("--ssh-executable", default="ssh")
    parser.add_argument("--ssh-provider-host", required=True)
    parser.add_argument("--ssh-provider-port", type=int, default=22)
    parser.add_argument("--ssh-provider-identity", required=True, type=Path)
    parser.add_argument("--ssh-provider-known-hosts", required=True, type=Path)
    parser.add_argument("--ssh-provider-container", required=True)
    args = parser.parse_args()
    if (
        not 1 <= args.batch_size <= MAX_BATCH_SIZE
        or not 1 <= args.rpc_batch_size <= 200
        or args.max_json_rpc_items < args.batch_size + 3
        or args.max_transport_requests < 1
        or args.max_retries < 0
        or args.max_runtime_seconds < 1
    ):
        print(
            json.dumps(
                bind_hash(
                    {
                        "schema": "phoenix.atlas.aave-economic-prefilter-error.v1",
                        "status": "failed_closed",
                        "failure_class": "prefilter_argument_bound_invalid",
                        "candidate_authority": False,
                        "execution_authority": False,
                    }
                ),
                sort_keys=True,
                separators=(",", ":"),
            )
        )
        return 1
    try:
        result = run(args)
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 0
    except Exception as error:
        cursor = 0
        state_path = args.resume_dir / "state.json"
        if state_path.exists():
            try:
                with state_path.open(encoding="utf-8") as handle:
                    raw_state = json.load(handle)
                candidate_cursor = raw_state.get("next_address_index")
                if isinstance(candidate_cursor, int) and not isinstance(
                    candidate_cursor, bool
                ):
                    cursor = candidate_cursor
            except (OSError, json.JSONDecodeError):
                cursor = 0
        failure = _safe_failure(error, cursor)
        failure_path = (
            args.resume_dir
            / "failures"
            / f"failure-{time.time_ns()}-{failure['content_sha256']}.json"
        )
        write_private_json(failure_path, failure)
        print(json.dumps(failure, sort_keys=True, separators=(",", ":")))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
