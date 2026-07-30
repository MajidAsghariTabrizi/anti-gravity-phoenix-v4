#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)

fail() {
  echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_FAILED: $1" >&2
  exit 1
}

for path in \
  compose.live-autonomous.yml \
  migrations/012_live_economic_truth.sql \
  live-executor/schema/005_closed_loop_economic_control.sql \
  live-executor/src/economic_control.rs \
  live-executor/src/autonomous_live_control_main.rs \
  scripts/deploy-release.sh \
  scripts/rollback-release.sh \
  scripts/activate-economic-canary.sh \
  scripts/economic_activation_runner.py \
  scripts/economic-dashboard-loop.sh \
  deploy/phoenix-economic-activation.path \
  deploy/phoenix-economic-activation.service \
  scripts/sql/economic-dashboard-snapshot.sql
do
  [ -f "$repo_root/$path" ] && [ ! -L "$repo_root/$path" ] ||
    fail "required contract is missing: $path"
done

PYTHONDONTWRITEBYTECODE=1 python3 -I -B - "$repo_root" <<'PY' ||
import re
import sys
from pathlib import Path


root = Path(sys.argv[1])


def read(path: str) -> str:
    return (root / path).read_text(encoding="utf-8")


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"AUTONOMOUS_LIVE_RELEASE_CONTRACT_INVALID:{message}")


compose = read("compose.live-autonomous.yml")
deploy = read("scripts/deploy-release.sh")
rollback = read("scripts/rollback-release.sh")
activate = read("scripts/activate-economic-canary.sh")
control = read("live-executor/src/autonomous_live_control_main.rs")
state = read("live-executor/src/economic_control.rs")
schema = read("live-executor/schema/005_closed_loop_economic_control.sql")
health = read("scripts/production-healthcheck.sh")
monitor = read("scripts/economic-dashboard-loop.sh")
dashboard_sql = read("scripts/sql/economic-dashboard-snapshot.sql")
economic_truth = read("migrations/012_live_economic_truth.sql")
activation_runner = read("scripts/economic_activation_runner.py")
activation_path = read("deploy/phoenix-economic-activation.path")
activation_service = read("deploy/phoenix-economic-activation.service")
release_model = read("scripts/phoenix_release/model.py")
release_gateway = read("scripts/phoenix_release/gateway.py")
assets = read("scripts/release_assets.py")
installer = read("scripts/install-production-release-context.sh")

service = re.search(
    r"(?ms)^  autonomous-control:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(service is not None, "signerless_control_service_missing")
control_service = service.group("body")
for forbidden in ("SIGNER_PRIVATE_KEY", "SIGNER_PRIVATE_KEY_FILE", "LIVE_EXECUTOR_SIGNER_FILE"):
    require(forbidden not in control_service, f"signerless_control_contains:{forbidden}")
require("autonomous-live-control" in control_service, "control_entrypoint_missing")
require('restart: "no"' in control_service, "control_service_must_be_one_shot")

live_service = re.search(
    r"(?ms)^  live-executor:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(live_service is not None, "live_executor_service_missing")
require(
    "phoenix-live-executor-signer" in live_service.group("body"),
    "authorized_executor_signer_mount_missing",
)
require("economic-monitor:" in compose, "economic_monitor_service_missing")
require("economic-supervisor:" in compose, "economic_supervisor_service_missing")
supervisor = re.search(
    r"(?ms)^  economic-supervisor:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(supervisor is not None, "economic_supervisor_service_missing")
supervisor_service = supervisor.group("body")
for required in (
    'user: "65532:65532"',
    "read_only: true",
    "cap_drop: [ALL]",
    "no-new-privileges:true",
    "/activation-outbox",
):
    require(required in supervisor_service, f"activation_producer_hardening_missing:{required}")
for forbidden in (
    "/var/run/docker.sock",
    "SIGNER_PRIVATE_KEY",
    "SIGNER_PRIVATE_KEY_FILE",
    "LIVE_EXECUTOR_SIGNER_FILE",
    ":/root",
):
    require(
        forbidden not in supervisor_service,
        f"activation_producer_authority_leak:{forbidden}",
    )
require(
    "PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS: \"45\"" in compose,
    "dashboard_refresh_interval_not_45_seconds",
)
require(
    "attempt.claimed_at >= evidence_window.window_start" in dashboard_sql
    and "attempt.created_at" not in dashboard_sql,
    "dashboard_attempt_timestamp_invalid",
)
for required in (
    "SELECT '1h'",
    "SELECT '24h'",
    "SELECT '7d'",
    "'route_matches'",
    "'complete_evaluations'",
    "'near_profitable'",
    "'closest_margin_to_gate_wei'",
    "'rpc_budget_exhaustions'",
    "'rpc_disagreements'",
    "'model_invariant_failures'",
    "'route_ranking_7d'",
    "'size_sweep_7d'",
):
    require(required in dashboard_sql, f"economic_truth_dashboard_missing:{required}")
require("'relevant_inputs'" not in dashboard_sql, "candidate_count_mislabeled_as_relevant_inputs")
for required in (
    "CREATE OR REPLACE VIEW phoenix_live_economic_truth",
    "initiating_transaction_hash",
    "initiating_pool_ids",
    "initiating_swap_direction",
    "active_liquidity_near_current_tick",
    "gross_spread_bps",
    "net_pnl_bps",
    "break_even_spread_bps",
    "fixed_cost_wei",
    "variable_cost_wei",
    "margin_to_profitability_gate_wei",
    "price_divergence_direction",
    "size_elasticity",
    "route_rank",
    "fork_status",
):
    require(required in economic_truth, f"economic_truth_contract_missing:{required}")
require(
    "FROM phoenix_live_economic_truth truth" in dashboard_sql,
    "dashboard_does_not_use_authoritative_economic_truth",
)

for required in (
    "autonomous-control migrate",
    "autonomous-control disarmed-deploy",
    "autonomous-control evidence-start",
    "INSTALL_DISARMED_EVIDENCE_RELEASE_42161",
    "START_DISARMED_EVIDENCE_42161",
    "mark_phase DISARMED_CONTROL_INSTALLED",
    "mark_phase DISARMED_EVIDENCE_STARTED",
    "PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE",
    'compose stop -t 30 live-executor',
    'live-executor started during disarmed deployment',
):
    require(required in deploy, f"disarmed_deploy_contract_missing:{required}")
for forbidden in (
    "live-executor activate",
    "live-executor owner-unpause",
    "compose up -d --no-deps live-executor",
    "LIVE_EXECUTOR_SIGNER_FILE",
    "AUTONOMOUS_ACTIVATED",
    "EXECUTOR_UNPAUSED",
):
    require(forbidden not in deploy, f"normal_deploy_authority_leak:{forbidden}")

operation = deploy.index("\ncompose pull\n")
migrate = deploy.index("autonomous-control migrate", operation)
disarmed = deploy.index("autonomous-control disarmed-deploy", operation)
engine = deploy.index("compose up -d --no-deps phoenix-engine", operation)
burn = deploy.index("run_live_engine_burn_in", operation)
healthcheck = deploy.index('"$deploy_dir/production-healthcheck.sh"', burn)
fail_closed = deploy.index(
    "runtime controls are not fail-closed before evidence-start", healthcheck
)
evidence_start = deploy.index("autonomous-control evidence-start", fail_closed)
evidence_verified = deploy.index(
    "runtime did not enter fail-closed DISARMED_EVIDENCE", evidence_start
)
external_evidence = deploy.index("mark_phase DISARMED_EVIDENCE_STARTED", evidence_verified)
require(
    migrate
    < disarmed
    < engine
    < burn
    < healthcheck
    < fail_closed
    < evidence_start
    < evidence_verified
    < external_evidence,
    "disarmed_release_sequence_invalid",
)

for required in (
    "autonomous-control disarm",
    "DISARM_AUTONOMOUS_LIVE_42161",
    "DISARMED_FAILURE",
):
    require(
        required in rollback or required in control,
        f"rollback_disarm_contract_missing:{required}",
    )
require(
    "live-executor disarm" not in rollback,
    "rollback_uses_signer_mounted_service_for_disarm",
)

for command in (
    '"disarmed-deploy"',
    '"evidence-start"',
    '"create-readiness"',
    '"install-authorization"',
    '"activate-ready-canary"',
    '"evaluate-economic-control"',
    '"supervise-economic-control"',
):
    require(command in control, f"control_command_missing:{command}")
require(
    '"activate" => return Err(' in control,
    "legacy_direct_activation_not_disabled",
)
require("phoenix.live-canary-schema.v5" in control, "schema_v5_not_required")
require(
    "previous.phase != EconomicPhase::DisarmedEvidence" in control,
    "readiness_does_not_require_durable_evidence_phase",
)
require(
    "input.binding.observed_from < previous.updated_at" not in control
    and "binding.observed_from < previous.updated_at" in control,
    "readiness_observation_not_bound_to_evidence_transition",
)
require(
    "economic_control_epoch" in state
    and "economic_control_epoch" in schema
    and "economic_control_epoch" in control,
    "readiness_economic_epoch_binding_missing",
)
for required in (
    "active execution attempt",
    "unresolved receipt reconciliation",
    "evidence-start requires fail-closed global and route controls",
    "clock_timestamp()",
    "disarmed_evidence_started",
):
    require(required in control, f"evidence_start_gate_missing:{required}")

phases = (
    "DISARMED_DEPLOY",
    "DISARMED_EVIDENCE",
    "CANARY_READY",
    "LIVE_CANARY_MIN",
    "LIVE_SCALE_L1",
    "LIVE_SCALE_L2",
    "LIVE_SCALE_L3",
    "LIVE_SCALE_L4",
    "LIVE_SCALE_L5",
    "LIVE_MAX_REVIEWED",
    "COOLDOWN",
    "DISARMED_FAILURE",
)
for phase in phases:
    require(phase in state and phase in schema, f"economic_phase_missing:{phase}")

for amount in (
    "100_000_000_000_000",
    "250_000_000_000_000",
    "500_000_000_000_000",
    "1_000_000_000_000_000",
    "2_500_000_000_000_000",
    "5_000_000_000_000_000",
    "10_000_000_000_000_000",
):
    require(amount in state, f"ladder_amount_missing:{amount}")
for threshold in (
    "MINIMUM_OBSERVATIONS: u64 = 100",
    "MINIMUM_VALID_ACCEPTANCE_BPS: u16 = 9_990",
    "MINIMUM_FORK_PASS_RATE_BPS: u16 = 9_500",
    "MAXIMUM_PREDICTION_ERROR_BPS: u16 = 1_000",
    "MINIMUM_PROMOTION_OUTCOMES: u64 = 20",
    "MINIMUM_SUCCESS_RATE_BPS: u16 = 9_500",
):
    require(threshold in state, f"economic_threshold_missing:{threshold}")

for required in (
    "CREATE TABLE IF NOT EXISTS live_canary.economic_control",
    "CREATE TABLE IF NOT EXISTS live_canary.canary_readiness_records",
    "CREATE TABLE IF NOT EXISTS live_canary.automation_authorizations",
    "CREATE TABLE IF NOT EXISTS live_canary.economic_transitions",
    "economic transitions are immutable",
    "realized_profit_by_route_level",
    "realized_profit_windows",
    "actual_output",
    "actual_balance_delta",
    "fork_simulated_net_pnl",
    "detection_to_submission_latency_ms",
    "receipt_latency_ms",
):
    require(required in schema, f"economic_schema_contract_missing:{required}")

for required in (
    "CREATE_HASH_BOUND_CANARY_READINESS_42161",
    "INSTALL_BOUNDED_AUTOMATION_AUTHORIZATION_42161",
    "ACTIVATE_READY_MIN_CANARY_42161",
    "live-executor owner-unpause",
    "owner-pause",
    "canary_activation_failure",
    "level=MIN input_wei=100000000000000",
):
    require(required in activate, f"owner_activation_boundary_missing:{required}")
require(
    activate.index("activate-ready-canary")
    < activate.index("live-executor owner-unpause")
    < activate.index("compose up -d --no-deps live-executor"),
    "authorized_canary_sequence_invalid",
)
for required in (
    "candidate.status = 'materialized'",
    "candidate.conservative_predicted_net_pnl > 0",
    "fact.verification_status = 'agreed'",
    "fact.independent_verification_status = 'agreed'",
    "result.status = 'passed'",
    "result.simulated_net_pnl > 0",
    "candidate.candidate_expires_at > $4",
    "eligible_rpc_disagreements",
    "unresolved_submissions",
):
    require(required in control, f"activation_request_gate_missing:{required}")
for required in (
    "REQUEST_OWNER_UID = 65532",
    "REQUEST_OWNER_GID = 65532",
    "MAX_REQUEST_BYTES = 256 * 1024",
    "os.O_NOFOLLOW",
    "metadata.st_nlink != 1",
    "request_replayed",
    "materialize-activation-contracts",
    "activate-economic-canary.sh",
):
    require(required in activation_runner, f"activation_runner_contract_missing:{required}")
require("shell=True" not in activation_runner, "activation_runner_shell_escape")
require(
    'PathExistsGlob=/opt/phoenix/evidence/activation-requests/activation-request-*.json'
    in activation_path,
    "activation_path_watch_invalid",
)
for required in (
    "User=root",
    "Group=root",
    "NoNewPrivileges=true",
    "ProtectSystem=strict",
    "economic_activation_runner.py",
):
    require(required in activation_service, f"activation_service_contract_missing:{required}")
require(
    "phoenix-economic-activation.path" in read("scripts/install-phoenix-release-platform.sh")
    and "systemctl enable --now phoenix-economic-activation.path"
    in read("scripts/install-phoenix-release-platform.sh"),
    "activation_runner_not_immutably_installed",
)

for required in (
    "refresh_interval_seconds",
    "'executive'",
    "'funnel'",
    "'economics'",
    "'safety'",
    "'growth'",
    "REALIZED NET PNL AFTER ALL COSTS",
):
    require(required in dashboard_sql, f"dashboard_section_missing:{required}")
require("interval must be 30-60 seconds" in monitor, "monitor_interval_guard_missing")

for phase in (
    "EVIDENCE_MODE_INSTALLED",
    "DISARMED_CONTROL_INSTALLED",
    "DISARMED_EVIDENCE_STARTED",
    "POST_DISARMED_VERIFYING",
    "POST_DISARMED_VERIFIED",
):
    require(phase in release_model, f"release_phase_missing:{phase}")
require(
    '"autonomous_armed": False' in release_gateway
    and '"kill_switch": True' in release_gateway,
    "release_completion_not_disarmed",
)
require("DISARMED_EVIDENCE" in health, "disarmed_health_mode_missing")

for asset in (
    "live-executor/schema/005_closed_loop_economic_control.sql",
    "scripts/activate-economic-canary.sh",
    "scripts/economic-dashboard-loop.sh",
):
    require(asset in assets, f"release_asset_missing:{asset}")
for installed in (
    "005_closed_loop_economic_control.sql",
    "activate-economic-canary.sh",
    "economic-dashboard-loop.sh",
    "economic-dashboard-snapshot.sql",
):
    require(installed in installer, f"release_context_install_missing:{installed}")
PY
  fail "closed-loop release contract validation failed"

echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_OK"
