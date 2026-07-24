#!/usr/bin/env python3
"""Validate and hash Phoenix Autonomous Hunter v1 contract artifacts."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import sys
from datetime import datetime
from pathlib import Path
from typing import Any, Iterable

from jsonschema import Draft202012Validator, FormatChecker


FIXTURE_SCHEMA_VERSION = "phoenix.autonomous-hunter-fixtures.v1"
SCHEMA_RELATIVE_PATH = Path("schemas/phoenix-autonomous-hunter-v1.schema.json")
FIXTURE_MANIFEST_RELATIVE_PATH = Path(
    "fixtures/autonomous-hunter/v1/fixture-manifest.json"
)
CANONICAL_DOMAIN_PREFIX = "phoenix.canonical-json.v1"
HASH_CONTRACTS: dict[str, tuple[str, str]] = {
    "phoenix.route-universe.v1": ("universe_hash", "route-universe"),
    "phoenix.route-policy.v1": ("policy_hash", "route-policy"),
    "phoenix.autonomous-global-control.v1": ("control_hash", "global-control"),
    "phoenix.autonomous-route-control.v1": ("control_hash", "route-control"),
    "phoenix.risk-snapshot.v1": ("risk_snapshot_hash", "risk-snapshot"),
    "phoenix.submission-quote.v1": ("quote_evidence_hash", "submission-quote"),
    "phoenix.autonomous-candidate.v1": ("candidate_hash", "autonomous-candidate"),
    "phoenix.automatic-approval.v1": (
        "automatic_approval_digest",
        "automatic-approval",
    ),
    "phoenix.outcome.v1": ("outcome_hash", "outcome"),
}


class ContractError(ValueError):
    """Raised when a Hunter contract fails closed."""


def _reject_float(value: str) -> None:
    raise ContractError(f"binary floating point is forbidden: {value}")


def _strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"duplicate JSON object key: {key}")
        result[key] = value
    return result


def load_json(path: Path) -> Any:
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise ContractError(f"cannot read JSON artifact: {path}") from exc
    try:
        return json.loads(
            raw,
            object_pairs_hook=_strict_object,
            parse_float=_reject_float,
            parse_constant=_reject_float,
        )
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ContractError(f"invalid JSON artifact: {path}") from exc


def _assert_no_floats(value: Any, path: str = "$") -> None:
    if isinstance(value, float):
        raise ContractError(f"binary floating point is forbidden at {path}")
    if isinstance(value, dict):
        for key, child in value.items():
            if not isinstance(key, str):
                raise ContractError(f"non-string JSON key at {path}")
            _assert_no_floats(child, f"{path}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            _assert_no_floats(child, f"{path}[{index}]")


def canonical_json(value: Any) -> bytes:
    _assert_no_floats(value)
    try:
        rendered = json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        )
    except (TypeError, ValueError) as exc:
        raise ContractError("artifact is not canonicalizable JSON") from exc
    return rendered.encode("utf-8")


def canonical_hash(document: dict[str, Any]) -> str:
    schema_version = document.get("schema_version")
    if not isinstance(schema_version, str) or schema_version not in HASH_CONTRACTS:
        raise ContractError("unsupported Hunter schema_version")
    hash_field, domain = HASH_CONTRACTS[schema_version]
    if hash_field not in document:
        raise ContractError(f"missing canonical hash field: {hash_field}")
    body = copy.deepcopy(document)
    del body[hash_field]
    domain_bytes = (
        f"{CANONICAL_DOMAIN_PREFIX}:{domain}:{schema_version}\n".encode("ascii")
    )
    return hashlib.sha256(domain_bytes + canonical_json(body)).hexdigest()


def verify_canonical_hash(document: dict[str, Any]) -> None:
    schema_version = document.get("schema_version")
    if not isinstance(schema_version, str) or schema_version not in HASH_CONTRACTS:
        raise ContractError("unsupported Hunter schema_version")
    hash_field = HASH_CONTRACTS[schema_version][0]
    actual = document.get(hash_field)
    expected = canonical_hash(document)
    if actual != expected:
        raise ContractError(
            f"{schema_version} canonical hash mismatch: expected {expected}"
        )


def _validator(repo_root: Path) -> Draft202012Validator:
    schema = load_json(repo_root / SCHEMA_RELATIVE_PATH)
    Draft202012Validator.check_schema(schema)
    return Draft202012Validator(schema, format_checker=FormatChecker())


def _schema_errors(validator: Draft202012Validator, document: Any) -> list[str]:
    return [
        f"{'.'.join(str(part) for part in error.absolute_path) or '$'}: "
        f"{error.message}"
        for error in sorted(validator.iter_errors(document), key=str)
    ]


def _parse_timestamp(value: str) -> datetime:
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ContractError(f"invalid canonical timestamp: {value}") from exc


def _unique(values: Iterable[str], label: str) -> None:
    materialized = list(values)
    if len(materialized) != len(set(materialized)):
        raise ContractError(f"{label} contains duplicates")


def _semantic_route_universe(document: dict[str, Any]) -> None:
    assets = document["settlement_assets"] + document["intermediate_assets"]
    _unique((item["asset_id"] for item in assets), "asset ids")
    _unique((item["address"] for item in assets), "asset addresses")
    _unique((item["router_id"] for item in document["routers"]), "router ids")
    _unique((item["address"] for item in document["routers"]), "router addresses")
    _unique((item["factory_id"] for item in document["factories"]), "factory ids")
    _unique((item["address"] for item in document["factories"]), "factory addresses")
    _unique((item["pool_id"] for item in document["pools"]), "pool ids")
    _unique((item["address"] for item in document["pools"]), "pool addresses")
    asset_addresses = {item["address"] for item in assets}
    factory_addresses = {item["address"] for item in document["factories"]}
    for pool in document["pools"]:
        if pool["token0"] == pool["token1"]:
            raise ContractError("pool token identities must differ")
        if pool["token0"] not in asset_addresses or pool["token1"] not in asset_addresses:
            raise ContractError("pool references an asset outside the universe")
        if pool["factory_address"] not in factory_addresses:
            raise ContractError("pool references a factory outside the universe")
    if document["maximum_routes_per_event"] > document["maximum_total_routes"]:
        raise ContractError("per-event route bound exceeds total route bound")


def _semantic_route_policy(document: dict[str, Any]) -> None:
    leg_count = len(document["pool_addresses"])
    parallel_fields = (
        "factory_addresses",
        "protocol_ids",
        "fees",
        "directions",
    )
    if len(document["token_path"]) != leg_count + 1:
        raise ContractError("route token path does not match leg count")
    for field in parallel_fields:
        if len(document[field]) != leg_count:
            raise ContractError(f"route {field} does not match leg count")
    if (
        document["token_path"][0] != document["settlement_asset"]
        or document["token_path"][-1] != document["settlement_asset"]
    ):
        raise ContractError("route must begin and end in the settlement asset")
    if any(
        left == right
        for left, right in zip(document["token_path"], document["token_path"][1:])
    ):
        raise ContractError("route contains a same-token leg")
    if int(document["minimum_input_amount"]) > int(document["maximum_input_amount"]):
        raise ContractError("route minimum input exceeds maximum input")


def _semantic_global_control(document: dict[str, Any]) -> None:
    if document["execution_mode"] == "live" and (
        not document["armed"] or document["kill_switch"]
    ):
        raise ContractError("live global control is not armed and open")
    if document["kill_switch"] and document["disarm_reason"] is None:
        raise ContractError("closed global control requires a reason")


def _semantic_route_control(document: dict[str, Any]) -> None:
    if not document["kill_switch"] and not document["enabled"]:
        raise ContractError("open route control must be enabled")
    if document["kill_switch"] and document["disarm_reason"] is None:
        raise ContractError("closed route control requires a reason")


def _semantic_risk_snapshot(document: dict[str, Any]) -> None:
    global_control = document["global_control_state"]
    route_control = document["route_control_state"]
    verify_canonical_hash(global_control)
    verify_canonical_hash(route_control)
    _semantic_global_control(global_control)
    _semantic_route_control(route_control)
    if document["route_policy_hash"] != route_control["route_policy_hash"]:
        raise ContractError("risk snapshot route policy identity mismatch")
    if document["current_size_level"] != route_control["current_size_level"]:
        raise ContractError("risk snapshot size level mismatch")
    if int(document["maximum_permitted_size"]) > int(
        route_control["maximum_permitted_size"]
    ):
        raise ContractError("risk snapshot exceeds route control size")
    if document["cooldown_until"] != route_control["cooldown_until"]:
        raise ContractError("risk snapshot cooldown mismatch")


def _semantic_submission_quote(document: dict[str, Any]) -> None:
    if _parse_timestamp(document["quote_expires_at"]) <= _parse_timestamp(
        document["quote_created_at"]
    ):
        raise ContractError("submission quote is already expired")
    if int(document["estimated_ordering_payment"]) > int(
        document["maximum_ordering_payment"]
    ):
        raise ContractError("submission quote exceeds ordering cap")
    if int(document["expected_net_after_ordering"]) < int(
        document["minimum_retained_profit"]
    ):
        raise ContractError("submission quote does not retain policy profit")
    fallback = document["fallback_channel_id"]
    if document["fallback_allowed"] != (fallback is not None):
        raise ContractError("submission fallback identity is inconsistent")
    if fallback == document["logical_channel_id"]:
        raise ContractError("submission fallback must be a different channel")


def _semantic_candidate(document: dict[str, Any]) -> None:
    if _parse_timestamp(document["candidate_expires_at"]) <= _parse_timestamp(
        document["candidate_created_at"]
    ):
        raise ContractError("autonomous candidate is already expired")
    if int(document["conservative_predicted_net_pnl"]) > (
        int(document["predicted_gross_profit"])
        - int(document["predicted_total_cost"])
    ):
        raise ContractError("candidate conservative net exceeds predicted net")
    if document["risk_policy_hash"] != document["route_policy_hash"]:
        raise ContractError(
            "v1 risk_policy_hash must bind the complete RoutePolicyV1 digest"
        )


def _semantic_automatic_approval(document: dict[str, Any]) -> None:
    if _parse_timestamp(document["approval_expires_at"]) <= _parse_timestamp(
        document["approval_created_at"]
    ):
        raise ContractError("automatic approval is already expired")


def _semantic_outcome(document: dict[str, Any]) -> None:
    transaction_fields = (
        document["transaction_hash"],
        document["block_number"],
        document["receipt_status"],
    )
    if any(value is None for value in transaction_fields) and not all(
        value is None for value in transaction_fields
    ):
        raise ContractError("outcome transaction attribution is incomplete")
    realized_chain = (
        int(document["realized_gross_profit"])
        - int(document["actual_gas_cost"])
        - int(document["actual_ordering_cost"])
    )
    if int(document["realized_chain_net_pnl"]) != realized_chain:
        raise ContractError("outcome chain net PnL is inconsistent")
    realized_business = realized_chain - int(document["allocated_infrastructure_cost"])
    if int(document["realized_business_net_pnl"]) != realized_business:
        raise ContractError("outcome business net PnL is inconsistent")
    if _parse_timestamp(document["attributed_at"]) < _parse_timestamp(
        document["terminal_at"]
    ):
        raise ContractError("outcome attribution predates terminal state")


SEMANTIC_VALIDATORS = {
    "phoenix.route-universe.v1": _semantic_route_universe,
    "phoenix.route-policy.v1": _semantic_route_policy,
    "phoenix.autonomous-global-control.v1": _semantic_global_control,
    "phoenix.autonomous-route-control.v1": _semantic_route_control,
    "phoenix.risk-snapshot.v1": _semantic_risk_snapshot,
    "phoenix.submission-quote.v1": _semantic_submission_quote,
    "phoenix.autonomous-candidate.v1": _semantic_candidate,
    "phoenix.automatic-approval.v1": _semantic_automatic_approval,
    "phoenix.outcome.v1": _semantic_outcome,
}


def validate_document(
    document: Any, validator: Draft202012Validator
) -> dict[str, Any]:
    if not isinstance(document, dict):
        raise ContractError("Hunter contract must be a JSON object")
    errors = _schema_errors(validator, document)
    if errors:
        raise ContractError("schema validation failed: " + " | ".join(errors))
    schema_version = document["schema_version"]
    verify_canonical_hash(document)
    SEMANTIC_VALIDATORS[schema_version](document)
    return document


def _fixture_path(root: Path, relative: str) -> Path:
    candidate = (root / relative).resolve()
    try:
        candidate.relative_to(root.resolve())
    except ValueError as exc:
        raise ContractError(f"fixture path escapes its root: {relative}") from exc
    if candidate.suffix != ".json":
        raise ContractError(f"fixture is not JSON: {relative}")
    return candidate


def _cross_validate(valid: dict[str, dict[str, Any]], repo_root: Path) -> None:
    expected_versions = set(HASH_CONTRACTS)
    if set(valid) != expected_versions:
        missing = sorted(expected_versions - set(valid))
        extra = sorted(set(valid) - expected_versions)
        raise ContractError(f"fixture contract set mismatch: missing={missing}, extra={extra}")

    universe = valid["phoenix.route-universe.v1"]
    policy = valid["phoenix.route-policy.v1"]
    global_control = valid["phoenix.autonomous-global-control.v1"]
    route_control = valid["phoenix.autonomous-route-control.v1"]
    risk = valid["phoenix.risk-snapshot.v1"]
    quote = valid["phoenix.submission-quote.v1"]
    candidate = valid["phoenix.autonomous-candidate.v1"]
    approval = valid["phoenix.automatic-approval.v1"]
    outcome = valid["phoenix.outcome.v1"]

    if policy["route_universe_hash"] != universe["universe_hash"]:
        raise ContractError("route policy does not bind the route universe fixture")
    pool_index = {pool["address"]: pool for pool in universe["pools"]}
    for index, pool_address in enumerate(policy["pool_addresses"]):
        pool = pool_index.get(pool_address)
        if pool is None:
            raise ContractError("route policy references a pool outside the universe")
        if policy["factory_addresses"][index] != pool["factory_address"]:
            raise ContractError("route policy factory does not match its pool")
        if policy["protocol_ids"][index] != pool["protocol_id"]:
            raise ContractError("route policy protocol does not match its pool")
        if policy["fees"][index] != pool["fee"]:
            raise ContractError("route policy fee does not match its pool")
        token_in = policy["token_path"][index]
        token_out = policy["token_path"][index + 1]
        direction = policy["directions"][index]
        if direction == "zero_for_one":
            expected_tokens = (pool["token0"], pool["token1"])
        else:
            expected_tokens = (pool["token1"], pool["token0"])
        if (token_in, token_out) != expected_tokens:
            raise ContractError("route policy direction does not match pool identity")

    if route_control["route_fingerprint"] != policy["route_fingerprint"]:
        raise ContractError("route control fingerprint mismatch")
    if route_control["route_policy_hash"] != policy["policy_hash"]:
        raise ContractError("route control policy mismatch")
    if risk["global_control_state"] != global_control:
        raise ContractError("risk snapshot global control fixture mismatch")
    if risk["route_control_state"] != route_control:
        raise ContractError("risk snapshot route control fixture mismatch")
    if candidate["route_fingerprint"] != policy["route_fingerprint"]:
        raise ContractError("candidate route fingerprint mismatch")
    if candidate["route_universe_hash"] != universe["universe_hash"]:
        raise ContractError("candidate route universe mismatch")
    if candidate["route_policy_hash"] != policy["policy_hash"]:
        raise ContractError("candidate route policy mismatch")
    if candidate["risk_snapshot_hash"] != risk["risk_snapshot_hash"]:
        raise ContractError("candidate risk snapshot mismatch")
    if candidate["submission_quote_hash"] != quote["quote_evidence_hash"]:
        raise ContractError("candidate submission quote mismatch")
    if candidate["submission_channel"] != quote["logical_channel_id"]:
        raise ContractError("candidate submission channel mismatch")
    if int(candidate["selected_size"]) > int(policy["maximum_input_amount"]):
        raise ContractError("candidate exceeds route policy maximum")
    if int(candidate["selected_size"]) > int(risk["maximum_permitted_size"]):
        raise ContractError("candidate exceeds current risk maximum")

    for field in (
        "candidate_id",
        "candidate_hash",
        "route_policy_hash",
        "route_universe_hash",
        "risk_snapshot_hash",
        "submission_quote_hash",
        "state_hash",
        "plan_hash",
        "calldata_hash",
        "executor_address",
        "executor_code_hash",
    ):
        if approval[field] != candidate[field]:
            raise ContractError(f"automatic approval {field} mismatch")
    if approval["approval_expires_at"] != candidate["candidate_expires_at"]:
        raise ContractError("automatic approval expiry mismatch")

    for field in (
        "candidate_id",
        "opportunity_id",
        "route_fingerprint",
        "route_universe_hash",
        "route_policy_hash",
        "risk_snapshot_hash",
        "submission_quote_hash",
        "candidate_hash",
        "state_hash",
        "plan_hash",
        "calldata_hash",
        "executor_code_hash",
        "predicted_gross_profit",
        "predicted_total_cost",
        "conservative_predicted_net_pnl",
    ):
        if outcome[field] != candidate[field]:
            raise ContractError(f"outcome {field} mismatch")
    if outcome["automatic_approval_digest"] != approval["automatic_approval_digest"]:
        raise ContractError("outcome automatic approval mismatch")

    release_universe = load_json(
        repo_root / "config/phoenix-route-universe-v1.json"
    )
    if release_universe != universe:
        raise ContractError("release route universe and valid fixture differ")


def validate_fixture_suite(repo_root: Path) -> tuple[int, int]:
    validator = _validator(repo_root)
    manifest_path = repo_root / FIXTURE_MANIFEST_RELATIVE_PATH
    manifest = load_json(manifest_path)
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema",
        "valid",
        "invalid",
    }:
        raise ContractError("fixture manifest contract is invalid")
    if manifest["schema"] != FIXTURE_SCHEMA_VERSION:
        raise ContractError("fixture manifest schema identity is invalid")
    root = manifest_path.parent
    valid_paths = manifest["valid"]
    invalid_paths = manifest["invalid"]
    if (
        not isinstance(valid_paths, list)
        or not valid_paths
        or not isinstance(invalid_paths, list)
        or not invalid_paths
        or not all(isinstance(path, str) for path in valid_paths + invalid_paths)
    ):
        raise ContractError("fixture manifest paths are invalid")
    _unique(valid_paths + invalid_paths, "fixture paths")

    valid: dict[str, dict[str, Any]] = {}
    for relative in valid_paths:
        document = validate_document(load_json(_fixture_path(root, relative)), validator)
        version = document["schema_version"]
        if version in valid:
            raise ContractError(f"duplicate valid fixture for {version}")
        valid[version] = document

    for relative in invalid_paths:
        document = load_json(_fixture_path(root, relative))
        try:
            validate_document(document, validator)
        except ContractError:
            continue
        raise ContractError(f"invalid fixture unexpectedly passed: {relative}")

    _cross_validate(valid, repo_root)
    return len(valid_paths), len(invalid_paths)


def _repo_root(value: str | None) -> Path:
    if value is not None:
        return Path(value).resolve(strict=True)
    return Path(__file__).resolve().parents[1]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root")
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate-fixtures")
    validate_parser = subparsers.add_parser("validate")
    validate_parser.add_argument("artifact")
    hash_parser = subparsers.add_parser("hash")
    hash_parser.add_argument("artifact")
    args = parser.parse_args(argv)
    repo_root = _repo_root(args.repo_root)

    if args.command == "validate-fixtures":
        valid_count, invalid_count = validate_fixture_suite(repo_root)
        print(
            f"HUNTER_CONTRACT_FIXTURES_OK: "
            f"valid={valid_count} invalid={invalid_count}"
        )
        return 0

    artifact = Path(args.artifact).resolve(strict=True)
    document = load_json(artifact)
    if not isinstance(document, dict):
        raise ContractError("Hunter contract must be a JSON object")
    if args.command == "hash":
        print(canonical_hash(document))
        return 0
    validate_document(document, _validator(repo_root))
    print(
        f"HUNTER_CONTRACT_OK: schema={document['schema_version']} "
        f"hash={canonical_hash(document)}"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ContractError as exc:
        print(f"HUNTER_CONTRACT_INVALID: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
