#!/usr/bin/env python3
"""Validate and compare fail-closed protected-maintenance evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
from datetime import datetime
from pathlib import Path, PurePosixPath
from typing import Any


PLAN_SCHEMA = "phoenix.protected-maintenance-plan.v1"
SNAPSHOT_SCHEMA = "phoenix.protected-maintenance-snapshot.v2"
CONTEXT_SCHEMA = "phoenix.protected-maintenance-context.v2"
RELEASE_SCHEMA = "phoenix.release.v1"
ASSET_SCHEMA = "phoenix.release-assets.v1"

OWNED_IMAGES = (
    "dashboard",
    "feed-ingestor",
    "fork-sandbox",
    "phoenix-engine",
    "recorder",
    "rpc-gateway",
)
PROTECTED_SERVICES = (
    "nitro-feed-relay",
    "feed-ingestor",
    "nats",
    "postgres",
    "recorder",
)
FIXED_SERVICES = ("nitro-feed-relay", "nats", "postgres")
MUTABLE_SERVICES = ("feed-ingestor", "recorder")
OPTIONAL_SERVICES = (
    "prometheus",
    "rpc-gateway",
    "shadow-dispatcher",
    "phoenix-engine",
    "dashboard",
)
MAINTENANCE_ORDER = ("recorder", "feed-ingestor")
STREAM_NAMES = ("PHOENIX_FEED_TX", "PHOENIX_ENGINE_INPUT")
CONSUMER_NAMES = ("PHOENIX_RECORDER", "PHOENIX_ENGINE_SHADOW")

FIXED_IMAGES = {
    "nitro-feed-relay": (
        "offchainlabs/nitro-node@"
        "sha256:ebc985e3b105980734630744981e1542001c22d74cba57509fe0d5ed8bb84c14"
    ),
    "nats": (
        "nats@"
        "sha256:b83efabe3e7def1e0a4a31ec6e078999bb17c80363f881df35edc70fcb6bb927"
    ),
    "postgres": (
        "postgres@"
        "sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777"
    ),
}

STATIC_CONTRACT_PATHS = (
    "compose.prod.yml",
    "deploy/nats-server.conf",
    "fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json",
    "scripts/production_context.py",
    "scripts/render-production-compose.sh",
    "scripts/validate-production-env.sh",
)
EXPECTED_MIGRATIONS = tuple(f"migrations/{index:03d}_{name}.sql" for index, name in (
    (1, "init"),
    (2, "event_signatures"),
    (3, "shadow_profitability_evidence"),
    (4, "shadow_engine_runtime"),
    (5, "shadow_decision_identity"),
    (6, "dependency_exhaustion_quarantine"),
    (7, "canonical_profitability_truth"),
    (8, "shadow_route_discovery_indexes"),
    (9, "profit_triggered_secondary_verification"),
    (10, "fork_simulation_evidence"),
))
CONTRACT_PATHS = STATIC_CONTRACT_PATHS + EXPECTED_MIGRATIONS

FEED_METRICS = (
    "feed_last_sequence",
    "feed_jetstream_publish_success_total",
    "feed_sequence_regressions_total",
    "feed_sequence_gaps_total",
    "feed_decode_failures_total",
    "feed_readiness",
)
RECORDER_METRICS = (
    "recorder_messages_persisted_total",
    "recorder_last_persisted_feed_sequence",
    "recorder_database_failures_total",
    "recorder_jetstream_ack_failures_total",
    "recorder_poison_messages_total",
    "recorder_readiness",
)
DATABASE_COUNTS = (
    "execution_attempts",
    "executions",
    "realized_pnl",
    "execution_eligible",
    "execution_requests",
    "fork_execution_eligible",
    "fork_execution_requests",
    "origin_transactions",
    "feed_events",
    "duplicate_origins",
    "duplicate_feed_events",
)
EXECUTION_COUNTS = (
    "execution_attempts",
    "executions",
    "realized_pnl",
    "execution_eligible",
    "execution_requests",
    "fork_execution_eligible",
    "fork_execution_requests",
)

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
CONTAINER_RE = re.compile(r"^[0-9a-f]{64}$")
IMAGE_RE = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
SAFE_NAME_RE = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")
INTEGER_METRIC_RE = re.compile(r"^(?:0|[1-9][0-9]*)(?:\.0+)?$")
MAX_JSON_BYTES = 4 * 1024 * 1024
MAX_STORAGE_METADATA_BYTES = 64 * 1024
MIN_DISK_FREE_BYTES = 5 * 1024 * 1024 * 1024
MAX_RECORDER_PENDING = 100_000
MAX_ACK_PENDING = 1_024
MAX_REDELIVERY_DELTA = 1_024


class MaintenanceError(ValueError):
    pass


def _fail(code: str) -> None:
    raise MaintenanceError(code)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail("duplicate_json_key")
        result[key] = value
    return result


def load_json(path: Path, missing_code: str = "artifact_missing") -> Any:
    if not path.is_file():
        _fail(missing_code)
    try:
        raw = path.read_bytes()
    except OSError:
        _fail(missing_code)
    if not raw or len(raw) > MAX_JSON_BYTES:
        _fail("artifact_invalid")
    try:
        return json.loads(
            raw,
            object_pairs_hook=_unique_object,
            parse_constant=lambda _value: _fail("artifact_invalid"),
        )
    except (UnicodeError, json.JSONDecodeError):
        _fail("artifact_invalid")


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        + "\n"
    ).encode("ascii")


def sha256_value(value: Any) -> str:
    return f"sha256:{hashlib.sha256(canonical_bytes(value)).hexdigest()}"


def sha256_file(path: Path) -> str:
    if not path.is_file() or path.is_symlink():
        _fail("protected_storage_evidence_missing")
    try:
        raw = path.read_bytes()
    except OSError:
        _fail("protected_storage_evidence_missing")
    if not raw or len(raw) > MAX_STORAGE_METADATA_BYTES:
        _fail("protected_storage_evidence_invalid")
    return f"sha256:{hashlib.sha256(raw).hexdigest()}"


def write_atomic(path: Path, value: Any, mode: int = 0o640) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(json.dumps(value, indent=2, sort_keys=True).encode("ascii"))
            handle.write(b"\n")
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _exact_object(value: Any, keys: set[str], code: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        _fail(code)
    return value


def _text(value: Any, pattern: re.Pattern[str], code: str) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        _fail(code)
    return value


def _count(value: Any, code: str = "snapshot_invalid") -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        _fail(code)
    return value


def _timestamp(value: Any) -> str:
    if not isinstance(value, str) or len(value) > 64:
        _fail("snapshot_invalid")
    candidate = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        datetime.fromisoformat(candidate)
    except ValueError:
        _fail("snapshot_invalid")
    return value


def _path(value: Any) -> str:
    if not isinstance(value, str) or len(value) > 255 or "\\" in value:
        _fail("release_assets_invalid")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        _fail("release_assets_invalid")
    return value


def validate_release_manifest(path: Path, expected_sha: str) -> dict[str, str]:
    _text(expected_sha, SHA_RE, "release_manifest_invalid")
    value = _exact_object(
        load_json(path),
        {"schema", "release_sha", "created_at", "images"},
        "release_manifest_invalid",
    )
    if value["schema"] != RELEASE_SCHEMA or value["release_sha"] != expected_sha:
        _fail("release_manifest_invalid")
    images = value["images"]
    if not isinstance(images, dict) or set(images) != set(OWNED_IMAGES):
        _fail("release_manifest_invalid")

    references: dict[str, str] = {}
    for name in OWNED_IMAGES:
        image = _exact_object(
            images[name],
            {"repository", "tag", "digest"},
            "release_manifest_invalid",
        )
        repository = f"ghcr.io/majidasgharitabrizi/{name}"
        if (
            image["repository"] != repository
            or image["tag"] != f"sha-{expected_sha}"
            or not isinstance(image["digest"], str)
            or DIGEST_RE.fullmatch(image["digest"]) is None
        ):
            _fail("mutable_image_reference")
        references[name] = f"{repository}@{image['digest']}"
    return references


def validate_asset_manifest(path: Path, expected_sha: str) -> dict[str, dict[str, Any]]:
    value = _exact_object(
        load_json(path, "release_assets_missing"),
        {"schema", "release_sha", "files"},
        "release_assets_invalid",
    )
    if value["schema"] != ASSET_SCHEMA or value["release_sha"] != expected_sha:
        _fail("release_assets_invalid")
    files = value["files"]
    if not isinstance(files, list) or not files or len(files) > 512:
        _fail("release_assets_invalid")
    result: dict[str, dict[str, Any]] = {}
    for raw in files:
        item = _exact_object(
            raw,
            {"path", "mode", "size_bytes", "sha256"},
            "release_assets_invalid",
        )
        name = _path(item["path"])
        if (
            name in result
            or item["mode"] not in {"0644", "0755"}
            or isinstance(item["size_bytes"], bool)
            or not isinstance(item["size_bytes"], int)
            or item["size_bytes"] < 1
            or item["size_bytes"] > 8 * 1024 * 1024
            or not isinstance(item["sha256"], str)
            or DIGEST_RE.fullmatch(item["sha256"]) is None
        ):
            _fail("release_assets_invalid")
        result[name] = item
    return result


def build_plan(
    release_manifest: Path,
    rollback_manifest: Path,
    release_assets_manifest: Path,
    rollback_assets_manifest: Path,
    release_sha: str,
    rollback_sha: str,
) -> dict[str, Any]:
    if release_sha == rollback_sha:
        _fail("release_identity_invalid")
    release_images = validate_release_manifest(release_manifest, release_sha)
    rollback_images = validate_release_manifest(rollback_manifest, rollback_sha)
    release_assets = validate_asset_manifest(release_assets_manifest, release_sha)
    rollback_assets = validate_asset_manifest(rollback_assets_manifest, rollback_sha)

    contract_digests: dict[str, str] = {}
    for path in CONTRACT_PATHS:
        candidate = release_assets.get(path)
        previous = rollback_assets.get(path)
        if candidate is None or previous is None:
            _fail("release_contract_missing")
        if candidate["sha256"] != previous["sha256"]:
            _fail("protected_contract_changed")
        contract_digests[path] = candidate["sha256"]

    changed = [
        name
        for name in MUTABLE_SERVICES
        if release_images[name] != rollback_images[name]
    ]
    if changed != list(MUTABLE_SERVICES):
        _fail("protected_allowlist_mismatch")

    return {
        "schema_version": PLAN_SCHEMA,
        "release_sha": release_sha,
        "rollback_sha": rollback_sha,
        "protected_allowlist": list(MUTABLE_SERVICES),
        "fixed_services": FIXED_IMAGES,
        "optional_services": list(OPTIONAL_SERVICES),
        "quiesce_before_update": ["feed-ingestor"],
        "maintenance_order": list(MAINTENANCE_ORDER),
        "images": {
            "release": release_images,
            "rollback": rollback_images,
        },
        "contract_sha256": contract_digests,
        "bounds": {
            "minimum_disk_free_bytes": MIN_DISK_FREE_BYTES,
            "maximum_ack_pending": MAX_ACK_PENDING,
            "maximum_recorder_pending": MAX_RECORDER_PENDING,
            "maximum_redelivery_delta": MAX_REDELIVERY_DELTA,
        },
        "safety": {
            "mode": "SHADOW",
            "live_execution": False,
            "execution_eligible": False,
            "execution_request_created": False,
            "signer_configured": False,
            "wallet_configured": False,
            "executor_configured": False,
            "public_submission_configured": False,
            "private_submission_configured": False,
            "broadcast_configured": False,
        },
    }


def validate_plan(value: Any) -> dict[str, Any]:
    plan = _exact_object(
        value,
        {
            "schema_version",
            "release_sha",
            "rollback_sha",
            "protected_allowlist",
            "fixed_services",
            "optional_services",
            "quiesce_before_update",
            "maintenance_order",
            "images",
            "contract_sha256",
            "bounds",
            "safety",
        },
        "plan_invalid",
    )
    if (
        plan["schema_version"] != PLAN_SCHEMA
        or SHA_RE.fullmatch(str(plan["release_sha"])) is None
        or SHA_RE.fullmatch(str(plan["rollback_sha"])) is None
        or plan["release_sha"] == plan["rollback_sha"]
        or plan["protected_allowlist"] != list(MUTABLE_SERVICES)
        or plan["fixed_services"] != FIXED_IMAGES
        or plan["optional_services"] != list(OPTIONAL_SERVICES)
        or plan["quiesce_before_update"] != ["feed-ingestor"]
        or plan["maintenance_order"] != list(MAINTENANCE_ORDER)
        or plan["bounds"]
        != {
            "minimum_disk_free_bytes": MIN_DISK_FREE_BYTES,
            "maximum_ack_pending": MAX_ACK_PENDING,
            "maximum_recorder_pending": MAX_RECORDER_PENDING,
            "maximum_redelivery_delta": MAX_REDELIVERY_DELTA,
        }
        or plan["safety"]
        != {
            "mode": "SHADOW",
            "live_execution": False,
            "execution_eligible": False,
            "execution_request_created": False,
            "signer_configured": False,
            "wallet_configured": False,
            "executor_configured": False,
            "public_submission_configured": False,
            "private_submission_configured": False,
            "broadcast_configured": False,
        }
    ):
        _fail("plan_invalid")
    images = plan["images"]
    if not isinstance(images, dict) or set(images) != {"release", "rollback"}:
        _fail("plan_invalid")
    for role in ("release", "rollback"):
        role_images = images[role]
        if not isinstance(role_images, dict) or set(role_images) != set(OWNED_IMAGES):
            _fail("plan_invalid")
        for name, reference in role_images.items():
            expected_prefix = f"ghcr.io/majidasgharitabrizi/{name}@"
            if (
                not isinstance(reference, str)
                or not reference.startswith(expected_prefix)
                or IMAGE_RE.fullmatch(reference) is None
            ):
                _fail("plan_invalid")
    if any(
        images["release"][service] == images["rollback"][service]
        for service in MUTABLE_SERVICES
    ):
        _fail("plan_invalid")
    contract = plan["contract_sha256"]
    if (
        not isinstance(contract, dict)
        or set(contract) != set(CONTRACT_PATHS)
        or any(DIGEST_RE.fullmatch(str(value)) is None for value in contract.values())
    ):
        _fail("plan_invalid")
    return plan


def load_plan(path: Path) -> dict[str, Any]:
    return validate_plan(load_json(path, "plan_missing"))


def validate_render_pair(
    plan: dict[str, Any], release_metadata: Any, rollback_metadata: Any
) -> None:
    for role, metadata in (
        ("release", release_metadata),
        ("rollback", rollback_metadata),
    ):
        if not isinstance(metadata, dict):
            _fail("render_contract_invalid")
        if (
            metadata.get("schema") != "phoenix.production-render.v1"
            or metadata.get("status") != "ok"
            or metadata.get("release_sha") != plan[f"{role}_sha"]
            or metadata.get("chain_id") != 42161
            or metadata.get("mode") != "SHADOW"
            or metadata.get("live_execution") is not False
            or not isinstance(metadata.get("images"), dict)
        ):
            _fail("render_contract_invalid")
        images = metadata["images"]
        expected = plan["images"][role]
        if images.get("feed-ingestor") != expected["feed-ingestor"]:
            _fail("render_contract_invalid")
        if images.get("recorder") != expected["recorder"]:
            _fail("render_contract_invalid")
        for service, reference in FIXED_IMAGES.items():
            if images.get(service) != reference:
                _fail("protected_contract_changed")
    if release_metadata.get("route_registry_hash") != rollback_metadata.get(
        "route_registry_hash"
    ):
        _fail("route_contract_changed")


def _normalize_mounts(value: Any) -> list[dict[str, str]]:
    if not isinstance(value, list) or len(value) > 16:
        _fail("service_identity_invalid")
    result: list[dict[str, str]] = []
    for raw in value:
        if not isinstance(raw, dict):
            _fail("service_identity_invalid")
        mount_type = raw.get("Type")
        destination = raw.get("Destination")
        source = raw.get("Name") if mount_type == "volume" else raw.get("Source")
        if (
            mount_type not in {"bind", "volume"}
            or not isinstance(destination, str)
            or not destination.startswith("/")
            or not isinstance(source, str)
            or not source
        ):
            _fail("service_identity_invalid")
        identity = {
            "type": mount_type,
            "source": source,
            "destination": destination,
            "mode": str(raw.get("Mode", "")),
            "rw": bool(raw.get("RW", False)),
            "propagation": str(raw.get("Propagation", "")),
        }
        result.append(
            {
                "type": mount_type,
                "destination": destination,
                "identity_sha256": sha256_value(identity),
            }
        )
    return sorted(result, key=lambda item: (item["destination"], item["type"]))


def _normalize_networks(value: Any) -> list[dict[str, str]]:
    if not isinstance(value, dict) or not value or len(value) > 8:
        _fail("service_identity_invalid")
    result: list[dict[str, str]] = []
    for name, raw in value.items():
        if not isinstance(name, str) or not isinstance(raw, dict):
            _fail("service_identity_invalid")
        network_id = raw.get("NetworkID")
        if not isinstance(network_id, str) or CONTAINER_RE.fullmatch(network_id) is None:
            _fail("service_identity_invalid")
        result.append({"name": name, "network_id": network_id})
    return sorted(result, key=lambda item: item["name"])


def normalize_service_inspect(path: Path) -> dict[str, Any]:
    value = _exact_object(
        load_json(path, "service_identity_missing"),
        {
            "container_id",
            "configured_image",
            "local_image_id",
            "created_at",
            "started_at",
            "restart_count",
            "oom_killed",
            "status",
            "health",
            "mounts",
            "networks",
        },
        "service_identity_invalid",
    )
    container_id = _text(
        value["container_id"], CONTAINER_RE, "service_identity_invalid"
    )
    configured_image = _text(
        value["configured_image"], IMAGE_RE, "service_identity_invalid"
    )
    local_image_id = _text(
        value["local_image_id"], DIGEST_RE, "service_identity_invalid"
    )
    created_at = _timestamp(value["created_at"])
    started_at = _timestamp(value["started_at"])
    restart_count = _count(value["restart_count"], "service_identity_invalid")
    if not isinstance(value["oom_killed"], bool):
        _fail("service_identity_invalid")
    if value["status"] not in {
        "created",
        "dead",
        "exited",
        "paused",
        "restarting",
        "running",
    } or value["health"] not in {"healthy", "none", "starting", "unhealthy"}:
        _fail("service_identity_invalid")
    return {
        "container_id": container_id,
        "configured_image": configured_image,
        "local_image_id": local_image_id,
        "created_at": created_at,
        "started_at": started_at,
        "restart_count": restart_count,
        "oom_killed": value["oom_killed"],
        "status": value["status"],
        "health": value["health"],
        "mounts": _normalize_mounts(value["mounts"]),
        "networks": _normalize_networks(value["networks"]),
    }


def _jetstream_resources(root: Any) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    if not isinstance(root, dict):
        _fail("jetstream_invalid")
    if isinstance(root.get("streams"), list) and isinstance(root.get("consumers"), list):
        return root["streams"], root["consumers"]
    accounts = root.get("account_details")
    if not isinstance(accounts, list):
        _fail("jetstream_invalid")
    streams: list[dict[str, Any]] = []
    consumers: list[dict[str, Any]] = []
    for account in accounts:
        if not isinstance(account, dict):
            _fail("jetstream_invalid")
        details = account.get("stream_detail", [])
        if not isinstance(details, list):
            _fail("jetstream_invalid")
        for stream in details:
            if not isinstance(stream, dict):
                _fail("jetstream_invalid")
            streams.append(stream)
            nested = stream.get("consumer_detail", [])
            if not isinstance(nested, list):
                _fail("jetstream_invalid")
            consumers.extend(nested)
    return streams, consumers


def normalize_jetstream(value: Any) -> dict[str, Any]:
    streams_raw, consumers_raw = _jetstream_resources(value)
    streams: dict[str, dict[str, Any]] = {}
    for raw in streams_raw:
        if not isinstance(raw, dict) or not isinstance(raw.get("config"), dict):
            _fail("jetstream_invalid")
        config = raw["config"]
        name = raw.get("name") or config.get("name")
        if name in streams or name not in STREAM_NAMES:
            _fail("unexpected_jetstream_resource")
        state = raw.get("state", {})
        if not isinstance(state, dict):
            _fail("jetstream_invalid")
        streams[name] = {
            "config_sha256": sha256_value(config),
            "messages": _count(state.get("messages", 0), "jetstream_invalid"),
            "first_seq": _count(state.get("first_seq", 0), "jetstream_invalid"),
            "last_seq": _count(state.get("last_seq", 0), "jetstream_invalid"),
        }
    if set(streams) != set(STREAM_NAMES):
        _fail("unexpected_jetstream_resource")

    consumers: dict[str, dict[str, Any]] = {}
    for raw in consumers_raw:
        if not isinstance(raw, dict) or not isinstance(raw.get("config"), dict):
            _fail("jetstream_invalid")
        config = raw["config"]
        name = raw.get("name") or config.get("durable_name") or config.get("name")
        if name in consumers or name not in CONSUMER_NAMES:
            _fail("unexpected_jetstream_resource")
        delivered = raw.get("delivered", {})
        ack_floor = raw.get("ack_floor", {})
        if not isinstance(delivered, dict) or not isinstance(ack_floor, dict):
            _fail("jetstream_invalid")
        consumers[name] = {
            "config_sha256": sha256_value(config),
            "pending": _count(raw.get("num_pending", 0), "jetstream_invalid"),
            "ack_pending": _count(
                raw.get("num_ack_pending", 0), "jetstream_invalid"
            ),
            "redelivered": _count(
                raw.get("num_redelivered", 0), "jetstream_invalid"
            ),
            "delivered_stream_seq": _count(
                delivered.get("stream_seq", 0), "jetstream_invalid"
            ),
            "ack_floor_stream_seq": _count(
                ack_floor.get("stream_seq", 0), "jetstream_invalid"
            ),
        }
    if set(consumers) != set(CONSUMER_NAMES):
        _fail("unexpected_jetstream_resource")
    return {"streams": streams, "consumers": consumers}


def normalize_database(value: Any) -> dict[str, Any]:
    value = _exact_object(
        value,
        {"migrations", "counts", "max_feed_sequence"},
        "database_snapshot_invalid",
    )
    migrations_raw = value["migrations"]
    if not isinstance(migrations_raw, list) or not migrations_raw or len(migrations_raw) > 64:
        _fail("database_snapshot_invalid")
    migrations: list[dict[str, str]] = []
    seen: set[str] = set()
    for raw in migrations_raw:
        item = _exact_object(
            raw, {"version", "checksum"}, "database_snapshot_invalid"
        )
        version = _text(item["version"], SAFE_NAME_RE, "database_snapshot_invalid")
        checksum = item["checksum"]
        if (
            version in seen
            or not isinstance(checksum, str)
            or re.fullmatch(r"[0-9a-f]{64}", checksum) is None
        ):
            _fail("database_snapshot_invalid")
        seen.add(version)
        migrations.append({"version": version, "checksum": checksum})
    migrations.sort(key=lambda item: item["version"])

    counts_raw = value["counts"]
    if not isinstance(counts_raw, dict) or set(counts_raw) != set(DATABASE_COUNTS):
        _fail("database_snapshot_invalid")
    counts = {
        name: _count(counts_raw[name], "database_snapshot_invalid")
        for name in DATABASE_COUNTS
    }
    maximum = value["max_feed_sequence"]
    if (
        isinstance(maximum, bool)
        or not isinstance(maximum, (str, int))
        or re.fullmatch(r"(?:0|[1-9][0-9]*)", str(maximum)) is None
    ):
        _fail("database_snapshot_invalid")
    return {
        "migrations": migrations,
        "counts": counts,
        "max_feed_sequence": int(maximum),
    }


def parse_metrics(
    path: Path, required: tuple[str, ...], allow_empty: bool = False
) -> dict[str, int | None]:
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        _fail("metrics_missing")
    if not raw.strip() and allow_empty:
        return {name: None for name in required}
    if not raw or len(raw.encode("utf-8")) > 1024 * 1024:
        _fail("metrics_invalid")
    values: dict[str, int] = {}
    for line in raw.splitlines():
        if not line or line.startswith("#"):
            continue
        fields = line.split()
        if len(fields) != 2 or "{" in fields[0]:
            continue
        name, raw_value = fields
        if name in required:
            if name in values or INTEGER_METRIC_RE.fullmatch(raw_value) is None:
                _fail("metrics_invalid")
            values[name] = int(raw_value.split(".", 1)[0])
    if set(values) != set(required):
        _fail("metrics_invalid")
    return values


def normalize_safety(value: Any) -> dict[str, Any]:
    expected = {
        "mode": "SHADOW",
        "live_execution": False,
        "signer_configured": False,
        "wallet_configured": False,
        "executor_configured": False,
        "public_submission_configured": False,
        "private_submission_configured": False,
        "broadcast_configured": False,
        "execution_eligible": False,
        "execution_request_created": False,
        "optional_services_stopped": True,
    }
    if value != expected:
        _fail("safety_invariant_failed")
    return expected


def build_snapshot(
    phase: str,
    release_sha: str,
    service_inputs: list[str],
    jetstream_path: Path,
    database_path: Path,
    feed_metrics_path: Path,
    recorder_metrics_path: Path,
    safety_path: Path,
    storage_metadata_path: Path,
    disk_free_bytes: int,
) -> dict[str, Any]:
    if phase not in {
        "pre",
        "recorder",
        "post-start",
        "final",
        "rollback-start",
        "rollback-final",
        "promoted",
    }:
        _fail("snapshot_invalid")
    _text(release_sha, SHA_RE, "snapshot_invalid")
    if disk_free_bytes < 0:
        _fail("snapshot_invalid")
    service_paths: dict[str, Path] = {}
    for item in service_inputs:
        if "=" not in item:
            _fail("service_identity_invalid")
        name, raw_path = item.split("=", 1)
        if name in service_paths or name not in PROTECTED_SERVICES:
            _fail("service_identity_invalid")
        service_paths[name] = Path(raw_path)
    if set(service_paths) != set(PROTECTED_SERVICES):
        _fail("service_identity_invalid")
    services = {
        name: normalize_service_inspect(service_paths[name])
        for name in PROTECTED_SERVICES
    }
    return {
        "schema_version": SNAPSHOT_SCHEMA,
        "phase": phase,
        "release_sha": release_sha,
        "observed_at": datetime.now().astimezone().isoformat(timespec="seconds"),
        "disk_free_bytes": disk_free_bytes,
        "services": services,
        "jetstream": normalize_jetstream(load_json(jetstream_path)),
        "database": normalize_database(load_json(database_path)),
        "metrics": {
            "feed": parse_metrics(
                feed_metrics_path, FEED_METRICS, allow_empty=phase == "recorder"
            ),
            "recorder": parse_metrics(recorder_metrics_path, RECORDER_METRICS),
        },
        "safety": normalize_safety(load_json(safety_path)),
        "protected_storage_identity_sha256": sha256_file(storage_metadata_path),
    }


def validate_snapshot(value: Any) -> dict[str, Any]:
    snapshot = _exact_object(
        value,
        {
            "schema_version",
            "phase",
            "release_sha",
            "observed_at",
            "disk_free_bytes",
            "services",
            "jetstream",
            "database",
            "metrics",
            "safety",
            "protected_storage_identity_sha256",
        },
        "snapshot_invalid",
    )
    if (
        snapshot["schema_version"] != SNAPSHOT_SCHEMA
        or snapshot["phase"]
        not in {
            "pre",
            "recorder",
            "post-start",
            "final",
            "rollback-start",
            "rollback-final",
            "promoted",
        }
        or SHA_RE.fullmatch(str(snapshot["release_sha"])) is None
    ):
        _fail("snapshot_invalid")
    _timestamp(snapshot["observed_at"])
    _count(snapshot["disk_free_bytes"])
    _text(
        snapshot["protected_storage_identity_sha256"],
        DIGEST_RE,
        "snapshot_invalid",
    )
    services = snapshot["services"]
    if not isinstance(services, dict) or set(services) != set(PROTECTED_SERVICES):
        _fail("snapshot_invalid")
    for service in PROTECTED_SERVICES:
        normalized = services[service]
        if not isinstance(normalized, dict):
            _fail("snapshot_invalid")
        # Re-run the shape checks against the redacted normalized form.
        if set(normalized) != {
            "container_id",
            "configured_image",
            "local_image_id",
            "created_at",
            "started_at",
            "restart_count",
            "oom_killed",
            "status",
            "health",
            "mounts",
            "networks",
        }:
            _fail("snapshot_invalid")
        _text(normalized["container_id"], CONTAINER_RE, "snapshot_invalid")
        _text(normalized["configured_image"], IMAGE_RE, "snapshot_invalid")
        _text(normalized["local_image_id"], DIGEST_RE, "snapshot_invalid")
        _timestamp(normalized["created_at"])
        _timestamp(normalized["started_at"])
        _count(normalized["restart_count"])
        if not isinstance(normalized["oom_killed"], bool):
            _fail("snapshot_invalid")
        if normalized["status"] not in {
            "created",
            "dead",
            "exited",
            "paused",
            "restarting",
            "running",
        } or normalized["health"] not in {
            "healthy",
            "none",
            "starting",
            "unhealthy",
        }:
            _fail("snapshot_invalid")
        mounts = normalized["mounts"]
        if not isinstance(mounts, list) or len(mounts) > 16:
            _fail("snapshot_invalid")
        for mount in mounts:
            if (
                not isinstance(mount, dict)
                or set(mount) != {"type", "destination", "identity_sha256"}
                or mount["type"] not in {"bind", "volume"}
                or not isinstance(mount["destination"], str)
                or not mount["destination"].startswith("/")
                or DIGEST_RE.fullmatch(str(mount["identity_sha256"])) is None
            ):
                _fail("snapshot_invalid")
        networks = normalized["networks"]
        if not isinstance(networks, list) or not networks or len(networks) > 8:
            _fail("snapshot_invalid")
        for network in networks:
            if (
                not isinstance(network, dict)
                or set(network) != {"name", "network_id"}
                or not isinstance(network["name"], str)
                or not network["name"]
                or CONTAINER_RE.fullmatch(str(network["network_id"])) is None
            ):
                _fail("snapshot_invalid")
    normalize_safety(snapshot["safety"])
    normalize_database(snapshot["database"])
    jetstream = snapshot["jetstream"]
    if not isinstance(jetstream, dict) or set(jetstream) != {"streams", "consumers"}:
        _fail("snapshot_invalid")
    streams = jetstream["streams"]
    if not isinstance(streams, dict) or set(streams) != set(STREAM_NAMES):
        _fail("snapshot_invalid")
    for stream in streams.values():
        if (
            not isinstance(stream, dict)
            or set(stream) != {"config_sha256", "messages", "first_seq", "last_seq"}
            or DIGEST_RE.fullmatch(str(stream["config_sha256"])) is None
        ):
            _fail("snapshot_invalid")
        for key in ("messages", "first_seq", "last_seq"):
            _count(stream[key])
    consumers = jetstream["consumers"]
    if not isinstance(consumers, dict) or set(consumers) != set(CONSUMER_NAMES):
        _fail("snapshot_invalid")
    for consumer in consumers.values():
        if (
            not isinstance(consumer, dict)
            or set(consumer)
            != {
                "config_sha256",
                "pending",
                "ack_pending",
                "redelivered",
                "delivered_stream_seq",
                "ack_floor_stream_seq",
            }
            or DIGEST_RE.fullmatch(str(consumer["config_sha256"])) is None
        ):
            _fail("snapshot_invalid")
        for key in (
            "pending",
            "ack_pending",
            "redelivered",
            "delivered_stream_seq",
            "ack_floor_stream_seq",
        ):
            _count(consumer[key])
    metrics = snapshot["metrics"]
    if not isinstance(metrics, dict) or set(metrics) != {"feed", "recorder"}:
        _fail("snapshot_invalid")
    for group, names in (("feed", FEED_METRICS), ("recorder", RECORDER_METRICS)):
        if not isinstance(metrics[group], dict) or set(metrics[group]) != set(names):
            _fail("snapshot_invalid")
        for item in metrics[group].values():
            if item is not None:
                _count(item)
            elif snapshot["phase"] != "recorder" or group != "feed":
                _fail("snapshot_invalid")
    return snapshot


def _service_healthy(service: dict[str, Any]) -> bool:
    return service["status"] == "running" and service["health"] == "healthy"


def _assert_no_execution(snapshot: dict[str, Any]) -> None:
    counts = snapshot["database"]["counts"]
    if any(counts[name] != 0 for name in EXECUTION_COUNTS):
        _fail("execution_activity_detected")
    if counts["duplicate_origins"] != 0 or counts["duplicate_feed_events"] != 0:
        _fail("database_integrity_failed")
    normalize_safety(snapshot["safety"])


def _assert_continuity(
    baseline: dict[str, Any], current: dict[str, Any]
) -> None:
    if current["disk_free_bytes"] < MIN_DISK_FREE_BYTES:
        _fail("disk_headroom_insufficient")
    if current["database"]["migrations"] != baseline["database"]["migrations"]:
        _fail("migration_state_changed")
    if (
        current["protected_storage_identity_sha256"]
        != baseline["protected_storage_identity_sha256"]
    ):
        _fail("protected_storage_metadata_changed")
    _assert_no_execution(baseline)
    _assert_no_execution(current)

    for service in PROTECTED_SERVICES:
        before = baseline["services"][service]
        after = current["services"][service]
        if before["mounts"] != after["mounts"]:
            _fail("mount_identity_changed")
        if before["networks"] != after["networks"]:
            _fail("network_identity_changed")
    for service in FIXED_SERVICES:
        before = baseline["services"][service]
        after = current["services"][service]
        if (
            before["container_id"] != after["container_id"]
            or before["created_at"] != after["created_at"]
            or before["started_at"] != after["started_at"]
            or before["restart_count"] != after["restart_count"]
            or before["local_image_id"] != after["local_image_id"]
            or after["configured_image"] != FIXED_IMAGES[service]
            or after["oom_killed"]
            or not _service_healthy(after)
        ):
            _fail("fixed_service_identity_changed")

    before_js = baseline["jetstream"]
    after_js = current["jetstream"]
    for name in STREAM_NAMES:
        if (
            before_js["streams"][name]["config_sha256"]
            != after_js["streams"][name]["config_sha256"]
            or after_js["streams"][name]["last_seq"]
            < before_js["streams"][name]["last_seq"]
        ):
            _fail("jetstream_stream_changed")
    for name in CONSUMER_NAMES:
        before = before_js["consumers"][name]
        after = after_js["consumers"][name]
        if (
            before["config_sha256"] != after["config_sha256"]
            or after["delivered_stream_seq"] < before["delivered_stream_seq"]
            or after["ack_floor_stream_seq"] < before["ack_floor_stream_seq"]
            or after["redelivered"] > before["redelivered"] + MAX_REDELIVERY_DELTA
        ):
            _fail("jetstream_consumer_changed")


def _assert_baseline_images(plan: dict[str, Any], baseline: dict[str, Any]) -> None:
    expected_migrations = [
        {
            "version": Path(path).stem,
            "checksum": plan["contract_sha256"][path].removeprefix("sha256:"),
        }
        for path in EXPECTED_MIGRATIONS
    ]
    if baseline["database"]["migrations"] != expected_migrations:
        _fail("migration_state_mismatch")
    for service in FIXED_SERVICES:
        if baseline["services"][service]["configured_image"] != FIXED_IMAGES[service]:
            _fail("baseline_image_mismatch")
    for service in MUTABLE_SERVICES:
        if (
            baseline["services"][service]["configured_image"]
            != plan["images"]["rollback"][service]
        ):
            _fail("baseline_image_mismatch")
    if any(
        baseline["services"][service]["oom_killed"]
        or not _service_healthy(baseline["services"][service])
        for service in PROTECTED_SERVICES
    ):
        _fail("baseline_service_unhealthy")
    _assert_no_execution(baseline)


def _assert_progress(
    baseline: dict[str, Any],
    progress_baseline: dict[str, Any],
    current: dict[str, Any],
) -> None:
    feed_start = progress_baseline["metrics"]["feed"]
    recorder_start = progress_baseline["metrics"]["recorder"]
    database_start = progress_baseline["database"]
    feed = current["metrics"]["feed"]
    recorder = current["metrics"]["recorder"]
    database = current["database"]
    if any(value is None for value in feed.values()):
        _fail("feed_metrics_unavailable")
    if feed["feed_readiness"] != 1:
        _fail("feed_readiness_not_ready")
    if recorder["recorder_readiness"] != 1:
        _fail("recorder_readiness_not_ready")
    if feed["feed_last_sequence"] <= feed_start["feed_last_sequence"]:
        _fail("feed_sequence_not_progressing")
    if (
        feed["feed_last_sequence"]
        < baseline["metrics"]["feed"]["feed_last_sequence"]
    ):
        _fail("feed_sequence_regressed")
    if (
        feed["feed_jetstream_publish_success_total"]
        <= feed_start["feed_jetstream_publish_success_total"]
    ):
        _fail("feed_publish_not_progressing")
    if (
        recorder["recorder_messages_persisted_total"]
        <= recorder_start["recorder_messages_persisted_total"]
    ):
        _fail("recorder_persist_count_not_progressing")
    if (
        recorder["recorder_last_persisted_feed_sequence"]
        <= recorder_start["recorder_last_persisted_feed_sequence"]
    ):
        _fail("recorder_sequence_not_progressing")
    if (
        recorder["recorder_last_persisted_feed_sequence"]
        < baseline["metrics"]["recorder"]["recorder_last_persisted_feed_sequence"]
    ):
        _fail("recorder_sequence_regressed")
    if (
        database["counts"]["feed_events"]
        <= database_start["counts"]["feed_events"]
    ):
        _fail("database_feed_events_not_progressing")
    if (
        database["counts"]["feed_events"]
        < baseline["database"]["counts"]["feed_events"]
    ):
        _fail("database_feed_events_regressed")
    if (
        database["counts"]["origin_transactions"]
        < baseline["database"]["counts"]["origin_transactions"]
    ):
        _fail("database_origin_transactions_regressed")
    if database["max_feed_sequence"] <= database_start["max_feed_sequence"]:
        _fail("database_feed_sequence_not_progressing")
    if database["max_feed_sequence"] < baseline["database"]["max_feed_sequence"]:
        _fail("database_feed_sequence_regressed")
    if any(
        feed[name] != 0
        for name in (
            "feed_sequence_regressions_total",
            "feed_sequence_gaps_total",
            "feed_decode_failures_total",
        )
    ) or any(
        recorder[name] != 0
        for name in (
            "recorder_database_failures_total",
            "recorder_jetstream_ack_failures_total",
            "recorder_poison_messages_total",
        )
    ):
        _fail("runtime_integrity_failed")
    consumer = current["jetstream"]["consumers"]["PHOENIX_RECORDER"]
    if (
        consumer["pending"] > MAX_RECORDER_PENDING
        or consumer["ack_pending"] > MAX_ACK_PENDING
    ):
        _fail("consumer_backlog_unbounded")


def validate_transition(
    plan: dict[str, Any],
    baseline: dict[str, Any],
    current: dict[str, Any],
    stage: str,
    progress_baseline: dict[str, Any] | None,
) -> None:
    _assert_baseline_images(plan, baseline)
    _assert_continuity(baseline, current)
    if stage == "recorder":
        if current["release_sha"] != plan["release_sha"]:
            _fail("transition_invalid")
        feed_before = baseline["services"]["feed-ingestor"]
        feed = current["services"]["feed-ingestor"]
        recorder_before = baseline["services"]["recorder"]
        recorder = current["services"]["recorder"]
        if (
            feed["container_id"] != feed_before["container_id"]
            or feed["configured_image"] != plan["images"]["rollback"]["feed-ingestor"]
            or feed["status"] != "exited"
            or recorder["container_id"] == recorder_before["container_id"]
            or recorder["configured_image"] != plan["images"]["release"]["recorder"]
            or recorder["restart_count"] != 0
            or recorder["oom_killed"]
            or not _service_healthy(recorder)
        ):
            _fail("maintenance_order_invalid")
        return

    if stage not in {"final", "rollback", "promoted"} or progress_baseline is None:
        _fail("transition_invalid")
    role = "release" if stage in {"final", "promoted"} else "rollback"
    expected_sha = plan[f"{role}_sha"]
    if current["release_sha"] != expected_sha:
        _fail("transition_invalid")
    for service in MUTABLE_SERVICES:
        before = baseline["services"][service]
        after = current["services"][service]
        if (
            before["container_id"] == after["container_id"]
            or after["configured_image"] != plan["images"][role][service]
            or after["restart_count"] != 0
            or after["oom_killed"]
            or not _service_healthy(after)
        ):
            _fail("mutable_service_transition_invalid")
    _assert_progress(baseline, progress_baseline, current)


def build_context(
    plan: dict[str, Any], snapshot: dict[str, Any], render_metadata: Any
) -> dict[str, Any]:
    if snapshot["release_sha"] != plan["release_sha"]:
        _fail("context_invalid")
    if not isinstance(render_metadata, dict) or (
        render_metadata.get("release_sha") != plan["release_sha"]
        or render_metadata.get("mode") != "SHADOW"
        or render_metadata.get("live_execution") is not False
        or DIGEST_RE.fullmatch(str(render_metadata.get("route_registry_hash", "")))
        is None
    ):
        _fail("context_invalid")
    _assert_no_execution(snapshot)
    for service in MUTABLE_SERVICES:
        if (
            snapshot["services"][service]["configured_image"]
            != plan["images"]["release"][service]
            or not _service_healthy(snapshot["services"][service])
        ):
            _fail("context_invalid")
    return {
        "schema_version": CONTEXT_SCHEMA,
        "status": "protected_maintenance_complete",
        "release_sha": plan["release_sha"],
        "rollback_sha": plan["rollback_sha"],
        "mode": "SHADOW",
        "live_execution": False,
        "route_registry_hash": render_metadata["route_registry_hash"],
        "protected_allowlist": list(MUTABLE_SERVICES),
        "maintenance_order": list(MAINTENANCE_ORDER),
        "protected_images": {
            service: plan["images"]["release"][service]
            for service in MUTABLE_SERVICES
        },
        "fixed_service_ids": {
            service: snapshot["services"][service]["container_id"]
            for service in FIXED_SERVICES
        },
        "protected_storage_identity_sha256": snapshot[
            "protected_storage_identity_sha256"
        ],
        "optional_services_stopped": True,
        "execution_eligible": False,
        "execution_request_created": False,
        "snapshot_sha256": sha256_value(snapshot),
        "observed_at": snapshot["observed_at"],
    }


def command_plan(args: argparse.Namespace) -> None:
    value = build_plan(
        Path(args.release_manifest),
        Path(args.rollback_manifest),
        Path(args.release_assets_manifest),
        Path(args.rollback_assets_manifest),
        args.release_sha,
        args.rollback_sha,
    )
    write_atomic(Path(args.output), value)
    print("PROTECTED_MAINTENANCE_PLAN_OK")


def command_render_pair(args: argparse.Namespace) -> None:
    plan = load_plan(Path(args.plan))
    validate_render_pair(
        plan,
        load_json(Path(args.release_metadata)),
        load_json(Path(args.rollback_metadata)),
    )
    print("PROTECTED_MAINTENANCE_RENDER_PAIR_OK")


def command_image_refs(args: argparse.Namespace) -> None:
    plan = load_plan(Path(args.plan))
    for role in ("release", "rollback"):
        for name in OWNED_IMAGES:
            print(
                "\t".join(
                    (
                        role,
                        name,
                        plan["images"][role][name],
                        plan[f"{role}_sha"],
                    )
                )
            )


def command_snapshot(args: argparse.Namespace) -> None:
    try:
        disk_free_bytes = int(args.disk_free_bytes)
    except ValueError:
        _fail("snapshot_invalid")
    value = build_snapshot(
        args.phase,
        args.release_sha,
        args.service,
        Path(args.jetstream),
        Path(args.database),
        Path(args.feed_metrics),
        Path(args.recorder_metrics),
        Path(args.safety),
        Path(args.storage_metadata),
        disk_free_bytes,
    )
    write_atomic(Path(args.output), value)
    print(f"PROTECTED_MAINTENANCE_SNAPSHOT_OK: phase={args.phase}")


def command_consumer_state(args: argparse.Namespace) -> None:
    value = normalize_jetstream(load_json(Path(args.jetstream)))
    consumer = value["consumers"]["PHOENIX_RECORDER"]
    print(
        f"{consumer['pending']}\t{consumer['ack_pending']}\t"
        f"{consumer['redelivered']}"
    )


def command_validate_baseline(args: argparse.Namespace) -> None:
    plan = load_plan(Path(args.plan))
    snapshot = validate_snapshot(load_json(Path(args.snapshot)))
    if snapshot["release_sha"] != plan["rollback_sha"]:
        _fail("baseline_image_mismatch")
    _assert_baseline_images(plan, snapshot)
    _assert_no_execution(snapshot)
    if snapshot["disk_free_bytes"] < MIN_DISK_FREE_BYTES:
        _fail("disk_headroom_insufficient")
    print("PROTECTED_MAINTENANCE_BASELINE_OK")


def command_validate_transition(args: argparse.Namespace) -> None:
    plan = load_plan(Path(args.plan))
    baseline = validate_snapshot(load_json(Path(args.baseline)))
    current = validate_snapshot(load_json(Path(args.current)))
    progress = (
        validate_snapshot(load_json(Path(args.progress_baseline)))
        if args.progress_baseline
        else None
    )
    validate_transition(plan, baseline, current, args.stage, progress)
    print(f"PROTECTED_MAINTENANCE_TRANSITION_OK: stage={args.stage}")


def command_context(args: argparse.Namespace) -> None:
    value = build_context(
        load_plan(Path(args.plan)),
        validate_snapshot(load_json(Path(args.snapshot))),
        load_json(Path(args.render_metadata)),
    )
    write_atomic(Path(args.output), value)
    print("PROTECTED_MAINTENANCE_CONTEXT_OK")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)

    plan = commands.add_parser("plan")
    plan.add_argument("--release-manifest", required=True)
    plan.add_argument("--rollback-manifest", required=True)
    plan.add_argument("--release-assets-manifest", required=True)
    plan.add_argument("--rollback-assets-manifest", required=True)
    plan.add_argument("--release-sha", required=True)
    plan.add_argument("--rollback-sha", required=True)
    plan.add_argument("--output", required=True)
    plan.set_defaults(handler=command_plan)

    render = commands.add_parser("validate-render-pair")
    render.add_argument("--plan", required=True)
    render.add_argument("--release-metadata", required=True)
    render.add_argument("--rollback-metadata", required=True)
    render.set_defaults(handler=command_render_pair)

    refs = commands.add_parser("image-refs")
    refs.add_argument("--plan", required=True)
    refs.set_defaults(handler=command_image_refs)

    snapshot = commands.add_parser("snapshot")
    snapshot.add_argument("--phase", required=True)
    snapshot.add_argument("--release-sha", required=True)
    snapshot.add_argument("--service", action="append", required=True)
    snapshot.add_argument("--jetstream", required=True)
    snapshot.add_argument("--database", required=True)
    snapshot.add_argument("--feed-metrics", required=True)
    snapshot.add_argument("--recorder-metrics", required=True)
    snapshot.add_argument("--safety", required=True)
    snapshot.add_argument("--storage-metadata", required=True)
    snapshot.add_argument("--disk-free-bytes", required=True)
    snapshot.add_argument("--output", required=True)
    snapshot.set_defaults(handler=command_snapshot)

    consumer = commands.add_parser("consumer-state")
    consumer.add_argument("--jetstream", required=True)
    consumer.set_defaults(handler=command_consumer_state)

    baseline = commands.add_parser("validate-baseline")
    baseline.add_argument("--plan", required=True)
    baseline.add_argument("--snapshot", required=True)
    baseline.set_defaults(handler=command_validate_baseline)

    transition = commands.add_parser("validate-transition")
    transition.add_argument("--plan", required=True)
    transition.add_argument("--baseline", required=True)
    transition.add_argument("--current", required=True)
    transition.add_argument(
        "--stage", required=True, choices=("recorder", "final", "rollback", "promoted")
    )
    transition.add_argument("--progress-baseline")
    transition.set_defaults(handler=command_validate_transition)

    context = commands.add_parser("context")
    context.add_argument("--plan", required=True)
    context.add_argument("--snapshot", required=True)
    context.add_argument("--render-metadata", required=True)
    context.add_argument("--output", required=True)
    context.set_defaults(handler=command_context)
    return root


def main() -> None:
    args = parser().parse_args()
    try:
        args.handler(args)
    except MaintenanceError as error:
        print(
            json.dumps(
                {"code": str(error), "status": "error"},
                sort_keys=True,
                separators=(",", ":"),
            ),
            file=sys.stderr,
        )
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
