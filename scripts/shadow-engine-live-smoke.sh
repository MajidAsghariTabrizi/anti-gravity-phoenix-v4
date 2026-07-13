#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=${PHOENIX_COMPOSE_FILE:-$repo_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$repo_dir/deploy/current-release.env}
engine_stream=PHOENIX_ENGINE_INPUT
engine_consumer=PHOENIX_ENGINE_SHADOW
max_pending=${SHADOW_ENGINE_SMOKE_MAX_PENDING:-100000}
max_ack_pending=512
evidence_timeout=${SHADOW_ENGINE_SMOKE_EVIDENCE_TIMEOUT_SECONDS:-180}
recovery_timeout=${SHADOW_ENGINE_SMOKE_RECOVERY_TIMEOUT_SECONDS:-180}

if [ ! -f "$release_env" ] && [ -f "$repo_dir/current-release.env" ]; then
  release_env="$repo_dir/current-release.env"
fi

blocked() {
  echo "SHADOW_ENGINE_LIVE_SMOKE_BLOCKED: $1" >&2
  exit 1
}

if ! command -v docker >/dev/null 2>&1; then
  blocked "docker is unavailable"
fi
if ! command -v python3 >/dev/null 2>&1; then
  blocked "python3 is unavailable"
fi
if [ ! -f "$env_file" ]; then
  blocked "production environment file is missing"
fi
if [ ! -f "$release_env" ]; then
  blocked "deploy/current-release.env is missing"
fi

set -a
# shellcheck disable=SC1090
. "$env_file"
# shellcheck disable=SC1090
. "$release_env"
set +a

[ "${PHOENIX_MODE:-}" = "SHADOW" ] || blocked "PHOENIX_MODE must be SHADOW"
[ "${LIVE_EXECUTION:-}" = "false" ] || blocked "LIVE_EXECUTION must be false"
[ -z "${SIGNER_PRIVATE_KEY:-}" ] || blocked "SIGNER_PRIVATE_KEY must be blank"
[ -z "${EXECUTOR_ADDRESS:-}" ] || blocked "EXECUTOR_ADDRESS must be blank"
[ -z "${WALLET_ADDRESS:-}" ] || blocked "WALLET_ADDRESS must be blank"
case "${ENGINE_ROUTE_REGISTRY_JSON:-[]}" in
  ''|'[]') blocked "a reviewed non-empty Engine route registry is required" ;;
esac
case "$max_pending:$evidence_timeout:$recovery_timeout" in
  *[!0-9:]*) blocked "smoke thresholds must be positive integers" ;;
esac
if [ "$max_pending" -le 0 ] || [ "$evidence_timeout" -le 0 ] || [ "$recovery_timeout" -le 0 ]; then
  blocked "smoke thresholds must be greater than zero"
fi

compose() {
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose --env-file "$env_file" --env-file "$release_env" -f "$compose_file" "$@"
}

number_or_zero() {
  value=$(printf '%s' "$1" | tr -d '[:space:]')
  case "$value" in
    ''|*[!0-9]*) printf '0\n' ;;
    *) printf '%s\n' "$value" ;;
  esac
}

sql_count() {
  query=$1
  value=$(compose exec -T postgres psql \
    -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
    -Atc "$query" 2>/dev/null || true)
  number_or_zero "$value"
}

service_ready() {
  service=$1
  port=$2
  compose exec -T "$service" wget -q -O - "http://127.0.0.1:$port/readyz" >/dev/null 2>&1
}

service_metric_count() {
  service=$1
  port=$2
  metric=$3
  payload=$(compose exec -T "$service" wget -q -O - \
    "http://127.0.0.1:$port/metrics" 2>/dev/null || true)
  value=$(printf '%s\n' "$payload" | awk -v metric="$metric" '$1 == metric { print $2; exit }')
  number_or_zero "$value"
}

postgres_ready() {
  compose exec -T postgres pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null 2>&1
}

engine_js_value() {
  field=$1
  payload=$(compose exec -T nats wget -q -O - \
    'http://127.0.0.1:8222/jsz?consumers=true&config=true' 2>/dev/null || true)
  printf '%s' "$payload" | python3 -c '
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
stream = next(
    (item for item in objects
     if item.get("name") == stream_name
     or item.get("config", {}).get("name") == stream_name),
    None,
)
consumer = next(
    (item for item in objects
     if item.get("name") == consumer_name
     or item.get("config", {}).get("durable_name") == consumer_name),
    None,
)
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
' "$engine_stream" "$engine_consumer" "$field"
}

wait_ready() {
  service=$1
  port=$2
  timeout=$3
  deadline=$(( $(date +%s) + timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if service_ready "$service" "$port"; then
      return 0
    fi
    sleep 3
  done
  return 1
}

wait_unready() {
  service=$1
  port=$2
  timeout=$3
  deadline=$(( $(date +%s) + timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if ! service_ready "$service" "$port"; then
      return 0
    fi
    sleep 2
  done
  return 1
}

wait_postgres() {
  deadline=$(( $(date +%s) + recovery_timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if postgres_ready; then
      return 0
    fi
    sleep 3
  done
  return 1
}

runtime_ready() {
  service_ready feed-ingestor 9100 &&
    service_ready recorder 9400 &&
    service_ready shadow-dispatcher 9500 &&
    service_ready rpc-gateway 9300 &&
    service_ready phoenix-engine 9200 &&
    [ "$(engine_js_value stream_exists)" -eq 1 ] &&
    [ "$(engine_js_value consumer_exists)" -eq 1 ]
}

wait_runtime() {
  deadline=$(( $(date +%s) + recovery_timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if runtime_ready; then
      return 0
    fi
    sleep 3
  done
  return 1
}

diagnostics() {
  echo "SHADOW_ENGINE_LIVE_SMOKE_DIAGNOSTICS: bounded status and logs follow" >&2
  compose ps postgres nats rpc-gateway feed-ingestor recorder shadow-dispatcher phoenix-engine >&2 || true
  echo "engine_pending=$(engine_js_value pending) engine_ack_pending=$(engine_js_value ack_pending)" >&2
  echo "outbox_pending=$(sql_count 'SELECT count(*) FROM engine_outbox WHERE published_at IS NULL')" >&2
  compose logs --no-color --tail 50 nats rpc-gateway recorder shadow-dispatcher phoenix-engine >&2 || true
}

fail() {
  echo "SHADOW_ENGINE_LIVE_SMOKE_FAIL: $1" >&2
  compose up -d postgres nats rpc-gateway recorder shadow-dispatcher phoenix-engine feed-ingestor >/dev/null 2>&1 || true
  diagnostics
  exit 1
}

assert_bounded_pending() {
  outbox_pending=$(sql_count 'SELECT count(*) FROM engine_outbox WHERE published_at IS NULL')
  engine_pending=$(engine_js_value pending)
  engine_ack_pending=$(engine_js_value ack_pending)
  [ "$outbox_pending" -le "$max_pending" ] || fail "outbox pending exceeded $max_pending"
  [ "$engine_pending" -le "$max_pending" ] || fail "Engine consumer pending exceeded $max_pending"
  [ "$engine_ack_pending" -le "$max_ack_pending" ] || fail "Engine ack_pending exceeded $max_ack_pending"
}

wait_for_pipeline_growth() {
  deadline=$(( $(date +%s) + evidence_timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    assert_bounded_pending
    feed_now=$(sql_count 'SELECT count(*) FROM feed_events')
    outbox_now=$(sql_count 'SELECT count(*) FROM engine_outbox')
    published_now=$(sql_count 'SELECT count(*) FROM engine_outbox WHERE published_at IS NOT NULL AND jetstream_ack_sequence IS NOT NULL')
    classifications_now=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
    if [ "$feed_now" -gt "$feed_before" ] &&
      [ "$outbox_now" -gt "$outbox_before" ] &&
      [ "$published_now" -gt "$published_before" ] &&
      [ "$classifications_now" -gt "$classifications_before" ]; then
      return 0
    fi
    sleep 3
  done
  return 1
}

wait_for_classification_growth() {
  baseline=$1
  deadline=$(( $(date +%s) + recovery_timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    current=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
    if [ "$current" -gt "$baseline" ]; then
      return 0
    fi
    sleep 3
  done
  return 1
}

shadow_label='SHADOW / SIMULATED — NOT REALIZED CAPITAL PNL'
grep -F "$shadow_label" "$repo_dir/dashboard/app.py" >/dev/null || blocked "dashboard SHADOW label is absent"

smoke_started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
compose config >/dev/null || fail "production Compose configuration is invalid"
compose stop feed-ingestor recorder shadow-dispatcher phoenix-engine dashboard prometheus >/dev/null 2>&1 || true
compose up -d postgres nats nitro-feed-relay rpc-gateway || fail "base runtime dependencies failed to start"
wait_postgres || fail "PostgreSQL did not become ready"
compose up --no-deps migration-runner || fail "database migrations failed"

feed_before=$(sql_count 'SELECT count(*) FROM feed_events')
outbox_before=$(sql_count 'SELECT count(*) FROM engine_outbox')
published_before=$(sql_count 'SELECT count(*) FROM engine_outbox WHERE published_at IS NOT NULL AND jetstream_ack_sequence IS NOT NULL')
classifications_before=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
execution_attempts_before=$(sql_count 'SELECT count(*) FROM execution_attempts')
executions_before=$(sql_count 'SELECT count(*) FROM executions')
realized_before=$(sql_count 'SELECT count(*) FROM realized_pnl')

compose up -d prometheus dashboard recorder shadow-dispatcher feed-ingestor phoenix-engine || fail "SHADOW runtime services failed to start"
wait_runtime || fail "SHADOW runtime did not become ready"
wait_for_pipeline_growth || fail "atomic outbox, Dispatcher ACK, or Engine processing evidence did not increase"

missing_outbox=$(sql_count "SELECT count(*) FROM feed_events feed LEFT JOIN engine_outbox outbox ON outbox.source_sequence = feed.sequence_number AND outbox.tx_hash = feed.tx_hash WHERE feed.recorded_at >= '$smoke_started'::timestamptz AND outbox.outbox_id IS NULL")
[ "$missing_outbox" -eq 0 ] || fail "a smoke-window feed event committed without its atomic outbox row"
missing_attempt=$(sql_count "SELECT count(*) FROM shadow_engine_classifications classification LEFT JOIN shadow_engine_processing_attempts attempt ON attempt.source_event_identity = classification.source_event_identity WHERE classification.classified_at >= '$smoke_started'::timestamptz AND attempt.id IS NULL")
[ "$missing_attempt" -eq 0 ] || fail "an Engine classification lacks auditable processing-attempt evidence"
assert_bounded_pending

container_shadow_check='[ "${PHOENIX_MODE:-}" = "SHADOW" ] && [ "${LIVE_EXECUTION:-}" = "false" ] && [ -z "${SIGNER_PRIVATE_KEY:-}" ] && [ -z "${EXECUTOR_ADDRESS:-}" ] && [ -z "${WALLET_ADDRESS:-}" ]'
compose exec -T phoenix-engine sh -c "$container_shadow_check" || fail "Engine LIVE or signer settings are not fail closed"
compose exec -T shadow-dispatcher sh -c "$container_shadow_check" || fail "Dispatcher LIVE or signer settings are not fail closed"
compose exec -T dashboard python -c 'from pathlib import Path; assert "SHADOW / SIMULATED — NOT REALIZED CAPITAL PNL" in Path("/app/app.py").read_text()' || fail "dashboard image lacks the SHADOW financial label"

restart_classifications=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
pending_before_restart=$(engine_js_value pending)
compose stop phoenix-engine || fail "Engine could not be stopped for replay verification"
restart_deadline=$(( $(date +%s) + evidence_timeout ))
pending_during_restart=$pending_before_restart
while [ "$(date +%s)" -lt "$restart_deadline" ]; do
  pending_during_restart=$(engine_js_value pending)
  [ "$pending_during_restart" -le "$max_pending" ] || fail "Engine backlog exceeded its bound while stopped"
  if [ "$pending_during_restart" -gt "$pending_before_restart" ]; then
    break
  fi
  sleep 3
done
[ "$pending_during_restart" -gt "$pending_before_restart" ] || fail "Engine durable backlog did not grow while Engine was stopped"

compose up -d phoenix-engine || fail "Engine could not be restarted"
wait_ready phoenix-engine 9200 "$recovery_timeout" || fail "Engine did not become ready after restart"
replay_deadline=$(( $(date +%s) + recovery_timeout ))
replay_proven=0
while [ "$(date +%s)" -lt "$replay_deadline" ]; do
  pending_now=$(engine_js_value pending)
  classifications_now=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
  if [ "$pending_now" -lt "$pending_during_restart" ] && [ "$classifications_now" -gt "$restart_classifications" ]; then
    replay_proven=1
    break
  fi
  sleep 3
done
[ "$replay_proven" -eq 1 ] || fail "Engine restart backlog was not replayed into persisted classifications"
assert_bounded_pending

rpc_failure_before=$(sql_count "SELECT count(*) FROM shadow_engine_processing_attempts WHERE error_class = 'rpc_gateway_unavailable'")
rpc_recovery_before=$(sql_count "SELECT count(DISTINCT attempt.source_event_identity) FROM shadow_engine_processing_attempts attempt JOIN shadow_engine_classifications classification USING (source_event_identity) WHERE attempt.error_class = 'rpc_gateway_unavailable' AND classification.classification <> 'transient_dependency_failure'")
compose stop rpc-gateway || fail "RPC Gateway could not be stopped for failure classification"
wait_unready phoenix-engine 9200 30 || fail "Engine readiness did not fail closed during the RPC outage"
rpc_deadline=$(( $(date +%s) + evidence_timeout ))
rpc_failure_after=$rpc_failure_before
while [ "$(date +%s)" -lt "$rpc_deadline" ]; do
  rpc_failure_after=$(sql_count "SELECT count(*) FROM shadow_engine_processing_attempts WHERE error_class = 'rpc_gateway_unavailable'")
  if [ "$rpc_failure_after" -gt "$rpc_failure_before" ]; then
    break
  fi
  sleep 3
done
[ "$rpc_failure_after" -gt "$rpc_failure_before" ] || fail "no real route event reached RPC failure classification"
compose up -d rpc-gateway || fail "RPC Gateway could not be restarted"
wait_ready rpc-gateway 9300 "$recovery_timeout" || fail "RPC Gateway did not recover"
wait_ready phoenix-engine 9200 "$recovery_timeout" || fail "Engine did not recover after the RPC outage"
rpc_recovery_deadline=$(( $(date +%s) + recovery_timeout ))
rpc_recovered=0
while [ "$(date +%s)" -lt "$rpc_recovery_deadline" ]; do
  rpc_recovery_after=$(sql_count "SELECT count(DISTINCT attempt.source_event_identity) FROM shadow_engine_processing_attempts attempt JOIN shadow_engine_classifications classification USING (source_event_identity) WHERE attempt.error_class = 'rpc_gateway_unavailable' AND classification.classification <> 'transient_dependency_failure'")
  if [ "$rpc_recovery_after" -gt "$rpc_recovery_before" ]; then
    rpc_recovered=1
    break
  fi
  sleep 3
done
[ "$rpc_recovered" -eq 1 ] || fail "RPC-failed Engine input did not recover to a non-transient classification"

postgres_recovery_baseline=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
compose stop postgres || fail "PostgreSQL could not be stopped for recovery verification"
wait_unready phoenix-engine 9200 30 || fail "Engine readiness did not fail closed during the PostgreSQL outage"
wait_unready shadow-dispatcher 9500 30 || fail "Dispatcher readiness did not fail closed during the PostgreSQL outage"
compose up -d postgres || fail "PostgreSQL could not be restarted"
wait_postgres || fail "PostgreSQL did not recover"
wait_runtime || fail "runtime did not recover after the PostgreSQL outage"
wait_for_classification_growth "$postgres_recovery_baseline" || fail "Engine processing did not resume after PostgreSQL recovery"

nats_recovery_baseline=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
compose stop nats || fail "NATS could not be stopped for recovery verification"
wait_unready phoenix-engine 9200 30 || fail "Engine readiness did not fail closed during the NATS outage"
wait_unready shadow-dispatcher 9500 30 || fail "Dispatcher readiness did not fail closed during the NATS outage"
compose up -d nats || fail "NATS could not be restarted"
wait_runtime || fail "runtime did not recover after the NATS outage"
wait_for_classification_growth "$nats_recovery_baseline" || fail "Engine processing did not resume after NATS recovery"
assert_bounded_pending

compose stop feed-ingestor || fail "feed-ingestor could not be stopped for backlog drain"
drain_deadline=$(( $(date +%s) + recovery_timeout ))
stable_drained=0
while [ "$(date +%s)" -lt "$drain_deadline" ]; do
  outbox_pending=$(sql_count 'SELECT count(*) FROM engine_outbox WHERE published_at IS NULL')
  recorder_pending=$(service_metric_count recorder 9400 recorder_consumer_pending_messages)
  recorder_ack_pending=$(service_metric_count recorder 9400 recorder_consumer_ack_pending)
  engine_pending=$(engine_js_value pending)
  engine_ack_pending=$(engine_js_value ack_pending)
  if [ "$recorder_pending" -eq 0 ] &&
    [ "$recorder_ack_pending" -eq 0 ] &&
    [ "$outbox_pending" -eq 0 ] &&
    [ "$engine_pending" -eq 0 ] &&
    [ "$engine_ack_pending" -eq 0 ]; then
    stable_drained=$((stable_drained + 1))
    if [ "$stable_drained" -ge 3 ]; then
      break
    fi
  else
    stable_drained=0
  fi
  sleep 3
done
[ "$stable_drained" -ge 3 ] || fail "outbox and Engine durable backlog did not drain"
compose stop recorder || fail "Recorder could not be stopped after the controlled drain"

duplicate_outbox=$(sql_count 'SELECT count(*) FROM (SELECT source_event_identity FROM engine_outbox GROUP BY source_event_identity HAVING count(*) > 1) duplicate_rows')
duplicate_classifications=$(sql_count 'SELECT count(*) FROM (SELECT source_event_identity FROM shadow_engine_classifications GROUP BY source_event_identity HAVING count(*) > 1) duplicate_rows')
duplicate_decisions=$(sql_count 'SELECT count(*) FROM (SELECT source_event_identity, strategy_version, route_fingerprint FROM shadow_decisions WHERE source_event_identity IS NOT NULL GROUP BY source_event_identity, strategy_version, route_fingerprint HAVING count(*) > 1) duplicate_rows')
[ "$duplicate_outbox" -eq 0 ] || fail "outbox idempotency was violated"
[ "$duplicate_classifications" -eq 0 ] || fail "Engine classification idempotency was violated"
[ "$duplicate_decisions" -eq 0 ] || fail "SHADOW decision idempotency was violated"

execution_attempts_after=$(sql_count 'SELECT count(*) FROM execution_attempts')
executions_after=$(sql_count 'SELECT count(*) FROM executions')
realized_after=$(sql_count 'SELECT count(*) FROM realized_pnl')
execution_eligible=$(sql_count "SELECT count(*) FROM shadow_decisions WHERE created_at >= '$smoke_started'::timestamptz AND execution_eligible")
[ "$execution_attempts_after" -eq "$execution_attempts_before" ] || fail "execution attempts changed during SHADOW smoke"
[ "$executions_after" -eq "$executions_before" ] || fail "executions changed during SHADOW smoke"
[ "$realized_after" -eq "$realized_before" ] || fail "realized PnL rows changed during SHADOW smoke"
[ "$execution_eligible" -eq 0 ] || fail "a SHADOW decision became execution eligible"

compose up -d recorder feed-ingestor || fail "Recorder or feed-ingestor could not be restored"
wait_runtime || fail "runtime did not return to ready after controlled drain"

echo "SHADOW_ENGINE_LIVE_SMOKE_PASS: atomic outbox, Dispatcher ACKs, durable Engine replay, outage recovery, idempotency, and fail-closed SHADOW evidence observed"
echo "pipeline_counts=feed:$feed_before->$feed_now,outbox:$outbox_before->$outbox_now,published:$published_before->$published_now,classifications:$classifications_before->$classifications_now"
echo "restart_pending=$pending_before_restart->$pending_during_restart rpc_failure_attempts=$rpc_failure_before->$rpc_failure_after"
