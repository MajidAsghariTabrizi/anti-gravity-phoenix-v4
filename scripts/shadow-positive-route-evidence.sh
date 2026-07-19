#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
compose_file=${PHOENIX_COMPOSE_FILE:-$deploy_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$deploy_dir/current-release.env}
release_manifest=${PHOENIX_RELEASE_MANIFEST:-}
current_release_file=${PHOENIX_CURRENT_RELEASE_FILE:-$deploy_dir/current-release}
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

decimal_or_zero() {
  positive_decimal=$(printf '%s' "$1" | tr -d '[:space:]')
  if printf '%s\n' "$positive_decimal" | grep -Eq '^(0|[1-9][0-9]*)(\.[0-9]+)?$'; then
    printf '%s\n' "$positive_decimal"
  else
    printf '0\n'
  fi
}

service_metric() {
  positive_metric_service=$1
  positive_metric_port=$2
  positive_metric_name=$3
  positive_metric_payload=$(compose exec -T "$positive_metric_service" wget -q -O - \
    "http://127.0.0.1:$positive_metric_port/metrics" 2>/dev/null || true)
  positive_metric_value=$(printf '%s\n' "$positive_metric_payload" | awk \
    -v metric="$positive_metric_name" '
      $1 == metric {
        print $2
        found = 1
        exit
      }
      END {
        if (!found) {
          print 0
        }
      }
    ')
  decimal_or_zero "$positive_metric_value"
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

positive_evidence_capture_run_baseline() {
  RUN_STARTED_AT_UTC=$(sql_query \
    "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')") || return 1
  printf '%s' "$RUN_STARTED_AT_UTC" |
    grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{6}Z$'
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

positive_evidence_prepare_runtime() {
  compose stop phoenix-engine rpc-gateway >/dev/null 2>&1 || return 1
}

positive_evidence_start_runtime() {
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

positive_evidence_candidate_record() {
  sql_query "
SELECT concat_ws(
         '|',
         classification.source_event_identity,
         classification.tx_hash,
         classification.source_sequence::text,
         route.fingerprint
       )
FROM shadow_engine_classifications AS classification
CROSS JOIN LATERAL jsonb_array_elements_text(
  COALESCE(classification.evidence->'route_fingerprints', '[]'::jsonb)
) WITH ORDINALITY AS route(fingerprint, position)
CROSS JOIN LATERAL jsonb_array_elements(
  COALESCE(classification.evidence->'evaluations', '[]'::jsonb)
) WITH ORDINALITY AS evaluation(value, position)
WHERE route.position = evaluation.position
  AND classification.classified_at >= '$RUN_STARTED_AT_UTC'::timestamptz
  AND classification.candidate_count > 0
  AND classification.classification IN ('candidate_rejected', 'shadow_accepted')
  AND EXISTS (
        SELECT 1
        FROM shadow_engine_processing_attempts AS current_attempt
        WHERE current_attempt.source_event_identity = classification.source_event_identity
          AND current_attempt.completed_at >= '$RUN_STARTED_AT_UTC'::timestamptz
          AND current_attempt.classification = classification.classification
      )
  AND COALESCE(
        evaluation.value->'state'->>'state_block',
        evaluation.value->>'state_block'
      ) IS NOT NULL
ORDER BY classification.classified_at,
         classification.source_event_identity,
         route.position
LIMIT 1"
}

positive_evidence_runtime_report() {
  sql_query "
WITH selected AS (
  SELECT classification.*,
         route.fingerprint AS matched_route_id,
         evaluation.value AS evaluation
  FROM shadow_engine_classifications AS classification
  CROSS JOIN LATERAL jsonb_array_elements_text(
    COALESCE(classification.evidence->'route_fingerprints', '[]'::jsonb)
  ) WITH ORDINALITY AS route(fingerprint, position)
  CROSS JOIN LATERAL jsonb_array_elements(
    COALESCE(classification.evidence->'evaluations', '[]'::jsonb)
  ) WITH ORDINALITY AS evaluation(value, position)
  WHERE classification.source_event_identity = '$positive_candidate_identity'
    AND classification.tx_hash = '$positive_candidate_tx_hash'
    AND classification.classified_at >= '$RUN_STARTED_AT_UTC'::timestamptz
    AND classification.candidate_count > 0
    AND classification.classification IN ('candidate_rejected', 'shadow_accepted')
    AND route.position = evaluation.position
    AND route.fingerprint = '$positive_candidate_matched_route_id'
    AND COALESCE(
          evaluation.value->'state'->>'state_block',
          evaluation.value->>'state_block'
        ) IS NOT NULL
  ORDER BY route.position
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
  SELECT processing_attempt.id,
         processing_attempt.delivery_attempt,
         processing_attempt.completed_at
  FROM shadow_engine_processing_attempts AS processing_attempt
  JOIN state
    ON state.source_event_identity = processing_attempt.source_event_identity
   AND state.classification = processing_attempt.classification
  WHERE processing_attempt.completed_at >= '$RUN_STARTED_AT_UTC'::timestamptz
  ORDER BY processing_attempt.completed_at DESC, processing_attempt.id DESC
  LIMIT 1
), decision AS (
  SELECT route_evidence, primary_rejection_reason, disposition
  FROM shadow_decisions
  JOIN state
    ON state.source_event_identity = shadow_decisions.source_event_identity
   AND state.matched_route_id = shadow_decisions.route_fingerprint
  WHERE shadow_decisions.created_at >= '$RUN_STARTED_AT_UTC'::timestamptz
  ORDER BY shadow_decisions.created_at DESC, shadow_decisions.id DESC
  LIMIT 1
), normalized AS (
  SELECT state.*,
         attempt.id AS processing_attempt_id,
         attempt.delivery_attempt,
         attempt.completed_at AS processing_attempt_completed_at,
         decision.primary_rejection_reason,
         decision.disposition,
         COALESCE(decision.primary_rejection_reason, state.detail_class) AS rejection_reason,
         state.state_evidence->>'verification_status' AS verification_status,
         state.state_evidence->>'agreement_provider_id' AS agreement_provider_id,
         state.state_evidence->>'provider_agreement' AS provider_agreement,
         COALESCE(
           state.state_evidence->>'verification_response_hash',
           state.state_evidence->>'primary_response_hash',
           state.state_evidence->>'rpc_response_hash'
         ) AS rpc_response_hash,
         state.state_evidence->>'route_config_hash' AS route_config_hash,
         state.state_evidence->>'secondary_state_hash' AS secondary_state_hash,
         state.state_evidence->>'secondary_block_number' AS secondary_block_number,
         state.state_evidence->>'secondary_block_hash' AS secondary_block_hash,
         state.state_evidence->>'secondary_route_config_hash' AS secondary_route_config_hash,
         state.state_evidence->>'independent_verification_status' AS explicit_independent_status,
         state.state_evidence->'independent_verification_lifecycle' AS independent_lifecycle,
         state.state_evidence->>'verification_skip_reason' AS explicit_skip_reason,
         state.state_evidence->>'secondary_skipped' AS secondary_skipped,
         state.state_evidence->>'primary_screen_rejected' AS primary_screen_rejected
  FROM state
  JOIN attempt ON true
  LEFT JOIN decision ON true
)
SELECT jsonb_strip_nulls(jsonb_build_object(
  'source_event_identity', normalized.source_event_identity,
  'source_sequence', normalized.source_sequence::text,
  'source_transaction_hash', normalized.tx_hash,
  'classification', normalized.classification,
  'rejection_reason', normalized.rejection_reason,
  'candidate_count', normalized.candidate_count,
  'matched_route_id', normalized.matched_route_id,
  'processing_attempt_id', normalized.processing_attempt_id,
  'delivery_attempt', normalized.delivery_attempt,
  'processing_attempt_completed_at', normalized.processing_attempt_completed_at,
  'persisted_timestamp', normalized.classified_at,
  'pinned_block_number', normalized.state_evidence->>'state_block',
  'pinned_block_hash', normalized.state_evidence->>'state_block_hash',
  'primary_state_hash', normalized.state_evidence->>'state_hash',
  'route_config_hash', normalized.route_config_hash,
  'rpc_response_hash', normalized.rpc_response_hash,
  'primary_provider_result', normalized.state_evidence->>'primary_provider_id',
  'verification_status', normalized.verification_status,
  'primary_screen_rejected', CASE
    WHEN normalized.primary_screen_rejected IS NULL THEN NULL
    ELSE normalized.primary_screen_rejected::boolean
  END,
  'secondary_skipped', CASE
    WHEN normalized.secondary_skipped IS NULL THEN NULL
    ELSE normalized.secondary_skipped::boolean
  END,
  'independent_verification_status', COALESCE(normalized.explicit_independent_status, CASE
    WHEN normalized.verification_status = 'primary_only'
      AND normalized.rejection_reason IN ('no_profitable_candidate', 'liquidity_insufficient')
      AND normalized.primary_screen_rejected = 'true'
      AND normalized.secondary_skipped = 'true'
      THEN 'not_requested'
    WHEN normalized.verification_status = 'agreed' THEN 'agreed'
    WHEN normalized.verification_status = 'disagreed' THEN 'disagreed'
    WHEN normalized.verification_status = 'secondary_unavailable' THEN 'provider_unavailable'
  END),
  'independent_verification_lifecycle', COALESCE(
    normalized.independent_lifecycle,
    CASE
      WHEN normalized.verification_status = 'primary_only'
        AND normalized.rejection_reason IN ('no_profitable_candidate', 'liquidity_insufficient')
        AND normalized.primary_screen_rejected = 'true'
        AND normalized.secondary_skipped = 'true'
        THEN jsonb_build_array('not_requested')
      WHEN normalized.verification_status IN ('agreed', 'disagreed')
        THEN jsonb_build_array('requested', normalized.verification_status)
      WHEN normalized.verification_status = 'secondary_unavailable'
        THEN jsonb_build_array('requested', 'provider_unavailable')
    END
  ),
  'independent_verification_skip_reason', COALESCE(normalized.explicit_skip_reason, CASE
    WHEN normalized.verification_status = 'primary_only'
      AND normalized.rejection_reason IN ('no_profitable_candidate', 'liquidity_insufficient')
      AND normalized.primary_screen_rejected = 'true'
      AND normalized.secondary_skipped = 'true'
      THEN 'primary_screen_no_profitable_candidate'
  END),
  'independent_provider_result', CASE
    WHEN normalized.verification_status IN ('agreed', 'disagreed')
      THEN normalized.agreement_provider_id
  END,
  'verification_agreement', CASE
    WHEN normalized.verification_status = 'agreed'
      AND normalized.provider_agreement = 'true'
      THEN 'agreed'
    WHEN normalized.verification_status = 'disagreed'
      AND normalized.provider_agreement = 'false'
      THEN 'disagreed'
  END,
  'secondary_state_hash', normalized.secondary_state_hash,
  'secondary_block_number', normalized.secondary_block_number,
  'secondary_block_hash', normalized.secondary_block_hash,
  'secondary_route_config_hash', normalized.secondary_route_config_hash,
  'shadow_disposition', normalized.disposition,
  'shadow_only', true,
  'execution_request_created', false
))::text
FROM normalized"
}

positive_evidence_decoder_report() {
  compose exec -T phoenix-engine /usr/local/bin/shadow-positive-route-evidence \
    scan-postgres \
    --dsn-env POSTGRES_DSN \
    --route-registry-env ENGINE_ROUTE_REGISTRY_JSON \
    --tx-hash "$positive_candidate_tx_hash" \
    --source-sequence "$positive_candidate_source_sequence" \
    --limit 1
}

positive_evidence_validate_runtime_report() {
  printf '%s' "$1" | python3 -c '
from datetime import datetime
import json
import re
import sys

identity, tx_hash, route_id, baseline = sys.argv[1:]
primary_only_rejections = {"no_profitable_candidate", "liquidity_insufficient"}
try:
    report = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)

required = {
    "source_event_identity",
    "source_sequence",
    "source_transaction_hash",
    "classification",
    "rejection_reason",
    "candidate_count",
    "matched_route_id",
    "processing_attempt_id",
    "delivery_attempt",
    "processing_attempt_completed_at",
    "persisted_timestamp",
    "pinned_block_number",
    "pinned_block_hash",
    "primary_state_hash",
    "route_config_hash",
    "primary_provider_result",
    "verification_status",
    "independent_verification_status",
    "independent_verification_lifecycle",
    "shadow_only",
    "execution_request_created",
}
if not isinstance(report, dict) or not required.issubset(report):
    raise SystemExit(1)
if "classification_id" in report or "classification_record_id" in report:
    raise SystemExit(1)
if report["source_event_identity"] != identity:
    raise SystemExit(1)
if report["source_transaction_hash"] != tx_hash or report["matched_route_id"] != route_id:
    raise SystemExit(1)
if not re.fullmatch(r"[0-9]+", str(report["source_sequence"])):
    raise SystemExit(1)
if report["classification"] not in {"candidate_rejected", "shadow_accepted"}:
    raise SystemExit(1)
if not isinstance(report["candidate_count"], int) or report["candidate_count"] < 1:
    raise SystemExit(1)
for field in ("processing_attempt_id", "delivery_attempt"):
    if not isinstance(report[field], int) or report[field] < 1:
        raise SystemExit(1)
if report["shadow_only"] is not True or report["execution_request_created"] is not False:
    raise SystemExit(1)
for field in (
    "pinned_block_number",
    "pinned_block_hash",
    "primary_state_hash",
    "route_config_hash",
    "primary_provider_result",
):
    if not isinstance(report[field], str) or not report[field]:
        raise SystemExit(1)
if not re.fullmatch(r"[0-9]+", report["pinned_block_number"]):
    raise SystemExit(1)
if not re.fullmatch(r"0x[0-9a-f]{64}", report["pinned_block_hash"]):
    raise SystemExit(1)
if not re.fullmatch(r"[0-9a-f]{64}", report["primary_state_hash"]):
    raise SystemExit(1)
if not re.fullmatch(r"[0-9a-f]{64}", report["route_config_hash"]):
    raise SystemExit(1)
if "://" in report["primary_provider_result"]:
    raise SystemExit(1)

def timestamp(value):
    if not isinstance(value, str):
        raise ValueError
    return datetime.fromisoformat(value.replace("Z", "+00:00"))

try:
    started = timestamp(baseline)
    attempt_completed = timestamp(report["processing_attempt_completed_at"])
    persisted = timestamp(report["persisted_timestamp"])
except (TypeError, ValueError):
    raise SystemExit(1)
if attempt_completed < started or persisted < started:
    raise SystemExit(1)

status = report["verification_status"]
independent = report["independent_verification_status"]
lifecycle = report["independent_verification_lifecycle"]
if status == "primary_only":
    if report["classification"] != "candidate_rejected":
        raise SystemExit(1)
    if report["rejection_reason"] not in primary_only_rejections:
        raise SystemExit(1)
    if report.get("primary_screen_rejected") is not True:
        raise SystemExit(1)
    if report.get("secondary_skipped") is not True:
        raise SystemExit(1)
    if independent != "not_requested":
        raise SystemExit(1)
    if lifecycle != ["not_requested"]:
        raise SystemExit(1)
    if report.get("independent_verification_skip_reason") != "primary_screen_no_profitable_candidate":
        raise SystemExit(1)
    if any(field in report for field in (
        "independent_provider_result",
        "verification_agreement",
        "secondary_block_number",
        "secondary_block_hash",
        "secondary_route_config_hash",
        "secondary_state_hash",
    )):
        raise SystemExit(1)
elif status in {"agreed", "disagreed"}:
    if independent != status or report.get("verification_agreement") != status:
        raise SystemExit(1)
    if lifecycle != ["requested", status]:
        raise SystemExit(1)
    if not report.get("independent_provider_result") or not report.get("rpc_response_hash"):
        raise SystemExit(1)
    if report["independent_provider_result"] == report["primary_provider_result"]:
        raise SystemExit(1)
    if "://" in report["independent_provider_result"]:
        raise SystemExit(1)
    if report.get("secondary_block_number") != report["pinned_block_number"]:
        raise SystemExit(1)
    if report.get("secondary_block_hash") != report["pinned_block_hash"]:
        raise SystemExit(1)
    if report.get("secondary_route_config_hash") != report["route_config_hash"]:
        raise SystemExit(1)
    secondary_state = report.get("secondary_state_hash")
    if not isinstance(secondary_state, str) or not re.fullmatch(r"[0-9a-f]{64}", secondary_state):
        raise SystemExit(1)
    if (status == "agreed") != (secondary_state == report["primary_state_hash"]):
        raise SystemExit(1)
    if "independent_verification_skip_reason" in report:
        raise SystemExit(1)
elif status == "secondary_unavailable":
    if independent not in {"provider_unavailable", "integrity_failure"}:
        raise SystemExit(1)
    if lifecycle != ["requested", independent]:
        raise SystemExit(1)
    if any(field in report for field in (
        "independent_provider_result",
        "verification_agreement",
        "independent_verification_skip_reason",
        "secondary_block_number",
        "secondary_block_hash",
        "secondary_route_config_hash",
        "secondary_state_hash",
    )):
        raise SystemExit(1)
else:
    raise SystemExit(1)
' "$positive_candidate_identity" "$positive_candidate_tx_hash" \
    "$positive_candidate_matched_route_id" "$RUN_STARTED_AT_UTC"
}

positive_evidence_rpc_budget_preflight() {
  positive_rendered_config=$isolated_canary_state_dir/compose.rendered.json
  [ -s "$positive_rendered_config" ] || return 1
  python3 -c '
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as handle:
        root = json.load(handle)
    value = root["services"]["rpc-gateway"]["environment"]["RPC_STATE_REQUESTS_PER_MINUTE"]
    if isinstance(value, bool):
        raise ValueError
    budget = int(str(value))
except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
    raise SystemExit(1)
raise SystemExit(0 if budget >= 12 else 1)
' "$positive_rendered_config"
}

positive_evidence_render_preflight() {
  "$script_dir/render-production-compose.sh" \
    --compose-file "$compose_file" \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --release-manifest "$release_manifest" \
    --output "$isolated_canary_state_dir/compose.rendered.json" \
    --metadata-output "$isolated_canary_state_dir/render.metadata.json" >/dev/null
}

positive_evidence_verify_preflight() {
  # Defined by the sourced isolated-canary helper.
  # shellcheck disable=SC2154
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
  # Populated by isolated_canary_verify_snapshot in the sourced helper.
  # shellcheck disable=SC2154
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

positive_evidence_no_evidence_diagnostics() {
  positive_feed_inputs=$(service_metric recorder 9400 recorder_feed_inputs_total)
  positive_irrelevant=$(service_metric recorder 9400 recorder_irrelevant_filtered_total)
  positive_unsupported=$(service_metric recorder 9400 recorder_unsupported_interesting_total)
  positive_relevant=$(service_metric recorder 9400 recorder_relevant_route_inputs_total)
  positive_dispatched=$(service_metric shadow-dispatcher 9500 shadow_dispatcher_rows_published_total)
  positive_oldest=$(service_metric shadow-dispatcher 9500 shadow_dispatcher_oldest_claimable_age_seconds)
  positive_pending=$(service_metric shadow-dispatcher 9500 shadow_dispatcher_pending_rows_estimate)
  positive_attempts=$(sql_count \
    "SELECT count(*) FROM shadow_engine_processing_attempts WHERE completed_at >= '$RUN_STARTED_AT_UTC'::timestamptz") ||
    return 1
  positive_classifications=$(sql_count \
    "SELECT count(*) FROM shadow_engine_classifications WHERE classified_at >= '$RUN_STARTED_AT_UTC'::timestamptz") ||
    return 1
  positive_candidates=$(sql_count \
    "SELECT COALESCE(sum(candidate_count), 0) FROM shadow_engine_classifications WHERE classified_at >= '$RUN_STARTED_AT_UTC'::timestamptz") ||
    return 1
  positive_decisions=$(sql_count \
    "SELECT count(*) FROM shadow_decisions WHERE created_at >= '$RUN_STARTED_AT_UTC'::timestamptz") ||
    return 1

  printf '%s\n' \
    "POSITIVE_ROUTE_NO_EVIDENCE_DIAGNOSTICS feed_inputs=$positive_feed_inputs irrelevant_filtered=$positive_irrelevant unsupported_interesting=$positive_unsupported relevant_route_inputs=$positive_relevant dispatcher_rows_published=$positive_dispatched processing_attempts=$positive_attempts classifications=$positive_classifications candidates=$positive_candidates decisions=$positive_decisions oldest_claimable_age_seconds=$positive_oldest pending_rows_estimate=$positive_pending"
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
  if [ "$timeout_seconds" -le 0 ] || [ "$timeout_seconds" -gt 86400 ]; then
    positive_evidence_blocked 'timeout must be between 1 and 86400 seconds'
  fi
  if [ "$poll_seconds" -le 0 ] || [ "$poll_seconds" -gt 60 ]; then
    positive_evidence_blocked 'poll interval must be between 1 and 60 seconds'
  fi
  command -v docker >/dev/null 2>&1 || positive_evidence_blocked 'docker is unavailable'
  command -v python3 >/dev/null 2>&1 || positive_evidence_blocked 'python3 is unavailable'
  [ -f "$env_file" ] || positive_evidence_blocked 'production environment file is missing'
  [ -f "$release_env" ] || positive_evidence_blocked 'release environment file is missing'
  if [ -z "$release_manifest" ]; then
    [ -f "$current_release_file" ] || positive_evidence_blocked 'current release pointer is missing'
    positive_release_sha=$(tr -d '\r\n' <"$current_release_file")
    case "$positive_release_sha" in
      *[!0-9a-f]*|"") positive_evidence_blocked 'current release pointer is invalid' ;;
    esac
    [ "${#positive_release_sha}" -eq 40 ] || positive_evidence_blocked 'current release pointer is invalid'
    release_manifest=$deploy_dir/manifests/$positive_release_sha.json
  fi
  [ -f "$release_manifest" ] || positive_evidence_blocked 'release manifest is missing'

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

  positive_evidence_render_preflight || positive_evidence_fail 'canonical production render failed'
  positive_evidence_rpc_budget_preflight ||
    positive_evidence_fail 'RPC_STATE_REQUESTS_PER_MINUTE must be at least 12'
  positive_evidence_verify_preflight || positive_evidence_fail 'dependency preflight failed'
  isolated_canary_record_snapshot || positive_evidence_fail 'protected service snapshot failed'

  positive_runtime_touched=1
  positive_evidence_prepare_runtime || positive_evidence_fail 'Engine and RPC Gateway failed to stop before baseline'
  positive_evidence_capture_run_baseline || positive_evidence_fail 'run baseline query failed'
  positive_ack_before=$(engine_js_value ack_pending)
  positive_pending_before=$(engine_js_value pending)
  positive_execution_attempts_before=$(sql_count 'SELECT count(*) FROM execution_attempts') || positive_evidence_fail 'execution evidence query failed'
  positive_executions_before=$(sql_count 'SELECT count(*) FROM executions') || positive_evidence_fail 'execution evidence query failed'
  positive_realized_before=$(sql_count 'SELECT count(*) FROM realized_pnl') || positive_evidence_fail 'execution evidence query failed'
  positive_execution_eligible_before=$(sql_count 'SELECT count(*) FROM shadow_decisions WHERE execution_eligible') || positive_evidence_fail 'execution eligibility query failed'

  positive_evidence_start_runtime || positive_evidence_fail 'Engine and RPC Gateway failed to start'
  positive_evidence_wait_ready || positive_evidence_fail 'Engine or RPC Gateway did not become ready'

  positive_deadline=$(( $(date +%s) + timeout_seconds ))
  positive_candidate_record=
  while [ "$(date +%s)" -lt "$positive_deadline" ]; do
    if positive_engine_failure=$(isolated_canary_engine_failure_reason); then
      positive_evidence_fail "Engine failed before positive evidence: $positive_engine_failure"
    fi
    positive_candidate_record=$(positive_evidence_candidate_record) || positive_evidence_fail 'candidate evidence query failed'
    if [ -n "$positive_candidate_record" ]; then
      positive_candidate_identity=${positive_candidate_record%%|*}
      positive_candidate_remainder=${positive_candidate_record#*|}
      positive_candidate_tx_hash=${positive_candidate_remainder%%|*}
      positive_candidate_remainder=${positive_candidate_remainder#*|}
      positive_candidate_source_sequence=${positive_candidate_remainder%%|*}
      positive_candidate_matched_route_id=${positive_candidate_remainder#*|}
      printf '%s' "$positive_candidate_identity" | grep -Eq '^phoenix\.engine\.input\.v1:[0-9]+:0x[0-9a-f]{64}$' || positive_evidence_fail 'candidate identity is invalid'
      printf '%s' "$positive_candidate_tx_hash" | grep -Eq '^0x[0-9a-f]{64}$' || positive_evidence_fail 'candidate transaction hash is invalid'
      printf '%s' "$positive_candidate_source_sequence" | grep -Eq '^[0-9]+$' || positive_evidence_fail 'candidate source sequence is invalid'
      printf '%s' "$positive_candidate_matched_route_id" | grep -Eq '^[A-Za-z0-9._:-]{1,256}$' || positive_evidence_fail 'candidate route identity is invalid'
      [ "$positive_candidate_identity" = "phoenix.engine.input.v1:$positive_candidate_source_sequence:$positive_candidate_tx_hash" ] ||
        positive_evidence_fail 'candidate identity does not match transaction and source sequence'
      positive_decoder_report=$(positive_evidence_decoder_report) || positive_evidence_fail 'production decoder replay failed'
      printf '%s\n' "$positive_decoder_report" | grep -F 'POSITIVE_ROUTE_EVIDENCE_FOUND' >/dev/null || positive_evidence_fail 'production decoder replay did not confirm the route candidate'
      positive_runtime_report=$(positive_evidence_runtime_report) || positive_evidence_fail 'persisted runtime evidence query failed'
      [ -n "$positive_runtime_report" ] || positive_evidence_fail 'persisted runtime evidence is incomplete'
      positive_evidence_validate_runtime_report "$positive_runtime_report" || positive_evidence_fail 'persisted runtime evidence validation failed'
      positive_evidence_finish POSITIVE_ROUTE_EVIDENCE_FOUND "$positive_decoder_report" "$positive_runtime_report"
      return 0
    fi
    sleep "$poll_seconds"
  done
  positive_evidence_no_evidence_diagnostics ||
    positive_evidence_fail 'bounded no-evidence diagnostics failed'
  positive_evidence_finish POSITIVE_ROUTE_EVIDENCE_NOT_FOUND '' ''
}

if [ "${SHADOW_POSITIVE_ROUTE_EVIDENCE_LIBRARY_ONLY:-0}" = 1 ]; then
  # The exit fallback is used only when the script is executed instead of sourced.
  # shellcheck disable=SC2317
  return 0 2>/dev/null || exit 0
fi

positive_evidence_main "$@"
