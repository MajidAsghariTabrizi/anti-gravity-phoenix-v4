#!/bin/sh
set -eu

usage() {
  echo "usage: prelive-v5-fresh-database-gate.sh <preflight|post-migration>" >&2
  exit 64
}

fail() {
  echo "PHOENIX_V5_DATABASE_GATE_ERROR: $1" >&2
  exit 1
}

require_value() {
  eval "required_value=\${$1:-}"
  [ -n "$required_value" ] || fail "$1 is required"
}

query() {
  psql "$candidate_dsn" -X -qAt -v ON_ERROR_STOP=1 -c "$1"
}

[ "$#" -eq 1 ] || usage
mode=$1
case "$mode" in
  preflight | post-migration) ;;
  *) usage ;;
esac

for required_name in \
  PHOENIX_V5_DATABASE_ROLE \
  PHOENIX_V5_DATABASE_GENERATION \
  PHOENIX_V5_CANDIDATE_DATABASE_NAME \
  PHOENIX_V4_FALLBACK_DATABASE_NAME \
  PHOENIX_V5_CANDIDATE_POSTGRES_DSN \
  PHOENIX_V5_FRESH_DATABASE_CONFIRM
do
  require_value "$required_name"
done

[ "$PHOENIX_V5_DATABASE_ROLE" = "v5_candidate" ] ||
  fail "database role must be v5_candidate"
[ "$PHOENIX_V5_DATABASE_GENERATION" = "fresh-001-011" ] ||
  fail "database generation must be fresh-001-011"
[ "$PHOENIX_V5_FRESH_DATABASE_CONFIRM" = "INITIALIZE_EMPTY_PHOENIX_V5_DATABASE" ] ||
  fail "fresh database confirmation is invalid"

case "$PHOENIX_V5_CANDIDATE_DATABASE_NAME" in
  *[!a-z0-9_]* | "") fail "candidate database name is invalid" ;;
esac
case "$PHOENIX_V4_FALLBACK_DATABASE_NAME" in
  *[!a-z0-9_]* | "") fail "fallback database name is invalid" ;;
esac
[ "$PHOENIX_V5_CANDIDATE_DATABASE_NAME" != "$PHOENIX_V4_FALLBACK_DATABASE_NAME" ] ||
  fail "candidate and fallback database names must differ"

candidate_dsn=$PHOENIX_V5_CANDIDATE_POSTGRES_DSN
case "$candidate_dsn" in
  postgres://* | postgresql://*) ;;
  *) fail "candidate PostgreSQL DSN shape is invalid" ;;
esac

actual_database=$(query "/* phoenix_v5_current_database */ SELECT current_database()")
[ "$actual_database" = "$PHOENIX_V5_CANDIDATE_DATABASE_NAME" ] ||
  fail "connected database does not match the explicit v5 candidate identity"

actual_tables=$(query "/* phoenix_v5_public_tables */ SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE' ORDER BY table_name")

if [ "$mode" = "preflight" ]; then
  case "$actual_tables" in
    "") ;;
    schema_migrations)
      migration_count=$(query "/* phoenix_v5_initial_migration_count */ SELECT count(*) FROM public.schema_migrations")
      [ "$migration_count" = "0" ] ||
        fail "allowed schema_migrations initialization table is not empty"
      ;;
    *) fail "candidate database contains an existing application table" ;;
  esac
  echo "PHOENIX_V5_DATABASE_PREFLIGHT_OK"
  exit 0
fi

expected_tables=$(cat <<'EOF'
contract_events
engine_outbox
execution_attempts
executions
feed_events
fork_simulation_results
gas_profiles
miss_reasons
money_path_ingress_daily
money_path_ingress_samples
opportunities
opportunity_legs
origin_transactions
pool_state_checkpoints
realized_pnl
rpc_quality_records
schema_migrations
shadow_decisions
shadow_engine_classifications
shadow_engine_processing_attempts
shadow_profitability_facts
shadow_replay_runs
EOF
)
[ "$actual_tables" = "$expected_tables" ] ||
  fail "post-migration public table contract is incomplete or unexpected"

expected_migrations=$(cat <<'EOF'
001_init
002_event_signatures
003_shadow_profitability_evidence
004_shadow_engine_runtime
005_shadow_decision_identity
006_dependency_exhaustion_quarantine
007_canonical_profitability_truth
008_shadow_route_discovery_indexes
009_profit_triggered_secondary_verification
010_fork_simulation_evidence
011_money_path_selective_persistence
EOF
)
actual_migrations=$(query "/* phoenix_v5_applied_migrations */ SELECT version FROM public.schema_migrations ORDER BY version")
[ "$actual_migrations" = "$expected_migrations" ] ||
  fail "applied migration set is not exactly 001-011"

missing_columns=$(query "/* phoenix_v5_required_columns */ WITH required(table_name, column_name) AS (VALUES ('origin_transactions','tx_hash'),('feed_events','sequence_number'),('engine_outbox','outbox_id'),('engine_outbox','claim_owner'),('engine_outbox','claim_expires_at'),('engine_outbox','published_at'),('shadow_engine_classifications','source_event_identity'),('shadow_decisions','execution_eligible'),('shadow_profitability_facts','execution_request_created'),('money_path_ingress_daily','event_count'),('money_path_ingress_samples','safe_decoder_summary')) SELECT required.table_name || '.' || required.column_name FROM required WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns AS columns WHERE columns.table_schema = 'public' AND columns.table_name = required.table_name AND columns.column_name = required.column_name) ORDER BY 1")
[ -z "$missing_columns" ] ||
  fail "Recorder, Dispatcher, Engine, or money-path schema verification failed"

zero_counts=$(query "/* phoenix_v5_zero_data_counts */ SELECT 'engine_outbox=' || count(*) FROM engine_outbox UNION ALL SELECT 'execution_attempts=' || count(*) FROM execution_attempts UNION ALL SELECT 'executions=' || count(*) FROM executions UNION ALL SELECT 'feed_events=' || count(*) FROM feed_events UNION ALL SELECT 'fork_simulation_results=' || count(*) FROM fork_simulation_results UNION ALL SELECT 'money_path_ingress_daily=' || count(*) FROM money_path_ingress_daily UNION ALL SELECT 'money_path_ingress_samples=' || count(*) FROM money_path_ingress_samples UNION ALL SELECT 'opportunities=' || count(*) FROM opportunities UNION ALL SELECT 'opportunity_legs=' || count(*) FROM opportunity_legs UNION ALL SELECT 'origin_transactions=' || count(*) FROM origin_transactions UNION ALL SELECT 'realized_pnl=' || count(*) FROM realized_pnl UNION ALL SELECT 'shadow_decisions=' || count(*) FROM shadow_decisions UNION ALL SELECT 'shadow_engine_classifications=' || count(*) FROM shadow_engine_classifications UNION ALL SELECT 'shadow_engine_processing_attempts=' || count(*) FROM shadow_engine_processing_attempts UNION ALL SELECT 'shadow_profitability_facts=' || count(*) FROM shadow_profitability_facts ORDER BY 1")
old_ifs=$IFS
IFS='
'
for count_evidence in $zero_counts; do
  case "$count_evidence" in
    *=0) ;;
    *) fail "fresh candidate contains application data" ;;
  esac
done
IFS=$old_ifs

echo "PHOENIX_V5_DATABASE_POST_MIGRATION_OK"
