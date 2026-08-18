#!/usr/bin/env python3
"""Bounded PhoenixExecutor rotation context and SPL-proof helper."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import tempfile
import time
import uuid
from pathlib import Path

ADDRESS = re.compile(r"^0x[0-9a-f]{40}$")
HASH = re.compile(r"^[0-9a-f]{64}$")
BLOCK_HASH = re.compile(r"^0x[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
OLD_EXECUTOR = "0x634f62d7cd28d1c4dcf503d901b88d666c2626ad"
OLD_HASH = "99a485d5a711180b4455028620bf4d5374558f85ef185ba00a51481c7c239c58"
OWNER = "0x9f30c00b68f7c0edb4b4117b9f04e0ca2eb2c17a"
WETH = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
NATIVE_USDC = "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
POOL = "0x6f38e884725a116c9c7fbf208e79fe8828a2595f"
FACTORY = "0x1f98431c8ad98523631ae4a59f267346ea31f984"
BORROWER = "0xf35e950921b85429444b2422bb23b1fabcb0e9d1"
MAX_INPUT = 10_000_000_000_000_000
PROFIT_FLOOR = 1_000_000_000_000


def load(path: Path) -> dict:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("ROTATION_CONTEXT_INVALID")
    return value


def validate_plan(value: dict) -> dict:
    required = {
        "schema", "chain_id", "source_sha", "base_release_sha", "old_executor",
        "old_runtime_sha256", "expected_new_runtime_sha256", "creation_bytecode_sha256",
        "config_digest", "owner", "flash_provider", "atlas", "weth",
        "maximum_input_amount", "searcher", "assets", "routers", "factory", "pools",
    }
    if set(value) != required or value.get("schema") != "phoenix.executor-rotation.v1":
        raise ValueError("ROTATION_PLAN_INVALID")
    if value.get("chain_id") != 42161 or value.get("old_executor") != OLD_EXECUTOR:
        raise ValueError("ROTATION_PLAN_INVALID")
    if value.get("old_runtime_sha256") != OLD_HASH or value.get("owner") != OWNER:
        raise ValueError("ROTATION_PLAN_INVALID")
    if value.get("weth") != WETH or value.get("maximum_input_amount") != MAX_INPUT:
        raise ValueError("ROTATION_PLAN_INVALID")
    if value.get("source_sha") is None or not COMMIT.fullmatch(value["source_sha"]):
        raise ValueError("ROTATION_PLAN_INVALID")
    return value


def validate(value: dict, plan: dict | None = None) -> tuple[str, str]:
    if value.get("schema") != "phoenix.executor-rotation.v1" or value.get("chain_id") != 42161:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if value.get("old_executor") != OLD_EXECUTOR or value.get("old_runtime_sha256") != OLD_HASH:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    new_address = value.get("new_executor")
    new_hash = value.get("new_runtime_sha256")
    if not isinstance(new_address, str) or not ADDRESS.fullmatch(new_address):
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if not isinstance(new_hash, str) or not HASH.fullmatch(new_hash):
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if new_address == OLD_EXECUTOR or new_hash == OLD_HASH:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if not isinstance(value.get("deployment_tx_hash"), str) or not BLOCK_HASH.fullmatch(value["deployment_tx_hash"]):
        raise ValueError("ROTATION_CONTEXT_INVALID")
    for name in (
        "config_verified", "pre_cutover_spl_absent", "old_bound_work_drained",
        "cutover_started", "cutover_completed", "identity_consumers_verified", "rollback_used", "rollback_completed",
    ):
        if type(value.get(name)) is not bool:
            raise ValueError("ROTATION_CONTEXT_INVALID")
    if value["rollback_completed"] and not value["rollback_used"]:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if value["rollback_used"] and not value["cutover_started"]:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if value["cutover_completed"] and not value["cutover_started"]:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    consumers = value.get("identity_consumers")
    if not isinstance(consumers, list) or len(consumers) > 8 or len(consumers) != len(set(consumers)) or any(item not in {"atlas-observer", "phoenix-engine", "economic-supervisor", "live-executor"} for item in consumers):
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if value["cutover_started"] and consumers != ["atlas-observer", "economic-supervisor", "live-executor", "phoenix-engine"]:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    if plan is not None:
        if any(value.get(a) != plan.get(b) for a, b in (
            ("tooling_source_sha", "source_sha"), ("base_release_sha", "base_release_sha"),
            ("creation_bytecode_sha256", "creation_bytecode_sha256"),
            ("config_digest", "config_digest"),
        )) or value.get("new_runtime_sha256") != plan.get("expected_new_runtime_sha256"):
            raise ValueError("ROTATION_CONTEXT_INVALID")
    return new_address, new_hash


def parse_env(path: Path) -> list[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    names = [line.split("=", 1)[0] for line in lines if line and not line.startswith("#") and "=" in line]
    if len(names) != len(set(names)):
        raise ValueError("ROTATION_ENV_INVALID")
    return lines


def write_atomic(path: Path, data: bytes, mode: int = 0o600) -> None:
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        if hasattr(os, "O_DIRECTORY"):
            directory_fd = os.open(path.parent, os.O_DIRECTORY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def write_json(path: Path, value: dict) -> None:
    write_atomic(path, (json.dumps(value, indent=2, sort_keys=True) + "\n").encode(), 0o600)


def materialize(env_path: Path, provenance_path: Path, output: Path, old: bool, plan_path: Path) -> None:
    plan = validate_plan(load(plan_path))
    value = load(provenance_path)
    new_address, new_hash = validate(value, plan)
    target_address = OLD_EXECUTOR if old else new_address
    target_hash = OLD_HASH if old else new_hash
    replacements = {"LIVE_EXECUTOR_EXECUTOR_ADDRESS": target_address, "LIVE_EXECUTOR_EXECUTOR_CODE_HASH": target_hash}
    lines = parse_env(env_path)
    seen: set[str] = set()
    rendered: list[str] = []
    for line in lines:
        if "=" in line and not line.startswith("#"):
            name = line.split("=", 1)[0]
            if name in replacements:
                rendered.append(f"{name}={replacements[name]}")
                seen.add(name)
                continue
        rendered.append(line)
    if seen != set(replacements):
        raise ValueError("ROTATION_ENV_INVALID")
    write_atomic(output, ("\n".join(rendered) + "\n").encode(), 0o640)


def exact_request(plan: dict) -> dict:
    return {
        "schema_version": "phoenix.rpc.aave-exact-request.v3",
        "chain_id": 42161,
        "request_id": f"phoenix-executor-rotation-exact-{uuid.uuid4().hex}",
        "borrower": BORROWER,
        "maximum_input_amount": str(MAX_INPUT),
    }


def simulation_request(plan: dict, provenance: dict, exact: dict) -> dict:
    new_address, new_hash = validate(provenance, plan)
    if (
        exact.get("schema_version") != "phoenix.rpc.aave-exact-response.v5"
        or exact.get("chain_id") != 42161
        or exact.get("confirmation") is not None
        or exact.get("quorum") != 1
        or exact.get("primary", {}).get("provider_id") != "production-nownodes-arbitrum"
    ):
        raise ValueError("ROTATION_SPL_EXACT_INVALID")
    liquidations = exact["primary"].get("liquidations", [])
    matches = [item for item in liquidations if item.get("debt_asset") == WETH and item.get("collateral_asset") == NATIVE_USDC]
    if len(matches) != 1:
        raise ValueError("ROTATION_SPL_POSITION_UNAVAILABLE")
    liquidation = matches[0]
    quotes = [item for item in liquidation.get("unwind_quotes", []) if item.get("pool") == POOL and item.get("factory") == FACTORY and item.get("fee") == 100]
    if len(quotes) != 1:
        raise ValueError("ROTATION_SPL_ROUTE_UNAVAILABLE")
    quote = quotes[0]
    repay = int(liquidation["repay_amount"])
    premium = int(liquidation["flash_premium_amount"])
    output = int(quote["output_debt_asset"])
    expected_profit = output - repay - premium
    if not 0 < repay <= MAX_INPUT or expected_profit <= PROFIT_FLOOR:
        raise ValueError("ROTATION_SPL_ECONOMICS_INVALID")
    request_id = f"phoenix-executor-rotation-sim-{uuid.uuid4().hex}"
    simulation = {
        "schema_version": "phoenix.rpc.aave-simulate-request.v4",
        "chain_id": 42161,
        "request_id": request_id,
        "block_number": exact["block_number"], "block_hash": exact["block_hash"], "state_root": exact["state_root"],
        "executor_address": new_address, "executor_code_hash": new_hash,
        "caller_address": OWNER, "release_sha": plan["source_sha"], "borrower": BORROWER,
        "debt_asset": WETH, "collateral_asset": NATIVE_USDC, "debt_asset_decimals": 18,
        "debt_asset_price_base": liquidation["debt_asset_price_base"], "weth_price_base": liquidation["weth_price_base"],
        "repay_amount": str(repay), "maximum_input_amount": str(MAX_INPUT),
        "live_maximum_input_amount": str(MAX_INPUT), "maximum_input_weth_wei": str(MAX_INPUT),
        "live_maximum_input_weth_wei": str(MAX_INPUT), "counterfactual": False,
        "minimum_collateral_received": liquidation["liquidator_collateral"],
        "minimum_unwind_output": str(repay + premium + PROFIT_FLOOR),
        "minimum_profit": str(PROFIT_FLOOR), "minimum_profit_weth_wei": str(PROFIT_FLOOR),
        "expected_profit": str(expected_profit), "retained_profit_floor": str(PROFIT_FLOOR),
        "selected_pool": POOL, "selected_factory": FACTORY, "selected_fee": 100,
        "zero_for_one": bool(quote["zero_for_one"]), "gas_limit": 5_000_000,
        "max_fee_per_gas": "10000000000", "max_priority_fee_per_gas": "1000000000",
        "deadline_unix_seconds": int(time.time()) + 120, "atlas_mode": False, "atlas_bid": "0",
    }
    return {"schema_version": "phoenix.rpc.aave-simulate-batch-request.v3", "chain_id": 42161, "request_id": f"batch-{request_id}", "simulations": [simulation]}


def verify_simulation(request: dict, response: dict, provenance_path: Path, plan: dict) -> None:
    provenance = load(provenance_path)
    validate(provenance, plan)
    sim = request["simulations"][0]
    results = response.get("results")
    if (
        response.get("schema_version") != "phoenix.rpc.aave-simulate-batch-response.v4"
        or response.get("chain_id") != 42161 or response.get("request_id") != request.get("request_id")
        or response.get("block_number") != sim.get("block_number") or response.get("block_hash") != sim.get("block_hash")
        or response.get("state_root") != sim.get("state_root")
        or response.get("primary_provider_id") != "production-nownodes-arbitrum"
        or response.get("confirmation_provider_id") is not None or response.get("quorum") != 1
        or response.get("evidence_mode") != "SINGLE_PRIMARY_FORK_VERIFIED"
        or not isinstance(results, list) or len(results) != 1
        or results[0].get("request_id") != sim.get("request_id")
        or results[0].get("error") is not None or not isinstance(results[0].get("response"), dict)
    ):
        raise ValueError("ROTATION_SPL_PROOF_FAILED")
    item = results[0]["response"]
    if item.get("schema_version") != "phoenix.rpc.aave-simulate-response.v5" or item.get("request_id") != sim.get("request_id"):
        raise ValueError("ROTATION_SPL_PROOF_FAILED")
    provenance["pre_cutover_spl_absent"] = True
    write_json(provenance_path, provenance)


def mark(provenance_path: Path, plan: dict, field: str) -> None:
    value = load(provenance_path)
    validate(value, plan)
    if field == "cutover_started":
        if not value["config_verified"] or not value["pre_cutover_spl_absent"] or not value["old_bound_work_drained"]:
            raise ValueError("ROTATION_CUTOVER_REJECTED")
        value[field] = True
    elif field == "cutover_completed":
        if not value["cutover_started"]:
            raise ValueError("ROTATION_CUTOVER_REJECTED")
        value[field] = True
    elif field == "identity_consumers_verified":
        if not value["cutover_completed"]:
            raise ValueError("ROTATION_RECONCILE_REJECTED")
        value[field] = True
    elif field == "rollback_used":
        if value["rollback_used"] or value["rollback_completed"] or not value["cutover_started"]:
            raise ValueError("ROTATION_ROLLBACK_REJECTED")
        value[field] = True
    elif field == "rollback_completed":
        if not value["rollback_used"] or value["rollback_completed"]:
            raise ValueError("ROTATION_ROLLBACK_REJECTED")
        value[field] = True
    else:
        raise ValueError("ROTATION_CONTEXT_INVALID")
    write_json(provenance_path, value)


def record_consumers(provenance_path: Path, plan: dict, consumers: str) -> None:
    value = load(provenance_path)
    validate(value, plan)
    selected = consumers.split()
    expected = {"atlas-observer", "phoenix-engine", "economic-supervisor", "live-executor"}
    if set(selected) != expected or len(selected) != len(expected):
        raise ValueError("ROTATION_CONSUMERS_INVALID")
    if value["cutover_started"] or value["rollback_used"]:
        raise ValueError("ROTATION_CONSUMERS_INVALID")
    value["identity_consumers"] = sorted(selected)
    write_json(provenance_path, value)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("validate", "materialize-new", "materialize-old", "exact-request", "simulation-request", "verify-simulation", "record-consumers", "mark-cutover-started", "mark-cutover", "mark-reconciled", "claim-rollback", "mark-rollback-complete"))
    parser.add_argument("--plan", required=True, type=Path)
    parser.add_argument("--provenance", type=Path)
    parser.add_argument("--env-file", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--exact-response", type=Path)
    parser.add_argument("--request", type=Path)
    parser.add_argument("--response", type=Path)
    parser.add_argument("--consumers")
    args = parser.parse_args()
    plan = validate_plan(load(args.plan))
    if args.mode == "exact-request":
        if args.output is None: raise ValueError("ROTATION_CONTEXT_ARGUMENTS_INVALID")
        write_json(args.output, exact_request(plan)); return 0
    if args.provenance is None: raise ValueError("ROTATION_CONTEXT_ARGUMENTS_INVALID")
    if args.mode == "validate": validate(load(args.provenance), plan)
    elif args.mode in ("materialize-new", "materialize-old"):
        if args.env_file is None or args.output is None: raise ValueError("ROTATION_CONTEXT_ARGUMENTS_INVALID")
        materialize(args.env_file, args.provenance, args.output, args.mode == "materialize-old", args.plan)
    elif args.mode == "simulation-request":
        if args.exact_response is None or args.output is None: raise ValueError("ROTATION_CONTEXT_ARGUMENTS_INVALID")
        write_json(args.output, simulation_request(plan, load(args.provenance), load(args.exact_response)))
    elif args.mode == "verify-simulation":
        if args.request is None or args.response is None: raise ValueError("ROTATION_CONTEXT_ARGUMENTS_INVALID")
        verify_simulation(load(args.request), load(args.response), args.provenance, plan)
    elif args.mode == "record-consumers":
        if args.consumers is None: raise ValueError("ROTATION_CONTEXT_ARGUMENTS_INVALID")
        record_consumers(args.provenance, plan, args.consumers)
    else:
        field = {"mark-cutover-started":"cutover_started", "mark-cutover":"cutover_completed", "mark-reconciled":"identity_consumers_verified", "claim-rollback":"rollback_used", "mark-rollback-complete":"rollback_completed"}[args.mode]
        mark(args.provenance, plan, field)
    print("PHOENIX_EXECUTOR_ROTATION_CONTEXT_OK")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise SystemExit(str(exc)) from None
