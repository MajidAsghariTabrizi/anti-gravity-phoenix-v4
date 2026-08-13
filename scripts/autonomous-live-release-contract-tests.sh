#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)

fail() {
  echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_FAILED: $1" >&2
  exit 1
}

for path in \
  compose.prod.yml \
  compose.live-autonomous.yml \
  migrations/012_live_economic_truth.sql \
  migrations/013_economic_loss_ledger.sql \
  migrations/014_exact_source_identity.sql \
  migrations/015_bounded_economic_view_plans.sql \
  live-executor/schema/005_closed_loop_economic_control.sql \
  live-executor/schema/007_aave_economic_diagnostics.sql \
  live-executor/schema/008_revenue_provider_authority.sql \
  live-executor/schema/009_single_primary_provider_authority.sql \
  live-executor/src/economic_control.rs \
  live-executor/src/autonomous_live_control_main.rs \
  live-executor/src/store.rs \
  live-executor/src/revenue.rs \
  atlas-observer/internal/hunter/postgres_sink.go \
  rpc-gateway/src/aave_state.rs \
  scripts/deploy-release.sh \
  scripts/rollback-release.sh \
  scripts/activate-economic-canary.sh \
  scripts/monitor-post-arm-revenue.sh \
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
import hashlib
import re
import sys
from pathlib import Path


root = Path(sys.argv[1])


def read(path: str) -> str:
    return (root / path).read_text(encoding="utf-8")


def sha256(path: str) -> str:
    return hashlib.sha256((root / path).read_bytes()).hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"AUTONOMOUS_LIVE_RELEASE_CONTRACT_INVALID:{message}")


base_compose = read("compose.prod.yml")
compose = read("compose.live-autonomous.yml")
deploy = read("scripts/deploy-release.sh")
rollback = read("scripts/rollback-release.sh")
activate = read("scripts/activate-economic-canary.sh")
post_arm_monitor = read("scripts/monitor-post-arm-revenue.sh")
control = read("live-executor/src/autonomous_live_control_main.rs")
executor_store = read("live-executor/src/store.rs")
revenue_executor = read("live-executor/src/revenue.rs")
observer_sink = read("atlas-observer/internal/hunter/postgres_sink.go")
state = read("live-executor/src/economic_control.rs")
canonical_state = read("rpc-gateway/src/aave_state.rs")
schema = read("live-executor/schema/005_closed_loop_economic_control.sql")
diagnostic_schema = read("live-executor/schema/007_aave_economic_diagnostics.sql")
provider_schema = read("live-executor/schema/008_revenue_provider_authority.sql")
single_primary_schema = read("live-executor/schema/009_single_primary_provider_authority.sql")
health = read("scripts/production-healthcheck.sh")
monitor = read("scripts/economic-dashboard-loop.sh")
dashboard_sql = read("scripts/sql/economic-dashboard-snapshot.sql")
economic_truth = read("migrations/012_live_economic_truth.sql")
economic_loss = read("migrations/013_economic_loss_ledger.sql")
source_identity = read("migrations/014_exact_source_identity.sql")
economic_plan = read("migrations/015_bounded_economic_view_plans.sql")
activation_runner = read("scripts/economic_activation_runner.py")
activation_path = read("deploy/phoenix-economic-activation.path")
activation_service = read("deploy/phoenix-economic-activation.service")
release_model = read("scripts/phoenix_release/model.py")
release_gateway = read("scripts/phoenix_release/gateway.py")
assets = read("scripts/release_assets.py")
installer = read("scripts/install-production-release-context.sh")

require(
    "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;\nSET LOCAL jit = off;"
    in dashboard_sql,
    "dashboard_query_must_disable_jit_inside_read_only_transaction",
)

rpc_service = re.search(
    r"(?ms)^  rpc-gateway:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(rpc_service is not None, "rpc_gateway_service_missing")
base_rpc_service = re.search(
    r"(?ms)^  rpc-gateway:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    base_compose,
)
require(base_rpc_service is not None, "base_rpc_gateway_service_missing")
require(
    'RPC_UPSTREAM_CALL_BURST: "${RPC_UPSTREAM_CALL_BURST:-16}"'
    in base_rpc_service.group("body"),
    "base_rpc_burst_must_fit_cold_dual_provider_aave_screen",
)
require(
    'RPC_PROVIDER_PROBE_INTERVAL_SECONDS: "${RPC_PROVIDER_PROBE_INTERVAL_SECONDS:-300}"'
    in base_rpc_service.group("body"),
    "base_rpc_probe_must_preserve_stable_production_cadence",
)
for reviewed_budget in (
    'RPC_UPSTREAM_CALLS_PER_SECOND: "16"',
    'RPC_UPSTREAM_CALL_BURST: "64"',
    'RPC_STATE_REQUESTS_PER_MINUTE: "60"',
):
    require(
        reviewed_budget in rpc_service.group("body"),
        f"live_rpc_budget_not_literal:{reviewed_budget}",
    )

observer_service = re.search(
    r"(?ms)^  atlas-observer:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(observer_service is not None, "atlas_observer_service_missing")
for reviewed_budget in (
    'PHOENIX_EXACT_STATE_REQUEST_BUDGET_PER_MINUTE: "60"',
    'PHOENIX_EXACT_DISCOVERY_RESERVE_PER_MINUTE: "24"',
):
    require(
        reviewed_budget in observer_service.group("body"),
        f"observer_exact_budget_not_literal:{reviewed_budget}",
    )

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
for required in (
    "PHOENIX_ACTIVATION_REQUEST_OUTBOX: /activation-outbox",
    "source: /opt/phoenix/evidence/activation-requests",
):
    require(required in control_service, f"post_arm_recovery_control_binding_missing:{required}")

live_service = re.search(
    r"(?ms)^  live-executor:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(live_service is not None, "live_executor_service_missing")
require(
    "phoenix-live-executor-signer" in live_service.group("body"),
    "authorized_executor_signer_mount_missing",
)
for required in (
    "PHOENIX_RELEASE_SHA: ${PHOENIX_RELEASE_SHA:?PHOENIX_RELEASE_SHA is required}",
):
    require(required in live_service.group("body"), f"owner_live_preflight_binding_missing:{required}")

for required in (
    "owner-live-preflight",
    "reconciliation-status",
    "post_arm_acceptance_failed",
    "owner-pause",
    "autonomous-control disarm",
    'production_mode.py" shadow',
    "duration_seconds=${2:-900}",
    "monitor duration must be between 600 and 900 seconds",
):
    require(required in post_arm_monitor, f"post_arm_monitor_contract_missing:{required}")
require(
    "owner-configured-preflight" not in post_arm_monitor,
    "post_arm_monitor_uses_paused_only_preflight",
)
require(
    post_arm_monitor.index("autonomous-control disarm")
    < post_arm_monitor.index("live-executor owner-pause")
    < post_arm_monitor.index('production_mode.py" shadow'),
    "post_arm_monitor_fail_close_order_invalid",
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
    "PHOENIX_CURRENT_RELEASE_PATH: /release-identity/current-release",
    "PHOENIX_RELEASE_ASSETS_PATH: /release-identity/release-assets.sha",
    "source: /opt/phoenix/deploy/current-release",
    "source: /opt/phoenix/deploy/release-assets.sha",
    "read_only: true",
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
    "PHOENIX_ECONOMIC_DASHBOARD_QUERY_TIMEOUT_SECONDS: \"30\"" in compose,
    "dashboard_query_timeout_not_30_seconds",
)
require(
    "find /evidence/latest-dashboard.json -maxdepth 0 -type f -size +0c -mmin -3"
    in compose,
    "dashboard_health_does_not_require_fresh_output",
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
    "'loss_ledger_7d'",
    "'daily_attack_surface_7d'",
    "'loss_cause_contract'",
):
    require(required in dashboard_sql, f"economic_truth_dashboard_missing:{required}")
require("'relevant_inputs'" not in dashboard_sql, "candidate_count_mislabeled_as_relevant_inputs")
require(
    "FROM phoenix_daily_economic_attack_surface" not in dashboard_sql,
    "dashboard_reexpands_unbounded_daily_attack_surface",
)
require(
    "FROM phoenix_live_economic_loss_ledger" not in dashboard_sql,
    "dashboard_reexpands_correlated_loss_ledger",
)
for required in (
    "bounded_loss_truth AS MATERIALIZED",
    "bounded_counterfactuals AS",
    "bounded_reverse_routes AS",
    "truth.classified_at >= now() - interval '7 days'",
):
    require(required in dashboard_sql, f"bounded_loss_snapshot_missing:{required}")
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
    "CREATE OR REPLACE VIEW phoenix_live_economic_loss_ledger",
    "CREATE OR REPLACE VIEW phoenix_daily_economic_attack_surface",
    "primary_loss_cause",
    "secondary_loss_causes",
    "missing_break_even_amount_wei",
    "best_counterfactual_route_fingerprint",
    "recoverable_pnl_if_bottleneck_removed_wei",
    "recommended_next_action",
):
    require(required in economic_loss, f"economic_loss_contract_missing:{required}")
require(
    "NOT MATERIALIZED" not in economic_truth
    and "NOT MATERIALIZED" not in economic_loss,
    "applied_economic_migration_content_changed",
)
require(
    sha256("migrations/012_live_economic_truth.sql")
    == "327dfc65fbe60b10b54b975777b6e6d95dbf165a9e1fb5d46adadc250d0017c7",
    "applied_migration_012_checksum_changed",
)
require(
    sha256("migrations/013_economic_loss_ledger.sql")
    == "453957be9f6c9eaa35c87b98b9e9466e9ab419d42195bdc8271623e605ad478c",
    "applied_migration_013_checksum_changed",
)
for required in (
    "phoenix_live_economic_truth",
    "phoenix_live_economic_loss_ledger",
    "phoenix_daily_economic_attack_surface",
    "ARRAY['size_points', 'facts']",
    "ARRAY['numeric_truth', 'contextual', 'caused']",
    "ARRAY['ranked']",
    "RAISE EXCEPTION 'expected CTE % is missing from %'",
):
    require(required in economic_plan, f"bounded_economic_plan_missing:{required}")
for cause in (
    "wrong_direction",
    "route_not_in_universe",
    "gross_spread_negative",
    "dex_fees_dominated",
    "fixed_gas_dominated",
    "l1_data_fee_dominated",
    "flash_fee_dominated",
    "price_impact_dominated",
    "liquidity_utilization_limit",
    "tick_crossing_limit",
    "state_incomplete",
    "state_stale",
    "quote_stale",
    "candidate_stale",
    "rpc_budget_exhausted",
    "rpc_disagreement",
    "fork_revert",
    "fork_pnl_below_gate",
    "candidate_decay",
    "contract_guard_rejection",
    "unknown",
):
    require(f"'{cause}'" in economic_loss, f"economic_loss_cause_missing:{cause}")
for required in (
    "CREATE TABLE IF NOT EXISTS source_event_identities",
    "CREATE TABLE IF NOT EXISTS source_block_enrichments",
    "CREATE TABLE IF NOT EXISTS source_enrichment_attempts",
    "CREATE TABLE IF NOT EXISTS transaction_boundary_state_evidence",
    "source_identity_unresolved_block_check",
    "source_identity_event_hash_unique",
    "source_block_identity_fk",
    "source_block_status_evidence_check",
    "transaction_boundary_enrichment_fk",
    "transaction_boundary_completeness_check",
    "prestate_hash",
    "state_diff_hash",
    "reject_exact_source_evidence_mutation",
):
    require(required in source_identity, f"source_identity_contract_missing:{required}")

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
    '--field intentional_absence',
    'assert_intentional_absence ||',
    'LIVE_EXECUTOR_HUNTING_STANDBY: "false"',
    'LIVE_EXECUTOR_ARMED: "true"',
    'LIVE_EXECUTOR_KILL_SWITCH: "false"',
    'LIVE_EXECUTOR_MAX_ATLAS_BID_WEI',
    'mark_phase HUNTING_STANDBY_STARTED',
    'hunting standby changed fail-closed runtime controls',
    'revenue_lanes = status["revenue_lanes"]',
    'and all(lane["armed"] is False for lane in revenue_lanes)',
    'and all(lane["kill_switch"] is True for lane in revenue_lanes)',
):
    require(required in deploy or required in compose, f"disarmed_deploy_contract_missing:{required}")
for forbidden in (
    "live-executor activate",
    "live-executor owner-unpause",
    "LIVE_EXECUTOR_SIGNER_FILE",
    "AUTONOMOUS_ACTIVATED",
    "EXECUTOR_UNPAUSED",
):
    require(forbidden not in deploy, f"normal_deploy_authority_leak:{forbidden}")

operation = deploy.index("\ncompose pull $pull_services\n")
observer_quiesced = deploy.index("for schema_client in atlas-observer", operation)
engine_quiesced = deploy.index(
    'compose_shadow_with_release_env "$rollback_release_env"',
    observer_quiesced,
)
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
standby = deploy.index("compose up -d --no-deps live-executor", external_evidence)
standby_controls = deploy.index(
    "hunting standby changed fail-closed runtime controls", standby
)
require(
    observer_quiesced
    < engine_quiesced
    < migrate
    < disarmed
    < engine
    < burn
    < healthcheck
    < fail_closed
    < evidence_start
    < evidence_verified
    < external_evidence
    < standby
    < standby_controls,
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
    '"arm-revenue-lanes"',
    '"evaluate-economic-control"',
    '"supervise-economic-control"',
):
    require(command in control, f"control_command_missing:{command}")
require(
    '"activate" => return Err(' in control,
    "legacy_direct_activation_not_disabled",
)
rpc_disagreement_branch = re.search(
    r"else if evidence\.rpc_disagreements >= 2 \{(?P<body>.*?)\n\s*\} else if",
    control,
    re.DOTALL,
)
require(rpc_disagreement_branch is not None, "provider_disagreement_branch_missing")
require(
    'fail_close_execution_authority(&mut transaction, "rpc_disagreement"'
    in rpc_disagreement_branch.group("body"),
    "provider_disagreement_must_atomically_close_execution_authority",
)
require(
    "WHERE lane IN ('atlas_solver', 'aave_liquidation')" in executor_store
    and "if revenue_lanes.rows_affected() != 2" in executor_store,
    "runtime_fail_close_must_update_the_exact_revenue_lane_pair",
)
require(
    "UPDATE live_canary.revenue_lane_controls" not in revenue_executor
    and revenue_executor.count("fail_close_execution_authority(") >= 2,
    "atlas_failure_paths_must_not_close_only_one_revenue_lane",
)
atlas_failed_start = revenue_executor.index("AtlasOutcome::Failed { transaction_hash } =>")
atlas_failed_end = revenue_executor.index("AtlasOutcome::Reconciled {", atlas_failed_start)
atlas_failed = revenue_executor[atlas_failed_start:atlas_failed_end]
require(
    atlas_failed.index("self.pool.begin()")
    < atlas_failed.index('fail_close_execution_authority(')
    < atlas_failed.index('"atlas_inclusion_failed"')
    < atlas_failed.index("status='lost'")
    < atlas_failed.index("release_lock(")
    < atlas_failed.index("transaction.commit()"),
    "atlas_inclusion_failure_must_fail_close_then_preserve_lost_and_release_lock",
)
kill_unknown_start = revenue_executor.index("async fn kill_unknown(")
kill_unknown_end = revenue_executor.index("\n}\n\nasync fn release_lock(", kill_unknown_start)
kill_unknown = revenue_executor[kill_unknown_start:kill_unknown_end]
require(
    kill_unknown.index("self.pool.begin()")
    < kill_unknown.index("fail_close_execution_authority(&mut transaction, reason")
    < kill_unknown.index("status = 'submission_unknown'")
    < kill_unknown.index("transaction.commit()"),
    "atlas_unknown_submission_must_fail_close_then_preserve_unknown_state",
)
require(
    "release_lock(" not in kill_unknown,
    "atlas_unknown_submission_must_retain_the_global_submission_lock",
)
require(
    "converge_revenue_provider_authority(&pool)" in control
    and '"provider_current_class_failure_streak"' in control
    and '"revenue_lane_authority_diverged"' in control,
    "revenue_supervisor_must_converge_partial_and_persistent_provider_failures",
)
record_signal_start = observer_sink.index(
    "func (s *PostgresSignalSink) RecordAaveSignal("
)
record_signal_end = observer_sink.index(
    "func normalizeCandidateAuthority(", record_signal_start
)
record_signal = observer_sink[record_signal_start:record_signal_end]
normalizer_end = observer_sink.index("func withoutCandidateAuthority(", record_signal_end)
normalizer = observer_sink[record_signal_end:normalizer_end]
require(
    record_signal.index("s.pool.Begin(ctx)")
    < record_signal.index("normalizeCandidateAuthority(ctx, tx, record)")
    < record_signal.index("insertExecutionCandidate(")
    < record_signal.index("insertAtlasCandidate(")
    < record_signal.index("tx.Commit(ctx)"),
    "observer_candidate_artifacts_must_follow_locked_authority_normalization",
)
require(
    normalizer.index("FROM live_canary.economic_control")
    < normalizer.index("FOR UPDATE")
    < normalizer.index("FROM live_canary.revenue_lane_controls")
    < normalizer.rindex("FOR UPDATE")
    < normalizer.index("validatedAaveLiveMaximumInputAmount(states)"),
    "observer_candidate_authority_must_lock_economic_then_exact_revenue_lanes",
)
require(
    "approval_digest, status, created_at, updated_at" in observer_sink
    and "'approved', $26, $26" in observer_sink,
    "aave_request_evidence_time_must_bind_exact_completion",
)
require("phoenix.live-canary-schema.v7" in control, "schema_v7_not_required")
require("phoenix.live-canary-schema.v8" in control, "schema_v8_not_required")
require("phoenix.live-canary-schema.v9" in control, "schema_v9_not_required")
require("revenue_provider_authority" in provider_schema, "provider_authority_schema_missing")
require("exact_execution_ready" in provider_schema, "provider_execution_gate_missing")
require("request_evidence_not_before" in provider_schema, "provider_request_evidence_floor_missing")
require(
    "production-nownodes-arbitrum" in single_primary_schema
    and "sample_1_confirmation_provider IS NULL" in single_primary_schema
    and "SINGLE_PRIMARY_FORK_VERIFIED" in single_primary_schema,
    "single_primary_provider_schema_contract_missing",
)
require(
    'RPC_AUTHORITY_MODE: single_primary' in base_compose
    and '"rpc_authority_mode"' in read("atlas-observer/cmd/atlas-aave-hunter/main.go")
    and '"single_primary"' in read("atlas-observer/cmd/atlas-aave-hunter/main.go"),
    "single_primary_health_contract_missing",
)
require("provider.exact_execution_ready" in executor_store, "aave_claim_missing_provider_gate")
require("provider.exact_execution_ready" in revenue_executor, "atlas_claim_missing_provider_gate")
require("r.created_at >= $9" in executor_store, "aave_claim_missing_post_failure_evidence_floor")
require("r.created_at >= $3" in revenue_executor, "atlas_claim_missing_post_failure_evidence_floor")
require("revenue_provider_authority" in observer_sink, "candidate_sink_missing_provider_gate")
require("provider_authority_auto_recovered" in control, "provider_recovery_transition_missing")
require("failure_control_epoch" in control, "provider_recovery_epoch_binding_missing")
for required in (
    "runtime_preflight_from_environment()",
    "exact_release_identity()",
    "provider_recovery_samples(payload)",
    "failure_transition_at",
    "active_attempts",
    "unresolved_submissions",
    "active_atlas",
    "lock_free",
    "current_daily_loss",
    "executor_paused",
    "recovery_attempted_total",
    "recovery_succeeded_total",
    "recovery_blocked_total",
    "recovery_evidence_hash",
    "WHERE lane IN ('aave_liquidation','atlas_solver') AND NOT armed AND kill_switch",
):
    require(required in control, f"provider_recovery_contract_missing:{required}")
for required in (
    "exact_diagnostics JSONB",
    "phoenix.aave-exact-diagnostics.v1",
    "jsonb_array_length(exact_diagnostics->'top_diagnostics') <= 3",
    "atlas_auction_ingress",
    "rejection_reason",
    "live_canary_revenue_signal_source_observed",
    "live_canary_atlas_ingress_observed",
    "live_canary_atlas_solver_request_created",
    "live_canary_execution_request_route_created",
):
    require(required in diagnostic_schema, f"aave_diagnostic_schema_missing:{required}")
for required in (
    "Generic Phoenix DEX Engine only",
    "simulation_evidence_insufficient",
    "revenue_lane_windows",
    "aave_exact_7d",
    "atlas_solver_7d",
    "diagnostic_rejection_occurrences",
    "unexpected_generic_reason_rows",
):
    require(required in dashboard_sql, f"lane_dashboard_contract_missing:{required}")
for required in (
    "ARM_ATLAS_AAVE_LIVE_42161",
    "WHERE lane IN ('atlas_solver', 'aave_liquidation')",
    "revenue lane activation is blocked by an active submission",
    "revenue lane activation is blocked by an active attempt",
    "revenue lane activation is blocked by an active Atlas request",
    "revenue lane activation requires fresh exact provider authority",
    "disarmed deployment is blocked by an active revenue submission",
    "disarmed deployment is blocked by an active Atlas request",
):
    require(required in control, f"revenue_lane_activation_contract_missing:{required}")
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
    require(phase in canonical_state and phase in schema, f"economic_phase_missing:{phase}")

require(
    "pub use rpc_gateway::aave_state::{" in state
    and "EconomicPhase" in state
    and "SizeLevel" in state
    and "MAXIMUM_REVIEWED_INPUT_WEI" in state,
    "canonical_economic_state_reexport_missing",
)

for amount in (
    "100_000_000_000_000",
    "250_000_000_000_000",
    "500_000_000_000_000",
    "1_000_000_000_000_000",
    "2_500_000_000_000_000",
    "5_000_000_000_000_000",
    "10_000_000_000_000_000",
):
    require(amount in canonical_state, f"ladder_amount_missing:{amount}")
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
require("query timeout must be 5-60 seconds" in monitor, "monitor_timeout_guard_missing")
require(
    'statement_timeout=${query_timeout}s' in monitor and "lock_timeout=5s" in monitor,
    "monitor_query_timeout_not_enforced",
)

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

postgres_image=postgres@sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777
test_suffix="$$"
postgres_container="phoenix-economic-schema-postgres-$test_suffix"
monitor_container="phoenix-economic-schema-monitor-$test_suffix"
test_network="phoenix-economic-schema-$test_suffix"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-economic-schema.XXXXXX")
monitor_output="$test_root/evidence"
monitor_health_cmd='find /evidence/latest-dashboard.json -maxdepth 0 -type f -size +0c -mmin -3 -print -quit 2>/dev/null | grep -q .'

cleanup_schema_compatibility_test() {
  docker rm -f -v "$monitor_container" "$postgres_container" >/dev/null 2>&1 || true
  docker network rm "$test_network" >/dev/null 2>&1 || true
  case "$test_root" in
    "${TMPDIR:-/tmp}"/phoenix-economic-schema.*)
      rm -rf -- "$test_root"
      ;;
  esac
}
trap cleanup_schema_compatibility_test EXIT HUP INT TERM

command -v docker >/dev/null 2>&1 ||
  fail "docker is required for economic dashboard schema compatibility"
mkdir "$monitor_output"
chmod 0777 "$monitor_output"

if PHOENIX_ECONOMIC_DASHBOARD_QUERY_TIMEOUT_SECONDS=4 \
  POSTGRES_DSN=postgres://invalid.invalid/invalid \
  "$repo_root/scripts/economic-dashboard-loop.sh" >/dev/null 2>&1
then
  fail "economic monitor accepted a query timeout below the fail-closed minimum"
fi

printf '%s\n' '{}' >"$monitor_output/latest-dashboard.json"
touch -d '10 minutes ago' "$monitor_output/latest-dashboard.json"
if docker run --rm \
  --network none \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -v "$monitor_output:/evidence:ro" \
  --entrypoint /bin/sh \
  "$postgres_image" \
  -c "$monitor_health_cmd"
then
  fail "economic monitor health accepted a nonempty stale dashboard"
fi
touch "$monitor_output/latest-dashboard.json"
docker run --rm \
  --network none \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -v "$monitor_output:/evidence:ro" \
  --entrypoint /bin/sh \
  "$postgres_image" \
  -c "$monitor_health_cmd" >/dev/null ||
  fail "economic monitor health rejected a nonempty fresh dashboard"
rm -f "$monitor_output/latest-dashboard.json"

docker network create --internal "$test_network" >/dev/null
docker run -d \
  --name "$postgres_container" \
  --network "$test_network" \
  --tmpfs /var/lib/postgresql/data:rw,nosuid,nodev,size=2g \
  -e POSTGRES_USER=phoenix_test \
  -e POSTGRES_PASSWORD=phoenix_test_password \
  -e POSTGRES_DB=phoenix_test \
  "$postgres_image" >/dev/null

postgres_ready=false
attempt=0
while [ "$attempt" -lt 30 ]; do
  if docker logs "$postgres_container" 2>&1 |
    grep -Fq "PostgreSQL init process complete; ready for start up." &&
    docker exec "$postgres_container" \
    pg_isready -U phoenix_test -d phoenix_test >/dev/null 2>&1
  then
    postgres_ready=true
    break
  fi
  attempt=$((attempt + 1))
  sleep 1
done
[ "$postgres_ready" = true ] ||
  fail "historical schema PostgreSQL did not become ready"

for migration in "$repo_root"/migrations/*.sql; do
  case "${migration##*/}" in
    012_*) break ;;
  esac
  docker exec -i "$postgres_container" \
    psql -X -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test \
    <"$migration" >/dev/null ||
    fail "historical migration failed: ${migration##*/}"
done
for schema in "$repo_root"/live-executor/schema/*.sql; do
  case "${schema##*/}" in
    007_*)
      # Seed the supplied 581,496-row Production observation at a rounded-up
      # 600,000-row v6 cohort before v7. These lightweight prefilter rows prove
      # the additive column/index migration against the existing table scale;
      # the bounded Exact JSON cohort is added after v7 below.
      docker exec -i "$postgres_container" \
        psql -X -q -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test <<'SQL' >/dev/null ||
INSERT INTO live_canary.revenue_hunting_signals(
  signal_id, signal_identity, source_lane, source_cursor, block_number,
  block_hash, retained_profit_floor, terminal_outcome, evidence_hash,
  observed_at
)
SELECT
  md5('production-scale-aave-' || value)::uuid,
  'production-scale-aave-' || value,
  'aave_liquidation', value, 1000000 + value,
  '0x' || repeat('0', 64), 100, 'prefiltered',
  md5('production-scale-evidence-a-' || value) ||
    md5('production-scale-evidence-b-' || value),
  now() - ((value % 604800) * interval '1 second')
FROM generate_series(1, 600000) AS generated(value);
ANALYZE live_canary.revenue_hunting_signals;
SQL
        fail "Production-scale v6 Aave signal fixture was rejected"
      ;;
  esac
  docker exec -i "$postgres_container" \
    psql -X -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test \
    <"$schema" >/dev/null ||
    fail "live schema failed: ${schema##*/}"
done

docker exec -i "$postgres_container" \
  psql -X -q -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test <<'SQL' >/dev/null ||
DO $$
DECLARE
  row_count BIGINT;
  gate_ready BOOLEAN;
BEGIN
  SELECT count(*), bool_or(exact_execution_ready)
  INTO row_count, gate_ready
  FROM live_canary.revenue_provider_authority
  WHERE singleton;
  IF row_count <> 1 OR gate_ready THEN
    RAISE EXCEPTION 'provider authority singleton did not install fail closed';
  END IF;

  BEGIN
    UPDATE live_canary.revenue_provider_authority
    SET exact_execution_ready = true, recovery_status = 'ready'
    WHERE singleton;
    RAISE EXCEPTION 'provider authority opened without three Exact samples';
  EXCEPTION WHEN check_violation THEN NULL;
  END;

  UPDATE live_canary.revenue_provider_authority
  SET recovery_status = 'ready', sample_count = 3,
      sample_1_at = now() - interval '3 seconds',
      sample_1_primary_provider = 'production-nownodes-arbitrum',
      sample_1_confirmation_provider = NULL,
      sample_2_at = now() - interval '2 seconds',
      sample_2_primary_provider = 'production-nownodes-arbitrum',
      sample_2_confirmation_provider = NULL,
      sample_3_at = now() - interval '1 second',
      sample_3_primary_provider = 'production-nownodes-arbitrum',
      sample_3_confirmation_provider = NULL,
      exact_execution_ready = true
  WHERE singleton;

  IF NOT (SELECT exact_execution_ready AND sample_count = 3
          FROM live_canary.revenue_provider_authority WHERE singleton) THEN
    RAISE EXCEPTION 'three Exact samples did not satisfy the provider authority contract';
  END IF;

  UPDATE live_canary.revenue_provider_authority
  SET exact_execution_ready = false, recovery_status = 'collecting',
      sample_count = 0,
      sample_1_at = NULL, sample_1_primary_provider = NULL, sample_1_confirmation_provider = NULL,
      sample_2_at = NULL, sample_2_primary_provider = NULL, sample_2_confirmation_provider = NULL,
      sample_3_at = NULL, sample_3_primary_provider = NULL, sample_3_confirmation_provider = NULL
  WHERE singleton;
  IF NOT EXISTS (
    SELECT 1 FROM live_canary.schema_contract
    WHERE version = 'phoenix.live-canary-schema.v9'
  ) THEN
    RAISE EXCEPTION 'schema v9 marker missing';
  END IF;
END;
$$;
SQL
  fail "revenue provider authority schema contract was rejected"

docker exec -i "$postgres_container" \
  psql -X -q -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test <<'SQL' >/dev/null ||
BEGIN;
DO $$
DECLARE
  rejected BOOLEAN;
BEGIN
  rejected := false;
  BEGIN
    INSERT INTO live_canary.revenue_hunting_signals(
      signal_id, signal_identity, source_lane, block_number, block_hash,
      retained_profit_floor, terminal_outcome, exact_diagnostics,
      evidence_hash, observed_at
    ) VALUES (
      '00000000-0000-0000-0000-000000000711', 'invalid-empty-diagnostics',
      'aave_liquidation', 1, '0x' || repeat('a', 64), 1,
      'economic_rejection', '{}'::jsonb, repeat('a', 64), now()
    );
  EXCEPTION WHEN check_violation THEN rejected := true;
  END;
  IF NOT rejected THEN RAISE EXCEPTION 'empty diagnostics were accepted'; END IF;

  rejected := false;
  BEGIN
    INSERT INTO live_canary.revenue_hunting_signals(
      signal_id, signal_identity, source_lane, block_number, block_hash,
      retained_profit_floor, terminal_outcome, exact_diagnostics,
      evidence_hash, observed_at
    ) VALUES (
      '00000000-0000-0000-0000-000000000712', 'invalid-missing-counts',
      'aave_liquidation', 1, '0x' || repeat('a', 64), 1,
      'economic_rejection', '{"schema":"phoenix.aave-exact-diagnostics.v1"}'::jsonb,
      repeat('b', 64), now()
    );
  EXCEPTION WHEN check_violation THEN rejected := true;
  END;
  IF NOT rejected THEN RAISE EXCEPTION 'missing rejection counts were accepted'; END IF;

  rejected := false;
  BEGIN
    INSERT INTO live_canary.revenue_hunting_signals(
      signal_id, signal_identity, source_lane, block_number, block_hash,
      retained_profit_floor, terminal_outcome, exact_diagnostics,
      evidence_hash, observed_at
    ) VALUES (
      '00000000-0000-0000-0000-000000000713', 'invalid-missing-schema',
      'aave_liquidation', 1, '0x' || repeat('a', 64), 1,
      'economic_rejection', '{"rejection_counts":{}}'::jsonb,
      repeat('c', 64), now()
    );
  EXCEPTION WHEN check_violation THEN rejected := true;
  END;
  IF NOT rejected THEN RAISE EXCEPTION 'missing diagnostic schema was accepted'; END IF;

  rejected := false;
  BEGIN
    INSERT INTO live_canary.revenue_hunting_signals(
      signal_id, signal_identity, source_lane, block_number, block_hash,
      retained_profit_floor, terminal_outcome, exact_diagnostics,
      evidence_hash, observed_at
    ) VALUES (
      '00000000-0000-0000-0000-000000000714', 'invalid-diagnostic-lane',
      'atlas_solver', 1, '0x' || repeat('a', 64), 1,
      'economic_rejection',
      '{"schema":"phoenix.aave-exact-diagnostics.v1","rejection_counts":{}}'::jsonb,
      repeat('d', 64), now()
    );
  EXCEPTION WHEN check_violation THEN rejected := true;
  END;
  IF NOT rejected THEN RAISE EXCEPTION 'wrong-lane diagnostics were accepted'; END IF;
END
$$;
INSERT INTO live_canary.revenue_hunting_signals(
  signal_id, signal_identity, source_lane, block_number, block_hash,
  retained_profit_floor, terminal_outcome, exact_diagnostics,
  evidence_hash, observed_at
) VALUES (
  '00000000-0000-0000-0000-000000000715', 'valid-minimal-diagnostics',
  'aave_liquidation', 1, '0x' || repeat('a', 64), 1,
  'economic_rejection',
  '{"schema":"phoenix.aave-exact-diagnostics.v1","rejection_counts":{}}'::jsonb,
  repeat('e', 64), now()
);
ROLLBACK;
SQL
  fail "Aave diagnostic JSON constraint accepted malformed evidence"

dashboard_index_plan=$(
  docker exec -i "$postgres_container" \
    psql -X -q -A -t -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test <<'SQL'
SET enable_seqscan = off;
EXPLAIN (COSTS OFF)
SELECT observed_at FROM live_canary.revenue_hunting_signals
WHERE source_lane = 'aave_liquidation' AND observed_at >= now() - interval '7 days';
EXPLAIN (COSTS OFF)
SELECT observed_at FROM live_canary.atlas_auction_ingress
WHERE observed_at >= now() - interval '7 days';
EXPLAIN (COSTS OFF)
SELECT created_at FROM live_canary.atlas_solver_requests
WHERE created_at >= now() - interval '7 days';
EXPLAIN (COSTS OFF)
SELECT created_at FROM live_canary.execution_requests
WHERE route_type = 'AAVE_LIQUIDATION_V1' AND created_at >= now() - interval '7 days';
SQL
) || fail "lane dashboard index plans could not be explained"
for expected_index in \
  live_canary_revenue_signal_source_observed \
  live_canary_atlas_ingress_observed \
  live_canary_atlas_solver_request_created \
  live_canary_execution_request_route_created
do
  printf '%s\n' "$dashboard_index_plan" | grep -Fq "$expected_index" ||
    fail "lane dashboard index is not usable: $expected_index"
done

historical_view=$(
  docker exec "$postgres_container" \
    psql -X -q -A -t -U phoenix_test -d phoenix_test \
    -c "SELECT to_regclass('public.phoenix_live_economic_truth') IS NULL"
)
[ "$historical_view" = t ] ||
  fail "historical schema unexpectedly contains phoenix_live_economic_truth"

{
  printf '%s\n' 'BEGIN TRANSACTION READ ONLY;'
  cat "$repo_root/scripts/sql/economic-dashboard-snapshot.sql"
  printf '%s\n' 'ROLLBACK;'
} | docker exec -i "$postgres_container" \
  psql -X -q -A -t -U phoenix_test -d phoenix_test \
  >"$test_root/historical-dashboard.json" ||
  fail "dashboard snapshot rejected the historical Production schema"
PYTHONDONTWRITEBYTECODE=1 python3 -I -B - "$test_root/historical-dashboard.json" <<'PY' ||
import json
import sys
from pathlib import Path

document = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if document["economics"]["size_sweep_7d"] != []:
    raise SystemExit("historical size sweep must be empty")
if set(document["funnel"]["windows"]) != {"1h", "24h", "7d"}:
    raise SystemExit("historical dashboard funnel is incomplete")
revenue = document["funnel"].get("revenue_lane_windows", {})
if set(revenue) != {"aave_liquidation", "atlas_solver"}:
    raise SystemExit("historical revenue-lane funnel is incomplete")
if any(set(windows) != {"1h", "24h", "7d"} for windows in revenue.values()):
    raise SystemExit("historical revenue-lane windows are incomplete")
if "Generic Phoenix DEX Engine only" not in document["funnel"]["semantics"].get("scope", ""):
    raise SystemExit("Generic funnel scope is ambiguous")
if "never an Aave fork" not in document["funnel"]["semantics"].get(
    "simulation_evidence_insufficient", ""
):
    raise SystemExit("Generic simulation rejection was mislabeled as Aave evidence")
if set(document["executive"].get("lane_authority", {})) != {
    "generic_dex", "aave_liquidation", "atlas_solver"
}:
    raise SystemExit("lane authority is not displayed independently")
PY
  fail "historical dashboard snapshot contract failed"

docker run -d \
  --name "$monitor_container" \
  --network "$test_network" \
  --user 1000:1000 \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m \
  --health-cmd "$monitor_health_cmd" \
  --health-interval 1s \
  --health-timeout 3s \
  --health-retries 20 \
  -e POSTGRES_DSN=postgres://phoenix_test:phoenix_test_password@"$postgres_container":5432/phoenix_test \
  -e PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS=30 \
  -e PHOENIX_ECONOMIC_DASHBOARD_QUERY_TIMEOUT_SECONDS=30 \
  -e PHOENIX_ECONOMIC_DASHBOARD_SQL=/opt/phoenix/economic-dashboard-snapshot.sql \
  -e PHOENIX_ECONOMIC_DASHBOARD_OUTPUT=/evidence/latest-dashboard.json \
  -v "$repo_root/scripts/economic-dashboard-loop.sh:/opt/phoenix/economic-dashboard-loop.sh:ro" \
  -v "$repo_root/scripts/sql/economic-dashboard-snapshot.sql:/opt/phoenix/economic-dashboard-snapshot.sql:ro" \
  -v "$monitor_output:/evidence" \
  --entrypoint /bin/sh \
  "$postgres_image" \
  /opt/phoenix/economic-dashboard-loop.sh >/dev/null

monitor_healthy=false
attempt=0
while [ "$attempt" -lt 30 ]; do
  monitor_health=$(
    docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' \
      "$monitor_container"
  )
  if [ "$monitor_health" = healthy ]; then
    monitor_healthy=true
    break
  fi
  [ "$monitor_health" != unhealthy ] ||
    fail "economic monitor became unhealthy on historical schema"
  attempt=$((attempt + 1))
  sleep 1
done
[ "$monitor_healthy" = true ] ||
  fail "economic monitor did not become healthy on historical schema"
[ "$(docker exec "$monitor_container" id -u)" = 1000 ] ||
  fail "economic monitor does not run as uid 1000"
[ "$(stat -c '%u:%g:%a:%h' "$monitor_output/latest-dashboard.json")" = "1000:1000:640:1" ] ||
  fail "economic monitor dashboard ownership or mode is invalid"
host_sql_sha=$(sha256sum "$repo_root/scripts/sql/economic-dashboard-snapshot.sql" | cut -d' ' -f1)
monitor_sql_sha=$(
  docker exec "$monitor_container" \
    sha256sum /opt/phoenix/economic-dashboard-snapshot.sql |
    cut -d' ' -f1
)
[ "$host_sql_sha" = "$monitor_sql_sha" ] ||
  fail "economic monitor did not mount the candidate SQL content"
docker rm -f -v "$monitor_container" >/dev/null

for pass in first idempotent; do
  for migration in \
    "$repo_root/migrations/012_live_economic_truth.sql" \
    "$repo_root/migrations/013_economic_loss_ledger.sql" \
    "$repo_root/migrations/014_exact_source_identity.sql" \
    "$repo_root/migrations/015_bounded_economic_view_plans.sql"
  do
    docker exec -i "$postgres_container" \
      psql -X -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test \
      <"$migration" >/dev/null ||
      fail "economic truth migration $pass application failed: ${migration##*/}"
  done
done
upgraded_view=$(
  docker exec "$postgres_container" \
    psql -X -q -A -t -U phoenix_test -d phoenix_test \
    -c "SELECT to_regclass('public.phoenix_live_economic_truth') IS NOT NULL"
)
[ "$upgraded_view" = t ] ||
  fail "migration 012 did not create phoenix_live_economic_truth"
upgraded_loss_views=$(
  docker exec "$postgres_container" \
    psql -X -q -A -t -U phoenix_test -d phoenix_test \
    -c "SELECT to_regclass('public.phoenix_live_economic_loss_ledger') IS NOT NULL
             AND to_regclass('public.phoenix_daily_economic_attack_surface') IS NOT NULL"
)
[ "$upgraded_loss_views" = t ] ||
  fail "migration 013 did not create the economic loss views"
bounded_economic_plans=$(
  docker exec "$postgres_container" \
    psql -X -q -A -t -U phoenix_test -d phoenix_test \
    -c "SELECT pg_get_viewdef('public.phoenix_live_economic_truth'::regclass, true)
                  ~* 'size_points[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED'
             AND pg_get_viewdef('public.phoenix_live_economic_truth'::regclass, true)
                  ~* 'facts[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED'
             AND pg_get_viewdef('public.phoenix_live_economic_loss_ledger'::regclass, true)
                  ~* 'numeric_truth[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED'
             AND pg_get_viewdef('public.phoenix_live_economic_loss_ledger'::regclass, true)
                  ~* 'contextual[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED'
             AND pg_get_viewdef('public.phoenix_live_economic_loss_ledger'::regclass, true)
                  ~* 'caused[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED'
             AND pg_get_viewdef('public.phoenix_daily_economic_attack_surface'::regclass, true)
                  ~* 'ranked[[:space:]]+AS[[:space:]]+NOT[[:space:]]+MATERIALIZED'"
)
[ "$bounded_economic_plans" = t ] ||
  fail "migration 015 did not install bounded economic view plans"
source_identity_tables=$(
  docker exec "$postgres_container" \
    psql -X -q -A -t -U phoenix_test -d phoenix_test \
    -c "SELECT to_regclass('public.source_event_identities') IS NOT NULL
             AND to_regclass('public.source_block_enrichments') IS NOT NULL
             AND to_regclass('public.source_enrichment_attempts') IS NOT NULL
             AND to_regclass('public.transaction_boundary_state_evidence') IS NOT NULL"
)
[ "$source_identity_tables" = t ] ||
  fail "migration 014 did not create the exact source-evidence tables"
{
  printf '%s\n' 'BEGIN TRANSACTION READ ONLY;'
  cat "$repo_root/scripts/sql/economic-dashboard-snapshot.sql"
  printf '%s\n' 'ROLLBACK;'
} | docker exec -i "$postgres_container" \
  psql -X -q -A -t -U phoenix_test -d phoenix_test \
  >"$test_root/upgraded-dashboard.json" ||
  fail "dashboard snapshot rejected the migration-012 schema"
PYTHONDONTWRITEBYTECODE=1 python3 -I -B - "$test_root/upgraded-dashboard.json" <<'PY' ||
import json
import sys
from pathlib import Path

document = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if document["schema"] != "phoenix.economic-dashboard.v1":
    raise SystemExit("upgraded dashboard schema is invalid")
if document["economics"]["size_sweep_7d"] != []:
    raise SystemExit("empty upgraded database must have an empty size sweep")
if document["economics"]["loss_ledger_7d"] != []:
    raise SystemExit("empty upgraded database must have an empty loss ledger")
if document["economics"]["daily_attack_surface_7d"] != []:
    raise SystemExit("empty upgraded database must have an empty attack surface")
if document["economics"].get("aave_exact_7d", {}).get("exact_evaluated_signals") != "0":
    raise SystemExit("empty upgraded database has Aave Exact evidence")
if document["economics"].get("atlas_solver_7d", {}).get("request_materialized") != "0":
    raise SystemExit("empty upgraded database has Atlas request evidence")
PY
  fail "upgraded dashboard snapshot contract failed"

docker exec -i "$postgres_container" \
  psql -X -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test <<'SQL' >/dev/null ||
INSERT INTO live_canary.revenue_hunting_signals(
    signal_id, signal_identity, source_lane, source_cursor, borrower,
    block_number, block_hash, state_root, zero_cost_profit_upper_bound,
    expected_net_pnl, conservative_net_pnl, retained_profit_floor,
    evidence_mode, terminal_outcome, rejection_reason, exact_diagnostics,
    evidence_hash, observed_at
) VALUES (
    '00000000-0000-0000-0000-000000000701', 'fixture-aave-exact',
    'aave_liquidation', 1, '0x1111111111111111111111111111111111111111',
    100, '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    '0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
    200, 120, 90, 100, 'DUAL_PROVIDER_FORK_VERIFIED',
    'economic_rejection', 'conservative_net_pnl_below_threshold',
    '{
      "schema":"phoenix.aave-exact-diagnostics.v1",
      "evaluation_stage":"fork",
      "route_eligibility":"eligible",
      "reviewed_combination_count":2,
      "rejection_counts":{"conservative_net_pnl_below_threshold":1},
      "top_diagnostics":[{
        "reviewed_size":"100000000000000","route":"WETH_IDENTITY",
        "gross_liquidation_edge_wei":"200","flash_premium_wei":"5",
        "dex_unwind_loss_wei":"0","price_impact_bps":"0","gas_limit":100,
        "gas_price_wei":"1","l1_cost_wei":"3","execution_cost_wei":"100",
        "atlas_exposure_wei":"0","atlas_bid_wei":"0","risk_reserve_wei":"10",
        "expected_net_pnl_wei":"120","conservative_net_pnl_wei":"90",
        "margin_to_retained_profit_gate_wei":"-10","live_authorized":true,
        "final_rejection_reason":"conservative_net_pnl_below_threshold",
        "evidence_mode":"DUAL_PROVIDER_FORK_VERIFIED"
      }],
      "best_diagnostic":{
        "reviewed_size":"100000000000000","route":"WETH_IDENTITY",
        "gross_liquidation_edge_wei":"200","flash_premium_wei":"5",
        "dex_unwind_loss_wei":"0","price_impact_bps":"0","gas_limit":100,
        "gas_price_wei":"1","l1_cost_wei":"3","execution_cost_wei":"100",
        "atlas_exposure_wei":"0","atlas_bid_wei":"0","risk_reserve_wei":"10",
        "expected_net_pnl_wei":"120","conservative_net_pnl_wei":"90",
        "margin_to_retained_profit_gate_wei":"-10","live_authorized":true,
        "final_rejection_reason":"conservative_net_pnl_below_threshold",
        "evidence_mode":"DUAL_PROVIDER_FORK_VERIFIED"
      },
      "closest_margin_to_retained_profit_gate_wei":"-10",
      "any_counterfactual_positive":false,
      "any_live_authorized_positive":false,
      "fork_attempted":true,
      "fork_passed":true,
      "fork_evidence_mode":"DUAL_PROVIDER_FORK_VERIFIED",
      "failure_class":"conservative_net_pnl_below_threshold",
      "liquidatable_to_exact_latency_ms":1200,
      "exact_fork_latency_ms":800
    }'::jsonb,
    'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc', now()
), (
    '00000000-0000-0000-0000-000000000702', 'fixture-aave-generic-integrity',
    'aave_liquidation', 2, '0x2222222222222222222222222222222222222222',
    101, '0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
    NULL, 200, NULL, NULL, 100, NULL, 'economic_rejection',
    'fork_simulation_failed', '{
      "schema":"phoenix.aave-exact-diagnostics.v1",
      "evaluation_stage":"exact",
      "route_eligibility":"eligible",
      "reviewed_combination_count":0,
      "rejection_counts":{"simulation_evidence_insufficient":1},
      "any_counterfactual_positive":false,
      "any_live_authorized_positive":false,
      "fork_attempted":false,
      "fork_passed":false,
      "failure_class":"simulation_evidence_insufficient",
      "liquidatable_to_exact_latency_ms":0,
      "exact_fork_latency_ms":0
    }'::jsonb,
    'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', now()
);

INSERT INTO live_canary.atlas_auction_ingress(
    auction_id, user_operation_hash, parallel_auction_identity,
    auction_deadline_block, oracle_gas_price_wei, solver_gas_limit,
    dapp, relevant_aave, parallel_eligible, evidence_hash,
    terminal_outcome, rejection_reason, observed_at
) VALUES (
    'fixture-atlas-auction',
    '0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    'fixture-parallel-identity', 1000, 1, 100000,
    '0x3333333333333333333333333333333333333333', true, true,
    'abababababababababababababababababababababababababababababababab',
    'economic_rejection', 'atlas_callback_evidence_unavailable', now()
);
SQL
  fail "lane-specific dashboard fixtures were rejected"

{
  printf '%s\n' 'BEGIN TRANSACTION READ ONLY;'
  cat "$repo_root/scripts/sql/economic-dashboard-snapshot.sql"
  printf '%s\n' 'ROLLBACK;'
} | docker exec -i "$postgres_container" \
  psql -X -q -A -t -U phoenix_test -d phoenix_test \
  >"$test_root/lane-dashboard.json" ||
  fail "lane-specific dashboard snapshot failed"
PYTHONDONTWRITEBYTECODE=1 python3 -I -B - "$test_root/lane-dashboard.json" <<'PY' ||
import json
import sys
from pathlib import Path

raw = Path(sys.argv[1]).read_text(encoding="utf-8")
document = json.loads(raw)
aave = document["funnel"]["revenue_lane_windows"]["aave_liquidation"]["7d"]
atlas = document["funnel"]["revenue_lane_windows"]["atlas_solver"]["7d"]
if aave["exact_attempted"] != "not_available" or aave["exact_completed"] != "2" or aave["fork_passed"] != "1":
    raise SystemExit(f"Aave exact/fork funnel is incorrect: {aave}")
if aave["unexpected_generic_reason_rows"] != "1":
    raise SystemExit("Generic simulation rejection leaked into the Aave reason ledger")
if aave["rejection_reason_counts"]["conservative_net_pnl_below_threshold"] != 1:
    raise SystemExit("Aave canonical rejection reason was not counted")
if aave["diagnostic_rejection_occurrences"]["conservative_net_pnl_below_threshold"] != 1:
    raise SystemExit("Aave per-size rejection evidence was not aggregated")
if aave["unknown_diagnostic_rejection_key_rows"] != "1":
    raise SystemExit("unknown Aave diagnostic reason was not bounded and exposed")
if atlas["ingress"] != "1" or atlas["relevant_aave"] != "1":
    raise SystemExit(f"Atlas ingress funnel is incorrect: {atlas}")
if atlas["parallel_eligible"] != "1" or atlas["candidate_signals"] != "0":
    raise SystemExit(f"Atlas ingress eligibility was mislabeled as a callback stage: {atlas}")
if atlas["actual_path_verified_candidate_signals"] != "0":
    raise SystemExit("Atlas callback authority was fabricated")
if atlas["request_materialized"] != "0":
    raise SystemExit("Atlas callback rejection fabricated a solver request")
if atlas["rejection_reason_counts"]["atlas_callback_evidence_unavailable"] != 1:
    raise SystemExit("Atlas callback capability rejection was not persisted")
best = document["economics"]["aave_exact_7d"]["best_observed_diagnostic"]
if best.get("route") != "WETH_IDENTITY" or best.get("reviewed_size") != "100000000000000":
    raise SystemExit(f"Aave bounded economics are incomplete: {best}")
if "0x1111111111111111111111111111111111111111" in raw or "fixture-atlas-auction" in raw:
    raise SystemExit("high-cardinality lane identity leaked into the dashboard")
PY
  fail "lane-specific dashboard contract failed"

# Exercise the complete snapshot with a full-week 10,080-row Exact cohort and
# a rounded-up 20,000-row Atlas cohort. Together with the 600,000 v6 signals
# above, this is larger than the supplied current Production signal/ingress
# counts while preserving the observed sparse Exact-diagnostics distribution.
docker exec -i "$postgres_container" \
  psql -X -q -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test <<'SQL' >/dev/null ||
INSERT INTO live_canary.revenue_hunting_signals(
  signal_id, signal_identity, source_lane, source_cursor, block_number,
  block_hash, zero_cost_profit_upper_bound, retained_profit_floor,
  terminal_outcome, rejection_reason, exact_diagnostics, evidence_hash,
  observed_at
)
SELECT
  md5('volume-aave-' || value)::uuid,
  'volume-aave-' || value,
  'aave_liquidation', value, 1000 + value,
  '0x' || repeat('1', 64), 200, 100,
  'economic_rejection', 'conservative_net_pnl_below_threshold',
  '{
    "schema":"phoenix.aave-exact-diagnostics.v1",
    "evaluation_stage":"fork",
    "route_eligibility":"eligible",
    "reviewed_combination_count":1,
    "rejection_counts":{"conservative_net_pnl_below_threshold":1},
    "any_counterfactual_positive":false,
    "any_live_authorized_positive":false,
    "fork_attempted":true,
    "fork_passed":true,
    "fork_evidence_mode":"DUAL_PROVIDER_FORK_VERIFIED",
    "failure_class":"conservative_net_pnl_below_threshold",
    "liquidatable_to_exact_latency_ms":1000,
    "exact_fork_latency_ms":500
  }'::jsonb,
  md5('volume-aave-evidence-a-' || value) || md5('volume-aave-evidence-b-' || value),
  now() - ((value % 604800) * interval '1 second')
FROM generate_series(1, 10080) AS generated(value);

INSERT INTO live_canary.atlas_auction_ingress(
  auction_id, user_operation_hash, parallel_auction_identity,
  auction_deadline_block, oracle_gas_price_wei, solver_gas_limit, dapp,
  relevant_aave, parallel_eligible, evidence_hash, terminal_outcome,
  rejection_reason, observed_at
)
SELECT
  'volume-atlas-' || value,
  '0x' || repeat('2', 64), 'volume-parallel-' || value,
  1000 + value, 1, 100000, '0x' || repeat('3', 40),
  true, true,
  md5('volume-atlas-evidence-a-' || value) || md5('volume-atlas-evidence-b-' || value),
  'economic_rejection', 'atlas_callback_evidence_unavailable',
  now() - ((value % 604800) * interval '1 second')
FROM generate_series(1, 20000) AS generated(value);

ANALYZE live_canary.atlas_auction_ingress;
SQL
  fail "representative lane dashboard fixtures were rejected"

volume_query_started=$(date +%s)
docker exec -i \
  -e PGOPTIONS='-c statement_timeout=30s -c lock_timeout=5s -c jit=on' \
  "$postgres_container" \
  psql -X -q -A -t -U phoenix_test -d phoenix_test \
  <"$repo_root/scripts/sql/economic-dashboard-snapshot.sql" \
  >"$test_root/representative-dashboard.json" ||
  fail "representative lane dashboard exceeded the Production query contract"
volume_query_seconds=$(($(date +%s) - volume_query_started))
[ "$volume_query_seconds" -le 35 ] ||
  fail "representative lane dashboard exceeded the bounded wall-clock margin"
PYTHONDONTWRITEBYTECODE=1 python3 -I -B - \
  "$test_root/representative-dashboard.json" "$volume_query_seconds" <<'PY' ||
import json
import sys
from pathlib import Path

document = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
aave = document["funnel"]["revenue_lane_windows"]["aave_liquidation"]["7d"]
atlas = document["funnel"]["revenue_lane_windows"]["atlas_solver"]["7d"]
if int(aave["exact_completed"]) < 10082:
    raise SystemExit("representative Aave rows were not fully aggregated")
if int(atlas["ingress"]) < 20001:
    raise SystemExit("representative Atlas rows were not fully aggregated")
print(f"REPRESENTATIVE_DASHBOARD_QUERY_OK: seconds={sys.argv[2]} aave={aave['exact_completed']} atlas={atlas['ingress']}")
PY
  fail "representative lane dashboard result was invalid"

echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_OK"
