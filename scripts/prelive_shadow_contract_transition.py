#!/usr/bin/env python3
"""Validate the one reviewed Phoenix SHADOW route-contract transition."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tarfile
import tempfile
from copy import deepcopy
from pathlib import Path
from typing import Any

try:
    from scripts import prelive_protected_maintenance as maintenance
    from scripts import production_context
    from scripts import release_assets
    from scripts import shadow_route_discovery
except (ImportError, ModuleNotFoundError):  # Direct execution from scripts/.
    import prelive_protected_maintenance as maintenance  # type: ignore[no-redef]
    import production_context  # type: ignore[no-redef]
    import release_assets  # type: ignore[no-redef]
    import shadow_route_discovery  # type: ignore[no-redef]


PLAN_SCHEMA = "phoenix.shadow-contract-transition-plan.v1"
RUNTIME_SCHEMA = "phoenix.shadow-contract-transition-runtime.v1"
ENV_SUMMARY_SCHEMA = "phoenix.shadow-contract-transition-environment.v1"

RELEASE_SHA = "f1bb82681b02c9f6371c0a8de8c1f498fb307034"
ROLLBACK_SHA = "654dad176fe705d90628b418750a122b8ae30283"
RELEASE_RUN_ID = "29896352400"
ROLLBACK_RUN_ID = "29689298132"
DATABASE_NAME = "phoenix_v5_654dad17"
CONFIRMATION = "APPLY_EXACT_PHOENIX_SHADOW_CONTRACT_TRANSITION"

ROUTE_PROOF_PATH = "fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json"
CANDIDATE_ROUTE_SOURCE = "fixtures/routes/weth_usdc_uniswap_v3.json"
CANDIDATE_PROOF_SHA256 = (
    "sha256:2a1e6ef082c74fecd30673be1261939208f9e0c21a51a76683c2717e55beee8a"
)
ROLLBACK_PROOF_SHA256 = (
    "sha256:0027d6367df0c00767a351794f48c98861b5c86caa2a3413f0e7eefaddd6afbb"
)
CANDIDATE_ROUTE_HASH = (
    "sha256:796a9a497990ada50c08d7050ced9e502a236fab769fd0687a2497b1f4e4349c"
)
ROLLBACK_ROUTE_HASH = (
    "sha256:ad8786f06023a37294a93a697bacfa6287b3a98fbde70ef9bf169e20202dc8ee"
)
REVIEWED_ROUTE_ID = "arbitrum-weth-usdc-uniswap-v3-500-3000"
REVIEWED_ROUTE_FINGERPRINT = f"{REVIEWED_ROUTE_ID}-v1"

LEGACY_IMAGES = maintenance.LEGACY_RELEASE_IMAGES
CURRENT_IMAGES = maintenance.CURRENT_RELEASE_IMAGES
FIXED_SERVICES = maintenance.FIXED_SERVICES
PROTECTED_SERVICES = maintenance.PROTECTED_SERVICES
OPTIONAL_STOP_ORDER = (
    "phoenix-engine",
    "shadow-dispatcher",
    "rpc-gateway",
    "dashboard",
    "prometheus",
)
START_ORDER = (
    "recorder",
    "feed-ingestor",
    "rpc-gateway",
    "phoenix-engine",
    "shadow-dispatcher",
    "dashboard",
    "prometheus",
)
RUNTIME_SERVICES = (*FIXED_SERVICES, *START_ORDER)
OWNED_RUNTIME_IMAGES = {
    "recorder": "recorder",
    "feed-ingestor": "feed-ingestor",
    "rpc-gateway": "rpc-gateway",
    "phoenix-engine": "phoenix-engine",
    "shadow-dispatcher": "recorder",
    "dashboard": "dashboard",
}
ROUTE_ENV_SERVICES = (
    "postgres",
    "migration-runner",
    "rpc-gateway",
    "feed-ingestor",
    "phoenix-engine",
    "shadow-dispatcher",
    "recorder",
)
ROUTE_ENV_NAME = "ENGINE_ROUTE_REGISTRY_JSON"
ENGINE_CONCURRENCY_ENV_NAME = "ENGINE_MAX_EVALUATION_CONCURRENCY"
REVIEWED_ENGINE_CONCURRENCY_DEFAULT = "1"

REPOSITORY = "MajidAsghariTabrizi/anti-gravity-phoenix-v4"
WORKFLOW = "Build Phoenix Images"
RELEASE_INTENT = "PHOENIX_PRELIVE_SHADOW_V5"
PROVENANCE_SCHEMA = "phoenix.release-provenance.v1"
RUN_EVIDENCE_SCHEMA = "phoenix.build-run-evidence.v1"
QUARANTINE = {
    "classification": "NON_CANONICAL_INCOMPLETE_BUILD",
    "run_ids": ["29683234024"],
}

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
RUN_ID_RE = re.compile(r"^[1-9][0-9]{0,19}$")
ENV_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
CONTAINER_ID_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_JSON_BYTES = 4 * 1024 * 1024
MAX_ENV_BYTES = 1024 * 1024
MAX_DIAGNOSTIC_LOG_INPUT_BYTES = 2 * 1024 * 1024
MAX_DIAGNOSTIC_LOG_LINES = 300
MAX_DIAGNOSTIC_LOG_LINE_BYTES = 2 * 1024
MAX_DIAGNOSTIC_LOG_BYTES = 640 * 1024

HANDOFF_SCHEMA = "phoenix.shadow-contract-transition-recorder-handoff.v1"
RECORDER_CONFIG_CHECK_SCHEMA = "phoenix.recorder-config-check.v1"
RECORDER_CONFIG_EVIDENCE_SCHEMA = (
    "phoenix.shadow-contract-transition-recorder-config.v1"
)
RECORDER_CONFIG_ENVIRONMENT_NAMES = (
    "PHOENIX_ENV",
    "PHOENIX_MODE",
    "LIVE_EXECUTION",
    "SIGNER_PRIVATE_KEY",
    "EXECUTOR_ADDRESS",
    "WALLET_ADDRESS",
    "RECORDER_DAEMON",
    "RECORDER_PERSISTENCE_POLICY",
    "RECORDER_HEALTH_ADDR",
    "POSTGRES_DSN",
    "PGSSLMODE",
    "NATS_URL",
    "RECORDER_BATCH_MAX_SIZE",
    "RECORDER_BATCH_MAX_WAIT_MS",
    "RECORDER_AGGREGATE_FLUSH_SECONDS",
    "RECORDER_AGGREGATE_FLUSH_EVENTS",
    "RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY",
    "RECORDER_MAX_SAMPLE_JSON_BYTES",
    "ENGINE_ROUTER_ADDRESSES",
    "ENGINE_ROUTE_REGISTRY_JSON",
)
RECORDER_CONFIG_SAFE_LENGTH_NAMES = frozenset(
    {
        "PHOENIX_ENV",
        "PHOENIX_MODE",
        "LIVE_EXECUTION",
        "RECORDER_DAEMON",
        "RECORDER_PERSISTENCE_POLICY",
        "PGSSLMODE",
        "RECORDER_BATCH_MAX_SIZE",
        "RECORDER_BATCH_MAX_WAIT_MS",
        "RECORDER_AGGREGATE_FLUSH_SECONDS",
        "RECORDER_AGGREGATE_FLUSH_EVENTS",
        "RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY",
        "RECORDER_MAX_SAMPLE_JSON_BYTES",
        "ENGINE_ROUTER_ADDRESSES",
        "ENGINE_ROUTE_REGISTRY_JSON",
    }
)
RECORDER_STRUCTURED_ENVIRONMENT_NAMES = (
    "ENGINE_ROUTER_ADDRESSES",
    "ENGINE_ROUTE_REGISTRY_JSON",
)
CONFIG_CHECK_CODE_RE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
MAX_CONFIG_CHECK_RESULT_BYTES = 4096
URL_VALUE_RE = re.compile(
    r"(?i)(?:https?|wss?|postgres(?:ql)?|nats)://[^\s\"']+"
)
ADDRESS_RE = re.compile(r"(?i)0x[0-9a-f]{40}")
PRIVATE_VALUE_RE = re.compile(r"(?i)0x[0-9a-f]{64,}")
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


class TransitionError(ValueError):
    pass


def _fail(code: str) -> None:
    raise TransitionError(code)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            _fail("duplicate_json_key")
        value[key] = item
    return value


def load_json(path: Path, missing_code: str = "artifact_missing") -> Any:
    if path.is_symlink() or not path.is_file():
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


def _read_file(path: Path, maximum: int = MAX_JSON_BYTES) -> bytes:
    if path.is_symlink() or not path.is_file():
        _fail("artifact_missing")
    try:
        size = path.stat().st_size
        if size <= 0 or size > maximum:
            _fail("artifact_invalid")
        return path.read_bytes()
    except OSError:
        _fail("artifact_invalid")


def _sha256_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def _sha256_file(path: Path, maximum: int = MAX_JSON_BYTES) -> str:
    return _sha256_bytes(_read_file(path, maximum))


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    ).encode("ascii")


def _exact(value: Any, keys: set[str], code: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        _fail(code)
    return value


def _digest(value: Any, code: str) -> str:
    if not isinstance(value, str) or DIGEST_RE.fullmatch(value) is None:
        _fail(code)
    return value


def _expected_jobs(images: tuple[str, ...]) -> tuple[str, ...]:
    return (
        "publication-preflight",
        *(f"build-{name}" for name in images),
        "release-assets",
        "release-manifest",
    )


def _expected_artifacts(images: tuple[str, ...], release_sha: str) -> tuple[str, ...]:
    return (
        *(f"release-fragment-{name}" for name in images),
        f"phoenix-release-assets-{release_sha}",
        f"phoenix-release-manifest-{release_sha}",
    )


def _validate_manifest(
    value: Any, release_sha: str, images: tuple[str, ...]
) -> tuple[dict[str, Any], dict[str, str]]:
    manifest = _exact(
        value,
        {"schema", "release_sha", "created_at", "images"},
        "release_manifest_invalid",
    )
    if (
        manifest["schema"] != maintenance.RELEASE_SCHEMA
        or manifest["release_sha"] != release_sha
        or not isinstance(manifest["created_at"], str)
        or not manifest["created_at"]
    ):
        _fail("release_manifest_invalid")
    raw_images = manifest["images"]
    if not isinstance(raw_images, dict) or tuple(sorted(raw_images)) != images:
        _fail("release_manifest_invalid")
    references: dict[str, str] = {}
    for name in images:
        image = _exact(
            raw_images[name],
            {"repository", "tag", "digest"},
            "release_manifest_invalid",
        )
        repository = f"ghcr.io/majidasgharitabrizi/{name}"
        if (
            image["repository"] != repository
            or image["tag"] != f"sha-{release_sha}"
            or _digest(image["digest"], "release_manifest_invalid")
            == f"sha256:{'0' * 64}"
        ):
            _fail("release_manifest_invalid")
        references[name] = f"{repository}@{image['digest']}"
    return manifest, references


def _validate_provenance(
    value: Any,
    manifest: dict[str, Any],
    manifest_raw: bytes,
    *,
    release_sha: str,
    run_id: str,
    images: tuple[str, ...],
    archive: Path,
    assets_manifest: Path,
    checksums: Path,
) -> dict[str, Any]:
    provenance = _exact(
        value,
        {
            "schema",
            "repository",
            "workflow",
            "release_sha",
            "release_intent",
            "build_run_id",
            "quarantine",
            "required_jobs",
            "required_release_artifacts",
            "image_fragments",
            "release_assets",
            "release_manifest_sha256",
        },
        "release_provenance_invalid",
    )
    if (
        provenance["schema"] != PROVENANCE_SCHEMA
        or provenance["repository"] != REPOSITORY
        or provenance["workflow"] != WORKFLOW
        or provenance["release_sha"] != release_sha
        or provenance["release_intent"] != RELEASE_INTENT
        or provenance["build_run_id"] != run_id
        or RUN_ID_RE.fullmatch(run_id) is None
        or provenance["quarantine"] != QUARANTINE
        or provenance["required_jobs"] != list(_expected_jobs(images))
        or provenance["required_release_artifacts"]
        != list(_expected_artifacts(images, release_sha))
        or provenance["release_manifest_sha256"] != _sha256_bytes(manifest_raw)
    ):
        _fail("release_provenance_invalid")
    _validate_manifest(manifest, release_sha, images)

    fragments = provenance["image_fragments"]
    if not isinstance(fragments, dict) or tuple(sorted(fragments)) != images:
        _fail("release_provenance_invalid")
    for name in images:
        fragment = _exact(
            fragments[name],
            {"artifact_name", "sha256"},
            "release_provenance_invalid",
        )
        if fragment["artifact_name"] != f"release-fragment-{name}":
            _fail("release_provenance_invalid")
        _digest(fragment["sha256"], "release_provenance_invalid")

    assets = _exact(
        provenance["release_assets"],
        {
            "artifact_name",
            "archive_name",
            "archive_sha256",
            "manifest_sha256",
            "checksums_sha256",
        },
        "release_provenance_invalid",
    )
    if (
        assets["artifact_name"] != f"phoenix-release-assets-{release_sha}"
        or assets["archive_name"]
        != f"phoenix-release-assets-{release_sha}.tar.gz"
        or assets["archive_sha256"] != _sha256_file(archive, release_assets.MAX_ARCHIVE_BYTES)
        or assets["manifest_sha256"] != _sha256_file(assets_manifest)
        or assets["checksums_sha256"] != _sha256_file(checksums)
    ):
        _fail("release_provenance_invalid")
    return provenance


def _validate_run_evidence(
    value: Any,
    *,
    release_sha: str,
    run_id: str,
    images: tuple[str, ...],
) -> None:
    evidence = _exact(
        value,
        {
            "schema",
            "repository",
            "workflow",
            "event",
            "run_id",
            "head_sha",
            "release_intent",
            "status",
            "conclusion",
            "jobs",
            "artifacts",
        },
        "build_run_evidence_invalid",
    )
    if (
        evidence["schema"] != RUN_EVIDENCE_SCHEMA
        or evidence["repository"] != REPOSITORY
        or evidence["workflow"] != WORKFLOW
        or evidence["event"] != "workflow_dispatch"
        or evidence["run_id"] != run_id
        or evidence["head_sha"] != release_sha
        or evidence["release_intent"] != RELEASE_INTENT
        or evidence["status"] != "completed"
        or evidence["conclusion"] != "success"
    ):
        _fail("build_run_evidence_invalid")
    jobs = evidence["jobs"]
    if not isinstance(jobs, list):
        _fail("build_run_evidence_invalid")
    by_name: dict[str, dict[str, Any]] = {}
    for raw in jobs:
        job = _exact(raw, {"name", "status", "conclusion"}, "build_run_evidence_invalid")
        name = job["name"]
        if not isinstance(name, str) or name in by_name:
            _fail("build_run_evidence_invalid")
        by_name[name] = job
    for name in _expected_jobs(images):
        job = by_name.get(name)
        if job is None or job["status"] != "completed" or job["conclusion"] != "success":
            _fail(f"build_job_not_successful:{name}")
    artifacts = evidence["artifacts"]
    if (
        not isinstance(artifacts, list)
        or len(artifacts) != len(set(artifacts))
        or any(not isinstance(name, str) or not name for name in artifacts)
    ):
        _fail("build_run_evidence_invalid")
    for name in _expected_artifacts(images, release_sha):
        if name not in artifacts:
            _fail(f"build_artifact_missing:{name}")


def _read_archive_member(archive_path: Path, release_sha: str, relative: str) -> bytes:
    expected = f"phoenix-release-{release_sha}/{relative}"
    try:
        with tarfile.open(archive_path, mode="r:gz") as archive:
            members = [member for member in archive.getmembers() if member.name == expected]
            if len(members) != 1 or not members[0].isfile():
                _fail("release_contract_missing")
            member = members[0]
            if member.size <= 0 or member.size > release_assets.MAX_FILE_BYTES:
                _fail("release_contract_invalid")
            handle = archive.extractfile(member)
            if handle is None:
                _fail("release_contract_invalid")
            value = handle.read(release_assets.MAX_FILE_BYTES + 1)
    except (OSError, tarfile.TarError):
        _fail("release_contract_invalid")
    if len(value) != member.size:
        _fail("release_contract_invalid")
    return value


def validate_release_evidence(
    *,
    manifest_path: Path,
    archive_path: Path,
    assets_manifest_path: Path,
    checksums_path: Path,
    provenance_path: Path,
    run_evidence_path: Path,
    release_sha: str,
    run_id: str,
    images: tuple[str, ...],
) -> dict[str, Any]:
    try:
        release_assets.verify_release_assets(
            archive_path, assets_manifest_path, checksums_path, release_sha
        )
    except release_assets.ReleaseAssetError:
        _fail("release_assets_invalid")
    manifest_raw = _read_file(manifest_path)
    manifest, references = _validate_manifest(
        load_json(manifest_path), release_sha, images
    )
    provenance = _validate_provenance(
        load_json(provenance_path),
        manifest,
        manifest_raw,
        release_sha=release_sha,
        run_id=run_id,
        images=images,
        archive=archive_path,
        assets_manifest=assets_manifest_path,
        checksums=checksums_path,
    )
    _validate_run_evidence(
        load_json(run_evidence_path),
        release_sha=release_sha,
        run_id=run_id,
        images=images,
    )
    assets = maintenance.validate_asset_manifest(assets_manifest_path, release_sha)
    migrations = tuple(
        sorted(path for path in assets if path.startswith("migrations/") and path.endswith(".sql"))
    )
    if migrations != tuple(sorted(maintenance.EXPECTED_MIGRATIONS)):
        _fail("root_migration_contract_changed")
    proof = _read_archive_member(archive_path, release_sha, ROUTE_PROOF_PATH)
    if assets.get(ROUTE_PROOF_PATH, {}).get("sha256") != _sha256_bytes(proof):
        _fail("route_proof_contract_invalid")
    return {
        "sha": release_sha,
        "run_id": run_id,
        "images": references,
        "assets": assets,
        "proof": proof,
        "artifact_sha256": {
            "archive": provenance["release_assets"]["archive_sha256"],
            "assets_manifest": provenance["release_assets"]["manifest_sha256"],
            "checksums": provenance["release_assets"]["checksums_sha256"],
            "release_manifest": provenance["release_manifest_sha256"],
            "provenance": _sha256_file(provenance_path),
            "run_evidence": _sha256_file(run_evidence_path),
        },
    }


def _load_candidate_route_mapping(route_path: Path, proof: bytes) -> list[dict[str, Any]]:
    try:
        raw = _read_file(route_path, production_context.MAX_ROUTE_BYTES).decode("utf-8")
        routes, route_hash = production_context.validate_route_registry(raw)
    except (UnicodeError, production_context.ContextError):
        _fail("candidate_route_mapping_invalid")
    if route_hash != CANDIDATE_ROUTE_HASH or len(routes) != 1:
        _fail("candidate_route_mapping_invalid")

    with tempfile.TemporaryDirectory() as directory:
        proof_path = Path(directory) / "pool-proofs.json"
        proof_path.write_bytes(proof)
        try:
            proofs, templates, _metadata = shadow_route_discovery.load_pool_proofs(
                proof_path
            )
        except shadow_route_discovery.DiscoveryError:
            _fail("candidate_route_evidence_invalid")

    route = routes[0]
    if not isinstance(route, dict):
        _fail("candidate_route_mapping_invalid")
    legs = route.get("legs")
    if not isinstance(legs, list) or len(legs) != 2:
        _fail("candidate_route_mapping_invalid")
    try:
        token0, token1 = sorted(
            {
                str(legs[0]["token_in"]),
                str(legs[0]["token_out"]),
                str(legs[1]["token_in"]),
                str(legs[1]["token_out"]),
            }
        )
        candidate_key = (
            token0,
            token1,
            str(route["settlement_asset"]),
            int(legs[0]["fee"]),
            int(legs[1]["fee"]),
        )
    except (KeyError, TypeError, ValueError):
        _fail("candidate_route_mapping_invalid")
    suggested = shadow_route_discovery.suggested_route(candidate_key, proofs, templates)
    if suggested is None:
        _fail("candidate_route_mapping_missing")
    suggested["route_id"] = REVIEWED_ROUTE_ID
    suggested["route_fingerprint"] = REVIEWED_ROUTE_FINGERPRINT
    if route != suggested:
        _fail("candidate_route_mapping_invalid")
    return routes


def build_plan_from_evidence(
    release: dict[str, Any],
    rollback: dict[str, Any],
    candidate_route_registry: Path,
) -> dict[str, Any]:
    if (
        release.get("sha") != RELEASE_SHA
        or release.get("run_id") != RELEASE_RUN_ID
        or rollback.get("sha") != ROLLBACK_SHA
        or rollback.get("run_id") != ROLLBACK_RUN_ID
    ):
        _fail("release_pair_not_reviewed")
    if tuple(sorted(release.get("images", {}))) != CURRENT_IMAGES:
        _fail("candidate_image_contract_invalid")
    if tuple(sorted(rollback.get("images", {}))) != LEGACY_IMAGES:
        _fail("rollback_image_contract_invalid")
    if _sha256_bytes(release.get("proof", b"")) != CANDIDATE_PROOF_SHA256:
        _fail("candidate_route_evidence_invalid")
    release_assets_index = release.get("assets")
    rollback_assets_index = rollback.get("assets")
    if not isinstance(release_assets_index, dict) or not isinstance(
        rollback_assets_index, dict
    ):
        _fail("release_assets_invalid")

    identical: dict[str, str] = {}
    for path in maintenance.EXACT_HASH_CONTRACT_PATHS:
        candidate = release_assets_index.get(path)
        previous = rollback_assets_index.get(path)
        if not isinstance(candidate, dict) or not isinstance(previous, dict):
            _fail("release_contract_missing")
        candidate_digest = candidate.get("sha256")
        rollback_digest = previous.get("sha256")
        _digest(candidate_digest, "release_contract_invalid")
        _digest(rollback_digest, "release_contract_invalid")
        if path == ROUTE_PROOF_PATH:
            if (
                candidate_digest != CANDIDATE_PROOF_SHA256
                or rollback_digest != ROLLBACK_PROOF_SHA256
                or candidate_digest == rollback_digest
            ):
                _fail("route_proof_transition_not_reviewed")
            continue
        if candidate_digest != rollback_digest:
            _fail(f"protected_contract_changed:{path}")
        identical[path] = candidate_digest

    release_compose = release_assets_index.get(maintenance.COMPOSE_CONTRACT_PATH)
    rollback_compose = rollback_assets_index.get(maintenance.COMPOSE_CONTRACT_PATH)
    if not isinstance(release_compose, dict) or not isinstance(rollback_compose, dict):
        _fail("release_contract_missing")
    route_registry = _load_candidate_route_mapping(
        candidate_route_registry, release["proof"]
    )
    migration_digests = {
        path: identical[path] for path in maintenance.EXPECTED_MIGRATIONS
    }
    plan = {
        "schema_version": PLAN_SCHEMA,
        "release_sha": RELEASE_SHA,
        "rollback_sha": ROLLBACK_SHA,
        "build_runs": {
            "release": RELEASE_RUN_ID,
            "rollback": ROLLBACK_RUN_ID,
        },
        "database": DATABASE_NAME,
        "images": {
            "release": release["images"],
            "rollback": rollback["images"],
        },
        "artifacts": {
            "release": release["artifact_sha256"],
            "rollback": rollback["artifact_sha256"],
        },
        "contracts": {
            "identical": identical,
            "compose_source_sha256": {
                "release": release_compose["sha256"],
                "rollback": rollback_compose["sha256"],
            },
            "permitted_transition": {
                "path": ROUTE_PROOF_PATH,
                "release_sha256": CANDIDATE_PROOF_SHA256,
                "rollback_sha256": ROLLBACK_PROOF_SHA256,
            },
        },
        "migrations": {
            "paths": list(maintenance.EXPECTED_MIGRATIONS),
            "sha256": migration_digests,
        },
        "route_contract": {
            "mapping": "scripts.shadow_route_discovery.suggested_route",
            "source": CANDIDATE_ROUTE_SOURCE,
            "release_registry_sha256": CANDIDATE_ROUTE_HASH,
            "rollback_registry_sha256": ROLLBACK_ROUTE_HASH,
            "release_registry": route_registry,
        },
        "services": {
            "fixed": list(FIXED_SERVICES),
            "protected": list(PROTECTED_SERVICES),
            "optional_stop_order": list(OPTIONAL_STOP_ORDER),
            "start_order": list(START_ORDER),
            "runtime": list(RUNTIME_SERVICES),
        },
        "safety": {
            "mode": "SHADOW",
            "live_execution": False,
            "chain_id": 42161,
            "signer_configured": False,
            "wallet_configured": False,
            "executor_configured": False,
            "public_submission_configured": False,
            "private_submission_configured": False,
            "broadcast_configured": False,
            "live_executor_armed": False,
            "live_executor_kill_switch": True,
            "execution_eligible": False,
            "execution_request_created": False,
            "recorder_persistence_policy": "money_path_v1",
        },
        "confirmation": CONFIRMATION,
    }
    return validate_plan(plan)


def build_plan(args: argparse.Namespace) -> dict[str, Any]:
    release = validate_release_evidence(
        manifest_path=Path(args.release_manifest),
        archive_path=Path(args.release_archive),
        assets_manifest_path=Path(args.release_assets_manifest),
        checksums_path=Path(args.release_checksums),
        provenance_path=Path(args.release_provenance),
        run_evidence_path=Path(args.release_run_evidence),
        release_sha=RELEASE_SHA,
        run_id=RELEASE_RUN_ID,
        images=CURRENT_IMAGES,
    )
    rollback = validate_release_evidence(
        manifest_path=Path(args.rollback_manifest),
        archive_path=Path(args.rollback_archive),
        assets_manifest_path=Path(args.rollback_assets_manifest),
        checksums_path=Path(args.rollback_checksums),
        provenance_path=Path(args.rollback_provenance),
        run_evidence_path=Path(args.rollback_run_evidence),
        release_sha=ROLLBACK_SHA,
        run_id=ROLLBACK_RUN_ID,
        images=LEGACY_IMAGES,
    )
    return build_plan_from_evidence(
        release, rollback, Path(args.candidate_route_registry)
    )


def _validate_image_references(
    value: Any, images: tuple[str, ...], release_sha: str
) -> dict[str, str]:
    if not isinstance(value, dict) or tuple(sorted(value)) != images:
        _fail("plan_invalid")
    result: dict[str, str] = {}
    for name in images:
        reference = value[name]
        prefix = f"ghcr.io/majidasgharitabrizi/{name}@"
        if (
            not isinstance(reference, str)
            or not reference.startswith(prefix)
            or maintenance.IMAGE_RE.fullmatch(reference) is None
        ):
            _fail("plan_invalid")
        result[name] = reference
    if release_sha not in {RELEASE_SHA, ROLLBACK_SHA}:
        _fail("plan_invalid")
    return result


def _validate_artifact_hashes(value: Any) -> dict[str, str]:
    expected = {
        "archive",
        "assets_manifest",
        "checksums",
        "release_manifest",
        "provenance",
        "run_evidence",
    }
    if not isinstance(value, dict) or set(value) != expected:
        _fail("plan_invalid")
    for digest in value.values():
        _digest(digest, "plan_invalid")
    return value


def validate_plan(value: Any) -> dict[str, Any]:
    plan = _exact(
        value,
        {
            "schema_version",
            "release_sha",
            "rollback_sha",
            "build_runs",
            "database",
            "images",
            "artifacts",
            "contracts",
            "migrations",
            "route_contract",
            "services",
            "safety",
            "confirmation",
        },
        "plan_invalid",
    )
    if (
        plan["schema_version"] != PLAN_SCHEMA
        or plan["release_sha"] != RELEASE_SHA
        or plan["rollback_sha"] != ROLLBACK_SHA
        or plan["build_runs"]
        != {"release": RELEASE_RUN_ID, "rollback": ROLLBACK_RUN_ID}
        or plan["database"] != DATABASE_NAME
        or plan["confirmation"] != CONFIRMATION
    ):
        _fail("plan_invalid")

    images = _exact(plan["images"], {"release", "rollback"}, "plan_invalid")
    _validate_image_references(images["release"], CURRENT_IMAGES, RELEASE_SHA)
    _validate_image_references(images["rollback"], LEGACY_IMAGES, ROLLBACK_SHA)
    artifacts = _exact(
        plan["artifacts"], {"release", "rollback"}, "plan_invalid"
    )
    _validate_artifact_hashes(artifacts["release"])
    _validate_artifact_hashes(artifacts["rollback"])

    contracts = _exact(
        plan["contracts"],
        {"identical", "compose_source_sha256", "permitted_transition"},
        "plan_invalid",
    )
    expected_identical = set(maintenance.EXACT_HASH_CONTRACT_PATHS) - {
        ROUTE_PROOF_PATH
    }
    identical = contracts["identical"]
    if not isinstance(identical, dict) or set(identical) != expected_identical:
        _fail("plan_invalid")
    for digest in identical.values():
        _digest(digest, "plan_invalid")
    compose = _exact(
        contracts["compose_source_sha256"],
        {"release", "rollback"},
        "plan_invalid",
    )
    _digest(compose["release"], "plan_invalid")
    _digest(compose["rollback"], "plan_invalid")
    if contracts["permitted_transition"] != {
        "path": ROUTE_PROOF_PATH,
        "release_sha256": CANDIDATE_PROOF_SHA256,
        "rollback_sha256": ROLLBACK_PROOF_SHA256,
    }:
        _fail("plan_invalid")

    migrations = _exact(plan["migrations"], {"paths", "sha256"}, "plan_invalid")
    if migrations["paths"] != list(maintenance.EXPECTED_MIGRATIONS):
        _fail("plan_invalid")
    if (
        not isinstance(migrations["sha256"], dict)
        or set(migrations["sha256"]) != set(maintenance.EXPECTED_MIGRATIONS)
    ):
        _fail("plan_invalid")
    for path, digest in migrations["sha256"].items():
        if identical.get(path) != digest:
            _fail("plan_invalid")
        _digest(digest, "plan_invalid")

    route = _exact(
        plan["route_contract"],
        {
            "mapping",
            "source",
            "release_registry_sha256",
            "rollback_registry_sha256",
            "release_registry",
        },
        "plan_invalid",
    )
    if (
        route["mapping"] != "scripts.shadow_route_discovery.suggested_route"
        or route["source"] != CANDIDATE_ROUTE_SOURCE
        or route["release_registry_sha256"] != CANDIDATE_ROUTE_HASH
        or route["rollback_registry_sha256"] != ROLLBACK_ROUTE_HASH
    ):
        _fail("plan_invalid")
    try:
        route_raw = _canonical_json(route["release_registry"]).decode("ascii")
        routes, route_hash = production_context.validate_route_registry(route_raw)
    except production_context.ContextError:
        _fail("plan_invalid")
    if route_hash != CANDIDATE_ROUTE_HASH or routes != route["release_registry"]:
        _fail("plan_invalid")

    if plan["services"] != {
        "fixed": list(FIXED_SERVICES),
        "protected": list(PROTECTED_SERVICES),
        "optional_stop_order": list(OPTIONAL_STOP_ORDER),
        "start_order": list(START_ORDER),
        "runtime": list(RUNTIME_SERVICES),
    }:
        _fail("plan_invalid")
    if plan["safety"] != {
        "mode": "SHADOW",
        "live_execution": False,
        "chain_id": 42161,
        "signer_configured": False,
        "wallet_configured": False,
        "executor_configured": False,
        "public_submission_configured": False,
        "private_submission_configured": False,
        "broadcast_configured": False,
        "live_executor_armed": False,
        "live_executor_kill_switch": True,
        "execution_eligible": False,
        "execution_request_created": False,
        "recorder_persistence_policy": "money_path_v1",
    }:
        _fail("plan_invalid")
    return plan


def load_plan(path: Path) -> dict[str, Any]:
    return validate_plan(load_json(path, "plan_missing"))


def write_atomic(path: Path, value: Any, mode: int = 0o640) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(json.dumps(value, indent=2, sort_keys=True).encode("ascii"))
            handle.write(b"\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def write_bytes_atomic(path: Path, value: bytes, mode: int = 0o640) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(
        prefix=f".{path.name}.", dir=path.parent
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def _parse_env(path: Path) -> tuple[list[str], dict[str, str]]:
    raw = _read_file(path, MAX_ENV_BYTES)
    try:
        lines = raw.decode("utf-8-sig").splitlines()
    except UnicodeError:
        _fail("operator_environment_invalid")
    values: dict[str, str] = {}
    for raw_line in lines:
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            _fail("operator_environment_invalid")
        name, candidate = line.split("=", 1)
        name = name.strip()
        if ENV_NAME_RE.fullmatch(name) is None or name in values:
            _fail("operator_environment_invalid")
        candidate = candidate.strip()
        if len(candidate) >= 2 and candidate[0] == candidate[-1] == "'":
            candidate = candidate[1:-1]
        elif len(candidate) >= 2 and candidate[0] == candidate[-1] == '"':
            try:
                decoded = json.loads(candidate)
            except json.JSONDecodeError:
                _fail("operator_environment_invalid")
            if not isinstance(decoded, str):
                _fail("operator_environment_invalid")
            candidate = decoded
        values[name] = candidate
    return lines, values


def _nonnegative_integer(value: Any, code: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        _fail(code)
    return value


def _recorder_render_environment(
    plan: dict[str, Any], compose_value: Any
) -> tuple[str, dict[str, str]]:
    rendered = maintenance._validate_rendered_compose(compose_value)
    recorder = rendered["services"]["recorder"]
    expected_image = plan["images"]["release"]["recorder"]
    if recorder.get("image") != expected_image:
        _fail("recorder_config_image_invalid")
    environment = recorder.get("environment")
    if not isinstance(environment, dict):
        _fail("recorder_config_environment_invalid")
    normalized: dict[str, str] = {}
    total_bytes = 0
    for name, value in environment.items():
        if (
            not isinstance(name, str)
            or ENV_NAME_RE.fullmatch(name) is None
            or not isinstance(value, str)
            or any(character in value for character in ("\0", "\r", "\n"))
        ):
            _fail("recorder_config_environment_invalid")
        encoded = value.encode("utf-8")
        total_bytes += len(name.encode("ascii")) + len(encoded) + 2
        if total_bytes > MAX_ENV_BYTES:
            _fail("recorder_config_environment_invalid")
        normalized[name] = value
    return expected_image, normalized


def _length_bound(value: str) -> str:
    length = len(value.encode("utf-8"))
    for maximum in (0, 16, 64, 256, 4096, 65536):
        if length <= maximum:
            return f"0-{maximum}" if maximum else "0"
    return "65537-plus"


def _structured_fingerprint(name: str, value: str) -> dict[str, Any]:
    canonical = False
    encoded = value.encode("utf-8")
    if name == "ENGINE_ROUTE_REGISTRY_JSON":
        try:
            decoded = json.loads(
                value,
                object_pairs_hook=_unique_object,
                parse_constant=lambda _value: _fail("structured_config_invalid"),
            )
            encoded = _canonical_json(decoded)
            canonical = True
        except (TransitionError, UnicodeError, json.JSONDecodeError):
            pass
    elif name == "ENGINE_ROUTER_ADDRESSES":
        values = value.split(",")
        if values and all(item and item == item.strip() for item in values):
            encoded = ",".join(values).encode("utf-8")
            canonical = True
    return {
        "canonical": canonical,
        "sha256": _sha256_bytes(encoded),
    }


def prepare_recorder_config_check(
    plan: dict[str, Any], compose_value: Any, output: Path
) -> None:
    _image, environment = _recorder_render_environment(plan, compose_value)
    lines = [f"{name}={environment[name]}\n" for name in sorted(environment)]
    write_bytes_atomic(output, "".join(lines).encode("utf-8"), mode=0o600)


def _validate_config_check_result(value: Any) -> dict[str, Any]:
    result = _exact(
        value,
        {"schema", "status", "error_code", "environment_name"},
        "recorder_config_check_result_invalid",
    )
    status = result["status"]
    code = result["error_code"]
    environment_name = result["environment_name"]
    if (
        result["schema"] != RECORDER_CONFIG_CHECK_SCHEMA
        or status not in {"ok", "error"}
        or not isinstance(code, str)
        or CONFIG_CHECK_CODE_RE.fullmatch(code) is None
        or (
            environment_name is not None
            and environment_name not in RECORDER_CONFIG_ENVIRONMENT_NAMES
        )
    ):
        _fail("recorder_config_check_result_invalid")
    if (status, code, environment_name) == ("ok", "ok", None):
        return result
    if status != "error" or code == "ok":
        _fail("recorder_config_check_result_invalid")
    return result


def load_config_check_result(path: Path) -> dict[str, Any]:
    raw = _read_file(path, MAX_CONFIG_CHECK_RESULT_BYTES)
    try:
        value = json.loads(
            raw,
            object_pairs_hook=_unique_object,
            parse_constant=lambda _value: _fail("recorder_config_check_result_invalid"),
        )
    except (TransitionError, UnicodeError, json.JSONDecodeError):
        _fail("recorder_config_check_result_invalid")
    return _validate_config_check_result(value)


def build_recorder_config_evidence(
    plan: dict[str, Any],
    compose_value: Any,
    result_value: Any,
    image_id: str,
    oci_revision: str,
    exit_code: int,
) -> dict[str, Any]:
    image_reference, environment = _recorder_render_environment(plan, compose_value)
    result = dict(_validate_config_check_result(result_value))
    if (
        DIGEST_RE.fullmatch(image_id) is None
        or oci_revision != plan["release_sha"]
        or isinstance(exit_code, bool)
        or not isinstance(exit_code, int)
        or not 0 <= exit_code <= 255
    ):
        _fail("recorder_config_image_identity_invalid")
    success = exit_code == 0 and result["status"] == "ok"
    if (exit_code == 0) != (result["status"] == "ok"):
        result = {
            "schema": RECORDER_CONFIG_CHECK_SCHEMA,
            "status": "error",
            "error_code": "config_check_process_mismatch",
            "environment_name": None,
        }
        success = False

    expected = []
    for name in RECORDER_CONFIG_ENVIRONMENT_NAMES:
        item: dict[str, Any] = {"name": name, "present": name in environment}
        if name in environment and name in RECORDER_CONFIG_SAFE_LENGTH_NAMES:
            item["length_bound"] = _length_bound(environment[name])
        expected.append(item)
    structured = {
        name: _structured_fingerprint(name, environment[name])
        for name in RECORDER_STRUCTURED_ENVIRONMENT_NAMES
        if name in environment
    }
    result["exit_code"] = exit_code
    return {
        "schema": RECORDER_CONFIG_EVIDENCE_SCHEMA,
        "release_sha": plan["release_sha"],
        "image": {
            "reference": image_reference,
            "local_image_id": image_id,
            "oci_revision": oci_revision,
        },
        "environment": {
            "expected": expected,
            "duplicate_name_detection": {"detected": False, "names": []},
            "unexpected_name_count": len(
                set(environment).difference(RECORDER_CONFIG_ENVIRONMENT_NAMES)
            ),
            "structured_configuration": structured,
        },
        "config_check": result,
        "status": "ok" if success else "error",
    }


def validate_recorder_handoff(
    jetstream_value: Any, container_status: str, container_id: str
) -> dict[str, Any]:
    if CONTAINER_ID_RE.fullmatch(container_id) is None:
        _fail("recorder_container_identity_invalid")
    if container_status != "exited":
        _fail("recorder_container_not_stopped")
    try:
        normalized = maintenance.normalize_jetstream(jetstream_value)
        _streams, consumers = maintenance._jetstream_resources(jetstream_value)
    except maintenance.MaintenanceError as error:
        _fail(str(error))
    recorder_consumers: list[dict[str, Any]] = []
    for consumer in consumers:
        if not isinstance(consumer, dict) or not isinstance(
            consumer.get("config"), dict
        ):
            _fail("jetstream_invalid")
        config = consumer["config"]
        name = (
            consumer.get("name")
            or config.get("durable_name")
            or config.get("name")
        )
        if name == "PHOENIX_RECORDER":
            recorder_consumers.append(consumer)
    if len(recorder_consumers) != 1:
        _fail("recorder_consumer_state_invalid")
    recorder = recorder_consumers[0]
    if "num_waiting" not in recorder:
        _fail("recorder_waiting_state_unavailable")
    waiting = _nonnegative_integer(
        recorder["num_waiting"], "recorder_waiting_state_invalid"
    )
    ack_pending = normalized["consumers"]["PHOENIX_RECORDER"]["ack_pending"]
    if ack_pending != 0:
        _fail("recorder_ack_pending_not_zero")
    if waiting != 0:
        _fail("recorder_pull_subscription_attached")
    return {
        "schema": HANDOFF_SCHEMA,
        "status": "detached",
        "container_status": container_status,
        "container_id": container_id,
        "consumer": "PHOENIX_RECORDER",
        "num_ack_pending": ack_pending,
        "num_waiting": waiting,
    }


def _read_bounded_tail(path: Path, maximum: int) -> tuple[bytes, bool]:
    if not path.is_file() or path.is_symlink():
        _fail("diagnostic_log_unavailable")
    try:
        size = path.stat().st_size
        with path.open("rb") as handle:
            truncated = size > maximum
            if truncated:
                handle.seek(-maximum, os.SEEK_END)
            return handle.read(maximum), truncated
    except OSError:
        _fail("diagnostic_log_unavailable")


def redact_diagnostic_log(
    input_path: Path, env_path: Path, output_path: Path
) -> None:
    raw, input_truncated = _read_bounded_tail(
        input_path, MAX_DIAGNOSTIC_LOG_INPUT_BYTES
    )
    _lines, environment = _parse_env(env_path)
    text = raw.decode("utf-8", errors="replace")
    sensitive_values = sorted(
        {
            value
            for name, value in environment.items()
            if SENSITIVE_ENV_NAME_RE.search(name) and len(value) >= 4
        },
        key=len,
        reverse=True,
    )
    for value in sensitive_values:
        text = text.replace(value, "[redacted-env]")

    lines: list[str] = []
    for raw_line in text.splitlines()[-MAX_DIAGNOSTIC_LOG_LINES:]:
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
            line = PRIVATE_VALUE_RE.sub("[redacted-private-value]", line)
            line = ADDRESS_RE.sub("[redacted-address]", line)
        encoded = line.encode("utf-8")
        if len(encoded) > MAX_DIAGNOSTIC_LOG_LINE_BYTES:
            line = (
                encoded[: MAX_DIAGNOSTIC_LOG_LINE_BYTES - 20]
                .decode("utf-8", errors="ignore")
                + "[line-truncated]"
            )
        lines.append(line)
    if input_truncated:
        lines.insert(0, "[input-truncated]")
    payload = ("\n".join(lines) + ("\n" if lines else "")).encode("utf-8")
    if len(payload) > MAX_DIAGNOSTIC_LOG_BYTES:
        _fail("diagnostic_log_bounds_invalid")
    write_bytes_atomic(output_path, payload)


def _false(value: str | None) -> bool:
    return value == "false"


def validate_environment(
    plan: dict[str, Any], values: dict[str, str], role: str
) -> dict[str, Any]:
    if role not in {"release", "rollback"}:
        _fail("operator_environment_invalid")
    if (
        values.get("PHOENIX_MODE") != "SHADOW"
        or not _false(values.get("LIVE_EXECUTION"))
        or values.get("CHAIN_ID") != "42161"
        or values.get("POSTGRES_DB") != DATABASE_NAME
        or not values.get("POSTGRES_DSN")
        or values.get("RECORDER_PERSISTENCE_POLICY") != "money_path_v1"
        or not _false(values.get("LIVE_EXECUTOR_ARMED"))
        or values.get("LIVE_EXECUTOR_KILL_SWITCH") != "true"
    ):
        _fail("shadow_safety_contract_invalid")
    blank_names = (
        "SIGNER_PRIVATE_KEY",
        "WALLET_ADDRESS",
        "EXECUTOR_ADDRESS",
        "PUBLIC_TRANSACTION_SUBMISSION",
        "PRIVATE_RELAY_SUBMISSION",
        "TRANSACTION_BROADCAST_URL",
        "PUBLIC_BROADCAST",
        "TRANSACTION_BROADCAST",
    )
    if any(values.get(name, "") != "" for name in blank_names):
        _fail("live_configuration_present")
    route_raw = values.get("ENGINE_ROUTE_REGISTRY_JSON", "")
    try:
        _routes, route_hash = production_context.validate_route_registry(route_raw)
    except production_context.ContextError:
        _fail("operator_route_registry_invalid")
    expected_hash = plan["route_contract"][f"{role}_registry_sha256"]
    if route_hash != expected_hash:
        _fail("operator_route_registry_mismatch")
    return {
        "schema": ENV_SUMMARY_SCHEMA,
        "role": role,
        "mode": "SHADOW",
        "live_execution": False,
        "chain_id": 42161,
        "database": DATABASE_NAME,
        "postgres_dsn_configured": True,
        "recorder_persistence_policy": "money_path_v1",
        "route_registry_sha256": route_hash,
        "signer_configured": False,
        "wallet_configured": False,
        "executor_configured": False,
        "submission_configured": False,
        "broadcast_configured": False,
        "live_executor_armed": False,
        "live_executor_kill_switch": True,
    }


def install_candidate_route_env(
    plan: dict[str, Any], source: Path, output: Path
) -> dict[str, Any]:
    lines, before = _parse_env(source)
    validate_environment(plan, before, "rollback")
    route_json = _canonical_json(plan["route_contract"]["release_registry"]).decode(
        "ascii"
    )
    replacement = f"ENGINE_ROUTE_REGISTRY_JSON='{route_json}'"
    found = False
    output_lines: list[str] = []
    for raw_line in lines:
        stripped = raw_line.strip()
        if stripped and not stripped.startswith("#") and "=" in stripped:
            name = stripped.split("=", 1)[0].strip()
            if name == "ENGINE_ROUTE_REGISTRY_JSON":
                if found:
                    _fail("operator_environment_invalid")
                output_lines.append(replacement)
                found = True
                continue
        output_lines.append(raw_line)
    if not found:
        _fail("operator_route_registry_missing")
    content = "\n".join(output_lines) + "\n"
    descriptor, temporary = tempfile.mkstemp(prefix=f".{output.name}.", dir=output.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, 0o600)
        os.replace(temporary, output)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
    _after_lines, after = _parse_env(output)
    summary = validate_environment(plan, after, "release")
    before_without_route = dict(before)
    after_without_route = dict(after)
    before_without_route.pop("ENGINE_ROUTE_REGISTRY_JSON", None)
    after_without_route.pop("ENGINE_ROUTE_REGISTRY_JSON", None)
    if before_without_route != after_without_route:
        _fail("operator_environment_changed")
    return summary


def _plan_for_compose(plan: dict[str, Any]) -> dict[str, Any]:
    return {
        "release_sha": plan["release_sha"],
        "rollback_sha": plan["rollback_sha"],
        "images": plan["images"],
    }


def _absolute_path(value: Any) -> bool:
    return isinstance(value, str) and bool(value) and (
        value.startswith("/")
        or os.path.isabs(value)
        or re.fullmatch(r"[A-Za-z]:[\\/].+", value) is not None
    )


def _normalize_render_environment(
    plan: dict[str, Any], rendered: dict[str, Any], role: str
) -> None:
    services = rendered["services"]
    route_services = {
        service
        for service, contract in services.items()
        if isinstance(contract.get("environment"), dict)
        and ROUTE_ENV_NAME in contract["environment"]
    }
    if route_services != set(ROUTE_ENV_SERVICES):
        _fail(f"route_render_contract_invalid:{role}:service_set")

    expected_hash = plan["route_contract"][f"{role}_registry_sha256"]
    for service in ROUTE_ENV_SERVICES:
        environment = services[service]["environment"]
        raw_registry = environment[ROUTE_ENV_NAME]
        if not isinstance(raw_registry, str):
            _fail(f"route_render_contract_invalid:{role}:{service}")
        try:
            _routes, route_hash = production_context.validate_route_registry(
                raw_registry
            )
        except production_context.ContextError:
            _fail(f"route_render_contract_invalid:{role}:{service}")
        if route_hash != expected_hash:
            _fail(f"route_render_contract_invalid:{role}:{service}")
        environment[ROUTE_ENV_NAME] = "<reviewed-route-registry>"

    common_environment = rendered["extensions"].get("x-common-env")
    if not isinstance(common_environment, dict):
        _fail("protected_compose_extensions_changed")
    env_files = common_environment.get("env_file")
    if (
        not isinstance(env_files, list)
        or len(env_files) != 1
        or not _absolute_path(env_files[0])
    ):
        _fail("protected_compose_extensions_changed")
    common_environment["env_file"] = ["<operator-environment>"]


def _normalize_reviewed_engine_default(
    release_services: dict[str, Any], rollback_services: dict[str, Any]
) -> None:
    release_environment = release_services["phoenix-engine"].get("environment")
    rollback_environment = rollback_services["phoenix-engine"].get("environment")
    if not isinstance(release_environment, dict) or not isinstance(
        rollback_environment, dict
    ):
        _fail("protected_compose_service_changed:phoenix-engine")
    if release_environment.get(ENGINE_CONCURRENCY_ENV_NAME) == rollback_environment.get(
        ENGINE_CONCURRENCY_ENV_NAME
    ):
        return
    if (
        release_environment.get(ENGINE_CONCURRENCY_ENV_NAME)
        != REVIEWED_ENGINE_CONCURRENCY_DEFAULT
        or ENGINE_CONCURRENCY_ENV_NAME in rollback_environment
    ):
        _fail("protected_compose_service_changed:phoenix-engine")
    release_environment.pop(ENGINE_CONCURRENCY_ENV_NAME)


def validate_render_pair(
    plan: dict[str, Any],
    release_metadata: Any,
    rollback_metadata: Any,
    release_compose: Any,
    rollback_compose: Any,
) -> None:
    compose_plan = _plan_for_compose(plan)
    metadata = {
        "release": maintenance._validate_render_metadata(
            compose_plan, "release", release_metadata
        ),
        "rollback": maintenance._validate_render_metadata(
            compose_plan, "rollback", rollback_metadata
        ),
    }
    if (
        metadata["release"]["route_registry_hash"] != CANDIDATE_ROUTE_HASH
        or metadata["rollback"]["route_registry_hash"] != ROLLBACK_ROUTE_HASH
    ):
        _fail("route_render_contract_invalid")
    rendered = {
        "release": deepcopy(maintenance._validate_rendered_compose(release_compose)),
        "rollback": deepcopy(maintenance._validate_rendered_compose(rollback_compose)),
    }
    release_services = rendered["release"]["services"]
    rollback_services = rendered["rollback"]["services"]
    expected_release = maintenance._expected_compose_images(compose_plan, "release")
    expected_rollback = maintenance._expected_compose_images(compose_plan, "rollback")
    for service in maintenance.COMPOSE_SERVICES:
        if (
            release_services[service]["image"] != expected_release[service]
            or rollback_services[service]["image"] != expected_rollback[service]
        ):
            _fail(f"protected_compose_service_changed:{service}")
    _normalize_render_environment(plan, rendered["release"], "release")
    _normalize_render_environment(plan, rendered["rollback"], "rollback")
    _normalize_reviewed_engine_default(release_services, rollback_services)
    for service in FIXED_SERVICES:
        if release_services[service] != rollback_services[service]:
            _fail(f"protected_compose_service_changed:{service}")
    for service in (*maintenance.MUTABLE_SERVICES, maintenance.MIGRATION_SERVICE):
        if maintenance._without_image(
            release_services[service]
        ) != maintenance._without_image(rollback_services[service]):
            _fail(f"protected_compose_service_changed:{service}")
    for service in maintenance.OPTIONAL_SERVICES:
        if service == "prometheus":
            maintenance._validate_prometheus_delta(
                release_services[service], rollback_services[service]
            )
        elif maintenance._without_image(
            release_services[service]
        ) != maintenance._without_image(rollback_services[service]):
            _fail(f"protected_compose_service_changed:{service}")
    for contract in ("networks", "volumes", "extensions"):
        if rendered["release"][contract] != rendered["rollback"][contract]:
            _fail(f"protected_compose_{contract}_changed")


def _expected_migrations(plan: dict[str, Any]) -> list[dict[str, str]]:
    return [
        {
            "version": Path(path).stem,
            "checksum": plan["migrations"]["sha256"][path].removeprefix("sha256:"),
        }
        for path in maintenance.EXPECTED_MIGRATIONS
    ]


def validate_database(plan: dict[str, Any], value: Any) -> dict[str, Any]:
    try:
        database = maintenance.normalize_database(value)
    except maintenance.MaintenanceError as error:
        _fail(str(error))
    if database["migrations"] != _expected_migrations(plan):
        _fail("migration_state_mismatch")
    counts = database["counts"]
    if any(counts[name] != 0 for name in maintenance.EXECUTION_COUNTS):
        _fail("execution_activity_detected")
    if counts["duplicate_origins"] != 0 or counts["duplicate_feed_events"] != 0:
        _fail("database_integrity_failed")
    return database


def _compatibility_plan(plan: dict[str, Any]) -> dict[str, Any]:
    return {
        "release_sha": plan["release_sha"],
        "rollback_sha": plan["rollback_sha"],
        "images": plan["images"],
        "contract_sha256": plan["migrations"]["sha256"],
    }


def _assert_transition_progress(
    baseline: dict[str, Any],
    progress: dict[str, Any],
    current: dict[str, Any],
    allow_quiet: bool,
) -> None:
    feed_start = progress["metrics"]["feed"]
    recorder_start = progress["metrics"]["recorder"]
    feed = current["metrics"]["feed"]
    recorder = current["metrics"]["recorder"]
    if any(value is None for value in feed.values()):
        _fail("feed_metrics_unavailable")
    if feed["feed_readiness"] != 1:
        _fail("feed_readiness_not_ready")
    if recorder["recorder_readiness"] != 1:
        _fail("recorder_readiness_not_ready")
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

    for name in ("feed_last_sequence", "feed_jetstream_publish_success_total"):
        if feed[name] < feed_start[name]:
            _fail(f"feed_metric_regressed:{name}")
    for name in (
        "recorder_messages_persisted_total",
        "recorder_last_persisted_feed_sequence",
    ):
        if recorder[name] < recorder_start[name]:
            _fail(f"recorder_metric_regressed:{name}")

    database = current["database"]
    for name, value in baseline["database"]["counts"].items():
        if progress["database"]["counts"][name] < value:
            _fail(f"database_count_regressed:{name}")
    if (
        progress["database"]["max_feed_sequence"]
        < baseline["database"]["max_feed_sequence"]
    ):
        _fail("database_feed_sequence_regressed")
    for reference in (baseline["database"], progress["database"]):
        for name, value in reference["counts"].items():
            if database["counts"][name] < value:
                _fail(f"database_count_regressed:{name}")
        if database["max_feed_sequence"] < reference["max_feed_sequence"]:
            _fail("database_feed_sequence_regressed")

    stream_start = progress["jetstream"]["streams"]["PHOENIX_FEED_TX"]
    stream = current["jetstream"]["streams"]["PHOENIX_FEED_TX"]
    if stream["last_seq"] < stream_start["last_seq"]:
        _fail("feed_stream_regressed")
    consumer_start = progress["jetstream"]["consumers"]["PHOENIX_RECORDER"]
    consumer = current["jetstream"]["consumers"]["PHOENIX_RECORDER"]
    if (
        consumer["delivered_stream_seq"] < consumer_start["delivered_stream_seq"]
        or consumer["ack_floor_stream_seq"]
        < consumer_start["ack_floor_stream_seq"]
    ):
        _fail("recorder_consumer_regressed")
    if (
        consumer["pending"] > maintenance.MAX_RECORDER_PENDING
        or consumer["ack_pending"] > maintenance.MAX_ACK_PENDING
        or consumer_start["pending"] > maintenance.MAX_RECORDER_PENDING
        or consumer_start["ack_pending"] > maintenance.MAX_ACK_PENDING
    ):
        _fail("consumer_backlog_unbounded")
    baseline_consumer = baseline["jetstream"]["consumers"]["PHOENIX_RECORDER"]
    if (
        consumer_start["redelivered"] > baseline_consumer["redelivered"]
        or consumer["redelivered"] > consumer_start["redelivered"]
    ):
        _fail("consumer_redelivery_increased")

    recorder_progress = (
        recorder["recorder_messages_persisted_total"]
        > recorder_start["recorder_messages_persisted_total"]
        or recorder["recorder_last_persisted_feed_sequence"]
        > recorder_start["recorder_last_persisted_feed_sequence"]
    )
    if recorder_progress:
        return

    feed_or_stream_progress = (
        feed["feed_last_sequence"] > feed_start["feed_last_sequence"]
        or feed["feed_jetstream_publish_success_total"]
        > feed_start["feed_jetstream_publish_success_total"]
        or stream["last_seq"] > stream_start["last_seq"]
    )
    delivered_progress = (
        consumer["delivered_stream_seq"]
        > consumer_start["delivered_stream_seq"]
    )
    ack_progress = (
        consumer["ack_floor_stream_seq"]
        > consumer_start["ack_floor_stream_seq"]
    )
    if feed_or_stream_progress:
        if not delivered_progress or not ack_progress:
            _fail("feed_progress_without_consumer_progress")
        return
    if delivered_progress or ack_progress:
        _fail("consumer_progress_without_feed_activity")
    if not allow_quiet:
        _fail("quiet_interval_not_elapsed")
    if consumer["pending"] != 0 or consumer["ack_pending"] != 0:
        _fail("quiet_interval_consumer_backlog")
    if database != progress["database"]:
        _fail("quiet_interval_database_changed")


def validate_data_transition(
    plan: dict[str, Any],
    baseline_value: Any,
    progress_value: Any,
    current_value: Any,
    role: str,
    allow_quiet: bool = False,
) -> None:
    if role not in {"release", "rollback"}:
        _fail("transition_invalid")
    try:
        baseline = maintenance.validate_snapshot(baseline_value)
        progress = maintenance.validate_snapshot(progress_value)
        current = maintenance.validate_snapshot(current_value)
        compatibility = _compatibility_plan(plan)
        if baseline["release_sha"] != ROLLBACK_SHA:
            _fail("baseline_release_mismatch")
        maintenance._assert_baseline_images(compatibility, baseline)
        maintenance._assert_continuity(baseline, progress)
        maintenance._assert_continuity(baseline, current)
        expected_sha = plan[f"{role}_sha"]
        if (
            current["release_sha"] != expected_sha
            or progress["release_sha"] != expected_sha
        ):
            _fail("transition_invalid")
        for service in maintenance.MUTABLE_SERVICES:
            after = current["services"][service]
            progress_service = progress["services"][service]
            if (
                after["configured_image"] != plan["images"][role][service]
                or after["restart_count"] != 0
                or after["oom_killed"]
                or not maintenance._service_healthy(after)
                or progress_service["configured_image"]
                != plan["images"][role][service]
                or progress_service["restart_count"] != 0
                or progress_service["oom_killed"]
                or not maintenance._service_healthy(progress_service)
            ):
                _fail("mutable_service_transition_invalid")
            if (
                role == "release"
                and after["container_id"] == baseline["services"][service]["container_id"]
            ):
                _fail("mutable_service_transition_invalid")
        _assert_transition_progress(baseline, progress, current, allow_quiet)
    except maintenance.MaintenanceError as error:
        _fail(str(error))


def validate_baseline(plan: dict[str, Any], value: Any) -> None:
    try:
        snapshot = maintenance.validate_snapshot(value)
        if snapshot["release_sha"] != ROLLBACK_SHA:
            _fail("baseline_release_mismatch")
        maintenance._assert_baseline_images(_compatibility_plan(plan), snapshot)
        maintenance._assert_no_execution(snapshot)
    except maintenance.MaintenanceError as error:
        _fail(str(error))
    validate_database(plan, snapshot["database"])
    if snapshot["disk_free_bytes"] < maintenance.MIN_DISK_FREE_BYTES:
        _fail("disk_headroom_insufficient")


def build_runtime_state(
    plan: dict[str, Any], phase: str, release_sha: str, service_inputs: list[str]
) -> dict[str, Any]:
    if phase not in {"candidate", "rollback"}:
        _fail("runtime_state_invalid")
    expected_sha = RELEASE_SHA if phase == "candidate" else ROLLBACK_SHA
    if release_sha != expected_sha:
        _fail("runtime_state_invalid")
    paths: dict[str, Path] = {}
    for item in service_inputs:
        if "=" not in item:
            _fail("runtime_state_invalid")
        name, raw_path = item.split("=", 1)
        if name in paths or name not in RUNTIME_SERVICES:
            _fail("runtime_state_invalid")
        paths[name] = Path(raw_path)
    if set(paths) != set(RUNTIME_SERVICES):
        _fail("runtime_state_invalid")
    try:
        services = {
            name: maintenance.normalize_service_inspect(paths[name])
            for name in RUNTIME_SERVICES
        }
    except maintenance.MaintenanceError as error:
        _fail(str(error))
    return {
        "schema_version": RUNTIME_SCHEMA,
        "phase": phase,
        "release_sha": release_sha,
        "services": services,
        "route_registry_sha256": plan["route_contract"][
            f"{'release' if phase == 'candidate' else 'rollback'}_registry_sha256"
        ],
        "live_executor_running": False,
        "migration_runner_running": False,
    }


def validate_runtime_state(value: Any) -> dict[str, Any]:
    runtime = _exact(
        value,
        {
            "schema_version",
            "phase",
            "release_sha",
            "services",
            "route_registry_sha256",
            "live_executor_running",
            "migration_runner_running",
        },
        "runtime_state_invalid",
    )
    if (
        runtime["schema_version"] != RUNTIME_SCHEMA
        or runtime["phase"] not in {"candidate", "rollback"}
        or runtime["release_sha"]
        != (RELEASE_SHA if runtime["phase"] == "candidate" else ROLLBACK_SHA)
        or runtime["live_executor_running"] is not False
        or runtime["migration_runner_running"] is not False
        or not isinstance(runtime["services"], dict)
        or set(runtime["services"]) != set(RUNTIME_SERVICES)
    ):
        _fail("runtime_state_invalid")
    expected_route = (
        CANDIDATE_ROUTE_HASH
        if runtime["phase"] == "candidate"
        else ROLLBACK_ROUTE_HASH
    )
    if runtime["route_registry_sha256"] != expected_route:
        _fail("runtime_state_invalid")
    for service in RUNTIME_SERVICES:
        state = runtime["services"][service]
        if not isinstance(state, dict) or set(state) != {
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
            _fail("runtime_state_invalid")
    return runtime


def validate_runtime_transition(
    plan: dict[str, Any], baseline_value: Any, runtime_value: Any, role: str
) -> None:
    if role not in {"release", "rollback"}:
        _fail("runtime_state_invalid")
    try:
        baseline = maintenance.validate_snapshot(baseline_value)
    except maintenance.MaintenanceError as error:
        _fail(str(error))
    runtime = validate_runtime_state(runtime_value)
    expected_phase = "candidate" if role == "release" else "rollback"
    if runtime["phase"] != expected_phase:
        _fail("runtime_state_invalid")
    services = runtime["services"]
    for service in FIXED_SERVICES:
        before = baseline["services"][service]
        after = services[service]
        if (
            after["container_id"] != before["container_id"]
            or after["created_at"] != before["created_at"]
            or after["started_at"] != before["started_at"]
            or after["restart_count"] != before["restart_count"]
            or after["configured_image"] != maintenance.FIXED_IMAGES[service]
            or after["local_image_id"] != before["local_image_id"]
            or after["mounts"] != before["mounts"]
            or after["networks"] != before["networks"]
            or after["oom_killed"]
            or not maintenance._service_healthy(after)
        ):
            _fail("fixed_service_identity_changed")
    for service in START_ORDER:
        state = services[service]
        if service == "prometheus":
            expected_image = maintenance.PROMETHEUS_IMAGE
        else:
            expected_image = plan["images"][role][OWNED_RUNTIME_IMAGES[service]]
        if (
            state["configured_image"] != expected_image
            or state["restart_count"] != 0
            or state["oom_killed"]
            or not maintenance._service_healthy(state)
        ):
            _fail(f"runtime_service_invalid:{service}")


def validate_release_state(plan: dict[str, Any], value: Any, role: str) -> None:
    if role not in {"release", "rollback"}:
        _fail("release_state_invalid")
    state = _exact(
        value,
        {
            "compose_config_sha256",
            "images",
            "manifest_sha256",
            "release_env_sha256",
            "release_sha",
            "route_registry_hash",
            "schema",
        },
        "release_state_invalid",
    )
    if (
        state["schema"] != "phoenix.release-state.v1"
        or state["release_sha"] != plan[f"{role}_sha"]
        or state["route_registry_hash"]
        != plan["route_contract"][f"{role}_registry_sha256"]
    ):
        _fail("release_state_invalid")
    for key in ("compose_config_sha256", "manifest_sha256", "release_env_sha256"):
        _digest(state[key], "release_state_invalid")
    expected = maintenance._expected_compose_images(_plan_for_compose(plan), role)
    if state["images"] != expected:
        _fail("release_state_invalid")


def command_plan(args: argparse.Namespace) -> None:
    value = build_plan(args)
    write_atomic(Path(args.output), value)
    print(
        "PHOENIX_SHADOW_CONTRACT_TRANSITION_PLAN_OK: "
        f"release={RELEASE_SHA} rollback={ROLLBACK_SHA}"
    )


def command_validate_plan(args: argparse.Namespace) -> None:
    load_plan(Path(args.plan))
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_PLAN_VALID")


def command_validate_env(args: argparse.Namespace) -> None:
    plan = load_plan(Path(args.plan))
    _lines, values = _parse_env(Path(args.env_file))
    summary = validate_environment(plan, values, args.role)
    if args.output:
        write_atomic(Path(args.output), summary)
    print(f"PHOENIX_SHADOW_CONTRACT_TRANSITION_ENV_OK: role={args.role}")


def command_install_route_env(args: argparse.Namespace) -> None:
    summary = install_candidate_route_env(
        load_plan(Path(args.plan)), Path(args.source), Path(args.output)
    )
    if args.summary_output:
        write_atomic(Path(args.summary_output), summary)
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_ROUTE_ENV_OK")


def command_render_pair(args: argparse.Namespace) -> None:
    validate_render_pair(
        load_plan(Path(args.plan)),
        load_json(Path(args.release_metadata)),
        load_json(Path(args.rollback_metadata)),
        load_json(Path(args.release_compose)),
        load_json(Path(args.rollback_compose)),
    )
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_RENDER_PAIR_OK")


def command_validate_database(args: argparse.Namespace) -> None:
    validate_database(load_plan(Path(args.plan)), load_json(Path(args.database)))
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_DATABASE_OK")


def command_validate_baseline(args: argparse.Namespace) -> None:
    validate_baseline(load_plan(Path(args.plan)), load_json(Path(args.snapshot)))
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_BASELINE_OK")


def command_validate_data_transition(args: argparse.Namespace) -> None:
    validate_data_transition(
        load_plan(Path(args.plan)),
        load_json(Path(args.baseline)),
        load_json(Path(args.progress_baseline)),
        load_json(Path(args.current)),
        args.role,
        args.allow_quiet,
    )
    print(f"PHOENIX_SHADOW_CONTRACT_TRANSITION_PROGRESS_OK: role={args.role}")


def command_validate_recorder_handoff(args: argparse.Namespace) -> None:
    value = validate_recorder_handoff(
        load_json(Path(args.jetstream)), args.container_status, args.container_id
    )
    if args.output:
        write_atomic(Path(args.output), value)
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_RECORDER_HANDOFF_OK")


def command_prepare_recorder_config_check(args: argparse.Namespace) -> None:
    prepare_recorder_config_check(
        load_plan(Path(args.plan)),
        load_json(Path(args.compose_config)),
        Path(args.env_output),
    )
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_RECORDER_CONFIG_ENV_OK")


def command_complete_recorder_config_check(args: argparse.Namespace) -> None:
    try:
        result = load_config_check_result(Path(args.result))
    except TransitionError:
        result = {
            "schema": RECORDER_CONFIG_CHECK_SCHEMA,
            "status": "error",
            "error_code": "config_check_result_invalid",
            "environment_name": None,
        }
    evidence = build_recorder_config_evidence(
        load_plan(Path(args.plan)),
        load_json(Path(args.compose_config)),
        result,
        args.image_id,
        args.oci_revision,
        args.exit_code,
    )
    write_atomic(Path(args.output), evidence)
    if evidence["status"] != "ok":
        result = evidence["config_check"]
        suffix = (
            f":{result['environment_name']}"
            if result["environment_name"] is not None
            else ""
        )
        _fail(f"recorder_config_check_failed:{result['error_code']}{suffix}")
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_RECORDER_CONFIG_OK")


def command_redact_diagnostic_log(args: argparse.Namespace) -> None:
    redact_diagnostic_log(
        Path(args.input), Path(args.env_file), Path(args.output)
    )
    print("PHOENIX_SHADOW_CONTRACT_TRANSITION_DIAGNOSTIC_LOG_OK")


def command_runtime_state(args: argparse.Namespace) -> None:
    value = build_runtime_state(
        load_plan(Path(args.plan)), args.phase, args.release_sha, args.service
    )
    write_atomic(Path(args.output), value)
    print(f"PHOENIX_SHADOW_CONTRACT_TRANSITION_RUNTIME_OK: phase={args.phase}")


def command_validate_runtime(args: argparse.Namespace) -> None:
    validate_runtime_transition(
        load_plan(Path(args.plan)),
        load_json(Path(args.baseline)),
        load_json(Path(args.runtime)),
        args.role,
    )
    print(f"PHOENIX_SHADOW_CONTRACT_TRANSITION_RUNTIME_VALID: role={args.role}")


def command_validate_state(args: argparse.Namespace) -> None:
    validate_release_state(
        load_plan(Path(args.plan)), load_json(Path(args.state)), args.role
    )
    print(f"PHOENIX_SHADOW_CONTRACT_TRANSITION_STATE_OK: role={args.role}")


def command_image_refs(args: argparse.Namespace) -> None:
    plan = load_plan(Path(args.plan))
    for role in ("release", "rollback"):
        for name in LEGACY_IMAGES:
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


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    plan = commands.add_parser("plan")
    for name in (
        "release-manifest",
        "release-archive",
        "release-assets-manifest",
        "release-checksums",
        "release-provenance",
        "release-run-evidence",
        "rollback-manifest",
        "rollback-archive",
        "rollback-assets-manifest",
        "rollback-checksums",
        "rollback-provenance",
        "rollback-run-evidence",
        "candidate-route-registry",
        "output",
    ):
        plan.add_argument(f"--{name}", required=True)
    plan.set_defaults(handler=command_plan)

    validate = commands.add_parser("validate-plan")
    validate.add_argument("--plan", required=True)
    validate.set_defaults(handler=command_validate_plan)

    environment = commands.add_parser("validate-env")
    environment.add_argument("--plan", required=True)
    environment.add_argument("--env-file", required=True)
    environment.add_argument("--role", choices=("release", "rollback"), required=True)
    environment.add_argument("--output")
    environment.set_defaults(handler=command_validate_env)

    install_env = commands.add_parser("install-route-env")
    install_env.add_argument("--plan", required=True)
    install_env.add_argument("--source", required=True)
    install_env.add_argument("--output", required=True)
    install_env.add_argument("--summary-output")
    install_env.set_defaults(handler=command_install_route_env)

    render = commands.add_parser("validate-render-pair")
    render.add_argument("--plan", required=True)
    render.add_argument("--release-metadata", required=True)
    render.add_argument("--rollback-metadata", required=True)
    render.add_argument("--release-compose", required=True)
    render.add_argument("--rollback-compose", required=True)
    render.set_defaults(handler=command_render_pair)

    database = commands.add_parser("validate-database")
    database.add_argument("--plan", required=True)
    database.add_argument("--database", required=True)
    database.set_defaults(handler=command_validate_database)

    baseline = commands.add_parser("validate-baseline")
    baseline.add_argument("--plan", required=True)
    baseline.add_argument("--snapshot", required=True)
    baseline.set_defaults(handler=command_validate_baseline)

    transition = commands.add_parser("validate-data-transition")
    transition.add_argument("--plan", required=True)
    transition.add_argument("--baseline", required=True)
    transition.add_argument("--progress-baseline", required=True)
    transition.add_argument("--current", required=True)
    transition.add_argument("--role", choices=("release", "rollback"), required=True)
    transition.add_argument("--allow-quiet", action="store_true")
    transition.set_defaults(handler=command_validate_data_transition)

    handoff = commands.add_parser("validate-recorder-handoff")
    handoff.add_argument("--jetstream", required=True)
    handoff.add_argument("--container-status", required=True)
    handoff.add_argument("--container-id", required=True)
    handoff.add_argument("--output")
    handoff.set_defaults(handler=command_validate_recorder_handoff)

    prepare_config = commands.add_parser("prepare-recorder-config-check")
    prepare_config.add_argument("--plan", required=True)
    prepare_config.add_argument("--compose-config", required=True)
    prepare_config.add_argument("--env-output", required=True)
    prepare_config.set_defaults(handler=command_prepare_recorder_config_check)

    complete_config = commands.add_parser("complete-recorder-config-check")
    complete_config.add_argument("--plan", required=True)
    complete_config.add_argument("--compose-config", required=True)
    complete_config.add_argument("--result", required=True)
    complete_config.add_argument("--image-id", required=True)
    complete_config.add_argument("--oci-revision", required=True)
    complete_config.add_argument("--exit-code", required=True, type=int)
    complete_config.add_argument("--output", required=True)
    complete_config.set_defaults(handler=command_complete_recorder_config_check)

    diagnostic_log = commands.add_parser("redact-diagnostic-log")
    diagnostic_log.add_argument("--input", required=True)
    diagnostic_log.add_argument("--env-file", required=True)
    diagnostic_log.add_argument("--output", required=True)
    diagnostic_log.set_defaults(handler=command_redact_diagnostic_log)

    runtime = commands.add_parser("runtime-state")
    runtime.add_argument("--plan", required=True)
    runtime.add_argument("--phase", choices=("candidate", "rollback"), required=True)
    runtime.add_argument("--release-sha", required=True)
    runtime.add_argument("--service", action="append", required=True)
    runtime.add_argument("--output", required=True)
    runtime.set_defaults(handler=command_runtime_state)

    validate_runtime = commands.add_parser("validate-runtime")
    validate_runtime.add_argument("--plan", required=True)
    validate_runtime.add_argument("--baseline", required=True)
    validate_runtime.add_argument("--runtime", required=True)
    validate_runtime.add_argument("--role", choices=("release", "rollback"), required=True)
    validate_runtime.set_defaults(handler=command_validate_runtime)

    state = commands.add_parser("validate-state")
    state.add_argument("--plan", required=True)
    state.add_argument("--state", required=True)
    state.add_argument("--role", choices=("release", "rollback"), required=True)
    state.set_defaults(handler=command_validate_state)

    refs = commands.add_parser("image-refs")
    refs.add_argument("--plan", required=True)
    refs.set_defaults(handler=command_image_refs)
    return root


def main() -> None:
    args = parser().parse_args()
    try:
        args.handler(args)
    except (
        TransitionError,
        maintenance.MaintenanceError,
        release_assets.ReleaseAssetError,
        production_context.ContextError,
        shadow_route_discovery.DiscoveryError,
    ) as error:
        code = getattr(error, "code", str(error))
        print(
            json.dumps(
                {"code": code, "status": "error"},
                sort_keys=True,
                separators=(",", ":"),
            ),
            file=sys.stderr,
        )
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
