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
  migrations/013_economic_loss_ledger.sql \
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
economic_loss = read("migrations/013_economic_loss_ledger.sql")
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
    "'loss_ledger_7d'",
    "'daily_attack_surface_7d'",
    "'loss_cause_contract'",
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

postgres_image=postgres@sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777
test_suffix="$$"
postgres_container="phoenix-economic-schema-postgres-$test_suffix"
monitor_container="phoenix-economic-schema-monitor-$test_suffix"
test_network="phoenix-economic-schema-$test_suffix"
test_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-economic-schema.XXXXXX")
monitor_output="$test_root/evidence"

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
docker network create --internal "$test_network" >/dev/null
docker run -d \
  --name "$postgres_container" \
  --network "$test_network" \
  --tmpfs /var/lib/postgresql/data:rw,nosuid,nodev,size=512m \
  -e POSTGRES_USER=phoenix_test \
  -e POSTGRES_PASSWORD=phoenix_test_password \
  -e POSTGRES_DB=phoenix_test \
  "$postgres_image" >/dev/null

postgres_ready=false
attempt=0
while [ "$attempt" -lt 30 ]; do
  if docker exec "$postgres_container" \
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
  docker exec -i "$postgres_container" \
    psql -X -v ON_ERROR_STOP=1 -U phoenix_test -d phoenix_test \
    <"$schema" >/dev/null ||
    fail "live schema failed: ${schema##*/}"
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
  --health-cmd "test -s /evidence/latest-dashboard.json" \
  --health-interval 1s \
  --health-timeout 3s \
  --health-retries 20 \
  -e POSTGRES_DSN=postgres://phoenix_test:phoenix_test_password@"$postgres_container":5432/phoenix_test \
  -e PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS=30 \
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
    "$repo_root/migrations/013_economic_loss_ledger.sql"
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
PY
  fail "upgraded dashboard snapshot contract failed"

echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_OK"
