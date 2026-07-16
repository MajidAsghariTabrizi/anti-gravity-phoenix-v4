#!/usr/bin/env python3
"""Validate deterministic PRE-LIVE SHADOW control-plane evidence."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import sys
from typing import Any


PLAN_SCHEMA = "phoenix.prelive.shadow-plan.v1"
EVIDENCE_SCHEMA = "phoenix.prelive.shadow-control-evidence.v1"
PROTECTED_IDENTITY_SCHEMA = "phoenix.prelive.protected-identity.v1"
SAMPLE_SCHEMA = "phoenix.prelive.shadow-sample.v1"
MAX_INPUT_BYTES = 2 * 1024 * 1024
MAX_ERRORS = 100
MAX_ARTIFACTS = 16
MAX_SAMPLES = 3_000
MAX_ATTEMPT_LOG_INPUT_BYTES = 1024 * 1024
MAX_ATTEMPT_LOG_BYTES = 64 * 1024
MAX_ATTEMPT_LOG_LINE_BYTES = 2 * 1024

MODE_DURATIONS = {
    "15m": 15 * 60,
    "1h": 60 * 60,
    "6h": 6 * 60 * 60,
    "24h": 24 * 60 * 60,
    "continuous": None,
}
PROTECTED_SERVICES = (
    "nitro-feed-relay",
    "feed-ingestor",
    "nats",
    "postgres",
    "recorder",
)
OPTIONAL_SERVICES = (
    "prometheus",
    "rpc-gateway",
    "shadow-dispatcher",
    "phoenix-engine",
    "dashboard",
)
FULL_SERVICES = PROTECTED_SERVICES + OPTIONAL_SERVICES
START_ORDER = OPTIONAL_SERVICES
STOP_ORDER = tuple(reversed(OPTIONAL_SERVICES))
JETSTREAM_STREAM_NAMES = ("PHOENIX_FEED_TX", "PHOENIX_ENGINE_INPUT")
JETSTREAM_CONSUMER_NAMES = ("PHOENIX_RECORDER", "PHOENIX_ENGINE_SHADOW")
ATTEMPT_LOG_TERMINAL_REASONS = {
    "child_exit",
    "evidence_found",
    "evidence_not_found",
    "terminal_marker_missing",
}

PREFLIGHT_CHECKS = (
    "canonical_render",
    "immutable_images",
    "release_manifest",
    "route_registry",
    "chain_id",
    "shadow_mode",
    "live_execution_disabled",
    "execution_configuration_blank",
    "broadcast_disabled",
    "rpc_state_budget",
    "rpc_upstream_budget",
    "postgres_connectivity",
    "nats_connectivity",
    "jetstream_resources",
    "migrations",
    "dashboard_read_only",
    "prometheus_config",
    "execution_requests_zero",
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
EVIDENCE_STATUSES = {"preflight_passed", "completed", "interrupted", "failed"}
ARTIFACT_KINDS = {
    "business_json",
    "dashboard_snapshot",
    "evidence_bundle",
    "preflight_report",
    "profitability_report",
    "release_manifest_checksum",
    "route_ranking",
    "samples_ndjson",
    "technical_json",
}

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
RUN_ID_RE = re.compile(r"^shadow-(?:15m|1h|6h|24h|continuous)-[0-9a-f]{12}$")
SAFE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")
SAFE_FILE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
CANONICAL_COUNT_RE = re.compile(r"^(?:0|[1-9][0-9]*)$")
SIGNED_COUNT_RE = re.compile(r"^(?:0|-?[1-9][0-9]*)$")
URL_RE = re.compile(r"(?i)(?:https?|wss?|postgres(?:ql)?|nats)://")
URL_VALUE_RE = re.compile(r"(?i)(?:https?|wss?|postgres(?:ql)?|nats)://[^\s\"']+")
ADDRESS_RE = re.compile(r"(?i)0x[0-9a-f]{40}")
SECRET_RE = re.compile(
    r"(?i)(?:password|passwd|private[_ -]?key|mnemonic|authorization|bearer|"
    r"credential|secret)\s*[:=]|\b(?:RPC_PROVIDER_URLS|POSTGRES_DSN|"
    r"SIGNER_PRIVATE_KEY|WALLET_ADDRESS|EXECUTOR_ADDRESS)\b"
)
SECRET_LINE_RE = re.compile(
    r"(?i)(?:password|passwd|private[_ -]?key|mnemonic|authorization|bearer|"
    r"credential|secret)[\"']?\s*[:=]|\b(?:RPC_PROVIDER_URLS|POSTGRES_DSN|"
    r"SIGNER_PRIVATE_KEY|WALLET_ADDRESS|EXECUTOR_ADDRESS)\b"
)
ENV_ASSIGNMENT_LINE_RE = re.compile(r"(?:^|\s)[A-Z][A-Z0-9_]{2,63}\s*[:=]")
SENSITIVE_ENV_NAME_RE = re.compile(
    r"(?i)(?:URL|URI|DSN|PASSWORD|PASSWD|TOKEN|SECRET|PRIVATE|KEY|MNEMONIC|"
    r"WALLET|EXECUTOR|CREDENTIAL|AUTH)"
)
CONTAINER_ID_RE = re.compile(r"^[0-9a-f]{64}$")


class ControlEvidenceError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def _fail(code: str) -> None:
    raise ControlEvidenceError(code)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, nested in pairs:
        if key in value:
            _fail("duplicate_json_key")
        value[key] = nested
    return value


def _reject_non_finite(_value: str) -> None:
    _fail("non_finite_number")


def load_json(path: Path) -> Any:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise ControlEvidenceError("input_unavailable") from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail("input_size_invalid")
    try:
        return json.loads(
            raw,
            object_pairs_hook=_unique_object,
            parse_constant=_reject_non_finite,
        )
    except UnicodeDecodeError as exc:
        raise ControlEvidenceError("input_encoding_invalid") from exc
    except json.JSONDecodeError as exc:
        raise ControlEvidenceError("input_json_invalid") from exc


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
        + "\n"
    ).encode("ascii")


def _sha256(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def _object(value: Any, keys: set[str], code: str = "evidence_shape_invalid") -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        _fail(code)
    return value


def _array(value: Any, maximum: int, exact: int | None = None) -> list[Any]:
    if not isinstance(value, list) or len(value) > maximum:
        _fail("evidence_shape_invalid")
    if exact is not None and len(value) != exact:
        _fail("evidence_shape_invalid")
    return value


def _text(
    value: Any,
    *,
    maximum: int = 512,
    pattern: re.Pattern[str] | None = None,
    choices: set[str] | None = None,
) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        _fail("evidence_shape_invalid")
    if pattern is not None and pattern.fullmatch(value) is None:
        _fail("evidence_shape_invalid")
    if choices is not None and value not in choices:
        _fail("evidence_shape_invalid")
    if URL_RE.search(value) or ADDRESS_RE.search(value) or SECRET_RE.search(value):
        _fail("sensitive_evidence")
    return value


def _boolean(value: Any) -> bool:
    if not isinstance(value, bool):
        _fail("evidence_shape_invalid")
    return value


def _integer(value: Any, minimum: int = 0, maximum: int = 2**63 - 1) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        _fail("evidence_shape_invalid")
    if value < minimum or value > maximum:
        _fail("evidence_shape_invalid")
    return value


def _count(value: Any) -> int:
    if not isinstance(value, str) or len(value) > 79 or CANONICAL_COUNT_RE.fullmatch(value) is None:
        _fail("evidence_shape_invalid")
    return int(value)


def _signed_count(value: Any) -> int:
    if not isinstance(value, str) or len(value) > 79 or SIGNED_COUNT_RE.fullmatch(value) is None:
        _fail("evidence_shape_invalid")
    return int(value)


def _timestamp(value: Any) -> datetime:
    if not isinstance(value, str) or len(value) > 32 or not value.endswith("Z"):
        _fail("evidence_shape_invalid")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise ControlEvidenceError("evidence_shape_invalid") from exc
    if parsed.tzinfo is None:
        _fail("evidence_shape_invalid")
    return parsed.astimezone(timezone.utc)


def _optional_timestamp(value: Any) -> datetime | None:
    if value is None:
        return None
    return _timestamp(value)


def mode_plan(mode: str) -> dict[str, Any]:
    if mode not in MODE_DURATIONS:
        _fail("mode_invalid")
    return {
        "schema_version": PLAN_SCHEMA,
        "mode": mode,
        "duration_seconds": MODE_DURATIONS[mode],
        "continuous": mode == "continuous",
        "full_services": list(FULL_SERVICES),
        "protected_services": list(PROTECTED_SERVICES),
        "optional_services": list(OPTIONAL_SERVICES),
        "start_order": list(START_ORDER),
        "stop_order": list(STOP_ORDER),
        "safety": {
            "mode": "SHADOW",
            "live_execution": False,
            "execution_eligible": False,
            "execution_request_created": False,
            "submission_methods_allowed": False,
        },
    }


def validate_plan(value: Any) -> dict[str, Any]:
    plan = _object(
        value,
        {
            "schema_version",
            "mode",
            "duration_seconds",
            "continuous",
            "full_services",
            "protected_services",
            "optional_services",
            "start_order",
            "stop_order",
            "safety",
        },
        "plan_shape_invalid",
    )
    if plan["schema_version"] != PLAN_SCHEMA or plan != mode_plan(plan["mode"]):
        _fail("plan_invalid")
    return plan


def _read_bounded_lines(path: Path, maximum: int, code: str) -> list[str]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise ControlEvidenceError(code) from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail(code)
    try:
        lines = raw.decode("utf-8").splitlines()
    except UnicodeDecodeError as exc:
        raise ControlEvidenceError(code) from exc
    if len(lines) != maximum:
        _fail(code)
    return lines


def normalize_service_states(path: Path, observed_at: str) -> list[dict[str, Any]]:
    _timestamp(observed_at)
    lines = _read_bounded_lines(path, len(FULL_SERVICES), "service_state_invalid")
    result: list[dict[str, Any]] = []
    for index, line in enumerate(lines):
        fields = line.split("\t")
        if len(fields) != 6:
            _fail("service_state_invalid")
        service, status, health, image_digest, restart_raw, exit_raw = fields
        if service != FULL_SERVICES[index] or status not in {
            "created",
            "dead",
            "exited",
            "missing",
            "paused",
            "restarting",
            "running",
        }:
            _fail("service_state_invalid")
        if health not in {"healthy", "missing", "none", "starting", "unhealthy"}:
            _fail("service_state_invalid")
        _text(image_digest, pattern=DIGEST_RE)
        if not restart_raw.isascii() or not restart_raw.isdigit():
            _fail("service_state_invalid")
        restart_count = _integer(int(restart_raw), 0, 1_000_000)
        if not exit_raw.isascii() or not exit_raw.isdigit():
            _fail("service_state_invalid")
        exit_code = _integer(int(exit_raw), 0, 255)

        if status == "running":
            state = {
                "healthy": "running_healthy",
                "unhealthy": "running_unhealthy",
                "starting": "running_unhealthy",
                "none": "running_no_healthcheck",
                "missing": "running_no_healthcheck",
            }[health]
        elif status == "exited":
            state = "stopped_clean" if exit_code == 0 else "stopped_failed"
        elif status in {"dead", "restarting"}:
            state = "stopped_failed"
        elif status == "created":
            state = "created_not_started"
        elif status == "missing":
            state = "missing"
        else:
            state = "unknown"
        result.append(
            {
                "service": service,
                "state": state,
                "image_digest": image_digest,
                "restart_count": restart_count,
                "observed_at": observed_at,
            }
        )
    return result


def _walk_objects(value: Any):
    if isinstance(value, dict):
        yield value
        for nested in value.values():
            yield from _walk_objects(nested)
    elif isinstance(value, list):
        for nested in value:
            yield from _walk_objects(nested)


def _jetstream_config(item: dict[str, Any]) -> dict[str, Any]:
    value = item.get("config")
    if value is None:
        return {}
    if not isinstance(value, dict):
        _fail("jetstream_identity_invalid")
    return value


def _nats_resource_identity(root: Any) -> dict[str, Any]:
    if not isinstance(root, dict):
        _fail("jetstream_identity_invalid")
    objects = list(_walk_objects(root))
    streams: list[dict[str, str]] = []
    for name in JETSTREAM_STREAM_NAMES:
        matches = []
        for item in objects:
            config = _jetstream_config(item)
            if item.get("name") == name or config.get("name") == name:
                matches.append(item)
        if not matches:
            _fail("jetstream_identity_invalid")
        selected = matches[0]
        config = _jetstream_config(selected)
        stable = {
            "name": name,
            "subjects": config.get("subjects", []),
            "storage": config.get("storage"),
            "retention": config.get("retention"),
            "max_age": config.get("max_age"),
            "max_bytes": config.get("max_bytes"),
            "max_msgs": config.get("max_msgs"),
        }
        streams.append({"name": name, "config_sha256": _sha256(canonical_bytes(stable))})

    consumers: list[dict[str, str]] = []
    for name in JETSTREAM_CONSUMER_NAMES:
        matches = []
        for item in objects:
            config = _jetstream_config(item)
            if (
                item.get("name") == name
                or config.get("durable_name") == name
                or config.get("name") == name
            ):
                matches.append(item)
        if not matches:
            _fail("jetstream_identity_invalid")
        selected = matches[0]
        config = _jetstream_config(selected)
        stable = {
            "name": name,
            "ack_policy": config.get("ack_policy"),
            "deliver_policy": config.get("deliver_policy"),
            "filter_subject": config.get("filter_subject"),
            "max_ack_pending": config.get("max_ack_pending"),
        }
        consumers.append({"name": name, "config_sha256": _sha256(canonical_bytes(stable))})
    return {"streams": streams, "consumers": consumers}


def jetstream_runtime_metrics(root: Any) -> dict[str, str]:
    if not isinstance(root, dict):
        _fail("jetstream_metrics_invalid")
    objects = list(_walk_objects(root))
    stream_count = 0
    for name in JETSTREAM_STREAM_NAMES:
        match = None
        for item in objects:
            config = _jetstream_config(item)
            if item.get("name") == name or config.get("name") == name:
                match = item
                break
        if match is None:
            _fail("jetstream_metrics_invalid")
        stream_count += 1

    pending = 0
    ack_pending = 0
    redeliveries = 0
    for name in JETSTREAM_CONSUMER_NAMES:
        match = None
        for item in objects:
            config = _jetstream_config(item)
            if (
                item.get("name") == name
                or config.get("durable_name") == name
                or config.get("name") == name
            ):
                match = item
                break
        if match is None:
            _fail("jetstream_metrics_invalid")
        state = match.get("state") if isinstance(match.get("state"), dict) else {}
        values: list[int] = []
        for key in ("num_pending", "num_ack_pending", "num_redelivered"):
            if key in state:
                value = state[key]
            elif key in match:
                value = match[key]
            else:
                _fail("jetstream_metrics_invalid")
            if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                _fail("jetstream_metrics_invalid")
            values.append(value)
        pending += values[0]
        ack_pending += values[1]
        redeliveries += values[2]
    return {
        "streams": str(stream_count),
        "consumers": str(len(JETSTREAM_CONSUMER_NAMES)),
        "pending": str(pending),
        "ack_pending": str(ack_pending),
        "redeliveries": str(redeliveries),
    }


def protected_identity(service_path: Path, jetstream_path: Path) -> dict[str, Any]:
    lines = _read_bounded_lines(
        service_path, len(PROTECTED_SERVICES), "protected_identity_invalid"
    )
    services: list[dict[str, str]] = []
    for index, line in enumerate(lines):
        fields = line.split("|", 6)
        if len(fields) != 7:
            _fail("protected_identity_invalid")
        service, container_id, image_digest, created, started, restart_raw, mounts_raw = fields
        if service != PROTECTED_SERVICES[index]:
            _fail("protected_identity_invalid")
        if CONTAINER_ID_RE.fullmatch(container_id) is None:
            _fail("protected_identity_invalid")
        _text(image_digest, pattern=DIGEST_RE)
        _timestamp(created)
        _timestamp(started)
        if not restart_raw.isascii() or not restart_raw.isdigit():
            _fail("protected_identity_invalid")
        _integer(int(restart_raw), 0, 1_000_000)
        try:
            mounts = json.loads(mounts_raw, object_pairs_hook=_unique_object)
        except json.JSONDecodeError as exc:
            raise ControlEvidenceError("protected_identity_invalid") from exc
        if not isinstance(mounts, list) or len(mounts) > 16:
            _fail("protected_identity_invalid")
        normalized_mounts: list[dict[str, str]] = []
        for mount in mounts:
            if not isinstance(mount, dict):
                _fail("protected_identity_invalid")
            mount_type = mount.get("Type")
            destination = mount.get("Destination")
            name = mount.get("Name") or "bind"
            if mount_type not in {"bind", "volume"}:
                _fail("protected_identity_invalid")
            if not isinstance(destination, str) or not destination.startswith("/"):
                _fail("protected_identity_invalid")
            if not isinstance(name, str) or len(name) > 128:
                _fail("protected_identity_invalid")
            normalized_mounts.append(
                {"type": mount_type, "destination": destination, "name": name}
            )
        identity = {
            "container_id": container_id,
            "image_digest": image_digest,
            "created": created,
            "started": started,
            "restart_count": restart_raw,
            "mounts": sorted(normalized_mounts, key=lambda item: item["destination"]),
        }
        services.append(
            {"service": service, "identity_sha256": _sha256(canonical_bytes(identity))}
        )

    jetstream = _nats_resource_identity(load_json(jetstream_path))
    identity = {"services": services, "jetstream": jetstream}
    return {
        "schema_version": PROTECTED_IDENTITY_SCHEMA,
        "services": services,
        "jetstream_sha256": _sha256(canonical_bytes(jetstream)),
        "fingerprint_sha256": _sha256(canonical_bytes(identity)),
    }


def _validate_safety(value: Any, *, include_run_counts: bool = False) -> None:
    keys = {
        "mode",
        "live_execution",
        "execution_eligible",
        "execution_request_created",
        "signer_configured",
        "wallet_configured",
        "executor_configured",
        "submission_method_invocations",
    }
    if include_run_counts:
        keys.update({"execution_request_count_before", "execution_request_count_after"})
    safety = _object(
        value,
        keys,
    )
    if safety["mode"] != "SHADOW":
        _fail("safety_invariant_failed")
    for key in (
        "live_execution",
        "execution_eligible",
        "execution_request_created",
        "signer_configured",
        "wallet_configured",
        "executor_configured",
    ):
        if _boolean(safety[key]):
            _fail("safety_invariant_failed")
    if _count(safety["submission_method_invocations"]) != 0:
        _fail("safety_invariant_failed")
    if include_run_counts and any(
        _count(safety[key]) != 0
        for key in ("execution_request_count_before", "execution_request_count_after")
    ):
        _fail("safety_invariant_failed")


def _validate_release(value: Any) -> None:
    release = _object(
        value,
        {
            "git_sha",
            "release_manifest_sha256",
            "release_checksum_sha256",
            "route_registry_hash",
            "images",
        },
    )
    _text(release["git_sha"], pattern=SHA_RE)
    for key in (
        "release_manifest_sha256",
        "release_checksum_sha256",
        "route_registry_hash",
    ):
        _text(release[key], pattern=DIGEST_RE)
    images = _array(release["images"], len(FULL_SERVICES), len(FULL_SERVICES))
    seen: set[str] = set()
    for row in images:
        image = _object(row, {"service", "digest"})
        service = _text(image["service"], choices=set(FULL_SERVICES))
        if service in seen:
            _fail("duplicate_identity")
        seen.add(service)
        _text(image["digest"], pattern=DIGEST_RE)
    if tuple(row["service"] for row in images) != FULL_SERVICES:
        _fail("evidence_shape_invalid")


def _validate_preflight(value: Any) -> bool:
    rows = _array(value, len(PREFLIGHT_CHECKS), len(PREFLIGHT_CHECKS))
    all_pass = True
    for index, raw in enumerate(rows):
        row = _object(raw, {"check", "status", "observed_at"})
        if row["check"] != PREFLIGHT_CHECKS[index]:
            _fail("evidence_shape_invalid")
        if _text(row["status"], choices={"pass", "fail"}) != "pass":
            all_pass = False
        _timestamp(row["observed_at"])
    return all_pass


def _validate_service_states(value: Any) -> dict[str, dict[str, str]]:
    phases = _object(value, {"before", "during", "after"})
    result: dict[str, dict[str, str]] = {}
    for phase, rows in phases.items():
        states = _array(rows, len(FULL_SERVICES), len(FULL_SERVICES))
        for index, raw in enumerate(states):
            state = _object(
                raw,
                {
                    "service",
                    "state",
                    "image_digest",
                    "restart_count",
                    "observed_at",
                },
            )
            service = _text(state["service"], choices=set(FULL_SERVICES))
            if service != FULL_SERVICES[index]:
                _fail("evidence_shape_invalid")
            _text(state["state"], choices=SERVICE_STATES)
            _text(state["image_digest"], pattern=DIGEST_RE)
            _integer(state["restart_count"], 0, 1_000_000)
            _timestamp(state["observed_at"])

        result[phase] = {row["service"]: row["state"] for row in states}
    return result


def _validate_funnels(value: Any) -> None:
    funnels = _object(value, {"candidate", "profitability", "verification", "fork"})
    keys = {
        "candidate": ("feed_inputs", "supported_swaps", "route_matches", "candidates"),
        "profitability": ("candidates", "complete", "primary_profitable", "accepted"),
        "verification": ("requested", "agreed", "disagreed", "unavailable"),
        "fork": ("planned", "simulated", "passed", "profitable", "reverted"),
    }
    for name, expected in keys.items():
        row = _object(funnels[name], set(expected))
        counts = [_count(row[key]) for key in expected]
        if name in {"candidate", "profitability"} and any(
            right > left for left, right in zip(counts, counts[1:])
        ):
            _fail("evidence_accounting_invalid")
    verification = funnels["verification"]
    if sum(_count(verification[key]) for key in ("agreed", "disagreed", "unavailable")) > _count(
        verification["requested"]
    ):
        _fail("evidence_accounting_invalid")
    fork = funnels["fork"]
    if _count(fork["passed"]) + _count(fork["reverted"]) != _count(fork["simulated"]):
        _fail("evidence_accounting_invalid")
    if _count(fork["profitable"]) > _count(fork["passed"]):
        _fail("evidence_accounting_invalid")


def _validate_metrics(value: Any) -> None:
    metrics = _object(value, {"rpc", "jetstream", "database", "feed"})
    metric_keys = {
        "rpc": (
            "requests",
            "success",
            "timeouts",
            "rate_limited",
            "unavailable",
            "disagreements",
        ),
        "jetstream": (
            "streams",
            "consumers",
            "pending",
            "ack_pending",
            "redeliveries",
        ),
        "database": ("size_start_bytes", "size_end_bytes", "growth_bytes"),
        "feed": ("messages", "gaps", "missing_sequences", "decode_failures"),
    }
    for name, expected in metric_keys.items():
        row = _object(metrics[name], set(expected))
        for key in expected:
            if name == "database" and key == "growth_bytes":
                _signed_count(row[key])
            else:
                _count(row[key])
    database = metrics["database"]
    start = _count(database["size_start_bytes"])
    end = _count(database["size_end_bytes"])
    growth = _signed_count(database["growth_bytes"])
    if end - start != growth:
        _fail("evidence_accounting_invalid")


def _validate_artifacts(value: Any) -> None:
    rows = _array(value, MAX_ARTIFACTS)
    seen: set[str] = set()
    for raw in rows:
        row = _object(raw, {"kind", "path", "sha256", "size_bytes"})
        kind = _text(row["kind"], choices=ARTIFACT_KINDS)
        if kind in seen:
            _fail("duplicate_identity")
        seen.add(kind)
        _text(row["path"], pattern=SAFE_FILE_RE)
        _text(row["sha256"], pattern=DIGEST_RE)
        _integer(row["size_bytes"], 1, MAX_INPUT_BYTES)


def _validate_errors(value: Any) -> None:
    for raw in _array(value, MAX_ERRORS):
        row = _object(raw, {"observed_at", "service", "class", "message"})
        _timestamp(row["observed_at"])
        _text(row["service"], choices=set(FULL_SERVICES) | {"control-plane"})
        _text(row["class"], maximum=64, pattern=SAFE_ID_RE)
        _text(row["message"], maximum=256)


def validate_evidence(value: Any) -> dict[str, Any]:
    evidence = _object(
        value,
        {
            "schema_version",
            "run_id",
            "mode",
            "planned_duration_seconds",
            "started_at",
            "ended_at",
            "status",
            "database_clock",
            "release",
            "safety",
            "preflight",
            "protected_identity",
            "service_states",
            "samples",
            "funnels",
            "metrics",
            "bounded_errors",
            "artifacts",
        },
    )
    if evidence["schema_version"] != EVIDENCE_SCHEMA:
        _fail("evidence_schema_invalid")
    mode = _text(evidence["mode"], choices=set(MODE_DURATIONS))
    if evidence["planned_duration_seconds"] != MODE_DURATIONS[mode]:
        _fail("duration_contract_invalid")
    if _text(evidence["run_id"], pattern=RUN_ID_RE) != evidence["run_id"] or not evidence[
        "run_id"
    ].startswith(f"shadow-{mode}-"):
        _fail("evidence_shape_invalid")
    started = _timestamp(evidence["started_at"])
    ended = _optional_timestamp(evidence["ended_at"])
    status = _text(evidence["status"], choices=EVIDENCE_STATUSES)
    if ended is not None and ended < started:
        _fail("duration_contract_invalid")

    database_clock = _object(
        evidence["database_clock"],
        {"preflight_baseline", "first_sample", "last_sample"},
    )
    clock_baseline = _timestamp(database_clock["preflight_baseline"])
    clock_first = _optional_timestamp(database_clock["first_sample"])
    clock_last = _optional_timestamp(database_clock["last_sample"])
    if (clock_first is None) != (clock_last is None) or (
        clock_first is not None
        and (clock_first < clock_baseline or clock_last is None or clock_last < clock_first)
    ):
        _fail("database_clock_invalid")

    _validate_release(evidence["release"])
    _validate_safety(evidence["safety"], include_run_counts=True)
    preflight_passed = _validate_preflight(evidence["preflight"])
    protected = _object(
        evidence["protected_identity"], {"before_sha256", "after_sha256", "stable"}
    )
    _text(protected["before_sha256"], pattern=DIGEST_RE)
    _text(protected["after_sha256"], pattern=DIGEST_RE)
    stable = _boolean(protected["stable"])
    if stable != (protected["before_sha256"] == protected["after_sha256"]):
        _fail("protected_identity_invalid")

    service_states = _validate_service_states(evidence["service_states"])
    samples = _object(
        evidence["samples"], {"count", "first_observed_at", "last_observed_at"}
    )
    sample_count = _integer(samples["count"], 0, 10_000_000)
    first_sample = _optional_timestamp(samples["first_observed_at"])
    last_sample = _optional_timestamp(samples["last_observed_at"])
    if sample_count == 0:
        if first_sample is not None or last_sample is not None:
            _fail("evidence_accounting_invalid")
    elif first_sample is None or last_sample is None or last_sample < first_sample:
        _fail("evidence_accounting_invalid")
    if clock_first != first_sample or clock_last != last_sample:
        _fail("database_clock_invalid")

    _validate_funnels(evidence["funnels"])
    _validate_metrics(evidence["metrics"])
    _validate_errors(evidence["bounded_errors"])
    _validate_artifacts(evidence["artifacts"])

    if status in {"preflight_passed", "completed", "interrupted"} and not preflight_passed:
        _fail("preflight_incomplete")
    if status in {"completed", "interrupted"} and not stable:
        _fail("protected_identity_changed")
    if status in {"preflight_passed", "completed", "interrupted"} and any(
        service_states[phase][name] != "running_healthy"
        for phase in ("before", "after")
        for name in PROTECTED_SERVICES
    ):
        _fail("protected_service_unavailable")
    if status in {"completed", "interrupted"} and any(
        state != "running_healthy" for state in service_states["during"].values()
    ):
        _fail("runtime_not_healthy")
    if status == "completed":
        if mode == "continuous" or ended is None:
            _fail("duration_contract_invalid")
        if (ended - started).total_seconds() < MODE_DURATIONS[mode]:
            _fail("duration_contract_invalid")
        if sample_count == 0:
            _fail("evidence_accounting_invalid")
    if status == "interrupted":
        if mode != "continuous" or ended is None or sample_count == 0:
            _fail("duration_contract_invalid")
    if status == "preflight_passed" and (ended is None or sample_count != 0):
        _fail("evidence_accounting_invalid")
    return evidence


def _metric_series(report: dict[str, Any]) -> dict[str, str]:
    rows = report.get("metric_series")
    if not isinstance(rows, list) or len(rows) > 2_048:
        _fail("money_path_invalid")
    values: dict[str, str] = {}
    for raw in rows:
        if not isinstance(raw, dict) or set(raw) != {"name", "labels", "value"}:
            _fail("money_path_invalid")
        if raw["labels"] != {}:
            continue
        name = raw["name"]
        value = raw["value"]
        if not isinstance(name, str) or not isinstance(value, str) or name in values:
            _fail("money_path_invalid")
        values[name] = value
    return values


def _required_metric(values: dict[str, str], name: str) -> str:
    value = values.get(name)
    _count(value)
    return value


def _required_labeled_metric(report: dict[str, Any], name: str, **labels: str) -> str:
    matches = [
        row
        for row in report.get("metric_series", [])
        if isinstance(row, dict) and row.get("name") == name and row.get("labels") == labels
    ]
    if len(matches) != 1:
        _fail("money_path_invalid")
    value = matches[0].get("value")
    _count(value)
    return value


def _sample_errors(funnels: dict[str, Any], metrics: dict[str, Any]) -> list[dict[str, str]]:
    observed_at = metrics.pop("_observed_at")
    errors: list[dict[str, str]] = []
    if _count(metrics["feed"]["gaps"]) > 0:
        errors.append(
            {
                "observed_at": observed_at,
                "service": "feed-ingestor",
                "class": "feed_gap",
                "message": "Bounded feed gap evidence requires operator review",
            }
        )
    if _count(metrics["rpc"]["disagreements"]) > 0:
        errors.append(
            {
                "observed_at": observed_at,
                "service": "rpc-gateway",
                "class": "verification_disagreement",
                "message": "Independent verification disagreement requires operator review",
            }
        )
    if _count(funnels["fork"]["reverted"]) > 0:
        errors.append(
            {
                "observed_at": observed_at,
                "service": "control-plane",
                "class": "fork_revert",
                "message": "Bounded fork evidence includes a reverted simulation",
            }
        )
    return errors


def sample_from_money_path(report: Any, jetstream_root: Any) -> dict[str, Any]:
    if not isinstance(report, dict):
        _fail("money_path_invalid")
    required = {
        "schema_version",
        "generated_at",
        "window_hours",
        "mode",
        "live_execution",
        "execution_eligible",
        "execution_request_created",
        "metric_counter_scope",
        "metric_series",
        "technical",
        "business",
    }
    if set(report) != required or report["schema_version"] != "phoenix.prelive.money-path-summary.v1":
        _fail("money_path_invalid")
    observed_at = report["generated_at"]
    _timestamp(observed_at)
    if (
        report["mode"] != "SHADOW"
        or report["live_execution"] is not False
        or report["execution_eligible"] is not False
        or report["execution_request_created"] is not False
    ):
        _fail("safety_invariant_failed")
    technical = report["technical"]
    business = report["business"]
    if not isinstance(technical, dict) or not isinstance(business, dict):
        _fail("money_path_invalid")
    values = _metric_series(report)
    fork_source = technical.get("fork")
    database = technical.get("database")
    if not all(isinstance(value, dict) for value in (fork_source, database)):
        _fail("money_path_invalid")

    candidates = _required_metric(values, "phoenix_engine_candidates_total")
    primary_profitable = _required_labeled_metric(
        report, "phoenix_profitability_primary_total", status="profitable"
    )
    primary_not_profitable = _required_labeled_metric(
        report, "phoenix_profitability_primary_total", status="not_profitable"
    )
    _required_labeled_metric(
        report, "phoenix_profitability_primary_total", status="incomplete"
    )
    complete = str(_count(primary_profitable) + _count(primary_not_profitable))

    funnels = {
        "candidate": {
            "feed_inputs": _required_metric(values, "feed_messages_total"),
            "supported_swaps": _required_metric(
                values, "phoenix_supported_exact_input_inputs_total"
            ),
            "route_matches": _required_metric(values, "phoenix_configured_route_matches_total"),
            "candidates": candidates,
        },
        "profitability": {
            "candidates": candidates,
            "complete": complete,
            "primary_profitable": primary_profitable,
            "accepted": _required_metric(values, "phoenix_engine_shadow_accepted_total"),
        },
        "verification": {
            "requested": _required_metric(values, "rpc_secondary_requested_total"),
            "agreed": _required_metric(values, "rpc_secondary_agreed_total"),
            "disagreed": _required_metric(values, "rpc_secondary_disagreed_total"),
            "unavailable": _required_metric(values, "rpc_secondary_unavailable_total"),
        },
        "fork": {
            "planned": fork_source.get("simulations_total"),
            "simulated": fork_source.get("simulations_total"),
            "passed": fork_source.get("passed_total"),
            "profitable": fork_source.get("simulated_profitable_total"),
            "reverted": fork_source.get("reverted_total"),
        },
    }
    _validate_funnels(funnels)
    metrics = {
        "rpc": {
            "requests": _required_metric(values, "rpc_state_requests_total"),
            "success": _required_metric(values, "rpc_primary_success_total"),
            "timeouts": technical.get("rpc_database", {}).get("timeouts_total"),
            "rate_limited": _required_metric(values, "rpc_provider_rate_limited_total"),
            "unavailable": _required_metric(values, "rpc_provider_unavailable_total"),
            "disagreements": _required_metric(values, "rpc_provider_disagreement_total"),
        },
        "jetstream": jetstream_runtime_metrics(jetstream_root),
        "database": {"size_bytes": database.get("size_bytes")},
        "feed": {
            "messages": _required_metric(values, "feed_messages_total"),
            "gaps": _required_metric(values, "feed_sequence_gaps_total"),
            "missing_sequences": _required_metric(values, "feed_missing_sequences_total"),
            "decode_failures": _required_metric(values, "feed_decode_failures_total"),
        },
        "_observed_at": observed_at,
    }
    for group, row in metrics.items():
        if group.startswith("_"):
            continue
        if not isinstance(row, dict):
            _fail("money_path_invalid")
        for value in row.values():
            _count(value)
    errors = _sample_errors(funnels, metrics)
    return {
        "schema_version": SAMPLE_SCHEMA,
        "observed_at": observed_at,
        "safety": {
            "mode": "SHADOW",
            "live_execution": False,
            "execution_eligible": False,
            "execution_request_created": False,
            "signer_configured": False,
            "wallet_configured": False,
            "executor_configured": False,
            "submission_method_invocations": "0",
        },
        "funnels": funnels,
        "metrics": metrics,
        "bounded_errors": errors,
    }


def validate_sample(value: Any) -> dict[str, Any]:
    sample = _object(
        value,
        {"schema_version", "observed_at", "safety", "funnels", "metrics", "bounded_errors"},
    )
    if sample["schema_version"] != SAMPLE_SCHEMA:
        _fail("sample_invalid")
    _timestamp(sample["observed_at"])
    _validate_safety(sample["safety"])
    _validate_funnels(sample["funnels"])
    metrics = sample["metrics"]
    if not isinstance(metrics, dict) or set(metrics) != {"rpc", "jetstream", "database", "feed"}:
        _fail("sample_invalid")
    for name, row in metrics.items():
        if not isinstance(row, dict):
            _fail("sample_invalid")
        expected = {
            "rpc": {"requests", "success", "timeouts", "rate_limited", "unavailable", "disagreements"},
            "jetstream": {"streams", "consumers", "pending", "ack_pending", "redeliveries"},
            "database": {"size_bytes"},
            "feed": {"messages", "gaps", "missing_sequences", "decode_failures"},
        }[name]
        if set(row) != expected:
            _fail("sample_invalid")
        for nested in row.values():
            _count(nested)
    _validate_errors(sample["bounded_errors"])
    return sample


def load_samples(path: Path) -> list[dict[str, Any]]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise ControlEvidenceError("samples_unavailable") from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail("samples_invalid")
    try:
        lines = raw.decode("utf-8").splitlines()
    except UnicodeDecodeError as exc:
        raise ControlEvidenceError("samples_invalid") from exc
    if not lines or len(lines) > MAX_SAMPLES:
        _fail("samples_invalid")
    samples: list[dict[str, Any]] = []
    previous: datetime | None = None
    for line in lines:
        if not line or len(line.encode("utf-8")) > 64 * 1024:
            _fail("samples_invalid")
        try:
            sample = json.loads(line, object_pairs_hook=_unique_object, parse_constant=_reject_non_finite)
        except json.JSONDecodeError as exc:
            raise ControlEvidenceError("samples_invalid") from exc
        validate_sample(sample)
        observed = _timestamp(sample["observed_at"])
        if previous is not None and observed <= previous:
            _fail("samples_invalid")
        previous = observed
        samples.append(sample)
    return samples


def append_sample(samples_path: Path, sample: dict[str, Any]) -> None:
    validate_sample(sample)
    existing: list[dict[str, Any]] = []
    if samples_path.exists():
        existing = load_samples(samples_path)
        if _timestamp(sample["observed_at"]) <= _timestamp(existing[-1]["observed_at"]):
            _fail("samples_invalid")
    existing.append(sample)
    existing = existing[-MAX_SAMPLES:]
    payload = b"".join(canonical_bytes(value) for value in existing)
    if len(payload) > MAX_INPUT_BYTES:
        while len(payload) > MAX_INPUT_BYTES and len(existing) > 1:
            existing.pop(0)
            payload = b"".join(canonical_bytes(value) for value in existing)
    write_atomic(samples_path, payload)


def _read_preflight(path: Path) -> list[dict[str, str]]:
    lines = _read_bounded_lines(path, len(PREFLIGHT_CHECKS), "preflight_invalid")
    result: list[dict[str, str]] = []
    for index, line in enumerate(lines):
        fields = line.split("\t")
        if len(fields) != 3 or fields[0] != PREFLIGHT_CHECKS[index] or fields[1] not in {
            "pass",
            "fail",
        }:
            _fail("preflight_invalid")
        _timestamp(fields[2])
        result.append({"check": fields[0], "status": fields[1], "observed_at": fields[2]})
    return result


def _load_identity_fingerprint(path: Path) -> str:
    value = load_json(path)
    if not isinstance(value, dict) or set(value) != {
        "schema_version",
        "services",
        "jetstream_sha256",
        "fingerprint_sha256",
    }:
        _fail("protected_identity_invalid")
    if value["schema_version"] != PROTECTED_IDENTITY_SCHEMA:
        _fail("protected_identity_invalid")
    _text(value["jetstream_sha256"], pattern=DIGEST_RE)
    return _text(value["fingerprint_sha256"], pattern=DIGEST_RE)


def _file_digest(path: Path) -> str:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise ControlEvidenceError("release_evidence_unavailable") from exc
    if not raw or len(raw) > MAX_INPUT_BYTES:
        _fail("release_evidence_unavailable")
    return _sha256(raw)


def assemble_evidence(args: argparse.Namespace) -> dict[str, Any]:
    mode = args.mode
    if mode not in MODE_DURATIONS:
        _fail("mode_invalid")
    metadata = load_json(Path(args.release_metadata))
    if (
        not isinstance(metadata, dict)
        or metadata.get("schema") != "phoenix.production-render.v1"
        or metadata.get("status") != "ok"
        or metadata.get("mode") != "SHADOW"
        or metadata.get("live_execution") is not False
    ):
        _fail("release_evidence_invalid")
    release_sha = metadata.get("release_sha")
    _text(release_sha, pattern=SHA_RE)
    images = metadata.get("images")
    if not isinstance(images, dict):
        _fail("release_evidence_invalid")
    image_rows: list[dict[str, str]] = []
    for service in FULL_SERVICES:
        reference = images.get(service)
        if not isinstance(reference, str) or "@" not in reference:
            _fail("release_evidence_invalid")
        digest = reference.rsplit("@", 1)[1]
        _text(digest, pattern=DIGEST_RE)
        image_rows.append({"service": service, "digest": digest})

    started = _timestamp(args.started_at)
    ended = _timestamp(args.ended_at)
    samples = load_samples(Path(args.samples))
    first = samples[0]
    last = samples[-1]
    database_start = _count(first["metrics"]["database"]["size_bytes"])
    database_end = _count(last["metrics"]["database"]["size_bytes"])
    metrics = {
        "rpc": last["metrics"]["rpc"],
        "jetstream": last["metrics"]["jetstream"],
        "database": {
            "size_start_bytes": str(database_start),
            "size_end_bytes": str(database_end),
            "growth_bytes": str(database_end - database_start),
        },
        "feed": last["metrics"]["feed"],
    }
    errors = [error for sample in samples for error in sample["bounded_errors"]][-MAX_ERRORS:]
    artifacts = load_json(Path(args.artifacts))
    _validate_artifacts(artifacts)
    before_fingerprint = _load_identity_fingerprint(Path(args.identity_before))
    after_fingerprint = _load_identity_fingerprint(Path(args.identity_after))
    states = {
        "before": load_json(Path(args.states_before)),
        "during": load_json(Path(args.states_during)),
        "after": load_json(Path(args.states_after)),
    }
    run_seed = f"{release_sha}|{mode}|{args.started_at}".encode("ascii")
    evidence = {
        "schema_version": EVIDENCE_SCHEMA,
        "run_id": f"shadow-{mode}-{hashlib.sha256(run_seed).hexdigest()[:12]}",
        "mode": mode,
        "planned_duration_seconds": MODE_DURATIONS[mode],
        "started_at": args.started_at,
        "ended_at": args.ended_at,
        "status": args.status,
        "database_clock": {
            "preflight_baseline": args.database_clock_baseline,
            "first_sample": first["observed_at"],
            "last_sample": last["observed_at"],
        },
        "release": {
            "git_sha": release_sha,
            "release_manifest_sha256": _file_digest(Path(args.release_manifest)),
            "release_checksum_sha256": _file_digest(Path(args.release_checksum)),
            "route_registry_hash": metadata.get("route_registry_hash"),
            "images": image_rows,
        },
        "safety": {
            **last["safety"],
            "execution_request_count_before": args.execution_request_count_before,
            "execution_request_count_after": args.execution_request_count_after,
        },
        "preflight": _read_preflight(Path(args.preflight)),
        "protected_identity": {
            "before_sha256": before_fingerprint,
            "after_sha256": after_fingerprint,
            "stable": before_fingerprint == after_fingerprint,
        },
        "service_states": states,
        "samples": {
            "count": len(samples),
            "first_observed_at": first["observed_at"],
            "last_observed_at": last["observed_at"],
        },
        "funnels": last["funnels"],
        "metrics": metrics,
        "bounded_errors": errors,
        "artifacts": artifacts,
    }
    return validate_evidence(evidence)


def render_image_digests(metadata: Any) -> list[tuple[str, str, str]]:
    if not isinstance(metadata, dict) or metadata.get("schema") != "phoenix.production-render.v1":
        _fail("release_evidence_invalid")
    images = metadata.get("images")
    if not isinstance(images, dict):
        _fail("release_evidence_invalid")
    result: list[tuple[str, str, str]] = []
    for service in FULL_SERVICES:
        reference = images.get(service)
        if not isinstance(reference, str) or "@" not in reference:
            _fail("release_evidence_invalid")
        digest = reference.rsplit("@", 1)[1]
        _text(digest, pattern=DIGEST_RE)
        result.append((service, digest, reference))
    return result


def _redact_attempt_log(raw: bytes) -> str:
    text = raw.decode("utf-8", errors="replace")
    sensitive_values = sorted(
        {
            value
            for name, value in os.environ.items()
            if SENSITIVE_ENV_NAME_RE.search(name) and len(value) >= 4
        },
        key=len,
        reverse=True,
    )
    for value in sensitive_values:
        text = text.replace(value, "[redacted-env]")

    lines: list[str] = []
    for raw_line in text.splitlines():
        line = "".join(
            character
            if character == "\t" or ord(character) >= 32 and ord(character) != 127
            else "?"
            for character in raw_line
        )
        if SECRET_LINE_RE.search(line):
            line = "[redacted-sensitive-line]"
        elif ENV_ASSIGNMENT_LINE_RE.search(line):
            line = "[redacted-environment-line]"
        else:
            line = URL_VALUE_RE.sub("[redacted-url]", line)
            line = ADDRESS_RE.sub("[redacted-address]", line)
        encoded = line.encode("utf-8")
        if len(encoded) > MAX_ATTEMPT_LOG_LINE_BYTES:
            line = (
                encoded[: MAX_ATTEMPT_LOG_LINE_BYTES - 20]
                .decode("utf-8", errors="ignore")
                + "[line-truncated]"
            )
        lines.append(line)
    return "\n".join(lines) + ("\n" if lines else "")


def _bounded_attempt_log_body(body: bytes, maximum: int) -> tuple[bytes, bool]:
    if len(body) <= maximum:
        return body, False
    marker = b"\n[... log truncated ...]\n"
    available = maximum - len(marker)
    head_size = available * 2 // 3
    tail_size = available - head_size
    head = body[:head_size].decode("utf-8", errors="ignore").encode("utf-8")
    tail = body[-tail_size:].decode("utf-8", errors="ignore").encode("utf-8")
    return head + marker + tail, True


def retain_attempt_log(
    input_path: Path,
    output_path: Path,
    attempt_id: str,
    terminal_reason: str,
    source_exit_code: int,
) -> dict[str, Any]:
    _text(attempt_id, maximum=64, pattern=SAFE_ID_RE)
    _text(terminal_reason, maximum=32, choices=ATTEMPT_LOG_TERMINAL_REASONS)
    _integer(source_exit_code, 0, 255)
    if SAFE_FILE_RE.fullmatch(output_path.name) is None or output_path.suffix != ".log":
        _fail("output_path_invalid")
    try:
        with input_path.open("rb") as handle:
            raw = handle.read(MAX_ATTEMPT_LOG_INPUT_BYTES + 1)
    except OSError as exc:
        raise ControlEvidenceError("attempt_log_unavailable") from exc

    input_truncated = len(raw) > MAX_ATTEMPT_LOG_INPUT_BYTES
    redacted = _redact_attempt_log(raw[:MAX_ATTEMPT_LOG_INPUT_BYTES]).encode("utf-8")
    bounded, output_truncated = _bounded_attempt_log_body(
        redacted, MAX_ATTEMPT_LOG_BYTES - 1024
    )
    header = (
        f"attempt_id={attempt_id}\n"
        f"terminal_reason={terminal_reason}\n"
        f"source_exit_code={source_exit_code}\n"
        f"input_truncated={'true' if input_truncated else 'false'}\n"
        f"output_truncated={'true' if output_truncated else 'false'}\n"
        "--- begin redacted log ---\n"
    ).encode("ascii")
    payload = header + bounded + b"--- end redacted log ---\n"
    if len(payload) > MAX_ATTEMPT_LOG_BYTES:
        _fail("attempt_log_bounds_invalid")
    write_atomic(output_path, payload, mode=0o600)
    return {
        "attempt_id": attempt_id,
        "terminal_reason": terminal_reason,
        "source_exit_code": source_exit_code,
        "input_truncated": input_truncated,
        "output_truncated": output_truncated,
        "size_bytes": len(payload),
    }


def write_atomic(path: Path, payload: bytes, *, mode: int = 0o640) -> None:
    import tempfile

    if mode not in {0o600, 0o640}:
        _fail("output_mode_invalid")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb", prefix=f".{path.name}.", suffix=".tmp", dir=path.parent, delete=False
        ) as handle:
            temporary = Path(handle.name)
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def command_plan(args: argparse.Namespace) -> None:
    sys.stdout.buffer.write(canonical_bytes(mode_plan(args.mode)))


def command_validate_plan(args: argparse.Namespace) -> None:
    validate_plan(load_json(Path(args.input)))
    print("PRELIVE_SHADOW_PLAN_OK")


def command_validate_evidence(args: argparse.Namespace) -> None:
    validate_evidence(load_json(Path(args.input)))
    print("PRELIVE_SHADOW_EVIDENCE_OK")


def command_normalize_service_states(args: argparse.Namespace) -> None:
    payload = normalize_service_states(Path(args.input), args.observed_at)
    write_atomic(Path(args.output), canonical_bytes(payload))
    print("PRELIVE_SHADOW_SERVICE_STATES_OK")


def command_protected_identity(args: argparse.Namespace) -> None:
    payload = protected_identity(Path(args.services), Path(args.jetstream))
    write_atomic(Path(args.output), canonical_bytes(payload))
    print(f"PRELIVE_SHADOW_PROTECTED_IDENTITY_OK: {payload['fingerprint_sha256']}")


def command_create_sample(args: argparse.Namespace) -> None:
    payload = sample_from_money_path(
        load_json(Path(args.money_path)), load_json(Path(args.jetstream))
    )
    write_atomic(Path(args.output), canonical_bytes(payload))
    print("PRELIVE_SHADOW_SAMPLE_OK")


def command_append_sample(args: argparse.Namespace) -> None:
    sample = validate_sample(load_json(Path(args.sample)))
    append_sample(Path(args.samples), sample)
    print("PRELIVE_SHADOW_SAMPLE_OK: appended")


def command_assemble_evidence(args: argparse.Namespace) -> None:
    payload = assemble_evidence(args)
    write_atomic(Path(args.output), canonical_bytes(payload))
    print("PRELIVE_SHADOW_EVIDENCE_OK: assembled")


def command_render_image_digests(args: argparse.Namespace) -> None:
    for service, digest, reference in render_image_digests(load_json(Path(args.metadata))):
        print(f"{service}\t{digest}\t{reference}")


def command_retain_attempt_log(args: argparse.Namespace) -> None:
    payload = retain_attempt_log(
        Path(args.input),
        Path(args.output),
        args.attempt_id,
        args.terminal_reason,
        args.source_exit_code,
    )
    print(
        "PRELIVE_SHADOW_ATTEMPT_LOG_OK: "
        f"id={payload['attempt_id']} reason={payload['terminal_reason']}"
    )


def command_promote_evidence(args: argparse.Namespace) -> None:
    source = Path(args.input)
    output = Path(args.output)
    try:
        if source.resolve(strict=True).parent != output.parent.resolve(strict=True):
            _fail("output_path_invalid")
    except OSError as exc:
        raise ControlEvidenceError("output_path_invalid") from exc
    if SAFE_FILE_RE.fullmatch(output.name) is None:
        _fail("output_path_invalid")
    evidence = validate_evidence(load_json(source))
    write_atomic(output, canonical_bytes(evidence))
    digest = "sha256:" + hashlib.sha256(canonical_bytes(evidence)).hexdigest()
    print(f"PRELIVE_SHADOW_EVIDENCE_OK: promoted={output.name} digest={digest}")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description="PRE-LIVE SHADOW control-plane contracts")
    commands = root.add_subparsers(dest="command", required=True)

    plan = commands.add_parser("plan")
    plan.add_argument("--mode", choices=tuple(MODE_DURATIONS), required=True)
    plan.set_defaults(handler=command_plan)

    validate_plan_parser = commands.add_parser("validate-plan")
    validate_plan_parser.add_argument("--input", required=True)
    validate_plan_parser.set_defaults(handler=command_validate_plan)

    validate = commands.add_parser("validate-evidence")
    validate.add_argument("--input", required=True)
    validate.set_defaults(handler=command_validate_evidence)

    states = commands.add_parser("normalize-service-states")
    states.add_argument("--input", required=True)
    states.add_argument("--observed-at", required=True)
    states.add_argument("--output", required=True)
    states.set_defaults(handler=command_normalize_service_states)

    identity = commands.add_parser("protected-identity")
    identity.add_argument("--services", required=True)
    identity.add_argument("--jetstream", required=True)
    identity.add_argument("--output", required=True)
    identity.set_defaults(handler=command_protected_identity)

    sample = commands.add_parser("create-sample")
    sample.add_argument("--money-path", required=True)
    sample.add_argument("--jetstream", required=True)
    sample.add_argument("--output", required=True)
    sample.set_defaults(handler=command_create_sample)

    append = commands.add_parser("append-sample")
    append.add_argument("--sample", required=True)
    append.add_argument("--samples", required=True)
    append.set_defaults(handler=command_append_sample)

    assemble = commands.add_parser("assemble-evidence")
    assemble.add_argument("--mode", choices=tuple(MODE_DURATIONS), required=True)
    assemble.add_argument("--status", choices=tuple(EVIDENCE_STATUSES), required=True)
    assemble.add_argument("--started-at", required=True)
    assemble.add_argument("--ended-at", required=True)
    assemble.add_argument("--database-clock-baseline", required=True)
    assemble.add_argument("--execution-request-count-before", required=True)
    assemble.add_argument("--execution-request-count-after", required=True)
    assemble.add_argument("--release-metadata", required=True)
    assemble.add_argument("--release-manifest", required=True)
    assemble.add_argument("--release-checksum", required=True)
    assemble.add_argument("--preflight", required=True)
    assemble.add_argument("--identity-before", required=True)
    assemble.add_argument("--identity-after", required=True)
    assemble.add_argument("--states-before", required=True)
    assemble.add_argument("--states-during", required=True)
    assemble.add_argument("--states-after", required=True)
    assemble.add_argument("--samples", required=True)
    assemble.add_argument("--artifacts", required=True)
    assemble.add_argument("--output", required=True)
    assemble.set_defaults(handler=command_assemble_evidence)

    image_digests = commands.add_parser("render-image-digests")
    image_digests.add_argument("--metadata", required=True)
    image_digests.set_defaults(handler=command_render_image_digests)

    attempt_log = commands.add_parser("retain-attempt-log")
    attempt_log.add_argument("--input", required=True)
    attempt_log.add_argument("--output", required=True)
    attempt_log.add_argument("--attempt-id", required=True)
    attempt_log.add_argument(
        "--terminal-reason",
        choices=tuple(sorted(ATTEMPT_LOG_TERMINAL_REASONS)),
        required=True,
    )
    attempt_log.add_argument("--source-exit-code", type=int, required=True)
    attempt_log.set_defaults(handler=command_retain_attempt_log)

    promote = commands.add_parser("promote-evidence")
    promote.add_argument("--input", required=True)
    promote.add_argument("--output", required=True)
    promote.set_defaults(handler=command_promote_evidence)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        args.handler(args)
    except ControlEvidenceError as exc:
        print(f"PRELIVE_SHADOW_CONTROL_ERROR: {exc.code}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
