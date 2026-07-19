from __future__ import annotations

import hashlib
import json
import math
import re
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any, Iterable


SNAPSHOT_SCHEMA = "phoenix.prelive.dashboard.v1"
DEFAULT_SNAPSHOT_PATH = Path("/evidence/latest-dashboard.json")
MAX_SNAPSHOT_BYTES = 2 * 1024 * 1024
MAX_ARTIFACT_BYTES = 2 * 1024 * 1024
MAX_LOG_ROWS = 500

SERVICE_NAMES = (
    "nitro-feed-relay",
    "feed-ingestor",
    "nats",
    "recorder",
    "postgres",
    "rpc-gateway",
    "phoenix-engine",
    "shadow-dispatcher",
    "prometheus",
    "dashboard",
    "fork-sandbox",
)
SERVICE_STATES = {
    "running_healthy",
    "running_unhealthy",
    "running_no_healthcheck",
    "stopped_clean",
    "stopped_failed",
    "created_not_started",
    "missing",
    "unknown",
}
FUNNEL_STAGES = (
    "feed_inputs",
    "supported_swaps",
    "route_matches",
    "candidates",
    "primary_profitable",
    "independently_verified",
    "fork_simulated",
    "fork_profitable",
)
ARTIFACT_KINDS = (
    "technical_json",
    "business_json",
    "preflight_report",
    "evidence_bundle",
    "soak_report",
    "route_ranking",
    "profitability_report",
    "fork_simulation_report",
    "release_manifest_checksum",
)

_UNSIGNED_RE = re.compile(r"^(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$")
_SIGNED_RE = re.compile(r"^(?:0|-?[1-9][0-9]*)(?:\.[0-9]+)?$")
_SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_GIT_SHA_RE = re.compile(r"^(?:[0-9a-f]{40}|not_available)$")
_ROUTE_ID_RE = re.compile(r"^route-[0-9a-f]{12}$")
_PROVIDER_ID_RE = re.compile(r"^provider-[a-z0-9-]{1,48}$")
_SAFE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")
_ARTIFACT_PATH_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
_URL_RE = re.compile(r"(?i)(?:https?|wss?|postgres(?:ql)?|nats)://")
_ADDRESS_RE = re.compile(r"(?i)0x[0-9a-f]{40}")
_SECRET_RE = re.compile(
    r"(?i)(?:password|passwd|private[_ -]?key|mnemonic|authorization|bearer|"
    r"credential|secret)\s*[:=]|\b(?:RPC_PROVIDER_URLS|POSTGRES_DSN|"
    r"SIGNER_PRIVATE_KEY|WALLET_ADDRESS|EXECUTOR_ADDRESS)\b"
)
_ARTIFACT_SENSITIVE_KEY_RE = re.compile(
    r"(?i)^(?:password|passwd|private_key|mnemonic|authorization|bearer|credential|secret|"
    r"POSTGRES_DSN|POSTGRES_PASSWORD|RPC_PROVIDER_URLS|ARBITRUM_RPC_URL|"
    r"PARENT_CHAIN_RPC_URL|SIGNER_PRIVATE_KEY|WALLET_ADDRESS|EXECUTOR_ADDRESS)$"
)


class SnapshotError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


@dataclass(frozen=True)
class DashboardSnapshot:
    data: dict[str, Any]
    path: Path
    generated_at: datetime
    age_seconds: int
    alerts: tuple[dict[str, str], ...]
    gate_status: str
    gate_reasons: tuple[str, ...]


def _fail(code: str = "snapshot_shape_invalid") -> None:
    raise SnapshotError(code)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail("snapshot_duplicate_key")
        result[key] = value
    return result


def _reject_non_finite(_value: str) -> None:
    _fail("snapshot_non_finite")


def _object(value: Any, keys: Iterable[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(keys):
        _fail()
    return value


def _array(value: Any, maximum: int, minimum: int = 0) -> list[Any]:
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        _fail("snapshot_bounds_invalid")
    return value


def _text(
    value: Any,
    *,
    maximum: int = 128,
    pattern: re.Pattern[str] | None = None,
    choices: set[str] | None = None,
) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        _fail()
    if pattern is not None and pattern.fullmatch(value) is None:
        _fail()
    if choices is not None and value not in choices:
        _fail()
    return value


def _boolean(value: Any) -> bool:
    if not isinstance(value, bool):
        _fail()
    return value


def _integer(value: Any, minimum: int | None = None, maximum: int | None = None) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        _fail()
    if minimum is not None and value < minimum:
        _fail()
    if maximum is not None and value > maximum:
        _fail()
    return value


def _decimal(value: Any, *, signed: bool = False) -> Decimal:
    pattern = _SIGNED_RE if signed else _UNSIGNED_RE
    if (
        not isinstance(value, str)
        or len(value) > 96
        or pattern.fullmatch(value) is None
    ):
        _fail()
    try:
        parsed = Decimal(value)
    except InvalidOperation:
        _fail()
    if not parsed.is_finite():
        _fail("snapshot_non_finite")
    return parsed


def _basis_points(value: Any) -> Decimal:
    parsed = _decimal(value)
    if parsed > 10_000:
        _fail()
    return parsed


def _optional_decimal(value: Any, *, signed: bool = False) -> Decimal | None:
    if value is None:
        return None
    return _decimal(value, signed=signed)


def _optional_basis_points(value: Any) -> Decimal | None:
    if value is None:
        return None
    return _basis_points(value)


def _timestamp(value: Any) -> datetime:
    text = _text(value, maximum=40)
    try:
        parsed = datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError:
        _fail()
    if parsed.tzinfo is None:
        _fail()
    return parsed.astimezone(timezone.utc)


def _optional_timestamp(value: Any) -> datetime | None:
    if value is None:
        return None
    return _timestamp(value)


def _safe_text_tree(value: Any) -> None:
    if isinstance(value, dict):
        for nested in value.values():
            _safe_text_tree(nested)
        return
    if isinstance(value, list):
        for nested in value:
            _safe_text_tree(nested)
        return
    if isinstance(value, str):
        if len(value) > 1024:
            _fail("snapshot_bounds_invalid")
        if (
            _URL_RE.search(value)
            or _ADDRESS_RE.search(value)
            or _SECRET_RE.search(value)
        ):
            _fail("snapshot_redaction_invalid")


def _validate_artifact_tree(
    value: Any, *, depth: int = 0, items: list[int] | None = None
) -> None:
    if items is None:
        items = [0]
    items[0] += 1
    if depth > 16 or items[0] > 20_000:
        _fail("artifact_bounds_invalid")
    if isinstance(value, dict):
        for key, nested in value.items():
            if (
                not isinstance(key, str)
                or len(key) > 128
                or _ARTIFACT_SENSITIVE_KEY_RE.fullmatch(key)
            ):
                _fail("artifact_redaction_invalid")
            _validate_artifact_tree(nested, depth=depth + 1, items=items)
        return
    if isinstance(value, list):
        for nested in value:
            _validate_artifact_tree(nested, depth=depth + 1, items=items)
        return
    if isinstance(value, str):
        if (
            len(value) > 4096
            or _URL_RE.search(value)
            or _ADDRESS_RE.search(value)
            or _SECRET_RE.search(value)
        ):
            _fail("artifact_redaction_invalid")
        return
    if value is None or isinstance(value, (bool, int, Decimal)):
        return
    if isinstance(value, float) and math.isfinite(value):
        return
    _fail("artifact_content_invalid")


def validate_artifact_payload(raw: bytes, content_type: str) -> None:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        _fail("artifact_encoding_invalid")
    if any(ord(character) < 32 and character not in "\n\r\t" for character in text):
        _fail("artifact_redaction_invalid")
    if content_type == "application/json":
        try:
            data = json.loads(
                text,
                object_pairs_hook=_unique_object,
                parse_constant=_reject_non_finite,
            )
            _validate_artifact_tree(data)
        except RecursionError:
            _fail("artifact_bounds_invalid")
        except json.JSONDecodeError:
            _fail("artifact_content_invalid")
        except SnapshotError as exc:
            if exc.code.startswith("snapshot_"):
                _fail("artifact_content_invalid")
            raise
        return
    if content_type == "text/plain":
        lines = text.splitlines()
        if len(lines) > 2_000 or any(len(line) > 1024 for line in lines):
            _fail("artifact_bounds_invalid")
        if _URL_RE.search(text) or _ADDRESS_RE.search(text) or _SECRET_RE.search(text):
            _fail("artifact_redaction_invalid")
        return
    _fail("artifact_content_invalid")


def _validate_safety(value: Any) -> None:
    item = _object(
        value,
        {
            "mode",
            "live_execution",
            "prelive_lock",
            "execution_eligible",
            "execution_request_created",
            "signer_configured",
            "wallet_configured",
            "executor_configured",
            "submission_method_invocations",
        },
    )
    _text(item["mode"], choices={"SHADOW", "LIVE", "UNKNOWN"})
    for key in (
        "live_execution",
        "prelive_lock",
        "execution_eligible",
        "execution_request_created",
        "signer_configured",
        "wallet_configured",
        "executor_configured",
    ):
        _boolean(item[key])
    _decimal(item["submission_method_invocations"])


def _validate_governance(value: Any) -> None:
    item = _object(
        value,
        {
            "model_version",
            "policy_version",
            "route_configuration_hash",
            "route_registry_matches",
            "image_manifest_matches",
            "freshness_threshold_seconds",
            "fork_prediction_error_limit",
            "database_headroom_min_bytes",
            "unsupported_message_alert_threshold",
        },
    )
    _text(item["model_version"], maximum=64, pattern=_SAFE_ID_RE)
    _text(item["policy_version"], maximum=64, pattern=_SAFE_ID_RE)
    _text(item["route_configuration_hash"], pattern=_SHA256_RE)
    _boolean(item["route_registry_matches"])
    _boolean(item["image_manifest_matches"])
    for key in (
        "freshness_threshold_seconds",
        "fork_prediction_error_limit",
        "database_headroom_min_bytes",
        "unsupported_message_alert_threshold",
    ):
        _decimal(item[key])


def _validate_business(value: Any) -> None:
    item = _object(
        value,
        {
            "sample_count",
            "active_shadow_routes",
            "nearest_to_profitable_count",
            "independently_verified_count",
            "fork_successful_count",
        },
    )
    for nested in item.values():
        _decimal(nested)


def _validate_funnel(value: Any) -> None:
    rows = _array(value, len(FUNNEL_STAGES), len(FUNNEL_STAGES))
    stages: list[str] = []
    for row in rows:
        item = _object(row, {"stage", "count", "dropoff_reasons"})
        stages.append(_text(item["stage"], choices=set(FUNNEL_STAGES)))
        _decimal(item["count"])
        for reason in _array(item["dropoff_reasons"], 8):
            reason_item = _object(reason, {"reason", "count"})
            _text(reason_item["reason"], maximum=64, pattern=_SAFE_ID_RE)
            _decimal(reason_item["count"])
    if tuple(stages) != FUNNEL_STAGES:
        _fail()


def _validate_profitability(value: Any) -> None:
    item = _object(
        value,
        {
            "summary",
            "cost_breakdown",
            "route_pnl",
            "nearest_opportunities",
            "distribution",
            "prediction_error",
            "daily_trend",
            "weekly_trend",
            "model_comparison",
        },
    )
    summary = _object(
        item["summary"],
        {
            "gross_profit",
            "total_cost",
            "net_pnl",
            "expected_net_pnl",
            "conservative_net_pnl",
            "severe_net_pnl",
            "fork_simulated_net_pnl",
        },
    )
    for key, nested in summary.items():
        _decimal(nested, signed=key != "total_cost")

    for row in _array(item["cost_breakdown"], 12):
        cost = _object(row, {"component", "amount"})
        _text(cost["component"], maximum=64, pattern=_SAFE_ID_RE)
        _decimal(cost["amount"])

    for row in _array(item["route_pnl"], 10):
        route = _object(
            row,
            {
                "route_id",
                "expected",
                "conservative",
                "severe",
                "fork_simulated",
                "sample_count",
            },
        )
        _text(route["route_id"], pattern=_ROUTE_ID_RE)
        for key in ("expected", "conservative", "severe", "fork_simulated"):
            _decimal(route[key], signed=True)
        _decimal(route["sample_count"])

    for row in _array(item["nearest_opportunities"], 10):
        near = _object(row, {"route_id", "shortfall", "reason"})
        _text(near["route_id"], pattern=_ROUTE_ID_RE)
        _decimal(near["shortfall"])
        _text(near["reason"], maximum=64, pattern=_SAFE_ID_RE)

    for row in _array(item["distribution"], 30):
        bucket = _object(row, {"scenario", "bucket", "count"})
        _text(
            bucket["scenario"],
            choices={"expected", "conservative", "severe", "fork_simulated"},
        )
        _text(bucket["bucket"], maximum=32, pattern=_SAFE_ID_RE)
        _decimal(bucket["count"])

    for row in _array(item["prediction_error"], 20):
        bucket = _object(row, {"bucket", "count"})
        _text(bucket["bucket"], maximum=32, pattern=_SAFE_ID_RE)
        _decimal(bucket["count"])

    for key, maximum, period_pattern in (
        ("daily_trend", 31, re.compile(r"^\d{4}-\d{2}-\d{2}$")),
        ("weekly_trend", 12, re.compile(r"^\d{4}-W\d{2}$")),
    ):
        for row in _array(item[key], maximum):
            trend = _object(
                row,
                {
                    "period",
                    "expected",
                    "conservative",
                    "severe",
                    "fork_simulated",
                    "sample_count",
                },
            )
            _text(trend["period"], maximum=10, pattern=period_pattern)
            for amount in ("expected", "conservative", "severe", "fork_simulated"):
                _decimal(trend[amount], signed=True)
            _decimal(trend["sample_count"])

    for row in _array(item["model_comparison"], 10):
        model = _object(
            row,
            {
                "model_version",
                "sample_count",
                "expected",
                "conservative",
                "absolute_fork_error",
            },
        )
        _text(model["model_version"], maximum=64, pattern=_SAFE_ID_RE)
        _decimal(model["sample_count"])
        _decimal(model["expected"], signed=True)
        _decimal(model["conservative"], signed=True)
        _decimal(model["absolute_fork_error"])


def _validate_routes(value: Any) -> None:
    rows = _array(value, 10)
    seen: set[str] = set()
    for row in rows:
        item = _object(
            row,
            {
                "route_id",
                "rank",
                "active_shadow",
                "candidate_count",
                "ranking_score_bps",
                "score_components",
                "data_quality_warnings",
                "expected_net_pnl",
                "conservative_net_pnl",
                "provider_failure_contribution_bps",
                "fork_success_rate_bps",
            },
        )
        route_id = _text(item["route_id"], pattern=_ROUTE_ID_RE)
        if route_id in seen:
            _fail("snapshot_duplicate_identity")
        seen.add(route_id)
        _integer(item["rank"], 1, 10)
        _boolean(item["active_shadow"])
        _decimal(item["candidate_count"])
        _basis_points(item["ranking_score_bps"])
        components = _object(
            item["score_components"],
            {"liquidity", "freshness", "confidence", "reliability"},
        )
        for nested in components.values():
            _basis_points(nested)
        for warning in _array(item["data_quality_warnings"], 4):
            _text(warning, maximum=64, pattern=_SAFE_ID_RE)
        _decimal(item["expected_net_pnl"], signed=True)
        _decimal(item["conservative_net_pnl"], signed=True)
        _basis_points(item["provider_failure_contribution_bps"])
        _basis_points(item["fork_success_rate_bps"])


def _validate_services(value: Any) -> None:
    rows = _array(value, len(SERVICE_NAMES), len(SERVICE_NAMES))
    seen: set[str] = set()
    for row in rows:
        item = _object(
            row,
            {
                "service",
                "state",
                "expected_state",
                "image_digest",
                "git_sha",
                "started_at",
                "exit_code",
                "oom",
                "restart_count",
                "readiness_freshness_seconds",
            },
        )
        service = _text(item["service"], choices=set(SERVICE_NAMES))
        if service in seen:
            _fail("snapshot_duplicate_identity")
        seen.add(service)
        _text(item["state"], choices=SERVICE_STATES)
        _text(item["expected_state"], choices={"running", "on_demand"})
        _text(item["image_digest"], choices={"not_available"}) if item[
            "image_digest"
        ] == "not_available" else _text(item["image_digest"], pattern=_SHA256_RE)
        _text(item["git_sha"], pattern=_GIT_SHA_RE)
        _optional_timestamp(item["started_at"])
        if item["exit_code"] is not None:
            _integer(item["exit_code"], -255, 255)
        _boolean(item["oom"])
        _integer(item["restart_count"], 0, 1_000_000)
        if item["readiness_freshness_seconds"] is not None:
            _decimal(item["readiness_freshness_seconds"])
    if seen != set(SERVICE_NAMES):
        _fail()


def _validate_feed(value: Any) -> None:
    item = _object(
        value,
        {
            "gap_count",
            "missing_sequences",
            "most_recent_gap_at",
            "unsupported_kinds",
            "ignored_kinds",
            "reconnects",
            "completeness_status",
            "affected_windows",
        },
    )
    for key in ("gap_count", "missing_sequences", "reconnects"):
        _decimal(item[key])
    _optional_timestamp(item["most_recent_gap_at"])
    for key in ("unsupported_kinds", "ignored_kinds"):
        for row in _array(item[key], 64):
            kind = _object(row, {"kind", "count"})
            _integer(kind["kind"], 0, 255)
            _decimal(kind["count"])
    _text(item["completeness_status"], choices={"complete", "incomplete", "unknown"})
    for window in _array(item["affected_windows"], 24):
        _text(window, maximum=96)


def _validate_rpc(value: Any) -> None:
    item = _object(
        value,
        {
            "providers",
            "secondary_requested",
            "agreed",
            "disagreed",
            "state_freshness_seconds",
            "pinned_block_status",
        },
    )
    seen: set[str] = set()
    roles: set[str] = set()
    for row in _array(item["providers"], 8, 2):
        provider = _object(
            row,
            {
                "provider_id",
                "role",
                "success_rate_bps",
                "timeouts",
                "rate_limits",
                "unavailable",
                "p50_latency_ms",
                "p95_latency_ms",
                "p99_latency_ms",
                "budget_utilization_bps",
                "self_verification_prevented",
            },
        )
        provider_id = _text(provider["provider_id"], pattern=_PROVIDER_ID_RE)
        if provider_id in seen:
            _fail("snapshot_duplicate_identity")
        seen.add(provider_id)
        roles.add(_text(provider["role"], choices={"primary", "secondary"}))
        _optional_basis_points(provider["success_rate_bps"])
        for key in ("timeouts", "rate_limits", "unavailable"):
            _decimal(provider[key])
        for key in ("p50_latency_ms", "p95_latency_ms", "p99_latency_ms"):
            if provider[key] is not None:
                _decimal(provider[key])
        _optional_basis_points(provider["budget_utilization_bps"])
        _boolean(provider["self_verification_prevented"])
    if roles != {"primary", "secondary"}:
        _fail()
    for key in (
        "secondary_requested",
        "agreed",
        "disagreed",
        "state_freshness_seconds",
    ):
        _decimal(item[key])
    _text(item["pinned_block_status"], choices={"pinned", "not_pinned", "unknown"})


def _validate_jetstream(value: Any) -> None:
    item = _object(value, {"streams", "consumers", "persistence"})
    for row in _array(item["streams"], 8):
        stream = _object(
            row,
            {
                "stream_id",
                "exists",
                "messages",
                "bytes",
                "configured_storage",
                "storage_used_bps",
            },
        )
        _text(stream["stream_id"], maximum=64, pattern=_SAFE_ID_RE)
        _boolean(stream["exists"])
        _decimal(stream["messages"])
        _decimal(stream["bytes"])
        _text(stream["configured_storage"], choices={"file", "memory", "unknown"})
        _optional_basis_points(stream["storage_used_bps"])
    for row in _array(item["consumers"], 16):
        consumer = _object(
            row,
            {
                "consumer_id",
                "exists",
                "ack_pending",
                "pending",
                "redeliveries",
                "oldest_pending_age_seconds",
            },
        )
        _text(consumer["consumer_id"], maximum=64, pattern=_SAFE_ID_RE)
        _boolean(consumer["exists"])
        for key in ("ack_pending", "pending", "redeliveries"):
            _decimal(consumer[key])
        _optional_decimal(consumer["oldest_pending_age_seconds"])
    persistence = _object(
        item["persistence"],
        {
            "throughput_per_second",
            "batch_size",
            "backlog_growth",
            "database_write_latency_ms",
        },
    )
    _optional_decimal(persistence["throughput_per_second"])
    _optional_decimal(persistence["batch_size"])
    _optional_decimal(persistence["backlog_growth"], signed=True)
    latency = _object(persistence["database_write_latency_ms"], {"p50", "p95", "p99"})
    for nested in latency.values():
        _optional_decimal(nested)


def _validate_postgres(value: Any) -> None:
    item = _object(
        value,
        {
            "readiness",
            "database_size_bytes",
            "growth_bytes_1h",
            "growth_bytes_6h",
            "growth_bytes_24h",
            "projected_disk_headroom_bytes",
            "active_connections",
            "checkpoints_timed",
            "checkpoints_requested",
            "wal_bytes",
            "oldest_relevant_event",
            "newest_relevant_event",
            "migration_version",
            "migration_checksum",
            "retention_status",
        },
    )
    _boolean(item["readiness"])
    for key in (
        "database_size_bytes",
        "projected_disk_headroom_bytes",
        "active_connections",
        "checkpoints_timed",
        "checkpoints_requested",
        "wal_bytes",
    ):
        _decimal(item[key])
    for key in ("growth_bytes_1h", "growth_bytes_6h", "growth_bytes_24h"):
        _optional_decimal(item[key], signed=True)
    _optional_timestamp(item["oldest_relevant_event"])
    _optional_timestamp(item["newest_relevant_event"])
    _text(item["migration_version"], maximum=64, pattern=_SAFE_ID_RE)
    _text(item["migration_checksum"], pattern=_SHA256_RE)
    _text(item["retention_status"], choices={"configured", "not_configured", "unknown"})


def _validate_money_path_ingress(value: Any) -> None:
    item = _object(
        value,
        {
            "aggregate_flush_failures_total",
            "aggregate_flush_total",
            "bounded_sample_failures_total",
            "bounded_samples_total",
            "database_bytes_per_input_estimate",
            "database_bytes_per_input_estimate_available",
            "dispatcher_backlog_refresh_failures_total",
            "dispatcher_backlog_refresh_total",
            "dispatcher_backlog_stale_seconds",
            "dispatcher_batch_cycle_seconds",
            "dispatcher_oldest_claimable_age_seconds",
            "dispatcher_pending_rows_estimate",
            "dispatcher_rows_published_total",
            "feed_inputs_total",
            "irrelevant_filtered_total",
            "persistence_ratio",
            "projected_disk_runway_days",
            "raw_rows_avoided_total",
            "relevant_route_inputs_total",
            "relevant_transaction_failures_total",
            "relevant_transactions_committed_total",
            "sample_limit_reached_total",
            "unsupported_interesting_total",
        },
    )
    for key, nested in item.items():
        if key == "projected_disk_runway_days":
            _optional_decimal(nested)
        else:
            _decimal(nested)
    if _number(item["database_bytes_per_input_estimate_available"]) not in {0, 1}:
        _fail()
    if _number(item["persistence_ratio"]) > 1:
        _fail()


def _validate_reliability(value: Any) -> None:
    item = _object(
        value,
        {
            "retry_attempts",
            "recovered_retries",
            "exhausted_or_quarantined",
            "terminal_integrity_failures",
            "runtime_exits",
            "restart_loops",
            "later_message_progress_after_quarantine",
            "protected_service_identity_status",
        },
    )
    for key in (
        "retry_attempts",
        "recovered_retries",
        "exhausted_or_quarantined",
        "terminal_integrity_failures",
        "restart_loops",
        "later_message_progress_after_quarantine",
    ):
        _decimal(item[key])
    for row in _array(item["runtime_exits"], 16):
        runtime_exit = _object(row, {"class", "count"})
        _text(runtime_exit["class"], maximum=64, pattern=_SAFE_ID_RE)
        _decimal(runtime_exit["count"])
    _text(
        item["protected_service_identity_status"],
        choices={"stable", "changed", "unknown"},
    )


def _validate_fork(value: Any) -> None:
    item = _object(
        value,
        {
            "unsigned_plan_count",
            "simulations",
            "success",
            "reverted",
            "gas_used",
            "balance_delta",
            "simulated_net_pnl",
            "absolute_prediction_error",
            "fork_block",
            "contract_guard_failures",
        },
    )
    for key in (
        "unsigned_plan_count",
        "simulations",
        "success",
        "reverted",
        "gas_used",
        "absolute_prediction_error",
        "fork_block",
        "contract_guard_failures",
    ):
        _decimal(item[key])
    _decimal(item["balance_delta"], signed=True)
    _decimal(item["simulated_net_pnl"], signed=True)


def _validate_artifacts(value: Any) -> None:
    rows = _array(value, len(ARTIFACT_KINDS), len(ARTIFACT_KINDS))
    seen: set[str] = set()
    for row in rows:
        item = _object(
            row,
            {
                "kind",
                "label",
                "available",
                "path",
                "sha256",
                "size_bytes",
                "generated_at",
                "content_type",
            },
        )
        kind = _text(item["kind"], choices=set(ARTIFACT_KINDS))
        if kind in seen:
            _fail("snapshot_duplicate_identity")
        seen.add(kind)
        _text(item["label"], maximum=80)
        available = _boolean(item["available"])
        if available:
            _text(item["path"], pattern=_ARTIFACT_PATH_RE)
            _text(item["sha256"], pattern=_SHA256_RE)
            _integer(item["size_bytes"], 1, MAX_ARTIFACT_BYTES)
            _timestamp(item["generated_at"])
            _text(item["content_type"], choices={"application/json", "text/plain"})
        elif any(
            item[key] is not None
            for key in ("path", "sha256", "size_bytes", "generated_at", "content_type")
        ):
            _fail()
    if seen != set(ARTIFACT_KINDS):
        _fail()


def _validate_logs(value: Any) -> None:
    for row in _array(value, MAX_LOG_ROWS):
        item = _object(
            row, {"timestamp", "service", "severity", "event_class", "message"}
        )
        _timestamp(item["timestamp"])
        _text(item["service"], choices=set(SERVICE_NAMES) | {"control-plane"})
        _text(
            item["severity"], choices={"debug", "info", "warning", "error", "critical"}
        )
        _text(item["event_class"], maximum=64, pattern=_SAFE_ID_RE)
        _text(item["message"], maximum=512)


def _validate_semantics(data: dict[str, Any]) -> None:
    funnel = data["funnel"]
    previous: Decimal | None = None
    funnel_counts: dict[str, Decimal] = {}
    for row in funnel:
        count = _number(row["count"])
        funnel_counts[row["stage"]] = count
        if previous is None:
            if row["dropoff_reasons"]:
                _fail("snapshot_accounting_invalid")
        else:
            if count > previous:
                _fail("snapshot_accounting_invalid")
            dropoff = sum(
                (_number(reason["count"]) for reason in row["dropoff_reasons"]),
                Decimal(0),
            )
            if dropoff != previous - count:
                _fail("snapshot_accounting_invalid")
        previous = count

    business = data["business"]
    if (
        _number(business["independently_verified_count"])
        != funnel_counts["independently_verified"]
    ):
        _fail("snapshot_accounting_invalid")
    if _number(business["fork_successful_count"]) != _number(data["fork"]["success"]):
        _fail("snapshot_accounting_invalid")
    active_routes = sum(1 for route in data["routes"] if route["active_shadow"])
    if (
        _number(business["active_shadow_routes"]) != active_routes
        or active_routes > 3
        or any(route["active_shadow"] and route["rank"] > 3 for route in data["routes"])
    ):
        _fail("snapshot_accounting_invalid")
    if _number(business["nearest_to_profitable_count"]) != len(
        data["profitability"]["nearest_opportunities"]
    ):
        _fail("snapshot_accounting_invalid")
    route_samples = sum(
        (
            _number(route["sample_count"])
            for route in data["profitability"]["route_pnl"]
        ),
        Decimal(0),
    )
    if _number(business["sample_count"]) != route_samples:
        _fail("snapshot_accounting_invalid")

    summary = data["profitability"]["summary"]
    if _number(summary["gross_profit"]) - _number(summary["total_cost"]) != _number(
        summary["net_pnl"]
    ):
        _fail("snapshot_accounting_invalid")
    cost_total = sum(
        (
            _number(component["amount"])
            for component in data["profitability"]["cost_breakdown"]
        ),
        Decimal(0),
    )
    if cost_total != _number(summary["total_cost"]):
        _fail("snapshot_accounting_invalid")
    route_amounts = {
        key: sum(
            (_number(route[key]) for route in data["profitability"]["route_pnl"]),
            Decimal(0),
        )
        for key in ("expected", "conservative", "severe", "fork_simulated")
    }
    if route_amounts != {
        "expected": _number(summary["expected_net_pnl"]),
        "conservative": _number(summary["conservative_net_pnl"]),
        "severe": _number(summary["severe_net_pnl"]),
        "fork_simulated": _number(summary["fork_simulated_net_pnl"]),
    }:
        _fail("snapshot_accounting_invalid")
    if _number(summary["fork_simulated_net_pnl"]) != _number(
        data["fork"]["simulated_net_pnl"]
    ):
        _fail("snapshot_accounting_invalid")

    expected_samples = _number(business["sample_count"])
    for key in ("daily_trend", "weekly_trend", "model_comparison"):
        rows = data["profitability"][key]
        if (
            rows
            and sum((_number(row["sample_count"]) for row in rows), Decimal(0))
            != expected_samples
        ):
            _fail("snapshot_accounting_invalid")
    distribution_totals: dict[str, Decimal] = {}
    for row in data["profitability"]["distribution"]:
        distribution_totals[row["scenario"]] = distribution_totals.get(
            row["scenario"], Decimal(0)
        ) + _number(row["count"])
    for scenario in ("expected", "conservative", "severe"):
        if distribution_totals.get(scenario) != expected_samples:
            _fail("snapshot_accounting_invalid")

    fork = data["fork"]
    if _number(fork["success"]) + _number(fork["reverted"]) != _number(
        fork["simulations"]
    ):
        _fail("snapshot_accounting_invalid")
    if _number(fork["unsigned_plan_count"]) < _number(fork["simulations"]):
        _fail("snapshot_accounting_invalid")
    if funnel_counts["fork_simulated"] != _number(fork["simulations"]):
        _fail("snapshot_accounting_invalid")
    if funnel_counts["fork_profitable"] > _number(fork["success"]):
        _fail("snapshot_accounting_invalid")
    if distribution_totals.get("fork_simulated") != _number(fork["simulations"]):
        _fail("snapshot_accounting_invalid")
    prediction_error_samples = sum(
        (_number(row["count"]) for row in data["profitability"]["prediction_error"]),
        Decimal(0),
    )
    if prediction_error_samples != _number(fork["simulations"]):
        _fail("snapshot_accounting_invalid")

    rpc = data["rpc"]
    if _number(rpc["agreed"]) + _number(rpc["disagreed"]) > _number(
        rpc["secondary_requested"]
    ):
        _fail("snapshot_accounting_invalid")

    ranks = [route["rank"] for route in data["routes"]]
    if len(ranks) != len(set(ranks)):
        _fail("snapshot_duplicate_identity")
    for service in data["services"]:
        if service["service"] == "fork-sandbox":
            if service["expected_state"] != "on_demand":
                _fail("snapshot_accounting_invalid")
        elif service["expected_state"] != "running":
            _fail("snapshot_accounting_invalid")
        if service["state"].startswith("running_"):
            if service["started_at"] is None or service["exit_code"] is not None:
                _fail("snapshot_accounting_invalid")
    if data["governance"]["image_manifest_matches"] and any(
        service["expected_state"] == "running"
        and service["image_digest"] == "not_available"
        for service in data["services"]
    ):
        _fail("snapshot_accounting_invalid")


def validate_snapshot(data: Any) -> dict[str, Any]:
    top = _object(
        data,
        {
            "schema_version",
            "generated_at",
            "window_hours",
            "safety",
            "governance",
            "business",
            "funnel",
            "profitability",
            "routes",
            "services",
            "feed",
            "rpc",
            "jetstream",
            "money_path_ingress",
            "postgres",
            "reliability",
            "fork",
            "artifacts",
            "logs",
        },
    )
    if top["schema_version"] != SNAPSHOT_SCHEMA:
        _fail("snapshot_schema_invalid")
    _timestamp(top["generated_at"])
    window = _integer(top["window_hours"], 1, 168)
    if window not in {1, 6, 24, 168}:
        _fail()
    _validate_safety(top["safety"])
    _validate_governance(top["governance"])
    _validate_business(top["business"])
    _validate_funnel(top["funnel"])
    _validate_profitability(top["profitability"])
    _validate_routes(top["routes"])
    _validate_services(top["services"])
    _validate_feed(top["feed"])
    _validate_rpc(top["rpc"])
    _validate_jetstream(top["jetstream"])
    _validate_money_path_ingress(top["money_path_ingress"])
    _validate_postgres(top["postgres"])
    _validate_reliability(top["reliability"])
    _validate_fork(top["fork"])
    _validate_artifacts(top["artifacts"])
    _validate_logs(top["logs"])
    _validate_semantics(top)
    _safe_text_tree(top)
    return top


def _number(value: str) -> Decimal:
    return Decimal(value)


def derive_alerts(data: dict[str, Any], age_seconds: int) -> tuple[dict[str, str], ...]:
    alerts: dict[str, dict[str, str]] = {}

    def add(code: str, severity: str, summary: str) -> None:
        alerts[code] = {"code": code, "severity": severity, "summary": summary}

    safety = data["safety"]
    if safety["mode"] != "SHADOW":
        add("mode_not_shadow", "critical", "Operating mode is not SHADOW")
    if safety["live_execution"]:
        add("live_execution_enabled", "critical", "LIVE execution flag is enabled")
    if not safety["prelive_lock"]:
        add("prelive_lock_open", "critical", "Pre-LIVE lock is open")
    if safety["execution_eligible"]:
        add("execution_eligible", "critical", "Execution eligibility was observed")
    if safety["execution_request_created"]:
        add(
            "execution_request_created", "critical", "An execution request was observed"
        )
    if any(
        safety[key]
        for key in ("signer_configured", "wallet_configured", "executor_configured")
    ):
        add(
            "sensitive_runtime_setting",
            "critical",
            "A prohibited runtime setting was detected",
        )
    if _number(safety["submission_method_invocations"]) > 0:
        add(
            "submission_method_invoked",
            "critical",
            "A transaction submission method was observed",
        )

    governance = data["governance"]
    if age_seconds < -60:
        add(
            "snapshot_clock_skew",
            "high",
            "Snapshot timestamp is ahead of the Dashboard clock",
        )
    if age_seconds > int(_number(governance["freshness_threshold_seconds"])):
        add("dashboard_data_stale", "high", "Dashboard evidence is stale")
    if not governance["route_registry_matches"]:
        add(
            "route_registry_mismatch",
            "critical",
            "Route registry evidence does not match",
        )
    if not governance["image_manifest_matches"]:
        add(
            "image_manifest_mismatch",
            "critical",
            "Runtime image evidence does not match the manifest",
        )

    for service in data["services"]:
        if service["expected_state"] == "running":
            if service["state"] == "running_no_healthcheck":
                add(
                    f"service_no_healthcheck_{service['service']}",
                    "warning",
                    "A required service has no healthcheck evidence",
                )
            elif service["state"] != "running_healthy":
                add(
                    f"protected_service_{service['service']}",
                    "high",
                    "A required service is not healthy",
                )
        if service["service"] == "phoenix-engine" and service["state"] in {
            "stopped_failed",
            "missing",
        }:
            add("engine_crash", "critical", "Phoenix Engine is failed or missing")
        if (
            service["service"] == "rpc-gateway"
            and service["state"] != "running_healthy"
        ):
            add("rpc_gateway_unavailable", "critical", "RPC Gateway is unavailable")
        if service["restart_count"] >= 3:
            add(
                f"restart_loop_{service['service']}",
                "high",
                "A service restart loop was detected",
            )

    persistence = data["jetstream"]["persistence"]
    if (
        persistence["backlog_growth"] is None
        or persistence["throughput_per_second"] is None
        or persistence["batch_size"] is None
    ):
        add(
            "jetstream_rate_evidence_unavailable",
            "high",
            "JetStream rate evidence has not reached a measurable interval",
        )
    elif _number(persistence["backlog_growth"]) > 0:
        add("jetstream_backlog_growth", "warning", "JetStream backlog is growing")
    if any(
        value is None
        for value in persistence["database_write_latency_ms"].values()
    ):
        add(
            "recorder_latency_distribution_unavailable",
            "high",
            "Recorder latency distribution evidence is unavailable",
        )
    if any(
        not row["exists"]
        for row in data["jetstream"]["streams"] + data["jetstream"]["consumers"]
    ):
        add(
            "jetstream_resource_missing",
            "high",
            "A required JetStream resource is missing",
        )
    if any(row["storage_used_bps"] is None for row in data["jetstream"]["streams"]):
        add(
            "jetstream_storage_capacity_unavailable",
            "high",
            "JetStream storage capacity evidence is unavailable",
        )
    if any(
        _number(row["pending"]) > 0 and row["oldest_pending_age_seconds"] is None
        for row in data["jetstream"]["consumers"]
    ):
        add(
            "jetstream_pending_age_unavailable",
            "high",
            "JetStream pending age evidence is unavailable",
        )

    postgres = data["postgres"]
    if not postgres["readiness"]:
        add("postgres_unavailable", "critical", "PostgreSQL is unavailable")
    if _number(postgres["projected_disk_headroom_bytes"]) < _number(
        governance["database_headroom_min_bytes"]
    ):
        add(
            "database_headroom_low",
            "high",
            "Projected database disk headroom is below policy",
        )
    if any(
        postgres[key] is None
        for key in ("growth_bytes_1h", "growth_bytes_6h", "growth_bytes_24h")
    ):
        add(
            "database_growth_evidence_unavailable",
            "high",
            "Database growth evidence has not covered every required window",
        )

    feed = data["feed"]
    if _number(feed["gap_count"]) > 0 or feed["completeness_status"] != "complete":
        add(
            "feed_data_incomplete",
            "warning",
            "Feed completeness evidence requires review",
        )
    unsupported = sum(
        (_number(str(row["count"])) for row in feed["unsupported_kinds"]), Decimal(0)
    )
    if unsupported >= _number(governance["unsupported_message_alert_threshold"]):
        add(
            "unsupported_message_spike",
            "warning",
            "Unsupported feed message volume crossed policy",
        )

    rpc = data["rpc"]
    if any(
        provider["success_rate_bps"] is None
        or provider["budget_utilization_bps"] is None
        for provider in rpc["providers"]
    ):
        add(
            "rpc_provider_observation_unavailable",
            "high",
            "RPC provider observation evidence is unavailable",
        )
    if _number(rpc["disagreed"]) > 0:
        add(
            "verification_disagreement",
            "warning",
            "Independent RPC verification disagreement was observed",
        )
    if any(
        not provider["self_verification_prevented"] for provider in rpc["providers"]
    ):
        add(
            "self_verification_risk",
            "critical",
            "RPC self-verification prevention evidence failed",
        )

    reliability = data["reliability"]
    if _number(reliability["terminal_integrity_failures"]) > 0:
        add(
            "terminal_integrity_failure",
            "critical",
            "Terminal integrity failure was observed",
        )
    if _number(reliability["restart_loops"]) > 0:
        add(
            "runtime_restart_loop", "high", "Runtime restart-loop evidence was observed"
        )
    if reliability["protected_service_identity_status"] != "stable":
        add(
            "protected_identity_unverified",
            "high",
            "Protected service identity is not stable",
        )

    fork = data["fork"]
    if _number(fork["absolute_prediction_error"]) > _number(
        governance["fork_prediction_error_limit"]
    ):
        add("fork_prediction_error", "warning", "Fork prediction error exceeds policy")
    if _number(fork["contract_guard_failures"]) > 0:
        add(
            "fork_contract_guard_failure",
            "high",
            "Fork contract guard failure was observed",
        )

    severity_order = {"critical": 0, "high": 1, "warning": 2, "info": 3}
    return tuple(
        sorted(
            alerts.values(),
            key=lambda item: (severity_order[item["severity"]], item["code"]),
        )
    )


def _gate(alerts: tuple[dict[str, str], ...]) -> tuple[str, tuple[str, ...]]:
    if any(item["severity"] in {"critical", "high"} for item in alerts):
        status = "blocked"
    elif alerts:
        status = "review_required"
    else:
        status = "evidence_clear"
    return status, tuple(item["code"] for item in alerts)


def load_snapshot(
    path: str | Path = DEFAULT_SNAPSHOT_PATH, *, now: datetime | None = None
) -> DashboardSnapshot:
    source = Path(path)
    if source.is_symlink():
        _fail("snapshot_path_invalid")
    try:
        stat = source.stat()
    except OSError:
        _fail("snapshot_missing")
    if not source.is_file():
        _fail("snapshot_missing")
    if stat.st_size <= 0 or stat.st_size > MAX_SNAPSHOT_BYTES:
        _fail("snapshot_size_invalid")
    try:
        raw = source.read_bytes()
    except OSError:
        _fail("snapshot_unreadable")
    try:
        data = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_unique_object,
            parse_constant=_reject_non_finite,
        )
    except UnicodeDecodeError:
        _fail("snapshot_encoding_invalid")
    except json.JSONDecodeError:
        _fail("snapshot_json_invalid")
    validated = validate_snapshot(data)
    generated_at = _timestamp(validated["generated_at"])
    current = now or datetime.now(timezone.utc)
    if current.tzinfo is None:
        current = current.replace(tzinfo=timezone.utc)
    age_seconds = math.floor(
        (current.astimezone(timezone.utc) - generated_at).total_seconds()
    )
    alerts = derive_alerts(validated, age_seconds)
    gate_status, gate_reasons = _gate(alerts)
    return DashboardSnapshot(
        data=validated,
        path=source.resolve(),
        generated_at=generated_at,
        age_seconds=age_seconds,
        alerts=alerts,
        gate_status=gate_status,
        gate_reasons=gate_reasons,
    )


def canonical_snapshot_bytes(data: dict[str, Any]) -> bytes:
    validate_snapshot(data)
    raw = (json.dumps(data, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode(
        "utf-8"
    )
    if len(raw) > MAX_SNAPSHOT_BYTES:
        _fail("snapshot_size_invalid")
    return raw


def read_artifact(snapshot: DashboardSnapshot, artifact: dict[str, Any]) -> bytes:
    if not artifact["available"]:
        _fail("artifact_unavailable")
    relative = Path(artifact["path"])
    if relative.is_absolute() or len(relative.parts) != 1:
        _fail("artifact_path_invalid")
    root = snapshot.path.parent.resolve()
    unresolved_target = root / relative
    if unresolved_target.is_symlink():
        _fail("artifact_path_invalid")
    target = unresolved_target.resolve()
    if target.parent != root:
        _fail("artifact_path_invalid")
    try:
        stat = target.stat()
    except OSError:
        _fail("artifact_missing")
    if not target.is_file() or stat.st_size <= 0 or stat.st_size > MAX_ARTIFACT_BYTES:
        _fail("artifact_size_invalid")
    if stat.st_size != artifact["size_bytes"]:
        _fail("artifact_size_mismatch")
    try:
        raw = target.read_bytes()
    except OSError:
        _fail("artifact_unreadable")
    digest = "sha256:" + hashlib.sha256(raw).hexdigest()
    if digest != artifact["sha256"]:
        _fail("artifact_digest_mismatch")
    validate_artifact_payload(raw, artifact["content_type"])
    return raw
