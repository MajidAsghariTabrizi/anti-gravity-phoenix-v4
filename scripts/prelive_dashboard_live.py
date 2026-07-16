#!/usr/bin/env python3
"""Compile bounded live SHADOW facts into the read-only Dashboard contract."""

from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
from typing import Any, Iterable


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(REPO_ROOT))
sys.path.insert(0, str(REPO_ROOT / "dashboard"))
sys.path.insert(0, str(SCRIPT_DIR))

try:
    from dashboard.snapshot_model import (  # noqa: E402
        ARTIFACT_KINDS,
        MAX_ARTIFACT_BYTES,
        MAX_SNAPSHOT_BYTES,
        SnapshotError,
        canonical_snapshot_bytes,
        validate_artifact_payload,
        validate_snapshot,
    )
except ModuleNotFoundError:  # pragma: no cover - installed host layout
    from snapshot_model import (  # type: ignore[no-redef]  # noqa: E402
        ARTIFACT_KINDS,
        MAX_ARTIFACT_BYTES,
        MAX_SNAPSHOT_BYTES,
        SnapshotError,
        canonical_snapshot_bytes,
        validate_artifact_payload,
        validate_snapshot,
    )
from prelive_shadow_control import (  # noqa: E402
    FULL_SERVICES,
    JETSTREAM_CONSUMER_NAMES,
    JETSTREAM_STREAM_NAMES,
    PREFLIGHT_CHECKS,
    load_json as load_control_json,
    sample_from_money_path,
)


SOURCE_SCHEMA = "phoenix.prelive.dashboard-source.v1"
SERVICES_SCHEMA = "phoenix.prelive.dashboard-services.v1"
HISTORY_SCHEMA = "phoenix.prelive.dashboard-history.v1"
MAX_INPUT_BYTES = 2 * 1024 * 1024
MAX_HISTORY_ROWS = 3_000
MAX_ROUTES = 10
MAX_PROVIDERS = 8
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
INTEGER_RE = re.compile(r"^(?:0|[1-9][0-9]*)$")
SIGNED_INTEGER_RE = re.compile(r"^(?:0|-?[1-9][0-9]*)$")
SAFE_FILE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
GENERATION_FILE_RE = re.compile(
    r"^(?:technical|business|preflight|route-ranking|profitability|fork-simulation|"
    r"release-checksum|evidence-bundle)-[0-9a-f]{12}\.json$"
)

PROJECT_SERVICES = {
    "feed-ingestor",
    "recorder",
    "rpc-gateway",
    "phoenix-engine",
    "shadow-dispatcher",
    "dashboard",
}
STREAM_NAMES = JETSTREAM_STREAM_NAMES
CONSUMER_NAMES = JETSTREAM_CONSUMER_NAMES
ARTIFACT_LABELS = {
    "technical_json": "Technical JSON",
    "business_json": "Business JSON",
    "preflight_report": "Preflight report",
    "evidence_bundle": "Evidence bundle",
    "soak_report": "Soak report",
    "route_ranking": "Route ranking",
    "profitability_report": "Profitability report",
    "fork_simulation_report": "Fork simulation report",
    "release_manifest_checksum": "Release manifest checksum",
}

SOURCE_KEYS = {
    "schema_version",
    "generated_at",
    "database_clock",
    "evidence_window_started_at",
    "window_hours",
    "database",
    "route_registry",
    "routes",
    "distribution",
    "prediction_error",
    "daily_trend",
    "weekly_trend",
    "model_comparison",
    "providers",
}
DATABASE_KEYS = {
    "size_bytes",
    "active_connections",
    "checkpoints_timed",
    "checkpoints_requested",
    "wal_bytes",
    "oldest_relevant_event",
    "newest_relevant_event",
    "migration_version",
    "migration_checksum",
    "retention_status",
}
ROUTE_REGISTRY_KEYS = {"fact_count", "mismatch_count", "self_verification_collisions"}
ROUTE_KEYS = {
    "route_key",
    "sample_count",
    "primary_profitable_count",
    "independently_verified_count",
    "verification_disagreed_count",
    "verification_unavailable_count",
    "gross_profit",
    "total_cost",
    "gas_cost",
    "flash_premium",
    "ordering_cost",
    "safety_cost",
    "expected",
    "conservative",
    "severe",
    "minimum_shortfall",
    "first_observed_at",
    "last_observed_at",
    "liquidity_score_bps",
    "provider_requests",
    "provider_failures",
    "fork_unsigned_plans",
    "fork_simulations",
    "fork_success",
    "fork_reverted",
    "fork_profitable",
    "fork_gas_used",
    "fork_balance_delta",
    "fork_simulated_net_pnl",
    "fork_absolute_prediction_error",
    "fork_block",
    "fork_guard_failures",
}
PROVIDER_KEYS = {
    "provider_key",
    "role",
    "requests",
    "success",
    "timeouts",
    "unavailable",
    "p50_latency_ms",
    "p95_latency_ms",
    "p99_latency_ms",
}


class LiveDashboardError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def _fail(code: str) -> None:
    raise LiveDashboardError(code)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail("duplicate_json_key")
        result[key] = value
    return result


def _reject_non_finite(_value: str) -> None:
    _fail("non_finite_json")


def load_json(path: Path) -> Any:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise LiveDashboardError("input_unavailable") from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail("input_bounds_invalid")
    try:
        return json.loads(
            raw,
            object_pairs_hook=_unique_object,
            parse_constant=_reject_non_finite,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise LiveDashboardError("input_json_invalid") from exc


def _object(value: Any, keys: set[str], code: str = "source_shape_invalid") -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        _fail(code)
    return value


def _array(value: Any, maximum: int, code: str = "source_shape_invalid") -> list[Any]:
    if not isinstance(value, list) or len(value) > maximum:
        _fail(code)
    return value


def _source_value_code(path: str | None) -> str:
    if path is None:
        return "source_value_invalid"
    if len(path) > 160 or re.fullmatch(r"[A-Za-z0-9_.\[\]-]+", path) is None:
        _fail("source_path_invalid")
    return f"source_value_invalid:{path}"


def _text(value: Any, maximum: int = 256, *, path: str | None = None) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        _fail(_source_value_code(path))
    return value


def _integer_text(
    value: Any, *, signed: bool = False, path: str | None = None
) -> str:
    pattern = SIGNED_INTEGER_RE if signed else INTEGER_RE
    if not isinstance(value, str) or len(value) > 96 or pattern.fullmatch(value) is None:
        _fail(_source_value_code(path))
    return value


def _number(value: Any, *, signed: bool = False, path: str | None = None) -> Decimal:
    text = _integer_text(value, signed=signed, path=path)
    try:
        return Decimal(text)
    except InvalidOperation as exc:  # pragma: no cover - guarded by regex
        raise LiveDashboardError(_source_value_code(path)) from exc


def _optional_number(
    value: Any, *, signed: bool = False, path: str | None = None
) -> Decimal | None:
    if value is None:
        return None
    return _number(value, signed=signed, path=path)


def _timestamp(value: Any, *, path: str | None = None) -> datetime:
    text = _text(value, 40, path=path)
    try:
        parsed = datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError as exc:
        code = _source_value_code(path) if path is not None else "timestamp_invalid"
        raise LiveDashboardError(code) from exc
    if parsed.tzinfo is None:
        _fail(_source_value_code(path) if path is not None else "timestamp_invalid")
    return parsed.astimezone(timezone.utc)


def _optional_timestamp(value: Any, *, path: str | None = None) -> datetime | None:
    return None if value is None else _timestamp(value, path=path)


def _period(value: Any, pattern: str, format_string: str, path: str) -> str:
    text = _text(value, 16, path=path)
    if re.fullmatch(pattern, text) is None:
        _fail(_source_value_code(path))
    try:
        parsed = datetime.strptime(
            text + ("-1" if format_string == "%G-W%V" else ""),
            format_string + ("-%u" if format_string == "%G-W%V" else ""),
        )
    except ValueError as exc:
        raise LiveDashboardError(_source_value_code(path)) from exc
    rendered = parsed.strftime(format_string)
    if rendered != text:
        _fail(_source_value_code(path))
    return text


def _canonical_timestamp(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _canonical_decimal(value: Decimal) -> str:
    if value == value.to_integral_value():
        return str(int(value))
    rendered = format(value.normalize(), "f")
    return rendered.rstrip("0").rstrip(".") if "." in rendered else rendered


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def _digest(raw: bytes) -> str:
    return "sha256:" + hashlib.sha256(raw).hexdigest()


def write_atomic(path: Path, raw: bytes, mode: int = 0o640) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb", prefix=f".{path.name}.", suffix=".tmp", dir=path.parent, delete=False
        ) as handle:
            temporary = Path(handle.name)
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def validate_source(value: Any) -> dict[str, Any]:
    source = _object(value, SOURCE_KEYS)
    if source["schema_version"] != SOURCE_SCHEMA:
        _fail("source_schema_invalid")
    generated = _timestamp(source["generated_at"], path="generated_at")
    clock = _timestamp(source["database_clock"], path="database_clock")
    if abs((generated - clock).total_seconds()) > 120:
        _fail("database_clock_mismatch")
    window = int(_integer_text(source["window_hours"], path="window_hours"))
    if window not in {1, 6, 24, 168}:
        _fail("source_window_invalid")
    evidence_start = _timestamp(
        source["evidence_window_started_at"], path="evidence_window_started_at"
    )
    evidence_age = (generated - evidence_start).total_seconds()
    if evidence_age < 0 or evidence_age > window * 3600 + 120:
        _fail("source_window_invalid")

    database = _object(source["database"], DATABASE_KEYS)
    for key in (
        "size_bytes",
        "active_connections",
        "checkpoints_timed",
        "checkpoints_requested",
        "wal_bytes",
    ):
        _number(database[key], path=f"database.{key}")
    _optional_timestamp(
        database["oldest_relevant_event"], path="database.oldest_relevant_event"
    )
    _optional_timestamp(
        database["newest_relevant_event"], path="database.newest_relevant_event"
    )
    _text(database["migration_version"], 64, path="database.migration_version")
    checksum = _text(
        database["migration_checksum"], 71, path="database.migration_checksum"
    )
    if not re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}", checksum):
        _fail(_source_value_code("database.migration_checksum"))
    if database["retention_status"] not in {"configured", "not_configured", "unknown"}:
        _fail(_source_value_code("database.retention_status"))

    registry = _object(source["route_registry"], ROUTE_REGISTRY_KEYS)
    for key, nested in registry.items():
        _number(nested, path=f"route_registry.{key}")

    route_keys: set[str] = set()
    for route_index, raw in enumerate(_array(source["routes"], MAX_ROUTES)):
        route = _object(raw, ROUTE_KEYS)
        route_path = f"routes[{route_index}]"
        route_key = _text(route["route_key"], path=f"{route_path}.route_key")
        if route_key in route_keys:
            _fail("source_duplicate_identity")
        route_keys.add(route_key)
        signed_fields = {
            "expected",
            "conservative",
            "severe",
            "gross_profit",
            "fork_balance_delta",
            "fork_simulated_net_pnl",
        }
        numeric_fields = ROUTE_KEYS - {
            "route_key",
            "minimum_shortfall",
            "first_observed_at",
            "last_observed_at",
            "liquidity_score_bps",
        }
        route_numbers = {
            key: _number(
                route[key],
                signed=key in signed_fields,
                path=f"{route_path}.{key}",
            )
            for key in numeric_fields
        }
        minimum_shortfall = _optional_number(
            route["minimum_shortfall"], path=f"{route_path}.minimum_shortfall"
        )
        _timestamp(
            route["first_observed_at"], path=f"{route_path}.first_observed_at"
        )
        _timestamp(route["last_observed_at"], path=f"{route_path}.last_observed_at")
        liquidity = _optional_number(
            route["liquidity_score_bps"], path=f"{route_path}.liquidity_score_bps"
        )
        if liquidity is not None and liquidity > 10_000:
            _fail(_source_value_code(f"{route_path}.liquidity_score_bps"))
        if (
            route_numbers["fork_success"] + route_numbers["fork_reverted"]
            != route_numbers["fork_simulations"]
        ):
            _fail("source_accounting_invalid")
        if route_numbers["fork_profitable"] > route_numbers["fork_success"]:
            _fail("source_accounting_invalid")
        costs = sum(
            (
                route_numbers[key]
                for key in ("gas_cost", "flash_premium", "ordering_cost", "safety_cost")
            ),
            Decimal(0),
        )
        if costs != route_numbers["total_cost"]:
            _fail("source_accounting_invalid")
        if (
            route_numbers["gross_profit"] - route_numbers["total_cost"]
            != route_numbers["expected"]
        ):
            _fail("source_accounting_invalid")
        if minimum_shortfall is not None and minimum_shortfall < 0:
            _fail(_source_value_code(f"{route_path}.minimum_shortfall"))

    for row_index, raw in enumerate(_array(source["distribution"], 30)):
        row = _object(raw, {"scenario", "bucket", "count"})
        row_path = f"distribution[{row_index}]"
        if row["scenario"] not in {"expected", "conservative", "severe", "fork_simulated"}:
            _fail(_source_value_code(f"{row_path}.scenario"))
        _text(row["bucket"], 32, path=f"{row_path}.bucket")
        _number(row["count"], path=f"{row_path}.count")
    for row_index, raw in enumerate(_array(source["prediction_error"], 20)):
        row = _object(raw, {"bucket", "count"})
        row_path = f"prediction_error[{row_index}]"
        _text(row["bucket"], 32, path=f"{row_path}.bucket")
        _number(row["count"], path=f"{row_path}.count")
    for name, maximum, period_pattern, period_format in (
        ("daily_trend", 31, r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$", "%Y-%m-%d"),
        ("weekly_trend", 12, r"^[0-9]{4}-W[0-9]{2}$", "%G-W%V"),
    ):
        for row_index, raw in enumerate(_array(source[name], maximum)):
            row = _object(
                raw,
                {"period", "expected", "conservative", "severe", "fork_simulated", "sample_count"},
            )
            row_path = f"{name}[{row_index}]"
            _period(
                row["period"],
                period_pattern,
                period_format,
                f"{row_path}.period",
            )
            for key in ("expected", "conservative", "severe", "fork_simulated"):
                _number(row[key], signed=True, path=f"{row_path}.{key}")
            _number(row["sample_count"], path=f"{row_path}.sample_count")
    for row_index, raw in enumerate(_array(source["model_comparison"], 10)):
        row = _object(
            raw,
            {"model_version", "sample_count", "expected", "conservative", "absolute_fork_error"},
        )
        row_path = f"model_comparison[{row_index}]"
        _text(row["model_version"], 64, path=f"{row_path}.model_version")
        _number(row["sample_count"], path=f"{row_path}.sample_count")
        _number(row["expected"], signed=True, path=f"{row_path}.expected")
        _number(row["conservative"], signed=True, path=f"{row_path}.conservative")
        _number(row["absolute_fork_error"], path=f"{row_path}.absolute_fork_error")

    provider_keys: set[tuple[str, str]] = set()
    for provider_index, raw in enumerate(_array(source["providers"], MAX_PROVIDERS)):
        provider = _object(raw, PROVIDER_KEYS)
        provider_path = f"providers[{provider_index}]"
        key = _text(
            provider["provider_key"], path=f"{provider_path}.provider_key"
        )
        role = provider["role"]
        if role not in {"primary", "secondary"}:
            _fail(_source_value_code(f"{provider_path}.role"))
        if (role, key) in provider_keys:
            _fail("source_duplicate_identity")
        provider_keys.add((role, key))
        provider_numbers: dict[str, Decimal] = {}
        for field in ("requests", "success", "timeouts", "unavailable"):
            provider_numbers[field] = _number(
                provider[field], path=f"{provider_path}.{field}"
            )
        if sum(
            (
                provider_numbers[field]
                for field in ("success", "timeouts", "unavailable")
            ),
            Decimal(0),
        ) != provider_numbers["requests"]:
            _fail("source_accounting_invalid")
        for field in ("p50_latency_ms", "p95_latency_ms", "p99_latency_ms"):
            _optional_number(provider[field], path=f"{provider_path}.{field}")
    return source


def normalize_services(path: Path, observed_at: str) -> dict[str, Any]:
    observed = _canonical_timestamp(_timestamp(observed_at))
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise LiveDashboardError("service_state_unavailable") from exc
    if len(lines) != len(FULL_SERVICES):
        _fail("service_state_invalid")
    services: list[dict[str, Any]] = []
    for index, line in enumerate(lines):
        fields = line.split("\t")
        if len(fields) != 8 or fields[0] != FULL_SERVICES[index]:
            _fail("service_state_invalid")
        service, status, health, digest, restart_raw, exit_raw, started_raw, oom_raw = fields
        if DIGEST_RE.fullmatch(digest) is None:
            _fail("service_state_invalid")
        restart = int(_integer_text(restart_raw))
        if oom_raw not in {"true", "false"}:
            _fail("service_state_invalid")
        if exit_raw == "null":
            exit_code = None
        else:
            exit_code = int(_integer_text(exit_raw))
            if exit_code > 255:
                _fail("service_state_invalid")
        if started_raw == "null" or started_raw.startswith("0001-"):
            started_at = None
        else:
            started_at = _canonical_timestamp(_timestamp(started_raw))
        if status == "running":
            state = {
                "healthy": "running_healthy",
                "unhealthy": "running_unhealthy",
                "starting": "running_unhealthy",
                "none": "running_no_healthcheck",
            }.get(health)
            if state is None or started_at is None:
                _fail("service_state_invalid")
            exit_code = None
        elif status == "exited":
            state = "stopped_clean" if exit_code == 0 else "stopped_failed"
        elif status in {"dead", "restarting"}:
            state = "stopped_failed"
        elif status == "created":
            state = "created_not_started"
        elif status == "missing":
            state = "missing"
            started_at = None
            exit_code = None
        elif status == "paused":
            state = "unknown"
        else:
            _fail("service_state_invalid")
        services.append(
            {
                "service": service,
                "state": state,
                "image_digest": digest,
                "started_at": started_at,
                "exit_code": exit_code,
                "oom": oom_raw == "true",
                "restart_count": restart,
            }
        )
    return {"schema_version": SERVICES_SCHEMA, "observed_at": observed, "services": services}


def _validate_services(value: Any) -> dict[str, Any]:
    root = _object(value, {"schema_version", "observed_at", "services"}, "service_state_invalid")
    if root["schema_version"] != SERVICES_SCHEMA:
        _fail("service_state_invalid")
    _timestamp(root["observed_at"])
    rows = _array(root["services"], len(FULL_SERVICES), "service_state_invalid")
    if len(rows) != len(FULL_SERVICES):
        _fail("service_state_invalid")
    expected = {
        "service",
        "state",
        "image_digest",
        "started_at",
        "exit_code",
        "oom",
        "restart_count",
    }
    for index, raw in enumerate(rows):
        row = _object(raw, expected, "service_state_invalid")
        if row["service"] != FULL_SERVICES[index] or DIGEST_RE.fullmatch(row["image_digest"]) is None:
            _fail("service_state_invalid")
        if row["started_at"] is not None:
            _timestamp(row["started_at"])
        if row["exit_code"] is not None and (
            isinstance(row["exit_code"], bool) or not isinstance(row["exit_code"], int)
        ):
            _fail("service_state_invalid")
        if row["exit_code"] is not None and not 0 <= row["exit_code"] <= 255:
            _fail("service_state_invalid")
        if not isinstance(row["oom"], bool) or isinstance(row["restart_count"], bool) or not isinstance(
            row["restart_count"], int
        ):
            _fail("service_state_invalid")
        if row["restart_count"] < 0 or row["state"] not in {
            "running_healthy",
            "running_unhealthy",
            "running_no_healthcheck",
            "stopped_clean",
            "stopped_failed",
            "created_not_started",
            "missing",
            "unknown",
        }:
            _fail("service_state_invalid")
    return root


def _walk(value: Any) -> Iterable[dict[str, Any]]:
    if isinstance(value, dict):
        yield value
        for nested in value.values():
            yield from _walk(nested)
    elif isinstance(value, list):
        for nested in value:
            yield from _walk(nested)


def _resource(root: Any, name: str, *, consumer: bool) -> dict[str, Any]:
    matches = []
    for item in _walk(root):
        config = item.get("config") if isinstance(item.get("config"), dict) else {}
        names = {item.get("name"), config.get("name"), config.get("durable_name")}
        if name in names and (not consumer or config.get("durable_name") == name or item.get("name") == name):
            matches.append(item)
    if not matches:
        _fail("jetstream_resource_unavailable")
    runtime_keys = {
        "messages",
        "bytes",
        "num_pending",
        "num_ack_pending",
        "num_redelivered",
    }
    return max(
        matches,
        key=lambda item: (
            isinstance(item.get("state"), dict)
            or any(key in item for key in runtime_keys),
            isinstance(item.get("config"), dict),
            len(item),
        ),
    )


def _resource_count(item: dict[str, Any], key: str) -> int:
    state = item.get("state") if isinstance(item.get("state"), dict) else {}
    if key in state:
        value = state[key]
    elif key in item:
        value = item[key]
    else:
        _fail("jetstream_value_invalid")
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        _fail("jetstream_value_invalid")
    return value


def _metric_rows(money: dict[str, Any], name: str, **labels: str) -> Decimal:
    total = Decimal(0)
    for row in money.get("metric_series", []):
        if row.get("name") == name and row.get("labels") == labels:
            try:
                total += Decimal(row["value"])
            except (InvalidOperation, KeyError):
                _fail("money_path_invalid")
    return total


def _metric_optional(money: dict[str, Any], name: str, **labels: str) -> str | None:
    matches = [
        row
        for row in money.get("metric_series", [])
        if row.get("name") == name and row.get("labels") == labels
    ]
    if not matches:
        return None
    return str(_metric_rows(money, name, **labels))


def _metric_matching(money: dict[str, Any], name: str, **labels: str) -> Decimal:
    total = Decimal(0)
    for row in money.get("metric_series", []):
        row_labels = row.get("labels")
        if (
            row.get("name") == name
            and isinstance(row_labels, dict)
            and all(row_labels.get(key) == value for key, value in labels.items())
        ):
            try:
                total += Decimal(row["value"])
            except (InvalidOperation, KeyError):
                _fail("money_path_invalid")
    return total


def _route_id(route_key: str) -> str:
    return "route-" + hashlib.sha256(route_key.encode("utf-8")).hexdigest()[:12]


def _provider_id(role: str, provider_key: str) -> str:
    digest = hashlib.sha256(f"{role}|{provider_key}".encode("utf-8")).hexdigest()[:12]
    return f"provider-{role}-{digest}"


def _configured_route_ids(rendered: Any) -> set[str]:
    try:
        environment = rendered["services"]["phoenix-engine"]["environment"]
    except (KeyError, TypeError):
        _fail("rendered_compose_invalid")
    if not isinstance(environment, dict):
        _fail("rendered_compose_invalid")
    raw = environment.get("ENGINE_ROUTE_REGISTRY_JSON")
    if not isinstance(raw, str) or len(raw.encode("utf-8")) > MAX_INPUT_BYTES:
        _fail("rendered_compose_invalid")
    try:
        routes = json.loads(raw, object_pairs_hook=_unique_object, parse_constant=_reject_non_finite)
    except json.JSONDecodeError as exc:
        raise LiveDashboardError("rendered_compose_invalid") from exc
    if not isinstance(routes, list) or not routes or len(routes) > 64:
        _fail("rendered_compose_invalid")
    result: set[str] = set()
    for route in routes:
        if not isinstance(route, dict):
            _fail("rendered_compose_invalid")
        route_id = _text(route.get("route_id"), 128)
        if route_id in result:
            _fail("rendered_compose_invalid")
        result.add(route_id)
    return result


def _load_history(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise LiveDashboardError("history_unavailable") from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail("history_invalid")
    rows: list[dict[str, Any]] = []
    for line in raw.decode("utf-8").splitlines():
        try:
            row = json.loads(line, object_pairs_hook=_unique_object, parse_constant=_reject_non_finite)
        except json.JSONDecodeError as exc:
            raise LiveDashboardError("history_invalid") from exc
        item = _object(
            row,
            {
                "schema_version",
                "observed_at",
                "database_size_bytes",
                "recorder_messages_persisted",
                "jetstream_pending",
            },
            "history_invalid",
        )
        if item["schema_version"] != HISTORY_SCHEMA:
            _fail("history_invalid")
        _timestamp(item["observed_at"])
        for key in (
            "database_size_bytes",
            "recorder_messages_persisted",
            "jetstream_pending",
        ):
            _number(item[key])
        if rows and _timestamp(item["observed_at"]) <= _timestamp(rows[-1]["observed_at"]):
            _fail("history_invalid")
        rows.append(item)
    if not rows or len(rows) > MAX_HISTORY_ROWS:
        _fail("history_invalid")
    return rows


def _append_history(path: Path, row: dict[str, Any]) -> list[dict[str, Any]]:
    rows = _load_history(path)
    if rows and _timestamp(row["observed_at"]) <= _timestamp(rows[-1]["observed_at"]):
        _fail("history_invalid")
    rows.append(row)
    rows = rows[-MAX_HISTORY_ROWS:]
    raw = b"".join(canonical_bytes(item) for item in rows)
    while len(raw) > MAX_INPUT_BYTES and len(rows) > 1:
        rows.pop(0)
        raw = b"".join(canonical_bytes(item) for item in rows)
    write_atomic(path, raw)
    return rows


def _history_growth(
    rows: list[dict[str, Any]],
    now: datetime,
    hours: int,
    current: Decimal,
    *,
    maximum_skew_seconds: int = 600,
) -> str | None:
    target = now - timedelta(hours=hours)
    eligible = [row for row in rows if _timestamp(row["observed_at"]) <= target]
    if not eligible:
        return None
    baseline = eligible[-1]
    if (target - _timestamp(baseline["observed_at"])).total_seconds() > maximum_skew_seconds:
        return None
    return str(current - _number(baseline["database_size_bytes"]))


def _preflight_rows(path: Path) -> list[dict[str, str]]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as exc:
        raise LiveDashboardError("preflight_unavailable") from exc
    if len(lines) != len(PREFLIGHT_CHECKS):
        _fail("preflight_invalid")
    rows = []
    for index, line in enumerate(lines):
        fields = line.split("\t")
        if len(fields) != 3 or fields[0] != PREFLIGHT_CHECKS[index] or fields[1] != "pass":
            _fail("preflight_invalid")
        _timestamp(fields[2])
        rows.append({"check": fields[0], "status": fields[1], "observed_at": fields[2]})
    return rows


def _identity_fingerprint(path: Path) -> str:
    value = load_control_json(path)
    fingerprint = value.get("fingerprint_sha256") if isinstance(value, dict) else None
    if not isinstance(fingerprint, str) or DIGEST_RE.fullmatch(fingerprint) is None:
        _fail("protected_identity_invalid")
    return fingerprint


def _file_digest(path: Path) -> str:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise LiveDashboardError("release_evidence_unavailable") from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail("release_evidence_unavailable")
    return _digest(raw)


def _unavailable_artifact(kind: str) -> dict[str, Any]:
    return {
        "kind": kind,
        "label": ARTIFACT_LABELS[kind],
        "available": False,
        "path": None,
        "sha256": None,
        "size_bytes": None,
        "generated_at": None,
        "content_type": None,
    }


def _write_artifact(directory: Path, kind: str, filename: str, payload: Any, generated_at: str) -> dict[str, Any]:
    if SAFE_FILE_RE.fullmatch(filename) is None:
        _fail("artifact_path_invalid")
    raw = canonical_bytes(payload)
    if not raw or len(raw) > MAX_ARTIFACT_BYTES:
        _fail("artifact_bounds_invalid")
    try:
        validate_artifact_payload(raw, "application/json")
    except SnapshotError as exc:
        raise LiveDashboardError(exc.code) from exc
    write_atomic(directory / filename, raw, 0o644)
    return {
        "kind": kind,
        "label": ARTIFACT_LABELS[kind],
        "available": True,
        "path": filename,
        "sha256": _digest(raw),
        "size_bytes": len(raw),
        "generated_at": generated_at,
        "content_type": "application/json",
    }


def _dropoffs(previous: Decimal, current: Decimal, reasons: list[tuple[str, Decimal]]) -> list[dict[str, str]]:
    difference = previous - current
    if difference < 0:
        _fail("snapshot_funnel_invalid")
    used = sum((count for _, count in reasons), Decimal(0))
    if used > difference:
        _fail("snapshot_funnel_invalid")
    result = [{"reason": reason, "count": str(count)} for reason, count in reasons if count > 0]
    if used < difference:
        result.append({"reason": "other_bounded_dropoff", "count": str(difference - used)})
    return result


def _model_and_policy(source: dict[str, Any]) -> tuple[str, str]:
    models = source["model_comparison"]
    model = models[-1]["model_version"] if models else "not_available"
    policy = "prelive-v2"
    return model, policy


def build_snapshot(args: argparse.Namespace) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    source = validate_source(load_json(Path(args.source)))
    money = load_json(Path(args.money_path))
    jetstream_raw = load_json(Path(args.jetstream))
    sample_from_money_path(money, jetstream_raw)
    services_source = _validate_services(load_json(Path(args.services)))
    metadata = load_json(Path(args.release_metadata))
    rendered = load_json(Path(args.rendered_compose))
    if (
        not isinstance(metadata, dict)
        or metadata.get("schema") != "phoenix.production-render.v1"
        or metadata.get("status") != "ok"
        or metadata.get("mode") != "SHADOW"
        or metadata.get("live_execution") is not False
        or SHA_RE.fullmatch(str(metadata.get("release_sha", ""))) is None
        or DIGEST_RE.fullmatch(str(metadata.get("route_registry_hash", ""))) is None
    ):
        _fail("release_evidence_invalid")
    generated = _timestamp(source["generated_at"])
    if abs((generated - _timestamp(money["generated_at"])).total_seconds()) > 120:
        _fail("source_time_mismatch")
    window = int(source["window_hours"])
    if int(money["window_hours"]) != window:
        _fail("source_window_invalid")
    configured_routes = _configured_route_ids(rendered)
    release_sha = metadata["release_sha"]

    stream_rows: list[dict[str, Any]] = []
    stream_messages = 0
    for name in STREAM_NAMES:
        item = _resource(jetstream_raw, name, consumer=False)
        config = item.get("config") if isinstance(item.get("config"), dict) else {}
        messages = _resource_count(item, "messages")
        byte_count = _resource_count(item, "bytes")
        stream_messages += messages
        max_bytes = config.get("max_bytes")
        if isinstance(max_bytes, int) and max_bytes > 0:
            storage_bps = str(min(10_000, byte_count * 10_000 // max_bytes))
        else:
            storage_bps = None
        storage = config.get("storage")
        stream_rows.append(
            {
                "stream_id": name,
                "exists": True,
                "messages": str(messages),
                "bytes": str(byte_count),
                "configured_storage": storage if storage in {"file", "memory"} else "unknown",
                "storage_used_bps": storage_bps,
            }
        )
    consumer_rows: list[dict[str, Any]] = []
    total_pending = 0
    for name in CONSUMER_NAMES:
        item = _resource(jetstream_raw, name, consumer=True)
        pending = _resource_count(item, "num_pending")
        ack_pending = _resource_count(item, "num_ack_pending")
        redeliveries = _resource_count(item, "num_redelivered")
        total_pending += pending + ack_pending
        consumer_rows.append(
            {
                "consumer_id": name,
                "exists": True,
                "ack_pending": str(ack_pending),
                "pending": str(pending),
                "redeliveries": str(redeliveries),
                "oldest_pending_age_seconds": None,
            }
        )

    recorder_persisted_raw = _metric_optional(money, "recorder_messages_persisted_total")
    if recorder_persisted_raw is None:
        _fail("recorder_persistence_metric_unavailable")
    recorder_persisted = _number(recorder_persisted_raw)

    history_row = {
        "schema_version": HISTORY_SCHEMA,
        "observed_at": source["generated_at"],
        "database_size_bytes": source["database"]["size_bytes"],
        "recorder_messages_persisted": str(recorder_persisted),
        "jetstream_pending": str(total_pending),
    }
    previous_history = _load_history(Path(args.history))
    history = _append_history(Path(args.history), history_row)
    throughput: str | None = None
    backlog_growth: str | None = None
    if previous_history:
        previous = previous_history[-1]
        elapsed = (generated - _timestamp(previous["observed_at"])).total_seconds()
        message_delta = recorder_persisted - _number(
            previous["recorder_messages_persisted"]
        )
        if message_delta < 0:
            _fail("recorder_persistence_counter_regressed")
        if elapsed > 0:
            throughput = _canonical_decimal(message_delta / Decimal(str(elapsed)))
            backlog_growth = str(
                Decimal(total_pending) - _number(previous["jetstream_pending"])
            )

    normalized_services: list[dict[str, Any]] = []
    service_observed = _timestamp(services_source["observed_at"])
    freshness = max(Decimal(0), Decimal(str((generated - service_observed).total_seconds())))
    images = metadata.get("images")
    if not isinstance(images, dict):
        _fail("release_evidence_invalid")
    manifest_matches = True
    for row in services_source["services"]:
        expected_ref = images.get(row["service"])
        expected_digest = expected_ref.rsplit("@", 1)[1] if isinstance(expected_ref, str) and "@" in expected_ref else None
        if expected_digest != row["image_digest"]:
            manifest_matches = False
        normalized_services.append(
            {
                "service": row["service"],
                "state": row["state"],
                "expected_state": "running",
                "image_digest": row["image_digest"],
                "git_sha": release_sha if row["service"] in PROJECT_SERVICES else "not_available",
                "started_at": row["started_at"],
                "exit_code": row["exit_code"],
                "oom": row["oom"],
                "restart_count": row["restart_count"],
                "readiness_freshness_seconds": _canonical_decimal(freshness),
            }
        )
    normalized_services.append(
        {
            "service": "fork-sandbox",
            "state": "stopped_clean",
            "expected_state": "on_demand",
            "image_digest": "not_available",
            "git_sha": release_sha,
            "started_at": None,
            "exit_code": 0,
            "oom": False,
            "restart_count": 0,
            "readiness_freshness_seconds": None,
        }
    )

    route_rows = source["routes"]
    routes: list[dict[str, Any]] = []
    route_pnl: list[dict[str, str]] = []
    nearest: list[dict[str, str]] = []
    active_count = 0
    for rank, raw in enumerate(route_rows, start=1):
        opaque = _route_id(raw["route_key"])
        active = raw["route_key"] in configured_routes and active_count < 3
        if active:
            active_count += 1
        sample_count = _number(raw["sample_count"])
        verified = _number(raw["independently_verified_count"])
        forks = _number(raw["fork_simulations"])
        fork_success = _number(raw["fork_success"])
        provider_requests = _number(raw["provider_requests"])
        provider_failures = _number(raw["provider_failures"])
        liquidity = _optional_number(raw["liquidity_score_bps"])
        age = max(Decimal(0), Decimal(str((generated - _timestamp(raw["last_observed_at"])).total_seconds())))
        freshness_score = max(Decimal(0), Decimal(10_000) - age * Decimal(10_000) / Decimal(window * 3600))
        confidence_score = Decimal(0) if sample_count == 0 else min(Decimal(10_000), verified * Decimal(10_000) / sample_count)
        reliability_denominator = provider_requests + forks
        reliability_numerator = provider_requests - provider_failures + fork_success
        reliability_score = (
            Decimal(0)
            if reliability_denominator == 0
            else max(Decimal(0), min(Decimal(10_000), reliability_numerator * Decimal(10_000) / reliability_denominator))
        )
        component_values = {
            "liquidity": liquidity or Decimal(0),
            "freshness": freshness_score,
            "confidence": confidence_score,
            "reliability": reliability_score,
        }
        ranking = sum(component_values.values(), Decimal(0)) / Decimal(4)
        warnings = []
        if liquidity is None:
            warnings.append("liquidity_evidence_unavailable")
        if verified == 0:
            warnings.append("verification_evidence_unavailable")
        if forks == 0:
            warnings.append("fork_evidence_unavailable")
        provider_failure_bps = (
            Decimal(0)
            if provider_requests == 0
            else min(Decimal(10_000), provider_failures * Decimal(10_000) / provider_requests)
        )
        fork_success_bps = Decimal(0) if forks == 0 else fork_success * Decimal(10_000) / forks
        routes.append(
            {
                "route_id": opaque,
                "rank": rank,
                "active_shadow": active,
                "candidate_count": raw["sample_count"],
                "ranking_score_bps": str(int(ranking)),
                "score_components": {key: str(int(value)) for key, value in component_values.items()},
                "data_quality_warnings": warnings[:4],
                "expected_net_pnl": raw["expected"],
                "conservative_net_pnl": raw["conservative"],
                "provider_failure_contribution_bps": str(int(provider_failure_bps)),
                "fork_success_rate_bps": str(int(fork_success_bps)),
            }
        )
        route_pnl.append(
            {
                "route_id": opaque,
                "expected": raw["expected"],
                "conservative": raw["conservative"],
                "severe": raw["severe"],
                "fork_simulated": raw["fork_simulated_net_pnl"],
                "sample_count": raw["sample_count"],
            }
        )
        if raw["minimum_shortfall"] is not None:
            nearest.append(
                {"route_id": opaque, "shortfall": raw["minimum_shortfall"], "reason": "minimum_margin"}
            )

    totals = {
        key: sum((_number(route[key], signed=key in {"gross_profit", "expected", "conservative", "severe", "fork_simulated_net_pnl", "fork_balance_delta"}) for route in route_rows), Decimal(0))
        for key in (
            "sample_count",
            "primary_profitable_count",
            "independently_verified_count",
            "verification_disagreed_count",
            "verification_unavailable_count",
            "gross_profit",
            "total_cost",
            "gas_cost",
            "flash_premium",
            "ordering_cost",
            "safety_cost",
            "expected",
            "conservative",
            "severe",
            "fork_unsigned_plans",
            "fork_simulations",
            "fork_success",
            "fork_reverted",
            "fork_profitable",
            "fork_gas_used",
            "fork_balance_delta",
            "fork_simulated_net_pnl",
            "fork_absolute_prediction_error",
            "fork_guard_failures",
        )
    }
    fork_block = max((_number(route["fork_block"]) for route in route_rows), default=Decimal(0))

    feed_inputs = _metric_rows(money, "feed_messages_total")
    supported = _metric_rows(money, "phoenix_supported_exact_input_inputs_total")
    route_matches = _metric_rows(money, "phoenix_configured_route_matches_total")
    candidates = totals["sample_count"]
    primary = totals["primary_profitable_count"]
    verified = totals["independently_verified_count"]
    simulated = totals["fork_simulations"]
    fork_profitable = totals["fork_profitable"]
    funnel_counts = [feed_inputs, supported, route_matches, candidates, primary, verified, simulated, fork_profitable]
    if any(right > left for left, right in zip(funnel_counts, funnel_counts[1:])):
        _fail("snapshot_funnel_invalid")
    funnel = []
    for index, (stage, count) in enumerate(
        zip(
            ("feed_inputs", "supported_swaps", "route_matches", "candidates", "primary_profitable", "independently_verified", "fork_simulated", "fork_profitable"),
            funnel_counts,
        )
    ):
        if index == 0:
            reasons: list[dict[str, str]] = []
        elif stage == "fork_profitable":
            reasons = _dropoffs(
                funnel_counts[index - 1],
                count,
                [
                    ("fork_reverted", totals["fork_reverted"]),
                    ("fork_not_profitable", totals["fork_success"] - totals["fork_profitable"]),
                ],
            )
        else:
            reason = {
                "supported_swaps": "unsupported_or_malformed",
                "route_matches": "no_configured_route",
                "candidates": "incomplete_candidate_evidence",
                "primary_profitable": "net_pnl_non_positive",
                "independently_verified": "independent_verification_not_agreed",
                "fork_simulated": "fork_evidence_unavailable",
            }[stage]
            reasons = _dropoffs(funnel_counts[index - 1], count, [(reason, funnel_counts[index - 1] - count)])
        funnel.append({"stage": stage, "count": str(count), "dropoff_reasons": reasons})

    distribution = list(source["distribution"])
    represented_scenarios = {row["scenario"] for row in distribution}
    for scenario in ("expected", "conservative", "severe", "fork_simulated"):
        if scenario not in represented_scenarios:
            distribution.append({"scenario": scenario, "bucket": "unobserved", "count": "0"})
    profitability = {
        "summary": {
            "gross_profit": str(totals["gross_profit"]),
            "total_cost": str(totals["total_cost"]),
            "net_pnl": str(totals["expected"]),
            "expected_net_pnl": str(totals["expected"]),
            "conservative_net_pnl": str(totals["conservative"]),
            "severe_net_pnl": str(totals["severe"]),
            "fork_simulated_net_pnl": str(totals["fork_simulated_net_pnl"]),
        },
        "cost_breakdown": [
            {"component": "gas", "amount": str(totals["gas_cost"])},
            {"component": "flash_premium", "amount": str(totals["flash_premium"])},
            {"component": "ordering", "amount": str(totals["ordering_cost"])},
            {"component": "safety_margin", "amount": str(totals["safety_cost"])},
        ],
        "route_pnl": route_pnl,
        "nearest_opportunities": nearest[:10],
        "distribution": distribution,
        "prediction_error": source["prediction_error"],
        "daily_trend": source["daily_trend"],
        "weekly_trend": source["weekly_trend"],
        "model_comparison": source["model_comparison"],
    }

    providers = []
    provider_rows = list(source["providers"])
    for role in ("primary", "secondary"):
        if not any(row["role"] == role for row in provider_rows):
            provider_rows.append(
                {
                    "provider_key": f"unobserved-{role}",
                    "role": role,
                    "requests": "0",
                    "success": "0",
                    "timeouts": "0",
                    "unavailable": "0",
                    "p50_latency_ms": None,
                    "p95_latency_ms": None,
                    "p99_latency_ms": None,
                }
            )
    budget_capacity = Decimal(args.rpc_calls_per_second) * Decimal(window * 3600)
    for row in provider_rows[:MAX_PROVIDERS]:
        requests = _number(row["requests"])
        success = _number(row["success"])
        success_bps = None if requests == 0 else str(int(min(Decimal(10_000), success * Decimal(10_000) / requests)))
        utilization = None if budget_capacity <= 0 else str(int(min(Decimal(10_000), requests * Decimal(10_000) / budget_capacity)))
        role = row["role"]
        rate_limits = _metric_matching(
            money,
            "rpc_upstream_calls_total",
            outcome="rate_limited",
            provider_slot=role,
        )
        providers.append(
            {
                "provider_id": _provider_id(role, row["provider_key"]),
                "role": role,
                "success_rate_bps": success_bps,
                "timeouts": row["timeouts"],
                "rate_limits": str(rate_limits),
                "unavailable": row["unavailable"],
                "p50_latency_ms": row["p50_latency_ms"],
                "p95_latency_ms": row["p95_latency_ms"],
                "p99_latency_ms": row["p99_latency_ms"],
                "budget_utilization_bps": utilization,
                "self_verification_prevented": _number(source["route_registry"]["self_verification_collisions"]) == 0,
            }
        )

    feed_gaps = _metric_rows(money, "feed_sequence_gaps_total")
    completeness = _metric_rows(money, "feed_data_completeness")
    gap_timestamp_seconds = _metric_rows(money, "feed_last_gap_timestamp_seconds")
    most_recent_gap_at = None
    affected_windows: list[str] = []
    if feed_gaps > 0 and gap_timestamp_seconds > 0:
        if gap_timestamp_seconds != gap_timestamp_seconds.to_integral_value():
            _fail("money_path_invalid")
        try:
            most_recent_gap_at = _canonical_timestamp(
                datetime.fromtimestamp(int(gap_timestamp_seconds), tz=timezone.utc)
            )
        except (OverflowError, OSError, ValueError) as exc:
            raise LiveDashboardError("money_path_invalid") from exc
        affected_windows.append(f"sequence_gap_at_{most_recent_gap_at}")
    unsupported: list[dict[str, Any]] = []
    ignored: list[dict[str, Any]] = []
    for row in money["metric_series"]:
        if row.get("name") != "feed_message_kind_total":
            continue
        labels = row.get("labels", {})
        try:
            kind = int(labels.get("kind", ""))
        except ValueError:
            _fail("money_path_invalid")
        target = unsupported if labels.get("classification") == "unsupported" else ignored
        target.append({"kind": kind, "count": row["value"]})
    feed = {
        "gap_count": str(feed_gaps),
        "missing_sequences": str(_metric_rows(money, "feed_missing_sequences_total")),
        "most_recent_gap_at": most_recent_gap_at,
        "unsupported_kinds": sorted(unsupported, key=lambda row: row["kind"]),
        "ignored_kinds": sorted(ignored, key=lambda row: row["kind"]),
        "reconnects": str(_metric_rows(money, "feed_reconnects_total")),
        "completeness_status": "complete" if completeness == 1 and feed_gaps == 0 else "incomplete",
        "affected_windows": affected_windows,
    }

    database = source["database"]
    migration_checksum = database["migration_checksum"]
    if not migration_checksum.startswith("sha256:"):
        migration_checksum = "sha256:" + migration_checksum
    postgres = {
        "readiness": True,
        "database_size_bytes": database["size_bytes"],
        "growth_bytes_1h": _history_growth(history, generated, 1, _number(database["size_bytes"])),
        "growth_bytes_6h": _history_growth(history, generated, 6, _number(database["size_bytes"])),
        "growth_bytes_24h": _history_growth(history, generated, 24, _number(database["size_bytes"])),
        "projected_disk_headroom_bytes": _integer_text(args.disk_headroom_bytes),
        "active_connections": database["active_connections"],
        "checkpoints_timed": database["checkpoints_timed"],
        "checkpoints_requested": database["checkpoints_requested"],
        "wal_bytes": database["wal_bytes"],
        "oldest_relevant_event": database["oldest_relevant_event"],
        "newest_relevant_event": database["newest_relevant_event"],
        "migration_version": database["migration_version"],
        "migration_checksum": migration_checksum,
        "retention_status": database["retention_status"],
    }

    identity_stable = _identity_fingerprint(Path(args.identity_before)) == _identity_fingerprint(
        Path(args.identity_current)
    )
    technical = money["technical"]
    runtime_exits = []
    for row in money["metric_series"]:
        if row.get("name") == "phoenix_engine_runtime_exits_total":
            runtime_exits.append({"class": row["labels"]["class"], "count": row["value"]})
    reliability = {
        "retry_attempts": str(
            _metric_rows(money, "phoenix_engine_retries_total")
            + _metric_rows(money, "recorder_database_retries_total")
            + _metric_rows(money, "shadow_dispatcher_retries_total")
        ),
        "recovered_retries": str(
            _metric_rows(money, "phoenix_engine_recovered_retries_total")
            + _metric_rows(money, "recorder_database_retry_recoveries_total")
            + _metric_rows(money, "shadow_dispatcher_retry_recoveries_total")
        ),
        "exhausted_or_quarantined": technical["engine_database"]["dependency_exhausted_total"],
        "terminal_integrity_failures": str(
            _number(technical["engine_database"]["terminal_integrity_total"])
            + _metric_rows(money, "shadow_dispatcher_terminal_integrity_failures_total")
        ),
        "runtime_exits": runtime_exits,
        "restart_loops": str(sum(1 for row in services_source["services"] if row["restart_count"] >= 3)),
        "later_message_progress_after_quarantine": str(
            _metric_rows(money, "phoenix_engine_later_message_progress_after_quarantine_total")
        ),
        "protected_service_identity_status": "stable" if identity_stable else "changed",
    }
    fork = {
        "unsigned_plan_count": str(totals["fork_unsigned_plans"]),
        "simulations": str(totals["fork_simulations"]),
        "success": str(totals["fork_success"]),
        "reverted": str(totals["fork_reverted"]),
        "gas_used": str(totals["fork_gas_used"]),
        "balance_delta": str(totals["fork_balance_delta"]),
        "simulated_net_pnl": str(totals["fork_simulated_net_pnl"]),
        "absolute_prediction_error": str(totals["fork_absolute_prediction_error"]),
        "fork_block": str(fork_block),
        "contract_guard_failures": str(totals["fork_guard_failures"]),
    }

    model_version, policy_version = _model_and_policy(source)
    registry = source["route_registry"]
    route_registry_matches = _number(registry["fact_count"]) > 0 and _number(registry["mismatch_count"]) == 0
    safety = {
        "mode": "SHADOW",
        "live_execution": False,
        "prelive_lock": True,
        "execution_eligible": False,
        "execution_request_created": False,
        "signer_configured": False,
        "wallet_configured": False,
        "executor_configured": False,
        "submission_method_invocations": "0",
    }

    preflight_payload = {
        "schema_version": "phoenix.prelive.preflight.v1",
        "generated_at": source["generated_at"],
        "checks": _preflight_rows(Path(args.preflight)),
    }
    release_payload = {
        "schema_version": "phoenix.prelive.release-checksum.v1",
        "generated_at": source["generated_at"],
        "git_sha": release_sha,
        "release_manifest_sha256": _file_digest(Path(args.release_manifest)),
        "release_checksum_sha256": _file_digest(Path(args.release_checksum)),
        "route_registry_hash": metadata["route_registry_hash"],
    }
    technical_payload = {
        "schema_version": "phoenix.prelive.technical.v1",
        "generated_at": source["generated_at"],
        "evidence_window_started_at": source["evidence_window_started_at"],
        "window_hours": window,
        "realization": "not_applicable_in_shadow",
        "safety": safety,
        "services": normalized_services,
        "feed": feed,
        "rpc": {
            "providers": providers,
            "secondary_requested": str(verified + totals["verification_disagreed_count"] + totals["verification_unavailable_count"]),
            "agreed": str(verified),
            "disagreed": str(totals["verification_disagreed_count"]),
            "state_freshness_seconds": str(_metric_rows(money, "rpc_state_freshness_seconds")),
            "pinned_block_status": "pinned" if candidates > 0 else "unknown",
        },
        "jetstream": {
            "streams": stream_rows,
            "consumers": consumer_rows,
            "persistence": {
                "throughput_per_second": throughput,
                "batch_size": _metric_optional(money, "recorder_batch_messages"),
                "backlog_growth": backlog_growth,
                "database_write_latency_ms": {"p50": None, "p95": None, "p99": None},
            },
        },
        "postgres": postgres,
        "reliability": reliability,
        "fork": fork,
    }
    business_payload = {
        "schema_version": "phoenix.prelive.business.v1",
        "generated_at": source["generated_at"],
        "evidence_window_started_at": source["evidence_window_started_at"],
        "window_hours": window,
        "projection": "counterfactual_not_realized",
        "sample_count": str(candidates),
        "funnel": funnel,
        "profitability": profitability,
        "routes": routes,
    }
    route_payload = {
        "schema_version": "phoenix.prelive.route-ranking.v1",
        "generated_at": source["generated_at"],
        "routes": routes,
        "production_registry_mutated": False,
    }
    profitability_payload = {
        "schema_version": "phoenix.prelive.profitability.v1",
        "generated_at": source["generated_at"],
        "realization": "counterfactual_not_realized",
        "profitability": profitability,
    }
    fork_payload = {
        "schema_version": "phoenix.prelive.fork-summary.v1",
        "generated_at": source["generated_at"],
        "fork_only": True,
        "shadow_only": True,
        "fork": fork,
    }

    generation_seed = canonical_bytes(
        {
            "generated_at": source["generated_at"],
            "release_sha": release_sha,
            "database_size": database["size_bytes"],
            "stream_messages": stream_messages,
            "recorder_messages_persisted": str(recorder_persisted),
        }
    )
    generation = hashlib.sha256(generation_seed).hexdigest()[:12]
    output_dir = Path(args.output_dir)
    artifacts_by_kind: dict[str, dict[str, Any]] = {
        "technical_json": _write_artifact(output_dir, "technical_json", f"technical-{generation}.json", technical_payload, source["generated_at"]),
        "business_json": _write_artifact(output_dir, "business_json", f"business-{generation}.json", business_payload, source["generated_at"]),
        "preflight_report": _write_artifact(output_dir, "preflight_report", f"preflight-{generation}.json", preflight_payload, source["generated_at"]),
        "route_ranking": _write_artifact(output_dir, "route_ranking", f"route-ranking-{generation}.json", route_payload, source["generated_at"]),
        "profitability_report": _write_artifact(output_dir, "profitability_report", f"profitability-{generation}.json", profitability_payload, source["generated_at"]),
        "fork_simulation_report": _write_artifact(output_dir, "fork_simulation_report", f"fork-simulation-{generation}.json", fork_payload, source["generated_at"]),
        "release_manifest_checksum": _write_artifact(output_dir, "release_manifest_checksum", f"release-checksum-{generation}.json", release_payload, source["generated_at"]),
    }
    artifacts = [artifacts_by_kind.get(kind, _unavailable_artifact(kind)) for kind in ARTIFACT_KINDS]
    snapshot = {
        "schema_version": "phoenix.prelive.dashboard.v1",
        "generated_at": source["generated_at"],
        "window_hours": window,
        "safety": safety,
        "governance": {
            "model_version": model_version,
            "policy_version": policy_version,
            "route_configuration_hash": metadata["route_registry_hash"],
            "route_registry_matches": route_registry_matches,
            "image_manifest_matches": manifest_matches,
            "freshness_threshold_seconds": "300",
            "fork_prediction_error_limit": "1000",
            "database_headroom_min_bytes": "1073741824",
            "unsupported_message_alert_threshold": "5",
        },
        "business": {
            "sample_count": str(candidates),
            "active_shadow_routes": str(active_count),
            "nearest_to_profitable_count": str(len(nearest[:10])),
            "independently_verified_count": str(verified),
            "fork_successful_count": str(totals["fork_success"]),
        },
        "funnel": funnel,
        "profitability": profitability,
        "routes": routes,
        "services": normalized_services,
        "feed": feed,
        "rpc": technical_payload["rpc"],
        "jetstream": technical_payload["jetstream"],
        "postgres": postgres,
        "reliability": reliability,
        "fork": fork,
        "artifacts": artifacts,
        "logs": [],
    }
    try:
        validate_snapshot(snapshot)
    except SnapshotError as exc:
        raise LiveDashboardError(exc.code) from exc
    if len(canonical_snapshot_bytes(snapshot)) > MAX_SNAPSHOT_BYTES:
        _fail("snapshot_size_invalid")
    control_artifacts = [
        {
            "kind": artifact["kind"],
            "path": artifact["path"],
            "sha256": artifact["sha256"],
            "size_bytes": artifact["size_bytes"],
        }
        for artifact in artifacts
        if artifact["available"]
    ]
    return snapshot, control_artifacts


def attach_evidence(source: Path, snapshot_path: Path, candidate: Path) -> None:
    try:
        snapshot_parent = snapshot_path.parent.resolve(strict=True)
        if candidate.parent.resolve(strict=True) != snapshot_parent:
            _fail("snapshot_output_path_invalid")
    except OSError as exc:
        raise LiveDashboardError("snapshot_output_path_invalid") from exc
    evidence = source.read_bytes()
    if not evidence or len(evidence) > MAX_ARTIFACT_BYTES:
        _fail("artifact_bounds_invalid")
    try:
        validate_artifact_payload(evidence, "application/json")
    except SnapshotError as exc:
        raise LiveDashboardError(exc.code) from exc
    snapshot = load_json(snapshot_path)
    validate_snapshot(snapshot)
    generation = hashlib.sha256(evidence).hexdigest()[:12]
    filename = f"evidence-bundle-{generation}.json"
    write_atomic(snapshot_path.parent / filename, evidence, 0o644)
    replacement = {
        "kind": "evidence_bundle",
        "label": ARTIFACT_LABELS["evidence_bundle"],
        "available": True,
        "path": filename,
        "sha256": _digest(evidence),
        "size_bytes": len(evidence),
        "generated_at": snapshot["generated_at"],
        "content_type": "application/json",
    }
    snapshot["artifacts"] = [
        replacement if row["kind"] == "evidence_bundle" else row for row in snapshot["artifacts"]
    ]
    validate_snapshot(snapshot)
    write_atomic(candidate, canonical_snapshot_bytes(snapshot))


def prune_artifacts(directory: Path, snapshot_path: Path, retain: int) -> None:
    if retain < 2 or retain > 10:
        _fail("retention_invalid")
    resolved = directory.resolve(strict=True)
    if snapshot_path.parent.resolve(strict=True) != resolved:
        _fail("retention_path_invalid")
    snapshot = load_json(snapshot_path)
    validate_snapshot(snapshot)
    current = {row["path"] for row in snapshot["artifacts"] if row["available"]}
    candidates = sorted(
        (
            path
            for path in resolved.iterdir()
            if path.is_file() and GENERATION_FILE_RE.fullmatch(path.name)
        ),
        key=lambda path: (path.stat().st_mtime_ns, path.name),
        reverse=True,
    )
    keep = set(current)
    for path in candidates:
        if len(keep) >= len(current) + retain * len(ARTIFACT_KINDS):
            break
        keep.add(path.name)
    for path in candidates:
        if path.name not in keep:
            path.unlink()


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    normalize = commands.add_parser("normalize-services")
    normalize.add_argument("--input", required=True)
    normalize.add_argument("--observed-at", required=True)
    normalize.add_argument("--output", required=True)

    validate = commands.add_parser("validate-source")
    validate.add_argument("--input", required=True)

    build = commands.add_parser("build")
    for name in (
        "money-path",
        "source",
        "jetstream",
        "services",
        "release-metadata",
        "rendered-compose",
        "identity-before",
        "identity-current",
        "preflight",
        "release-manifest",
        "release-checksum",
        "history",
        "output-dir",
        "candidate",
        "artifact-manifest",
    ):
        build.add_argument(f"--{name}", required=True)
    build.add_argument("--disk-headroom-bytes", required=True)
    build.add_argument("--rpc-calls-per-second", required=True)

    attach = commands.add_parser("attach-evidence")
    attach.add_argument("--evidence", required=True)
    attach.add_argument("--snapshot", required=True)
    attach.add_argument("--candidate", required=True)

    prune = commands.add_parser("prune")
    prune.add_argument("--directory", required=True)
    prune.add_argument("--snapshot", required=True)
    prune.add_argument("--retain", type=int, default=3)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "normalize-services":
            write_atomic(
                Path(args.output),
                canonical_bytes(normalize_services(Path(args.input), args.observed_at)),
            )
        elif args.command == "validate-source":
            validate_source(load_json(Path(args.input)))
        elif args.command == "build":
            snapshot, artifacts = build_snapshot(args)
            candidate = Path(args.candidate)
            if candidate.parent.resolve() != Path(args.output_dir).resolve():
                _fail("snapshot_output_path_invalid")
            write_atomic(candidate, canonical_snapshot_bytes(snapshot))
            write_atomic(Path(args.artifact_manifest), canonical_bytes(artifacts))
        elif args.command == "attach-evidence":
            attach_evidence(Path(args.evidence), Path(args.snapshot), Path(args.candidate))
        elif args.command == "prune":
            prune_artifacts(Path(args.directory), Path(args.snapshot), args.retain)
        else:  # pragma: no cover - argparse enforces subcommands
            _fail("command_invalid")
    except (LiveDashboardError, SnapshotError, OSError) as exc:
        if isinstance(exc, (LiveDashboardError, SnapshotError)):
            code = exc.code
        else:
            code = "io_failure"
        print(f"PRELIVE_DASHBOARD_LIVE_ERROR: {code}", file=sys.stderr)
        return 2
    print(f"PRELIVE_DASHBOARD_LIVE_OK: action={args.command}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
