#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=${PHOENIX_COMPOSE_FILE:-$repo_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$repo_dir/deploy/current-release.env}
timeout_seconds=${SHADOW_POSITIVE_ROUTE_TIMEOUT_SECONDS:-900}
poll_seconds=${SHADOW_POSITIVE_ROUTE_POLL_SECONDS:-3}
engine_stream=PHOENIX_ENGINE_INPUT
engine_consumer=PHOENIX_ENGINE_SHADOW

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi

# shellcheck disable=SC1091
. "$script_dir/shadow-engine-isolated-canary.sh"

positive_evidence_blocked() {
  echo "SHADOW_POSITIVE_ROUTE_EVIDENCE_BLOCKED: $1" >&2
  exit 1
}

compose() {
  (
    unset ENGINE_ROUTE_REGISTRY_JSON
    PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
      docker compose --env-file "$env_file" --env-file "$release_env" -f "$compose_file" "$@"
  )
}

number_or_zero() {
  positive_number=$(printf '%s' "$1" | tr -d '[:space:]')
  case "$positive_number" in
    ''|*[!0-9]*) printf '0\n' ;;
    *) printf '%s\n' "$positive_number" ;;
  esac
}

sql_query() {
  compose exec -T postgres psql \
    -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "$1"
}

sql_count() {
  positive_count=$(sql_query "$1") || return 1
  number_or_zero "$positive_count"
}

service_ready() {
  positive_service=$1
  positive_port=$2
  compose exec -T "$positive_service" wget -q -O - \
    "http://127.0.0.1:$positive_port/readyz" >/dev/null 2>&1
}

postgres_ready() {
  compose exec -T postgres pg_isready \
    -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null 2>&1
}

engine_js_value() {
  positive_field=$1
  positive_payload=$(compose exec -T nats wget -q -O - \
    'http://127.0.0.1:8222/jsz?consumers=true&config=true' 2>/dev/null || true)
  printf '%s' "$positive_payload" | python3 -c '
import json
import sys

stream_name, consumer_name, field = sys.argv[1:]
try:
    root = json.load(sys.stdin)
except Exception:
    print(0)
    raise SystemExit(0)

def walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk(child)

objects = list(walk(root))
stream = next((item for item in objects if item.get("name") == stream_name or item.get("config", {}).get("name") == stream_name), None)
consumer = next((item for item in objects if item.get("name") == consumer_name or item.get("config", {}).get("durable_name") == consumer_name), None)
if field == "stream_exists":
    print(1 if stream else 0)
elif field == "consumer_exists":
    print(1 if consumer else 0)
elif field == "pending":
    print(int((consumer or {}).get("num_pending", 0)))
elif field == "ack_pending":
    print(int((consumer or {}).get("num_ack_pending", 0)))
else:
    print(0)
' "$engine_stream" "$engine_consumer" "$positive_field"
}

positive_evidence_start_runtime() {
  compose stop phoenix-engine rpc-gateway >/dev/null 2>&1 || return 1
  compose up -d --no-deps rpc-gateway phoenix-engine >/dev/null 2>&1
}

positive_evidence_cleanup_runtime() {
  compose stop phoenix-engine rpc-gateway >/dev/null 2>&1
}

positive_evidence_wait_ready() {
  positive_ready_deadline=$(( $(date +%s) + 90 ))
  while [ "$(date +%s)" -lt "$positive_ready_deadline" ]; do
    if service_ready rpc-gateway 9300 && service_ready phoenix-engine 9200; then
      return 0
    fi
    sleep 2
  done
  return 1
}

positive_evidence_candidate_identity() {
  sql_query "
SELECT classification.source_event_identity
FROM shadow_engine_classifications AS classification
CROSS JOIN LATERAL jsonb_array_elements(
  COALESCE(classification.evidence->'evaluations', '[]'::jsonb)
) AS evaluation(value)
WHERE classification.classified_at >= '$positive_evidence_started'::timestamptz
  AND classification.candidate_count > 0
  AND classification.classification IN ('candidate_rejected', 'shadow_accepted')
  AND COALESCE(
        evaluation.value->'state'->>'state_block',
        evaluation.value->>'state_block'
      ) IS NOT NULL
ORDER BY classification.classified_at, classification.source_event_identity
LIMIT 1"
}

positive_evidence_runtime_report() {
  sql_query "
WITH selected AS (
  SELECT classification.*,
         evaluation.value AS evaluation
  FROM shadow_engine_classifications AS classification
  CROSS JOIN LATERAL jsonb_array_elements(
    COALESCE(classification.evidence->'evaluations', '[]'::jsonb)
  ) AS evaluation(value)
  WHERE classification.source_event_identity = '$positive_candidate_identity'
    AND COALESCE(
          evaluation.value->'state'->>'state_block',
          evaluation.value->>'state_block'
        ) IS NOT NULL
  LIMIT 1
), state AS (
  SELECT selected.*,
         CASE
           WHEN jsonb_typeof(selected.evaluation->'state') = 'object'
             THEN selected.evaluation->'state'
           ELSE selected.evaluation
         END AS state_evidence
  FROM selected
), attempt AS (
  SELECT id
  FROM shadow_engine_processing_attempts
  WHERE source_event_identity = '$positive_candidate_identity'
  ORDER BY created_at, id
  LIMIT 1
), decision AS (
  SELECT route_evidence, primary_rejection_reason, disposition
  FROM shadow_decisions
  WHERE source_event_identity = '$positive_candidate_identity'
  ORDER BY created_at, id
  LIMIT 1
)
SELECT jsonb_build_object(
  'source_transaction_hash', state.tx_hash,
  'matched_route_id', COALESCE(
    decision.route_evidence->>'route_id',
    state.evidence->'route_fingerprints'->>0
  ),
  'candidate_count', state.candidate_count,
  'primary_provider_result', state.state_evidence->>'primary_provider_id',
  'independent_provider_result', state.state_evidence->>'agreement_provider_id',
  'pinned_block_number', state.state_evidence->>'state_block',
  'pinned_block_hash', state.state_evidence->>'state_block_hash',
  'primary_state_hash', state.state_evidence->>'state_hash',
  'verification_status', state.state_evidence->>'verification_status',
  'verification_agreement', state.state_evidence->>'provider_agreement',
  'rpc_response_hash', state.state_evidence->>'rpc_response_hash',
  'classification_id', state.source_event_identity,
  'processing_attempt_id', attempt.id,
  'source_sequence', state.source_sequence::text,
  'persisted_timestamp', state.classified_at,
  'classification', state.classification,
  'rejection_reason', COALESCE(decision.primary_rejection_reason, state.detail_class),
  'shadow_disposition', decision.disposition
)::text
FROM state
LEFT JOIN attempt ON true
LEFT JOIN decision ON true"
}

positive_evidence_decoder_report() {
  positive_tx_hash=$(sql_query "SELECT tx_hash FROM shadow_engine_classifications WHERE source_event_identity = '$positive_candidate_identity'") || return 1
  positive_source_sequence=$(sql_query "SELECT source_sequence::text FROM shadow_engine_classifications WHERE source_event_identity = '$positive_candidate_identity'") || return 1
  printf '%s' "$positive_tx_hash" | grep -Eq '^0x[0-9a-f]{64}$' || return 1
  printf '%s' "$positive_source_sequence" | grep -Eq '^[0-9]+$' || return 1
  compose exec -T phoenix-engine /usr/local/bin/shadow-positive-route-evidence \
    scan-postgres \
    --dsn-env POSTGRES_DSN \
    --route-registry-env ENGINE_ROUTE_REGISTRY_JSON \
    --tx-hash "$positive_tx_hash" \
    --source-sequence "$positive_source_sequence" \
    --limit 1
}

positive_evidence_verify_preflight() {
  isolated_canary_route_registry_preflight || return 1
  for positive_service in $isolated_canary_required_services; do
    isolated_canary_container_is_healthy "$positive_service" || return 1
  done
  service_ready feed-ingestor 9100 || return 1
  service_ready recorder 9400 || return 1
  postgres_ready || return 1
  [ "$(engine_js_value stream_exists)" = '1' ] || return 1
  [ "$(engine_js_value consumer_exists)" = '1' ] || return 1
  isolated_canary_image_is_local_and_pinned "${RPC_GATEWAY_IMAGE:-}" || return 1
  isolated_canary_image_is_local_and_pinned "${PHOENIX_ENGINE_IMAGE:-}" || return 1
}

positive_evidence_finish() {
  positive_result=$1
  positive_decoder_report=${2:-}
  positive_runtime_report=${3:-}

  positive_evidence_cleanup_runtime || positive_evidence_fail 'optional runtime cleanup failed'
  positive_runtime_touched=0
  positive_ack_after=$(engine_js_value ack_pending)
  positive_pending_after=$(engine_js_value pending)
  isolated_canary_verify_snapshot || positive_evidence_fail "protected service changed: $isolated_canary_changed_service"

  positive_execution_attempts_after=$(sql_count 'SELECT count(*) FROM execution_attempts') || positive_evidence_fail 'execution evidence query failed'
  positive_executions_after=$(sql_count 'SELECT count(*) FROM executions') || positive_evidence_fail 'execution evidence query failed'
  positive_realized_after=$(sql_count 'SELECT count(*) FROM realized_pnl') || positive_evidence_fail 'execution evidence query failed'
  positive_execution_eligible_after=$(sql_count 'SELECT count(*) FROM shadow_decisions WHERE execution_eligible') || positive_evidence_fail 'execution eligibility query failed'
  [ "$positive_execution_attempts_after" -eq "$positive_execution_attempts_before" ] || positive_evidence_fail 'execution attempts changed during SHADOW evidence run'
  [ "$positive_executions_after" -eq "$positive_executions_before" ] || positive_evidence_fail 'executions changed during SHADOW evidence run'
  [ "$positive_realized_after" -eq "$positive_realized_before" ] || positive_evidence_fail 'realized PnL changed during SHADOW evidence run'
  [ "$positive_execution_eligible_after" -eq "$positive_execution_eligible_before" ] || positive_evidence_fail 'execution eligibility changed during SHADOW evidence run'

  isolated_canary_remove_state
  positive_finalized=1
  trap - EXIT HUP INT TERM
  [ -z "$positive_decoder_report" ] || printf 'PRODUCTION_DECODER_EVIDENCE %s\n' "$positive_decoder_report"
  [ -z "$positive_runtime_report" ] || printf 'PERSISTED_RUNTIME_EVIDENCE %s\n' "$positive_runtime_report"
  printf 'JETSTREAM_OPERATIONAL_EVIDENCE ack_pending_before=%s ack_pending_after=%s pending_before=%s pending_after=%s\n' \
    "$positive_ack_before" "$positive_ack_after" "$positive_pending_before" "$positive_pending_after"
  printf '%s\n' "$positive_result"
}

positive_evidence_fail() {
  positive_failure_reason=$1
  if [ "${positive_runtime_touched:-0}" -eq 1 ]; then
    positive_evidence_cleanup_runtime >/dev/null 2>&1 || true
    positive_runtime_touched=0
  fi
  if [ "${isolated_canary_snapshot_recorded:-0}" -eq 1 ] && ! isolated_canary_verify_snapshot; then
    positive_failure_reason="protected service changed: $isolated_canary_changed_service"
  fi
  isolated_canary_remove_state
  positive_finalized=1
  trap - EXIT HUP INT TERM
  echo "SHADOW_POSITIVE_ROUTE_EVIDENCE_FAIL: $positive_failure_reason" >&2
  exit 1
}

positive_evidence_exit_guard() {
  positive_exit_status=$?
  trap - EXIT HUP INT TERM
  if [ "${positive_finalized:-0}" -ne 1 ]; then
    [ "${positive_runtime_touched:-0}" -ne 1 ] || positive_evidence_cleanup_runtime >/dev/null 2>&1 || true
    echo 'SHADOW_POSITIVE_ROUTE_EVIDENCE_FAIL: unexpected workflow failure' >&2
  fi
  [ "$positive_exit_status" -ne 0 ] || positive_exit_status=1
  exit "$positive_exit_status"
}

positive_evidence_main() {
  case "${1:-}" in
    '') ;;
    --timeout-seconds)
      [ "$#" -eq 2 ] || positive_evidence_blocked 'usage: --timeout-seconds SECONDS'
      timeout_seconds=$2
      ;;
    *) positive_evidence_blocked 'usage: --timeout-seconds SECONDS' ;;
  esac
  case "$timeout_seconds:$poll_seconds" in
    *[!0-9:]*) positive_evidence_blocked 'timeouts must be positive integers' ;;
  esac
  [ "$timeout_seconds" -gt 0 ] && [ "$timeout_seconds" -le 86400 ] || positive_evidence_blocked 'timeout must be between 1 and 86400 seconds'
  [ "$poll_seconds" -gt 0 ] && [ "$poll_seconds" -le 60 ] || positive_evidence_blocked 'poll interval must be between 1 and 60 seconds'
  command -v docker >/dev/null 2>&1 || positive_evidence_blocked 'docker is unavailable'
  command -v python3 >/dev/null 2>&1 || positive_evidence_blocked 'python3 is unavailable'
  [ -f "$env_file" ] || positive_evidence_blocked 'production environment file is missing'
  if [ ! -f "$release_env" ] && [ -f "$repo_dir/current-release.env" ]; then
    release_env="$repo_dir/current-release.env"
  fi
  [ -f "$release_env" ] || positive_evidence_blocked 'release environment file is missing'

  set -a
  # shellcheck disable=SC1090
  . "$env_file"
  # shellcheck disable=SC1090
  . "$release_env"
  set +a
  [ "${PHOENIX_MODE:-}" = SHADOW ] || positive_evidence_blocked 'PHOENIX_MODE must be SHADOW'
  [ "${LIVE_EXECUTION:-}" = false ] || positive_evidence_blocked 'LIVE_EXECUTION must be false'
  [ -z "${SIGNER_PRIVATE_KEY:-}" ] || positive_evidence_blocked 'SIGNER_PRIVATE_KEY must be blank'
  [ -z "${EXECUTOR_ADDRESS:-}" ] || positive_evidence_blocked 'EXECUTOR_ADDRESS must be blank'
  [ -z "${WALLET_ADDRESS:-}" ] || positive_evidence_blocked 'WALLET_ADDRESS must be blank'

  isolated_canary_state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-positive-route.XXXXXX") || positive_evidence_blocked 'workflow state could not be created'
  isolated_canary_snapshot_recorded=0
  positive_runtime_touched=0
  positive_finalized=0
  trap positive_evidence_exit_guard EXIT HUP INT TERM

  positive_evidence_verify_preflight || positive_evidence_fail 'dependency or route-registry preflight failed'
  isolated_canary_record_snapshot || positive_evidence_fail 'protected service snapshot failed'
  positive_ack_before=$(engine_js_value ack_pending)
  positive_pending_before=$(engine_js_value pending)
  positive_execution_attempts_before=$(sql_count 'SELECT count(*) FROM execution_attempts') || positive_evidence_fail 'execution evidence query failed'
  positive_executions_before=$(sql_count 'SELECT count(*) FROM executions') || positive_evidence_fail 'execution evidence query failed'
  positive_realized_before=$(sql_count 'SELECT count(*) FROM realized_pnl') || positive_evidence_fail 'execution evidence query failed'
  positive_execution_eligible_before=$(sql_count 'SELECT count(*) FROM shadow_decisions WHERE execution_eligible') || positive_evidence_fail 'execution eligibility query failed'

  positive_evidence_started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  positive_runtime_touched=1
  positive_evidence_start_runtime || positive_evidence_fail 'Engine and RPC Gateway failed to start'
  positive_evidence_wait_ready || positive_evidence_fail 'Engine or RPC Gateway did not become ready'

  positive_deadline=$(( $(date +%s) + timeout_seconds ))
  positive_candidate_identity=
  while [ "$(date +%s)" -lt "$positive_deadline" ]; do
    if positive_engine_failure=$(isolated_canary_engine_failure_reason); then
      positive_evidence_fail "Engine failed before positive evidence: $positive_engine_failure"
    fi
    positive_candidate_identity=$(positive_evidence_candidate_identity) || positive_evidence_fail 'candidate evidence query failed'
    if [ -n "$positive_candidate_identity" ]; then
      printf '%s' "$positive_candidate_identity" | grep -Eq '^phoenix\.engine\.input\.v1:[0-9]+:0x[0-9a-f]{64}$' || positive_evidence_fail 'candidate identity is invalid'
      positive_decoder_report=$(positive_evidence_decoder_report) || positive_evidence_fail 'production decoder replay failed'
      printf '%s\n' "$positive_decoder_report" | grep -F 'POSITIVE_ROUTE_EVIDENCE_FOUND' >/dev/null || positive_evidence_fail 'production decoder replay did not confirm the route candidate'
      positive_runtime_report=$(positive_evidence_runtime_report) || positive_evidence_fail 'persisted runtime evidence query failed'
      [ -n "$positive_runtime_report" ] || positive_evidence_fail 'persisted runtime evidence is incomplete'
      positive_evidence_finish POSITIVE_ROUTE_EVIDENCE_FOUND "$positive_decoder_report" "$positive_runtime_report"
      return 0
    fi
    sleep "$poll_seconds"
  done
  positive_evidence_finish POSITIVE_ROUTE_EVIDENCE_NOT_FOUND '' ''
}

if [ "${SHADOW_POSITIVE_ROUTE_EVIDENCE_LIBRARY_ONLY:-0}" = 1 ]; then
  return 0 2>/dev/null || exit 0
fi

positive_evidence_main "$@"
