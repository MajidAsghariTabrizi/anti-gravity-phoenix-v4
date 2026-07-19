#!/usr/bin/env python3
"""Validate and materialize the Phoenix v5 clean-database release contract."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any, Mapping

try:
    from scripts import release_provenance
except (ImportError, ModuleNotFoundError):  # Direct execution from scripts/.
    import release_provenance  # type: ignore[no-redef]


SCHEMA = "phoenix.prelive-v5-release.v1"
PLAN_SCHEMA = "phoenix.prelive-v5-cutover-plan.v1"
CANDIDATE_TAG = "phoenix-prelive-shadow-v5"
FALLBACK_TAG = "phoenix-prelive-shadow-v4"
FALLBACK_SHA = "a7f19ab165d93dafb4bcc20463f9d010f587281a"
FALLBACK_BUILD_RUN_ID = "29638026962"
PLACEHOLDER = "UNSET"
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
RUN_ID_PATTERN = re.compile(r"^[1-9][0-9]{0,19}$")
MAX_CONTRACT_BYTES = 512 * 1024
FALLBACK_MIGRATIONS = (
    "001_init",
    "002_event_signatures",
    "003_shadow_profitability_evidence",
    "004_shadow_engine_runtime",
    "005_shadow_decision_identity",
    "006_dependency_exhaustion_quarantine",
    "007_canonical_profitability_truth",
    "008_shadow_route_discovery_indexes",
    "009_profit_triggered_secondary_verification",
    "010_fork_simulation_evidence",
)
CANDIDATE_MIGRATIONS = (
    *FALLBACK_MIGRATIONS,
    "011_money_path_selective_persistence",
)
ZERO_DATA_TABLES = (
    "origin_transactions",
    "feed_events",
    "engine_outbox",
    "opportunities",
    "opportunity_legs",
    "shadow_engine_processing_attempts",
    "shadow_engine_classifications",
    "shadow_decisions",
    "shadow_profitability_facts",
    "fork_simulation_results",
    "money_path_ingress_daily",
    "money_path_ingress_samples",
    "execution_attempts",
    "executions",
    "realized_pnl",
)
EVIDENCE_FIELDS = (
    "feed_input_count",
    "irrelevant_filtered_count",
    "unsupported_count",
    "relevant_count",
    "persistence_ratio",
    "postgresql_growth_bytes",
    "bytes_per_input",
    "projected_mb_per_day",
    "outbox_input_rate",
    "outbox_output_rate",
    "dispatcher_oldest_claimable_age_seconds",
    "classifications",
    "candidates",
    "decisions",
    "execution_attempts",
    "executions",
    "realized_pnl",
)
REQUIRED_EXACT_ENV = {
    "PHOENIX_MODE": "SHADOW",
    "LIVE_EXECUTION": "false",
    "CHAIN_ID": "42161",
    "RECORDER_PERSISTENCE_POLICY": "money_path_v1",
}
REQUIRED_EMPTY_ENV = (
    "SIGNER_PRIVATE_KEY",
    "WALLET_ADDRESS",
    "EXECUTOR_ADDRESS",
)
BOUNDED_INTEGER_ENV = {
    "RECORDER_AGGREGATE_FLUSH_SECONDS": (1, 300),
    "RECORDER_AGGREGATE_FLUSH_EVENTS": (1, 100_000),
    "RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY": (1, 1_000),
    "RECORDER_MAX_SAMPLE_JSON_BYTES": (256, 65_536),
    "SHADOW_DISPATCHER_BACKLOG_REFRESH_SECONDS": (1, 3_600),
}
FORBIDDEN_CAPABILITY_ENV = (
    "ETH_SEND_RAW_TRANSACTION",
    "ETH_SEND_TRANSACTION",
    "PUBLIC_TRANSACTION_SUBMISSION",
    "PRIVATE_RELAY_SUBMISSION",
    "PUBLIC_BROADCAST",
    "TRANSACTION_BROADCAST",
)


class V5ReleaseError(ValueError):
    pass


def _canonical_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _sha256_file(path: Path) -> str:
    return f"sha256:{hashlib.sha256(path.read_bytes()).hexdigest()}"


def _require_keys(value: object, expected: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        raise V5ReleaseError(f"{label} contract is invalid")
    return value


def load_contract(path: Path) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise V5ReleaseError("release contract must be a regular file")
    if path.stat().st_size > MAX_CONTRACT_BYTES:
        raise V5ReleaseError("release contract exceeds the size limit")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise V5ReleaseError("release contract is not valid UTF-8 JSON") from exc
    if not isinstance(value, dict):
        raise V5ReleaseError("release contract must be a JSON object")
    return value


def _validate_release_identity(value: object, allow_placeholders: bool) -> None:
    release = _require_keys(
        value,
        {
            "tag",
            "release_intent",
            "source_sha",
            "build_run_id",
            "build_run_evidence_sha256",
            "release_manifest_sha256",
            "release_provenance_sha256",
            "canonical_build_required",
        },
        "candidate release",
    )
    if release["tag"] != CANDIDATE_TAG:
        raise V5ReleaseError("candidate tag is invalid")
    if release["release_intent"] != release_provenance.RELEASE_INTENT:
        raise V5ReleaseError("candidate release intent is invalid")
    if release["canonical_build_required"] is not True:
        raise V5ReleaseError("candidate must require a canonical build")

    placeholder_fields = (
        "source_sha",
        "build_run_id",
        "build_run_evidence_sha256",
        "release_manifest_sha256",
        "release_provenance_sha256",
    )
    if allow_placeholders and all(release[name] == PLACEHOLDER for name in placeholder_fields):
        return
    if any(release[name] == PLACEHOLDER for name in placeholder_fields):
        raise V5ReleaseError("candidate release identity is only partially materialized")
    if not isinstance(release["source_sha"], str) or not SHA_PATTERN.fullmatch(
        release["source_sha"]
    ):
        raise V5ReleaseError("candidate source SHA is invalid")
    if not isinstance(release["build_run_id"], str) or not RUN_ID_PATTERN.fullmatch(
        release["build_run_id"]
    ):
        raise V5ReleaseError("candidate build run ID is invalid")
    if release["build_run_id"] in release_provenance.QUARANTINED_RUNS:
        raise V5ReleaseError("candidate build run is quarantined")
    for name in (
        "build_run_evidence_sha256",
        "release_manifest_sha256",
        "release_provenance_sha256",
    ):
        if not isinstance(release[name], str) or not DIGEST_PATTERN.fullmatch(
            release[name]
        ):
            raise V5ReleaseError(f"candidate {name} is invalid")


def validate_contract(value: object, *, allow_placeholders: bool = False) -> dict[str, Any]:
    contract = _require_keys(
        value,
        {
            "schema",
            "release",
            "fallback_environment",
            "candidate_environment",
            "runtime_environment",
            "safety",
            "post_deploy_validation",
            "rollback",
        },
        "v5 release",
    )
    if contract["schema"] != SCHEMA:
        raise V5ReleaseError("v5 release schema is invalid")
    _validate_release_identity(contract["release"], allow_placeholders)

    fallback = _require_keys(
        contract["fallback_environment"],
        {
            "name",
            "tag",
            "release_sha",
            "build_run_id",
            "migrations",
            "database_reused_for_candidate",
            "mutate_during_candidate_validation",
        },
        "fallback environment",
    )
    if fallback != {
        "name": "environment-a",
        "tag": FALLBACK_TAG,
        "release_sha": FALLBACK_SHA,
        "build_run_id": FALLBACK_BUILD_RUN_ID,
        "migrations": list(FALLBACK_MIGRATIONS),
        "database_reused_for_candidate": False,
        "mutate_during_candidate_validation": False,
    }:
        raise V5ReleaseError("fallback environment must remain the exact untouched v4 pair")

    candidate = _require_keys(
        contract["candidate_environment"],
        {
            "name",
            "database_role",
            "database_generation",
            "database_name_env",
            "database_dsn_env",
            "fallback_database_name_env",
            "initialization_confirmation",
            "fresh_database_required",
            "allowed_initial_tables",
            "migrations",
            "migration_apply_count",
            "required_schema_consumers",
            "zero_data_tables",
            "historical_import",
            "pending_outbox_import",
            "backfill",
            "shares_database_with_fallback",
        },
        "candidate environment",
    )
    expected_candidate = {
        "name": "environment-b",
        "database_role": "v5_candidate",
        "database_generation": "fresh-001-011",
        "database_name_env": "PHOENIX_V5_CANDIDATE_DATABASE_NAME",
        "database_dsn_env": "PHOENIX_V5_CANDIDATE_POSTGRES_DSN",
        "fallback_database_name_env": "PHOENIX_V4_FALLBACK_DATABASE_NAME",
        "initialization_confirmation": "INITIALIZE_EMPTY_PHOENIX_V5_DATABASE",
        "fresh_database_required": True,
        "allowed_initial_tables": ["schema_migrations"],
        "migrations": list(CANDIDATE_MIGRATIONS),
        "migration_apply_count": 2,
        "required_schema_consumers": [
            "recorder",
            "shadow-dispatcher",
            "phoenix-engine",
        ],
        "zero_data_tables": list(ZERO_DATA_TABLES),
        "historical_import": False,
        "pending_outbox_import": False,
        "backfill": False,
        "shares_database_with_fallback": False,
    }
    if candidate != expected_candidate:
        raise V5ReleaseError("candidate must use the exact fresh 001-011 database contract")

    runtime = _require_keys(
        contract["runtime_environment"],
        {"required_exact", "required_empty", "bounded_integers", "forbidden_capabilities"},
        "runtime environment",
    )
    expected_bounds = {
        name: {"minimum": limits[0], "maximum": limits[1]}
        for name, limits in BOUNDED_INTEGER_ENV.items()
    }
    if runtime != {
        "required_exact": REQUIRED_EXACT_ENV,
        "required_empty": list(REQUIRED_EMPTY_ENV),
        "bounded_integers": expected_bounds,
        "forbidden_capabilities": list(FORBIDDEN_CAPABILITY_ENV),
    }:
        raise V5ReleaseError("runtime environment contract is invalid")

    safety = _require_keys(
        contract["safety"],
        {
            "execution_eligible",
            "execution_request_created",
            "wallet_configured",
            "signer_configured",
            "executor_configured",
            "submission_enabled",
            "broadcast_enabled",
        },
        "safety",
    )
    if safety != {
        "execution_eligible": False,
        "execution_request_created": False,
        "wallet_configured": False,
        "signer_configured": False,
        "executor_configured": False,
        "submission_enabled": False,
        "broadcast_enabled": False,
    }:
        raise V5ReleaseError("SHADOW execution-zero safety contract is invalid")

    validation = _require_keys(
        contract["post_deploy_validation"],
        {
            "stages",
            "requires_prior_stage_success",
            "evidence_fields",
            "acceptance",
        },
        "post-deploy validation",
    )
    if validation["stages"] != ["15m", "1h"]:
        raise V5ReleaseError("post-deploy validation must run 15m before 1h")
    if validation["requires_prior_stage_success"] is not True:
        raise V5ReleaseError("the one-hour gate must require a passing 15-minute gate")
    if validation["evidence_fields"] != list(EVIDENCE_FIELDS):
        raise V5ReleaseError("post-deploy evidence field contract is invalid")
    acceptance = _require_keys(
        validation["acceptance"],
        {
            "safety_regression_allowed",
            "relevant_fixture_loss_allowed",
            "irrelevant_raw_persistence_max_percent",
            "unsupported_raw_persistence_max_percent",
            "old_storage_growth_mib_per_day",
            "material_storage_reduction_required",
            "execution_attempts",
            "executions",
            "realized_pnl",
        },
        "post-deploy acceptance",
    )
    if acceptance != {
        "safety_regression_allowed": False,
        "relevant_fixture_loss_allowed": False,
        "irrelevant_raw_persistence_max_percent": 0,
        "unsupported_raw_persistence_max_percent": 0,
        "old_storage_growth_mib_per_day": 7168,
        "material_storage_reduction_required": True,
        "execution_attempts": 0,
        "executions": 0,
        "realized_pnl": 0,
    }:
        raise V5ReleaseError("post-deploy acceptance contract is invalid")

    rollback = _require_keys(
        contract["rollback"],
        {
            "scope",
            "target_environment",
            "candidate_database_downgrade",
            "candidate_database_reused_by_fallback",
            "v4_compatible_with_candidate_database",
        },
        "rollback",
    )
    if rollback != {
        "scope": "environment",
        "target_environment": "environment-a",
        "candidate_database_downgrade": False,
        "candidate_database_reused_by_fallback": False,
        "v4_compatible_with_candidate_database": False,
    }:
        raise V5ReleaseError("rollback must return to untouched environment A")
    return contract


def validate_runtime_environment(environ: Mapping[str, str]) -> None:
    for name, expected in REQUIRED_EXACT_ENV.items():
        if environ.get(name) != expected:
            raise V5ReleaseError(f"{name} must be exactly {expected}")
    for name in REQUIRED_EMPTY_ENV:
        if environ.get(name, "") != "":
            raise V5ReleaseError(f"{name} must remain empty")
    for name, (minimum, maximum) in BOUNDED_INTEGER_ENV.items():
        raw = environ.get(name)
        if raw is None or not raw.isdigit():
            raise V5ReleaseError(f"{name} must be an integer")
        value = int(raw)
        if value < minimum or value > maximum:
            raise V5ReleaseError(
                f"{name} must be between {minimum} and {maximum}"
            )
    false_values = {"", "0", "false"}
    for name in FORBIDDEN_CAPABILITY_ENV:
        if environ.get(name, "").lower() not in false_values:
            raise V5ReleaseError(f"{name} must remain disabled")


def materialize_contract(
    template_value: object,
    manifest_path: Path,
    provenance_path: Path,
    run_evidence_path: Path,
) -> dict[str, Any]:
    contract = validate_contract(template_value, allow_placeholders=True)
    manifest = release_provenance._read_json(manifest_path, "release manifest")
    provenance = release_provenance._read_json(
        provenance_path, "release provenance"
    )
    run_evidence = release_provenance._read_json(
        run_evidence_path, "build run evidence"
    )
    release_provenance.validate_provenance(
        provenance, manifest, manifest_bytes=manifest_path.read_bytes()
    )
    release_provenance.validate_canonical_run(provenance, manifest, run_evidence)
    materialized = json.loads(json.dumps(contract))
    release = materialized["release"]
    release["source_sha"] = provenance["release_sha"]
    release["build_run_id"] = str(provenance["build_run_id"])
    release["build_run_evidence_sha256"] = _sha256_file(run_evidence_path)
    release["release_manifest_sha256"] = _sha256_file(manifest_path)
    release["release_provenance_sha256"] = _sha256_file(provenance_path)
    validate_contract(materialized)
    return materialized


def render_plan(contract_value: object) -> dict[str, Any]:
    contract = validate_contract(contract_value)
    release = contract["release"]
    return {
        "schema": PLAN_SCHEMA,
        "candidate_tag": release["tag"],
        "release_sha": release["source_sha"],
        "build_run_id": release["build_run_id"],
        "fallback": {
            "environment": "environment-a",
            "tag": FALLBACK_TAG,
            "database_action": "none",
        },
        "candidate": {
            "environment": "environment-b",
            "database_role": "v5_candidate",
            "database_generation": "fresh-001-011",
        },
        "ordered_gates": [
            "validate-canonical-build",
            "verify-environment-a-untouched",
            "fresh-database-preflight",
            "apply-migrations-001-011",
            "reapply-migrations-idempotency",
            "fresh-database-post-migration",
            "start-v5-shadow-services",
            "shadow-gate-15m",
            "shadow-gate-1h",
        ],
        "rollback": {
            "scope": "environment",
            "target": "environment-a",
            "candidate_database_action": "none",
        },
    }


def _write_output(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(_canonical_json(value))


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate")
    validate.add_argument("--contract", type=Path, required=True)
    validate.add_argument("--allow-placeholders", action="store_true")

    environment = subparsers.add_parser("validate-environment")
    environment.add_argument("--contract", type=Path, required=True)

    materialize = subparsers.add_parser("materialize")
    materialize.add_argument("--template", type=Path, required=True)
    materialize.add_argument("--release-manifest", type=Path, required=True)
    materialize.add_argument("--release-provenance", type=Path, required=True)
    materialize.add_argument("--run-evidence", type=Path, required=True)
    materialize.add_argument("--output", type=Path, required=True)

    plan = subparsers.add_parser("render-plan")
    plan.add_argument("--contract", type=Path, required=True)
    plan.add_argument("--output", type=Path, required=True)
    return parser


def main() -> None:
    args = _parser().parse_args()
    try:
        if args.command == "validate":
            validate_contract(
                load_contract(args.contract),
                allow_placeholders=args.allow_placeholders,
            )
        elif args.command == "validate-environment":
            validate_contract(load_contract(args.contract), allow_placeholders=True)
            validate_runtime_environment(os.environ)
        elif args.command == "materialize":
            value = materialize_contract(
                load_contract(args.template),
                args.release_manifest,
                args.release_provenance,
                args.run_evidence,
            )
            _write_output(args.output, value)
        else:
            _write_output(args.output, render_plan(load_contract(args.contract)))
    except (
        V5ReleaseError,
        release_provenance.ReleaseProvenanceError,
    ) as exc:
        raise SystemExit(f"PHOENIX_V5_RELEASE_ERROR: {exc}") from exc
    print("PHOENIX_V5_RELEASE_OK")


if __name__ == "__main__":
    main()
