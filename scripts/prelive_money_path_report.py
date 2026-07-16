#!/usr/bin/env python3
"""Render bounded, identity-free PRE-LIVE SHADOW money-path evidence."""

from __future__ import annotations

import argparse
from decimal import Decimal, InvalidOperation
import json
from pathlib import Path
import re
import sys
from typing import Any
from urllib import parse, request


REPORT_SCHEMA = "phoenix.prelive.money-path-summary.v1"
SOURCE_SCHEMA = "phoenix.prelive.money-path-source.v1"
PROMETHEUS_URL = "http://127.0.0.1:9090/api/v1/query"
MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_METRIC_SERIES = 2_048
MAX_OUTPUT_BYTES = 2 * 1024 * 1024
INTEGER_RE = re.compile(r"^(?:0|[1-9][0-9]*|-?[1-9][0-9]*)$")
TIMESTAMP_RE = re.compile(
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?Z$"
)

EXPECTED_JOBS = (
    "feed-ingestor",
    "phoenix-engine",
    "recorder",
    "rpc-gateway",
    "shadow-dispatcher",
)

NO_LABEL_METRICS = {
    "feed_connections_total",
    "feed_data_completeness",
    "feed_decode_failures_total",
    "feed_ignored_messages_total",
    "feed_last_gap_timestamp_seconds",
    "feed_messages_total",
    "feed_missing_sequences_total",
    "feed_normalized_transactions_total",
    "feed_publish_failures_total",
    "feed_publish_success_total",
    "feed_readiness",
    "feed_reconnects_total",
    "feed_sequence_gap_messages_total",
    "feed_sequence_gaps_total",
    "feed_unsupported_messages_total",
    "phoenix_configured_route_matches_total",
    "phoenix_engine_candidates_total",
    "phoenix_engine_consumer_ack_pending",
    "phoenix_engine_consumer_pending",
    "phoenix_engine_dependency_exhausted_total",
    "phoenix_engine_inputs_processed_total",
    "phoenix_engine_inputs_received_total",
    "phoenix_engine_later_message_progress_after_quarantine_total",
    "phoenix_engine_no_route_total",
    "phoenix_engine_persistence_latency_seconds",
    "phoenix_engine_processing_failures_total",
    "phoenix_engine_processing_latency_seconds",
    "phoenix_engine_readiness",
    "phoenix_engine_recovered_retries_total",
    "phoenix_engine_redeliveries_total",
    "phoenix_engine_retries_total",
    "phoenix_engine_shadow_accepted_total",
    "phoenix_engine_shadow_rejected_total",
    "phoenix_engine_terminal_integrity_total",
    "phoenix_malformed_inputs_total",
    "phoenix_official_router_inputs_total",
    "phoenix_profitability_estimated_execution_gas_total",
    "phoenix_profitability_near_profitable_total",
    "phoenix_route_discovery_eligible_total",
    "phoenix_supported_exact_input_inputs_total",
    "recorder_batch_messages",
    "recorder_batch_persist_latency_seconds",
    "recorder_batches_persisted_total",
    "recorder_consumer_ack_pending",
    "recorder_consumer_pending_messages",
    "recorder_database_failures_total",
    "recorder_database_retries_total",
    "recorder_database_retry_recoveries_total",
    "recorder_jetstream_ack_failures_total",
    "recorder_jetstream_fetch_failures_total",
    "recorder_jetstream_redeliveries_total",
    "recorder_messages_persisted_total",
    "recorder_messages_received_total",
    "recorder_nats_reconnects_total",
    "recorder_readiness",
    "rpc_coalesced_requests_total",
    "rpc_gateway_readiness",
    "rpc_primary_success_total",
    "rpc_provider_disagreement_total",
    "rpc_provider_rate_limited_total",
    "rpc_provider_unavailable_total",
    "rpc_secondary_agreed_total",
    "rpc_secondary_disagreed_total",
    "rpc_secondary_requested_total",
    "rpc_secondary_unavailable_total",
    "rpc_state_freshness_seconds",
    "rpc_state_request_budget_rejected_total",
    "rpc_state_request_latency_seconds_count",
    "rpc_state_request_latency_seconds_sum",
    "rpc_state_requests_total",
    "rpc_upstream_call_budget_rejected_total",
    "shadow_dispatcher_oldest_pending_age_seconds",
    "shadow_dispatcher_pending_rows",
    "shadow_dispatcher_publish_failures_total",
    "shadow_dispatcher_publish_latency_seconds",
    "shadow_dispatcher_publish_success_total",
    "shadow_dispatcher_readiness",
    "shadow_dispatcher_retries_total",
    "shadow_dispatcher_retry_recoveries_total",
    "shadow_dispatcher_terminal_integrity_failures_total",
}

METRIC_LABELS = {
    "feed_message_kind_total": ("classification", "kind", "layer"),
    "phoenix_engine_runtime_exits_total": ("class",),
    "phoenix_profitability_pnl_bucket_total": ("bucket", "scenario"),
    "phoenix_profitability_primary_total": ("status",),
    "phoenix_profitability_rejections_total": ("reason",),
    "phoenix_route_ranking_exclusions_total": ("reason",),
    "rpc_state_request_latency_seconds_bucket": ("le",),
    "rpc_upstream_calls_total": ("method", "outcome", "provider_slot"),
    "up": ("job",),
}
ALLOWED_METRICS = NO_LABEL_METRICS | set(METRIC_LABELS)

REJECTION_REASONS = {
    "confidence_too_low",
    "contract_path_unavailable",
    "duplicate_opportunity",
    "gas_too_high",
    "gross_spread_insufficient",
    "liquidity_insufficient",
    "net_pnl_negative",
    "opportunity_expired",
    "protocol_not_allowed",
    "quote_stale",
    "risk_budget_exceeded",
    "rpc_state_disagreement",
    "sequence_discontinuity",
    "simulation_evidence_insufficient",
    "simulation_reverted",
    "stress_pnl_negative",
    "token_not_allowed",
}
ROUTE_EXCLUSIONS = {
    "dependency_unavailable",
    "ineligible_origin",
    "integrity_failure",
    "no_affected_route",
    "not_profitable",
    "policy_rejected",
    "unsupported_origin",
}


class ReportError(ValueError):
    pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render bounded PRE-LIVE SHADOW technical and business evidence."
    )
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--metrics-input", type=Path)
    parser.add_argument("--format", choices=("json", "text"), default="text")
    parser.add_argument("--window-hours", type=int, default=24)
    args = parser.parse_args()
    if not 1 <= args.window_hours <= 168:
        parser.error("--window-hours must be between 1 and 168")
    return args


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ReportError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def reject_non_finite(_value: str) -> None:
    raise ReportError("JSON contains a non-finite number")


def load_json_bytes(raw: bytes, label: str) -> Any:
    if len(raw) > MAX_FILE_BYTES:
        raise ReportError(f"{label} exceeds the byte limit")
    try:
        return json.loads(
            raw,
            object_pairs_hook=unique_object,
            parse_float=Decimal,
            parse_constant=reject_non_finite,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReportError(f"{label} is not valid JSON") from error


def load_path(path: Path, label: str) -> Any:
    try:
        return load_json_bytes(path.read_bytes(), label)
    except OSError as error:
        raise ReportError(f"{label} is unavailable") from error


def require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ReportError(f"{label} must be an object")
    return value


def require_exact_keys(value: dict[str, Any], keys: set[str], label: str) -> None:
    if set(value) != keys:
        raise ReportError(f"{label} schema is not exact")


def require_integer(value: Any, label: str, signed: bool = False) -> str:
    if not isinstance(value, str) or not INTEGER_RE.fullmatch(value) or len(value) > 79:
        raise ReportError(f"{label} must be a bounded canonical integer string")
    if not signed and value.startswith("-"):
        raise ReportError(f"{label} must be non-negative")
    return value


def require_count_object(value: Any, keys: set[str], label: str) -> dict[str, str]:
    parsed = require_object(value, label)
    require_exact_keys(parsed, keys, label)
    return {key: require_integer(parsed[key], f"{label}.{key}") for key in sorted(keys)}


def require_count_array(
    value: Any, label: str, enum_values: set[str], maximum: int
) -> list[dict[str, str]]:
    if not isinstance(value, list) or len(value) > maximum:
        raise ReportError(f"{label} must be a bounded array")
    result: list[dict[str, str]] = []
    seen: set[str] = set()
    for index, raw in enumerate(value):
        item = require_object(raw, f"{label}[{index}]")
        require_exact_keys(item, {"reason", "count"}, f"{label}[{index}]")
        reason = item["reason"]
        if reason not in enum_values or reason in seen:
            raise ReportError(f"{label} contains an unsupported or duplicate reason")
        seen.add(reason)
        result.append({"reason": reason, "count": require_integer(item["count"], label)})
    return sorted(result, key=lambda item: item["reason"])


def validate_source(raw: Any, window_hours: int) -> dict[str, Any]:
    source = require_object(raw, "database source")
    keys = {
        "schema_version",
        "generated_at",
        "window_hours",
        "mode",
        "live_execution",
        "execution_eligible",
        "execution_request_created",
        "database",
        "engine",
        "outbox",
        "rpc",
        "profitability",
        "fork",
    }
    require_exact_keys(source, keys, "database source")
    if source["schema_version"] != SOURCE_SCHEMA:
        raise ReportError("database source schema version is unsupported")
    if not isinstance(source["generated_at"], str) or not TIMESTAMP_RE.fullmatch(
        source["generated_at"]
    ):
        raise ReportError("database source timestamp is invalid")
    if source["window_hours"] != str(window_hours):
        raise ReportError("database source window does not match the requested window")
    if (
        source["mode"] != "SHADOW"
        or source["live_execution"] is not False
        or source["execution_eligible"] is not False
        or source["execution_request_created"] is not False
    ):
        raise ReportError("database source violates SHADOW safety invariants")

    source["database"] = validate_database(source["database"])
    source["engine"] = require_count_object(
        source["engine"],
        {
            "candidate_count",
            "classifications_total",
            "decisions_total",
            "dependency_exhausted_total",
            "processing_attempts_total",
            "processing_latency_ns_max",
            "processing_latency_ns_sum",
            "redeliveries_total",
            "terminal_integrity_total",
        },
        "engine",
    )
    source["outbox"] = require_count_object(
        source["outbox"],
        {
            "backlog_growth",
            "oldest_pending_age_seconds",
            "pending_at_window_start",
            "pending_rows",
            "publish_attempts_total",
            "published_in_window",
            "retry_rows",
        },
        "outbox",
    )
    source["rpc"] = require_count_object(
        source["rpc"],
        {
            "disagreements_total",
            "latency_ns_max",
            "latency_ns_sum",
            "records_total",
            "retries_total",
            "stale_total",
            "success_total",
            "timeouts_total",
        },
        "rpc",
    )
    source["profitability"] = validate_profitability(source["profitability"])
    source["fork"] = validate_fork(source["fork"])
    return source


def validate_database(raw: Any) -> dict[str, Any]:
    value = require_object(raw, "database")
    require_exact_keys(value, {"size_bytes", "relations"}, "database")
    relations = value["relations"]
    allowed = {
        "engine_outbox",
        "fork_simulation_results",
        "rpc_quality_records",
        "shadow_engine_classifications",
        "shadow_engine_processing_attempts",
        "shadow_profitability_facts",
    }
    if not isinstance(relations, list) or len(relations) != len(allowed):
        raise ReportError("database relations are incomplete")
    parsed_relations = []
    seen = set()
    for raw_relation in relations:
        relation = require_object(raw_relation, "database relation")
        require_exact_keys(relation, {"name", "size_bytes"}, "database relation")
        if relation["name"] not in allowed or relation["name"] in seen:
            raise ReportError("database relation is unsupported or duplicated")
        seen.add(relation["name"])
        parsed_relations.append(
            {
                "name": relation["name"],
                "size_bytes": require_integer(relation["size_bytes"], "relation size"),
            }
        )
    return {
        "size_bytes": require_integer(value["size_bytes"], "database size"),
        "relations": sorted(parsed_relations, key=lambda item: item["name"]),
    }


def validate_profitability(raw: Any) -> dict[str, Any]:
    value = require_object(raw, "profitability")
    count_keys = {
        "accepted_total",
        "complete_total",
        "facts_total",
        "incomplete_total",
        "near_profitable_total",
        "not_profitable_total",
        "profitable_total",
        "rejected_total",
        "sum_conservative_net_pnl",
        "sum_expected_net_pnl",
        "sum_severe_net_pnl",
        "sum_total_cost",
    }
    require_exact_keys(value, count_keys | {"rejection_reasons"}, "profitability")
    result = {
        key: require_integer(value[key], f"profitability.{key}", signed=key.startswith("sum_"))
        for key in sorted(count_keys)
    }
    result["rejection_reasons"] = require_count_array(
        value["rejection_reasons"], "profitability.rejection_reasons", REJECTION_REASONS, 17
    )
    return result


def validate_fork(raw: Any) -> dict[str, Any]:
    value = require_object(raw, "fork")
    keys = {
        "gas_utilization_at_most_50_total",
        "gas_utilization_at_most_90_total",
        "gas_utilization_over_90_total",
        "passed_total",
        "prediction_error_negative_total",
        "prediction_error_non_negative_total",
        "reverted_total",
        "simulated_not_profitable_total",
        "simulated_profitable_total",
        "simulations_total",
        "sum_absolute_prediction_error",
        "sum_gas_used",
    }
    return require_count_object(value, keys, "fork")


def fetch_prometheus() -> Any:
    names = "|".join(sorted(re.escape(name) for name in ALLOWED_METRICS))
    query = f'{{__name__=~"^({names})$"}}'
    body = parse.urlencode({"query": query}).encode("ascii")
    try:
        with request.urlopen(request.Request(PROMETHEUS_URL, data=body), timeout=5) as response:
            raw = response.read(MAX_FILE_BYTES + 1)
    except OSError as error:
        raise ReportError("loopback Prometheus query failed") from error
    return load_json_bytes(raw, "Prometheus response")


def canonical_decimal(value: Any, label: str) -> str:
    if not isinstance(value, str) or len(value) > 128:
        raise ReportError(f"{label} is not a bounded Prometheus number")
    try:
        parsed = Decimal(value)
    except InvalidOperation as error:
        raise ReportError(f"{label} is not numeric") from error
    if not parsed.is_finite() or parsed < 0:
        raise ReportError(f"{label} must be finite and non-negative")
    normalized = format(parsed, "f")
    if "." in normalized:
        normalized = normalized.rstrip("0").rstrip(".")
    return normalized or "0"


def expected_job(metric_name: str) -> str | None:
    if metric_name.startswith("feed_"):
        return "feed-ingestor"
    if metric_name.startswith("phoenix_") or metric_name in {
        "rpc_primary_screen_rejected_total",
        "rpc_secondary_skipped_total",
    }:
        return "phoenix-engine"
    if metric_name.startswith("recorder_"):
        return "recorder"
    if metric_name.startswith("rpc_"):
        return "rpc-gateway"
    if metric_name.startswith("shadow_dispatcher_"):
        return "shadow-dispatcher"
    return None


def validate_semantic_labels(name: str, labels: dict[str, str]) -> None:
    if name == "feed_message_kind_total":
        if (
            labels["classification"] not in {"ignored", "unsupported"}
            or labels["layer"] not in {"l1", "l2"}
            or not labels["kind"].isdigit()
            or not 0 <= int(labels["kind"]) <= 255
        ):
            raise ReportError("feed kind metric has an invalid bounded label")
    elif name == "phoenix_engine_runtime_exits_total" and labels["class"] not in {
        "acknowledgement_failed",
        "fetch_failed",
        "integrity_failure",
        "shutdown",
        "store_failed",
    }:
        raise ReportError("runtime exit metric has an invalid class")
    elif name == "phoenix_profitability_primary_total" and labels["status"] not in {
        "incomplete",
        "not_profitable",
        "profitable",
    }:
        raise ReportError("profitability status is invalid")
    elif name == "phoenix_profitability_rejections_total" and labels["reason"] not in REJECTION_REASONS:
        raise ReportError("profitability rejection metric has an invalid reason")
    elif name == "phoenix_route_ranking_exclusions_total" and labels["reason"] not in ROUTE_EXCLUSIONS:
        raise ReportError("route exclusion metric has an invalid reason")
    elif name == "phoenix_profitability_pnl_bucket_total":
        if labels["scenario"] not in {"conservative", "expected", "severe"} or labels[
            "bucket"
        ] not in {"at_least_two_x", "below_minimum", "minimum_to_two_x", "non_positive"}:
            raise ReportError("profitability bucket labels are invalid")
    elif name == "rpc_upstream_calls_total":
        if (
            labels["provider_slot"] not in {"primary", "probe", "secondary"}
            or labels["outcome"] not in {"failure", "rate_limited", "success", "timeout"}
            or labels["method"] not in {
                "eth_blockNumber",
                "eth_call",
                "eth_chainId",
                "eth_getBalance",
                "eth_getBlockByNumber",
                "eth_getCode",
                "eth_getLogs",
                "eth_getStorageAt",
            }
        ):
            raise ReportError("RPC upstream labels are invalid")
    elif name == "rpc_state_request_latency_seconds_bucket":
        if labels["le"] not in {"+Inf", "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "2.5", "5"}:
            raise ReportError("RPC latency bucket is invalid")
    elif name == "up" and labels["job"] not in EXPECTED_JOBS:
        raise ReportError("Prometheus job is invalid")


def validate_metrics(raw: Any) -> list[dict[str, Any]]:
    root = require_object(raw, "Prometheus response")
    require_exact_keys(root, {"status", "data"}, "Prometheus response")
    if root["status"] != "success":
        raise ReportError("Prometheus query did not succeed")
    data = require_object(root["data"], "Prometheus data")
    require_exact_keys(data, {"resultType", "result"}, "Prometheus data")
    if data["resultType"] != "vector" or not isinstance(data["result"], list):
        raise ReportError("Prometheus response is not an instant vector")
    if len(data["result"]) > MAX_METRIC_SERIES:
        raise ReportError("Prometheus series limit exceeded")

    series = []
    seen: set[tuple[str, tuple[tuple[str, str], ...]]] = set()
    for index, raw_sample in enumerate(data["result"]):
        sample = require_object(raw_sample, f"Prometheus sample {index}")
        require_exact_keys(sample, {"metric", "value"}, f"Prometheus sample {index}")
        raw_labels = require_object(sample["metric"], f"Prometheus labels {index}")
        name = raw_labels.get("__name__")
        if name not in ALLOWED_METRICS:
            raise ReportError("Prometheus returned a metric outside the reviewed allowlist")
        job = raw_labels.get("job")
        expected = expected_job(name)
        if expected is not None and job != expected:
            raise ReportError("Prometheus metric came from an unexpected job")
        instance = raw_labels.get("instance")
        if (
            not isinstance(instance, str)
            or not 1 <= len(instance) <= 255
            or any(ord(character) < 32 or ord(character) == 127 for character in instance)
        ):
            raise ReportError("Prometheus sample is missing its scrape instance")

        labels = {
            key: value
            for key, value in raw_labels.items()
            if key not in {"__name__", "instance", "job"}
        }
        if name == "up":
            labels = {"job": job}
        expected_labels = set(METRIC_LABELS.get(name, ()))
        if set(labels) != expected_labels or any(
            not isinstance(value, str) or len(value) > 64 for value in labels.values()
        ):
            raise ReportError("Prometheus metric labels do not match the reviewed schema")
        validate_semantic_labels(name, labels)
        value = sample["value"]
        if not isinstance(value, list) or len(value) != 2:
            raise ReportError("Prometheus sample value is invalid")
        timestamp = value[0]
        if (
            isinstance(timestamp, bool)
            or not isinstance(timestamp, (int, Decimal))
            or timestamp < 0
            or isinstance(timestamp, Decimal)
            and not timestamp.is_finite()
        ):
            raise ReportError("Prometheus sample timestamp is invalid")
        canonical_value = canonical_decimal(value[1], name)
        key = (name, tuple(sorted(labels.items())))
        if key in seen:
            raise ReportError("Prometheus response contains duplicate series")
        seen.add(key)
        series.append({"name": name, "labels": dict(sorted(labels.items())), "value": canonical_value})

    for job in EXPECTED_JOBS:
        if not any(item["name"] == "up" and item["labels"] == {"job": job} for item in series):
            raise ReportError(f"Prometheus scrape evidence is missing for {job}")
    for readiness in (
        "feed_readiness",
        "phoenix_engine_readiness",
        "recorder_readiness",
        "rpc_gateway_readiness",
        "shadow_dispatcher_readiness",
    ):
        if not any(item["name"] == readiness for item in series):
            raise ReportError(f"readiness evidence is missing for {readiness}")
    return sorted(series, key=lambda item: (item["name"], tuple(item["labels"].items())))


def metric_value(series: list[dict[str, Any]], name: str, **labels: str) -> str:
    values = [Decimal(item["value"]) for item in series if item["name"] == name and item["labels"] == labels]
    return canonical_decimal(str(sum(values, Decimal(0))), name)


def build_report(source: dict[str, Any], series: list[dict[str, Any]]) -> dict[str, Any]:
    scrape_health = {
        job: metric_value(series, "up", job=job) for job in EXPECTED_JOBS
    }
    technical = {
        "scrape_health": scrape_health,
        "feed": {
            "connections_total": metric_value(series, "feed_connections_total"),
            "data_completeness": metric_value(series, "feed_data_completeness"),
            "decode_failures_total": metric_value(series, "feed_decode_failures_total"),
            "missing_sequences_total": metric_value(series, "feed_missing_sequences_total"),
            "reconnects_total": metric_value(series, "feed_reconnects_total"),
            "sequence_gaps_total": metric_value(series, "feed_sequence_gaps_total"),
        },
        "rpc": {
            "primary_success_total": metric_value(series, "rpc_primary_success_total"),
            "provider_disagreement_total": metric_value(series, "rpc_provider_disagreement_total"),
            "provider_rate_limited_total": metric_value(series, "rpc_provider_rate_limited_total"),
            "provider_unavailable_total": metric_value(series, "rpc_provider_unavailable_total"),
            "secondary_agreed_total": metric_value(series, "rpc_secondary_agreed_total"),
            "secondary_disagreed_total": metric_value(series, "rpc_secondary_disagreed_total"),
            "secondary_requested_total": metric_value(series, "rpc_secondary_requested_total"),
            "secondary_unavailable_total": metric_value(series, "rpc_secondary_unavailable_total"),
            "state_freshness_seconds": metric_value(series, "rpc_state_freshness_seconds"),
            "state_requests_total": metric_value(series, "rpc_state_requests_total"),
        },
        "reliability": {
            "engine_recovered_retries_total": metric_value(series, "phoenix_engine_recovered_retries_total"),
            "engine_retries_total": metric_value(series, "phoenix_engine_retries_total"),
            "engine_terminal_integrity_total": metric_value(series, "phoenix_engine_terminal_integrity_total"),
            "recorder_database_recoveries_total": metric_value(series, "recorder_database_retry_recoveries_total"),
            "recorder_database_retries_total": metric_value(series, "recorder_database_retries_total"),
            "shadow_dispatcher_retry_recoveries_total": metric_value(series, "shadow_dispatcher_retry_recoveries_total"),
            "shadow_dispatcher_retries_total": metric_value(series, "shadow_dispatcher_retries_total"),
            "shadow_dispatcher_terminal_integrity_total": metric_value(series, "shadow_dispatcher_terminal_integrity_failures_total"),
        },
        "jetstream": {
            "engine_ack_pending": metric_value(series, "phoenix_engine_consumer_ack_pending"),
            "engine_pending": metric_value(series, "phoenix_engine_consumer_pending"),
            "recorder_ack_pending": metric_value(series, "recorder_consumer_ack_pending"),
            "recorder_pending": metric_value(series, "recorder_consumer_pending_messages"),
            "dispatcher_oldest_pending_age_seconds": metric_value(series, "shadow_dispatcher_oldest_pending_age_seconds"),
            "dispatcher_pending_rows": metric_value(series, "shadow_dispatcher_pending_rows"),
        },
        "database": source["database"],
        "engine_database": source["engine"],
        "outbox_database": source["outbox"],
        "rpc_database": source["rpc"],
        "fork": source["fork"],
    }
    business = {
        "profitability_funnel": {
            "configured_route_matches": metric_value(series, "phoenix_configured_route_matches_total"),
            "feed_inputs": metric_value(series, "feed_messages_total"),
            "fork_passed": source["fork"]["passed_total"],
            "normalized_transactions": metric_value(series, "feed_normalized_transactions_total"),
            "official_router_inputs": metric_value(series, "phoenix_official_router_inputs_total"),
            "primary_profitable": metric_value(series, "phoenix_profitability_primary_total", status="profitable"),
            "shadow_accepted": metric_value(series, "phoenix_engine_shadow_accepted_total"),
            "supported_exact_input_inputs": metric_value(series, "phoenix_supported_exact_input_inputs_total"),
        },
        "profitability": source["profitability"],
        "route_intelligence": {
            "candidate_count": source["engine"]["candidate_count"],
            "configured_route_matches": metric_value(series, "phoenix_configured_route_matches_total"),
            "no_route_total": metric_value(series, "phoenix_engine_no_route_total"),
            "route_discovery_eligible_total": metric_value(series, "phoenix_route_discovery_eligible_total"),
        },
    }
    return {
        "schema_version": REPORT_SCHEMA,
        "generated_at": source["generated_at"],
        "window_hours": source["window_hours"],
        "metric_counter_scope": "process_lifetime",
        "mode": "SHADOW",
        "live_execution": False,
        "execution_eligible": False,
        "execution_request_created": False,
        "technical": technical,
        "business": business,
        "metric_series": series,
    }


def render_text(report: dict[str, Any]) -> str:
    technical = report["technical"]
    business = report["business"]
    lines = [
        "Phoenix PRE-LIVE SHADOW Money-Path Evidence",
        f"Database window: {report['window_hours']} hours",
        "Runtime counter scope: process lifetime",
        "Safety: PHOENIX_MODE=SHADOW LIVE_EXECUTION=false execution_eligible=false execution_request_created=false",
        "",
        "Technical Report",
        "Scrapes: " + ", ".join(f"{job}={value}" for job, value in technical["scrape_health"].items()),
        "Feed: " + ", ".join(f"{key}={value}" for key, value in technical["feed"].items()),
        "RPC: " + ", ".join(f"{key}={value}" for key, value in technical["rpc"].items()),
        "JetStream: " + ", ".join(f"{key}={value}" for key, value in technical["jetstream"].items()),
        f"Database bytes: {technical['database']['size_bytes']}",
        f"Fork simulations: {technical['fork']['simulations_total']} (passed={technical['fork']['passed_total']}, reverted={technical['fork']['reverted_total']})",
        "",
        "Business Report",
        "Funnel: " + ", ".join(f"{key}={value}" for key, value in business["profitability_funnel"].items()),
        f"Expected PnL sum: {business['profitability']['sum_expected_net_pnl']}",
        f"Conservative PnL sum: {business['profitability']['sum_conservative_net_pnl']}",
        f"Severe PnL sum: {business['profitability']['sum_severe_net_pnl']}",
        f"Realization status: not realized; SHADOW evidence only",
    ]
    return "\n".join(lines) + "\n"


def main() -> int:
    args = parse_args()
    try:
        source = validate_source(load_path(args.source, "database source"), args.window_hours)
        metrics_raw = (
            load_path(args.metrics_input, "Prometheus fixture")
            if args.metrics_input is not None
            else fetch_prometheus()
        )
        report = build_report(source, validate_metrics(metrics_raw))
        output = (
            json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n"
            if args.format == "json"
            else render_text(report)
        )
        if len(output.encode("utf-8")) > MAX_OUTPUT_BYTES:
            raise ReportError("report exceeds the output byte limit")
        sys.stdout.write(output)
        return 0
    except ReportError as error:
        print(f"PRELIVE_MONEY_PATH_REPORT_BLOCKED: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
