#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
gate=$repo_root/scripts/prelive-v5-fresh-database-gate.sh
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir -p "$fixture/bin"

cat >"$fixture/bin/psql" <<'EOF'
#!/bin/sh
case "$*" in
  *phoenix_v5_current_database*)
    printf '%s\n' "${FAKE_DATABASE:-phoenix_v5_candidate}"
    ;;
  *phoenix_v5_public_tables*)
    case "${FAKE_PSQL_SCENARIO:-empty}" in
      empty) ;;
      initialized-empty) echo "schema_migrations" ;;
      existing) echo "origin_transactions" ;;
      post*)
        cat <<'TABLES'
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
TABLES
        ;;
    esac
    ;;
  *phoenix_v5_initial_migration_count*)
    printf '%s\n' "${FAKE_MIGRATION_COUNT:-0}"
    ;;
  *phoenix_v5_applied_migrations*)
    cat <<'MIGRATIONS'
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
MIGRATIONS
    [ "${FAKE_PSQL_SCENARIO:-}" = "post-bad-migrations" ] ||
      echo "011_money_path_selective_persistence"
    ;;
  *phoenix_v5_required_columns*)
    if [ "${FAKE_PSQL_SCENARIO:-}" = "post-missing-column" ]; then
      echo "engine_outbox.claim_owner"
    fi
    ;;
  *phoenix_v5_zero_data_counts*)
    cat <<'COUNTS'
engine_outbox=0
execution_attempts=0
executions=0
feed_events=0
fork_simulation_results=0
money_path_ingress_daily=0
money_path_ingress_samples=0
opportunities=0
opportunity_legs=0
origin_transactions=0
realized_pnl=0
shadow_decisions=0
shadow_engine_classifications=0
shadow_engine_processing_attempts=0
shadow_profitability_facts=0
COUNTS
    [ "${FAKE_PSQL_SCENARIO:-}" != "post-data" ] ||
      echo "origin_transactions=1"
    ;;
  *)
    echo "unexpected fake psql query" >&2
    exit 1
    ;;
esac
EOF
chmod +x "$fixture/bin/psql"

export PATH="$fixture/bin:$PATH"
export PHOENIX_V5_DATABASE_ROLE=v5_candidate
export PHOENIX_V5_DATABASE_GENERATION=fresh-001-011
export PHOENIX_V5_CANDIDATE_DATABASE_NAME=phoenix_v5_candidate
export PHOENIX_V4_FALLBACK_DATABASE_NAME=phoenix_v4_fallback
export PHOENIX_V5_CANDIDATE_POSTGRES_DSN=postgres://fixture:fixture@127.0.0.1:5432/phoenix_v5_candidate
export PHOENIX_V5_FRESH_DATABASE_CONFIRM=INITIALIZE_EMPTY_PHOENIX_V5_DATABASE

expect_failure() {
  scenario=$1
  mode=$2
  if FAKE_PSQL_SCENARIO=$scenario sh "$gate" "$mode" >"$fixture/output" 2>"$fixture/error"; then
    echo "expected $scenario $mode to fail" >&2
    exit 1
  fi
  grep -F "PHOENIX_V5_DATABASE_GATE_ERROR:" "$fixture/error" >/dev/null
}

FAKE_PSQL_SCENARIO=empty sh "$gate" preflight |
  grep -F "PHOENIX_V5_DATABASE_PREFLIGHT_OK" >/dev/null
FAKE_PSQL_SCENARIO=initialized-empty sh "$gate" preflight |
  grep -F "PHOENIX_V5_DATABASE_PREFLIGHT_OK" >/dev/null
expect_failure existing preflight

FAKE_PSQL_SCENARIO=post sh "$gate" post-migration |
  grep -F "PHOENIX_V5_DATABASE_POST_MIGRATION_OK" >/dev/null
expect_failure post-bad-migrations post-migration
expect_failure post-missing-column post-migration
expect_failure post-data post-migration

if PHOENIX_V4_FALLBACK_DATABASE_NAME=phoenix_v5_candidate \
  FAKE_PSQL_SCENARIO=empty sh "$gate" preflight >"$fixture/output" 2>"$fixture/error"; then
  echo "matching fallback and candidate database names must fail" >&2
  exit 1
fi
grep -F "candidate and fallback database names must differ" "$fixture/error" >/dev/null

if FAKE_DATABASE=unexpected_database \
  FAKE_PSQL_SCENARIO=empty sh "$gate" preflight >"$fixture/output" 2>"$fixture/error"; then
  echo "connected database identity mismatch must fail" >&2
  exit 1
fi
grep -F "explicit v5 candidate identity" "$fixture/error" >/dev/null

echo "prelive v5 fresh database gate tests passed"
