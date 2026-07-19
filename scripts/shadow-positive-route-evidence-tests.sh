#!/usr/bin/env sh
# Literal grep patterns and variables consumed by the sourced workflow are intentional.
# shellcheck disable=SC2016,SC2034,SC2329
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
workflow=$script_dir/shadow-positive-route-evidence.sh
test_root=$(mktemp -d)
test_log=$test_root/compose.log
fake_psql_query=$test_root/fake-psql-query.sql
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "shadow-positive-route-evidence-tests: $1" >&2
  exit 1
}

SHADOW_POSITIVE_ROUTE_EVIDENCE_LIBRARY_ONLY=1
export SHADOW_POSITIVE_ROUTE_EVIDENCE_LIBRARY_ONLY
# shellcheck disable=SC1090
. "$workflow"

test_tx_a=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
test_tx_b=0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
test_sequence=461219428
test_identity_b=phoenix.engine.input.v1:$test_sequence:$test_tx_b
test_route=arb1-weth-usdc-uni500-uni3000-canary-v2
RUN_STARTED_AT_UTC=2026-07-14T14:20:00.000000Z

primary_report='{"source_event_identity":"'$test_identity_b'","source_sequence":"'$test_sequence'","source_transaction_hash":"'$test_tx_b'","classification":"candidate_rejected","rejection_reason":"liquidity_insufficient","candidate_count":1,"matched_route_id":"'$test_route'","processing_attempt_id":20931,"delivery_attempt":4,"processing_attempt_completed_at":"2026-07-14T14:24:02.000000+00:00","persisted_timestamp":"2026-07-14T14:24:02.000000+00:00","pinned_block_number":"483792695","pinned_block_hash":"0xfdb4b9a0a59ecf4c675b725390d41cb2820fe59a89caa0b7359b47eb644dda45","primary_state_hash":"1397b50a50d7b6128075572a6c730d731e0a5512c2463999cb509b7c989aa013","route_config_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","primary_provider_result":"publicnode","verification_status":"primary_only","primary_screen_rejected":true,"secondary_skipped":true,"independent_verification_status":"not_requested","independent_verification_lifecycle":["not_requested"],"independent_verification_skip_reason":"primary_screen_no_profitable_candidate","shadow_only":true,"execution_request_created":false}'
primary_no_profit_report=$(printf '%s' "$primary_report" | python3 -c '
import json, sys
report = json.load(sys.stdin)
report["rejection_reason"] = "no_profitable_candidate"
print(json.dumps(report, separators=(",", ":")))
')
agreed_report='{"source_event_identity":"'$test_identity_b'","source_sequence":"'$test_sequence'","source_transaction_hash":"'$test_tx_b'","classification":"shadow_accepted","rejection_reason":"shadow_policy_accepted","candidate_count":1,"matched_route_id":"'$test_route'","processing_attempt_id":20932,"delivery_attempt":5,"processing_attempt_completed_at":"2026-07-14T14:25:02.000000+00:00","persisted_timestamp":"2026-07-14T14:25:02.000000+00:00","pinned_block_number":"483792696","pinned_block_hash":"0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","primary_state_hash":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","route_config_hash":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","rpc_response_hash":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","primary_provider_result":"publicnode","verification_status":"agreed","independent_verification_status":"agreed","independent_verification_lifecycle":["requested","agreed"],"independent_provider_result":"secondary","verification_agreement":"agreed","secondary_state_hash":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","secondary_block_number":"483792696","secondary_block_hash":"0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","secondary_route_config_hash":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","shadow_disposition":"accepted","shadow_only":true,"execution_request_created":false}'

fake_psql_mode=historical_only
sql_query() {
  printf '%s\n' "$1" >"$fake_psql_query"
  case "$fake_psql_mode" in
    baseline) printf '2026-07-14T14:20:00.000000Z\n' ;;
    historical_only) : ;;
    current_candidate)
      printf '%s|%s|%s|%s\n' "$test_identity_b" "$test_tx_b" "$test_sequence" "$test_route"
      ;;
    primary_report) printf '%s\n' "$primary_report" ;;
    primary_no_profit_report) printf '%s\n' "$primary_no_profit_report" ;;
    agreed_report) printf '%s\n' "$agreed_report" ;;
    *) return 1 ;;
  esac
}

# 1. A historical candidate cannot satisfy the current run.
fake_psql_mode=historical_only
candidate=$(positive_evidence_candidate_record) || fail 'historical-only fake PostgreSQL query failed'
[ -z "$candidate" ] || fail 'historical evidence produced a current-run candidate'
grep -F "classification.classified_at >= '$RUN_STARTED_AT_UTC'::timestamptz" "$fake_psql_query" >/dev/null ||
  fail 'candidate query is not classification-time scoped'
grep -F "current_attempt.completed_at >= '$RUN_STARTED_AT_UTC'::timestamptz" "$fake_psql_query" >/dev/null ||
  fail 'candidate query is not current-attempt scoped'
grep -F "positive_evidence_no_evidence_diagnostics" "$workflow" >/dev/null ||
  fail 'an exhausted current-run query does not emit bounded diagnostics'
grep -F "positive_evidence_finish POSITIVE_ROUTE_EVIDENCE_NOT_FOUND '' ''" "$workflow" >/dev/null ||
  fail 'an exhausted current-run query does not return NOT_FOUND'

# The baseline itself is the PostgreSQL server UTC clock, captured before startup.
fake_psql_mode=baseline
positive_evidence_capture_run_baseline || fail 'database-clock baseline was rejected'
[ "$RUN_STARTED_AT_UTC" = 2026-07-14T14:20:00.000000Z ] || fail 'database-clock baseline changed'
grep -F 'clock_timestamp()' "$fake_psql_query" >/dev/null || fail 'baseline did not use PostgreSQL time'

# 2 and 3. The report query uses the newest current-run attempt and exact identity/tx pair.
positive_candidate_identity=$test_identity_b
positive_candidate_tx_hash=$test_tx_b
positive_candidate_source_sequence=$test_sequence
positive_candidate_matched_route_id=$test_route
fake_psql_mode=primary_report
runtime_report=$(positive_evidence_runtime_report) || fail 'fake persisted report query failed'
grep -F 'ORDER BY processing_attempt.completed_at DESC, processing_attempt.id DESC' "$fake_psql_query" >/dev/null ||
  fail 'newest processing-attempt ordering is missing'
grep -F "processing_attempt.completed_at >= '$RUN_STARTED_AT_UTC'::timestamptz" "$fake_psql_query" >/dev/null ||
  fail 'processing attempt is not run-scoped'
grep -F 'state.classification = processing_attempt.classification' "$fake_psql_query" >/dev/null ||
  fail 'attempt does not match the final classification'
grep -F "classification.source_event_identity = '$test_identity_b'" "$fake_psql_query" >/dev/null ||
  fail 'report does not match source identity'
grep -F "classification.tx_hash = '$test_tx_b'" "$fake_psql_query" >/dev/null ||
  fail 'same-sequence transactions are not disambiguated by hash'
if grep -F "classification.tx_hash = '$test_tx_a'" "$fake_psql_query" >/dev/null; then
  fail 'report selected the other transaction sharing the source sequence'
fi
positive_evidence_validate_runtime_report "$runtime_report" || fail 'primary-only report validation failed'
printf '%s' "$runtime_report" | python3 -c '
import json, sys
report = json.load(sys.stdin)
assert report["processing_attempt_id"] == 20931
assert report["delivery_attempt"] == 4
assert report["source_transaction_hash"].endswith("b" * 64)
' || fail 'newest fake attempt tuple was not preserved'

fake_psql_mode=primary_no_profit_report
no_profit_report=$(positive_evidence_runtime_report) || fail 'fake no-profit report query failed'
positive_evidence_validate_runtime_report "$no_profit_report" ||
  fail 'no-profitable-candidate primary-only report validation failed'
grep -F "normalized.rejection_reason IN ('no_profitable_candidate', 'liquidity_insufficient')" \
  "$fake_psql_query" >/dev/null || fail 'primary-only rejection allowlist is missing from SQL'
grep -F "THEN jsonb_build_array('not_requested')" "$fake_psql_query" >/dev/null ||
  fail 'not-requested lifecycle is not built structurally'
grep -F "THEN jsonb_build_array('requested', 'provider_unavailable')" \
  "$fake_psql_query" >/dev/null || fail 'provider-unavailable lifecycle is not built structurally'
if grep -F "'[\"not_requested\"]'::jsonb" "$workflow" >/dev/null ||
  grep -F "'[\"requested\", \"provider_unavailable\"]'::jsonb" "$workflow" >/dev/null; then
  fail 'runtime SQL still uses quoted JSON array literals'
fi

# 4, 5 and 6. Identity fields describe the schema honestly.
printf '%s' "$runtime_report" | python3 -c '
import json, sys
report = json.load(sys.stdin)
assert "classification_id" not in report
assert "classification_record_id" not in report
assert report["source_event_identity"].startswith("phoenix.engine.input.v1:")
' || fail 'classification identity fields are misleading'
if grep -F "'classification_id'," "$workflow" >/dev/null; then
  fail 'runtime SQL still emits classification_id'
fi
if grep -F "'classification_record_id'," "$workflow" >/dev/null; then
  fail 'runtime SQL invents a classification record ID'
fi

# 7 and 8. A persisted primary-screen rejection means independent verification was not requested.
printf '%s' "$runtime_report" | python3 -c '
import json, sys
report = json.load(sys.stdin)
assert report["verification_status"] == "primary_only"
assert report["classification"] == "candidate_rejected"
assert report["rejection_reason"] == "liquidity_insufficient"
assert report["primary_screen_rejected"] is True
assert report["secondary_skipped"] is True
assert report["independent_verification_status"] == "not_requested"
assert report["independent_verification_lifecycle"] == ["not_requested"]
assert report["independent_verification_skip_reason"] == "primary_screen_no_profitable_candidate"
assert "independent_provider_result" not in report
assert "verification_agreement" not in report
assert "rpc_response_hash" not in report
' || fail 'primary-only skip semantics are ambiguous'

assert_primary_report_rejected() {
  rejected_report=$1
  rejected_label=$2
  if positive_evidence_validate_runtime_report "$rejected_report"; then
    fail "$rejected_label was accepted"
  fi
}

mutate_primary_report() {
  mutation=$1
  printf '%s' "$primary_report" | python3 -c '
import json
import sys

mutation = sys.argv[1]
report = json.load(sys.stdin)
if mutation == "unknown_reason":
    report["rejection_reason"] = "unreviewed_reason"
elif mutation == "accepted_primary_only":
    report["classification"] = "shadow_accepted"
elif mutation == "secondary_evidence":
    report["secondary_state_hash"] = "d" * 64
elif mutation == "old_baseline":
    report["processing_attempt_completed_at"] = "2026-07-14T14:19:59+00:00"
elif mutation == "execution_request":
    report["execution_request_created"] = True
elif mutation == "primary_screen_not_rejected":
    report["primary_screen_rejected"] = False
elif mutation == "secondary_not_skipped":
    report["secondary_skipped"] = False
else:
    raise SystemExit(2)
print(json.dumps(report, separators=(",", ":")))
' "$mutation"
}

for rejected_case in \
  unknown_reason \
  accepted_primary_only \
  secondary_evidence \
  old_baseline \
  execution_request \
  primary_screen_not_rejected \
  secondary_not_skipped; do
  rejected_report=$(mutate_primary_report "$rejected_case") ||
    fail "could not construct $rejected_case report"
  assert_primary_report_rejected "$rejected_report" "$rejected_case"
done

# 9. A genuine independent-provider agreement remains distinct and explicit.
fake_psql_mode=agreed_report
runtime_report=$(positive_evidence_runtime_report) || fail 'fake agreed report query failed'
positive_evidence_validate_runtime_report "$runtime_report" || fail 'agreed report validation failed'
printf '%s' "$runtime_report" | python3 -c '
import json, sys
report = json.load(sys.stdin)
assert report["verification_status"] == "agreed"
assert report["independent_verification_status"] == "agreed"
assert report["independent_verification_lifecycle"] == ["requested", "agreed"]
assert report["verification_agreement"] == "agreed"
assert report["independent_provider_result"] == "secondary"
assert report["secondary_block_number"] == report["pinned_block_number"]
assert report["secondary_block_hash"] == report["pinned_block_hash"]
assert report["secondary_route_config_hash"] == report["route_config_hash"]
assert report["secondary_state_hash"] == report["primary_state_hash"]
' || fail 'agreed verification was collapsed or omitted'

write_rendered_budget() {
  isolated_canary_state_dir=$test_root/budget-$1
  mkdir -p "$isolated_canary_state_dir"
  printf '{"services":{"rpc-gateway":{"environment":{"RPC_STATE_REQUESTS_PER_MINUTE":"%s","RPC_UPSTREAM_CALLS_PER_SECOND":"1","RPC_UPSTREAM_CALL_BURST":"4"}}}}\n' "$1" \
    >"$isolated_canary_state_dir/compose.rendered.json"
}

# shellcheck disable=SC2317  # Test double invoked by the sourced workflow.
compose() {
  printf '%s\n' "$*" >>"$test_log"
}

assert_budget_rejected_before_start() {
  positive_budget=$1
  : >"$test_log"
  write_rendered_budget "$positive_budget"
  positive_error=$test_root/budget-$positive_budget.err
  if (
    positive_runtime_touched=0
    positive_finalized=0
    isolated_canary_snapshot_recorded=0
    positive_evidence_rpc_budget_preflight ||
      positive_evidence_fail 'RPC_STATE_REQUESTS_PER_MINUTE must be at least 12'
    positive_evidence_start_runtime
  ) 2>"$positive_error"; then
    fail "budget $positive_budget unexpectedly passed"
  fi
  grep -Fx 'SHADOW_POSITIVE_ROUTE_EVIDENCE_FAIL: RPC_STATE_REQUESTS_PER_MINUTE must be at least 12' \
    "$positive_error" >/dev/null || fail "budget $positive_budget omitted the exact failure marker"
  [ ! -s "$test_log" ] || fail "budget $positive_budget started Docker services"
}

assert_budget_accepted() {
  positive_budget=$1
  : >"$test_log"
  write_rendered_budget "$positive_budget"
  positive_evidence_rpc_budget_preflight || fail "budget $positive_budget was rejected"
  positive_evidence_start_runtime || fail "budget $positive_budget did not reach optional startup"
  grep -Fx 'up -d --no-deps rpc-gateway phoenix-engine' "$test_log" >/dev/null ||
    fail "budget $positive_budget did not use isolated startup"
}

# 10 through 13. The rendered request budget gates startup at exactly 12.
assert_budget_rejected_before_start 2
assert_budget_rejected_before_start 11
assert_budget_accepted 12
assert_budget_accepted 13

# 14. Budget validation reads rendered Compose data and never rewrites supplied env files.
env_file=$test_root/phoenix.env
release_env=$test_root/release.env
printf 'RPC_STATE_REQUESTS_PER_MINUTE=12\n' >"$env_file"
printf 'RPC_GATEWAY_IMAGE=example.invalid/rpc@sha256:%064d\n' 0 >"$release_env"
env_before=$(cksum "$env_file" "$release_env")
write_rendered_budget 12
positive_evidence_rpc_budget_preflight || fail 'env-preservation budget check failed'
env_after=$(cksum "$env_file" "$release_env")
[ "$env_before" = "$env_after" ] || fail 'workflow rewrote a supplied env file'

# 15 and 16. Only the two optional services can be stopped or started.
: >"$test_log"
positive_evidence_prepare_runtime || fail 'optional runtime preparation failed'
positive_evidence_start_runtime || fail 'optional runtime start failed'
positive_evidence_cleanup_runtime || fail 'optional runtime cleanup failed'
grep -Fx 'stop phoenix-engine rpc-gateway' "$test_log" >/dev/null ||
  fail 'preparation/cleanup did not stop exactly Engine and RPC Gateway'
grep -Fx 'up -d --no-deps rpc-gateway phoenix-engine' "$test_log" >/dev/null ||
  fail 'start did not use the exact isolated Compose command'
if grep -E '^(up|stop).*(nitro-feed-relay|nats|postgres|migration-runner|feed-ingestor|recorder|shadow-dispatcher|prometheus|dashboard)' "$test_log" >/dev/null; then
  fail 'workflow touched a protected service'
fi
if grep -E 'compose[[:space:]]+(down|pull|build|rm)|--remove-orphans|compose[[:space:]]+up.*migration-runner' "$workflow" >/dev/null; then
  fail 'workflow contains a forbidden Compose mutation'
fi

# Fake Docker state proves protected-service snapshot drift remains fail-closed.
compose() {
  if [ "${1:-}" = ps ]; then
    positive_last=
    for positive_argument in "$@"; do
      positive_last=$positive_argument
    done
    printf 'container-%s\n' "$positive_last"
    return 0
  fi
  printf '%s\n' "$*" >>"$test_log"
}
snapshot_generation=0
docker() {
  [ "${1:-}" = inspect ] || return 1
  positive_last=
  for positive_argument in "$@"; do
    positive_last=$positive_argument
  done
  positive_service=${positive_last#container-}
  if [ "$snapshot_generation" -eq 0 ]; then
    printf 'id-%s|image-%s|digest-%s|created-%s|started-%s|0|true\n' \
      "$positive_service" "$positive_service" "$positive_service" "$positive_service" "$positive_service"
  else
    printf 'id-%s|changed-%s|digest-%s|created-%s|started-%s|1|true\n' \
      "$positive_service" "$positive_service" "$positive_service" "$positive_service" "$positive_service"
  fi
}
isolated_canary_state_dir=$test_root/snapshot
mkdir -p "$isolated_canary_state_dir"
isolated_canary_snapshot_recorded=0
isolated_canary_record_snapshot || fail 'protected snapshot could not be recorded with fake Docker'
isolated_canary_verify_snapshot || fail 'unchanged protected services failed snapshot verification'
snapshot_generation=1
if isolated_canary_verify_snapshot; then
  fail 'protected service identity/restart changes were not detected'
fi
[ -n "$isolated_canary_changed_service" ] || fail 'changed protected service was not identified'

# 17. All SQL remains read-only and JetStream administration is forbidden.
if grep -Ei '(consumer|stream).*(delete|reset|purge|create|update)|(delete|reset|purge|create|update).*(consumer|stream)' "$workflow" >/dev/null; then
  fail 'workflow contains a durable consumer or stream administrative mutation'
fi
if grep -E '^[[:space:]]*(INSERT|UPDATE|DELETE|TRUNCATE|ALTER|DROP|CREATE)[[:space:]]' "$workflow" >/dev/null; then
  fail 'workflow SQL is not read only'
fi

# 18. SHADOW guards remain mandatory and no execution/submission path is introduced.
grep -F '[ "${PHOENIX_MODE:-}" = SHADOW ]' "$workflow" >/dev/null || fail 'SHADOW guard is missing'
grep -F '[ "${LIVE_EXECUTION:-}" = false ]' "$workflow" >/dev/null || fail 'LIVE guard is missing'
grep -F '[ -z "${SIGNER_PRIVATE_KEY:-}" ]' "$workflow" >/dev/null || fail 'signer guard is missing'
grep -F '[ -z "${EXECUTOR_ADDRESS:-}" ]' "$workflow" >/dev/null || fail 'executor guard is missing'
grep -F '[ -z "${WALLET_ADDRESS:-}" ]' "$workflow" >/dev/null || fail 'wallet guard is missing'
if grep -E 'LIVE_EXECUTION=(true|1)|eth_sendRawTransaction|transaction submission|SIGNER_PRIVATE_KEY=[^}]' "$workflow" >/dev/null; then
  fail 'workflow introduces LIVE or transaction-submission behavior'
fi
grep -F 'execution eligibility changed during SHADOW evidence run' "$workflow" >/dev/null ||
  fail 'SHADOW execution-eligibility guard is missing'
grep -F 'positive_evidence_render_preflight' "$workflow" >/dev/null ||
  fail 'canonical production rendering preflight is missing'
grep -F -- '--release-manifest "$release_manifest"' "$workflow" >/dev/null ||
  fail 'evidence workflow does not bind the release manifest'
if grep -F '$repo_dir/deploy/current-release.env' "$workflow" >/dev/null ||
  grep -F '$repo_dir/current-release.env' "$workflow" >/dev/null; then
  fail 'evidence workflow retains a repository-local release fallback'
fi
grep -F 'timeout_seconds=${SHADOW_POSITIVE_ROUTE_TIMEOUT_SECONDS:-900}' "$workflow" >/dev/null ||
  fail 'default timeout is not 15 minutes'
grep -F 'POSITIVE_ROUTE_EVIDENCE_FOUND' "$workflow" >/dev/null || fail 'positive terminal result is missing'
grep -F 'POSITIVE_ROUTE_EVIDENCE_NOT_FOUND' "$workflow" >/dev/null || fail 'no-evidence terminal result is missing'
service_metric() {
  case "$3" in
    recorder_feed_inputs_total) printf '100\n' ;;
    recorder_irrelevant_filtered_total) printf '90\n' ;;
    recorder_unsupported_interesting_total) printf '5\n' ;;
    recorder_relevant_route_inputs_total) printf '5\n' ;;
    shadow_dispatcher_rows_published_total) printf '5\n' ;;
    shadow_dispatcher_oldest_claimable_age_seconds) printf '15.5\n' ;;
    shadow_dispatcher_pending_rows_estimate) printf '3\n' ;;
    *) return 1 ;;
  esac
}
sql_count() {
  case "$1" in
    *shadow_engine_processing_attempts*) printf '10\n' ;;
    *'sum(candidate_count)'*) printf '4\n' ;;
    *shadow_engine_classifications*) printf '8\n' ;;
    *shadow_decisions*) printf '3\n' ;;
    *) return 1 ;;
  esac
}
diagnostics=$(positive_evidence_no_evidence_diagnostics) ||
  fail 'bounded no-evidence diagnostic harness failed'
printf '%s\n' "$diagnostics" | grep -Fx \
  'POSITIVE_ROUTE_NO_EVIDENCE_DIAGNOSTICS feed_inputs=100 irrelevant_filtered=90 unsupported_interesting=5 relevant_route_inputs=5 dispatcher_rows_published=5 processing_attempts=10 classifications=8 candidates=4 decisions=3 oldest_claimable_age_seconds=15.5 pending_rows_estimate=3' >/dev/null ||
  fail 'bounded no-evidence diagnostics are incomplete'
if printf '%s\n' "$diagnostics" | grep -Ei '0x[0-9a-f]{40}|https?://|postgres://|nats://|tx_hash|source_event_identity' >/dev/null; then
  fail 'bounded no-evidence diagnostics expose sensitive or high-cardinality material'
fi
grep -F -- '--source-sequence "$positive_candidate_source_sequence"' "$workflow" >/dev/null ||
  fail 'production replay is not scoped to the exact source sequence'

grep -F 'sh ./scripts/shadow-positive-route-evidence-tests.sh' "$repo_dir/.github/workflows/ci.yml" >/dev/null ||
  fail 'positive-evidence orchestration tests are not wired into CI'
grep -F 'cargo test --manifest-path phoenix-engine/Cargo.toml --test positive_route_evidence' \
  "$repo_dir/.github/workflows/ci.yml" >/dev/null ||
  fail 'positive-route Rust fixture test is not wired into CI'

echo 'shadow-positive-route-evidence-tests: ok'
