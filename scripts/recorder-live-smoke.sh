#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=${PHOENIX_COMPOSE_FILE:-$repo_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$repo_dir/deploy/current-release.env}
stream_name=PHOENIX_FEED_TX
consumer_name=PHOENIX_RECORDER
max_pending=${RECORDER_SMOKE_MAX_PENDING:-100000}
max_ack_pending=1024
observation_seconds=${RECORDER_SMOKE_OBSERVATION_SECONDS:-60}

if [ ! -f "$release_env" ] && [ -f "$repo_dir/current-release.env" ]; then
  release_env="$repo_dir/current-release.env"
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "RECORDER_LIVE_SMOKE_BLOCKED: docker is unavailable" >&2
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "RECORDER_LIVE_SMOKE_BLOCKED: python3 is unavailable" >&2
  exit 1
fi
if [ ! -f "$env_file" ]; then
  echo "RECORDER_LIVE_SMOKE_BLOCKED: production environment file is missing" >&2
  exit 1
fi
if [ ! -f "$release_env" ]; then
  echo "RECORDER_LIVE_SMOKE_BLOCKED: deploy/current-release.env is missing" >&2
  exit 1
fi

set -a
# shellcheck disable=SC1090
. "$env_file"
# shellcheck disable=SC1090
. "$release_env"
set +a

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

service_metric() {
  service=$1
  port=$2
  metric=$3
  payload=$(compose exec -T "$service" wget -q -O - "http://127.0.0.1:$port/metrics" 2>/dev/null || true)
  value=$(printf '%s\n' "$payload" | awk -v metric="$metric" '$1 == metric { print $2; exit }')
  number_or_zero "$value"
}

recorder_metric() {
  service_metric recorder 9400 "$1"
}

feed_metric() {
  service_metric feed-ingestor 9100 "$1"
}

table_count() {
  table=$1
  value=$(compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "SELECT count(*) FROM public.$table" 2>/dev/null || true)
  number_or_zero "$value"
}

duplicate_group_count() {
  table=$1
  columns=$2
  value=$(compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "SELECT count(*) FROM (SELECT $columns FROM public.$table GROUP BY $columns HAVING count(*) > 1) duplicates" 2>/dev/null || true)
  number_or_zero "$value"
}

jetstream_payload() {
  compose exec -T nats wget -q -O - 'http://127.0.0.1:8222/jsz?consumers=true&config=true' 2>/dev/null || true
}

jetstream_value() {
  field=$1
  payload=$(jetstream_payload)
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
' "$stream_name" "$consumer_name" "$field"
}

recorder_ready() {
  compose exec -T recorder wget -q -O - http://127.0.0.1:9400/readyz >/dev/null 2>&1
}

feed_ready() {
  compose exec -T feed-ingestor wget -q -O - http://127.0.0.1:9100/readyz >/dev/null 2>&1
}

diagnostics() {
  echo "RECORDER_LIVE_SMOKE_DIAGNOSTICS: bounded status and last 40 service log lines follow" >&2
  compose ps postgres nats recorder nitro-feed-relay feed-ingestor >&2 || true
  echo "jetstream_stream_exists=$(jetstream_value stream_exists) jetstream_consumer_exists=$(jetstream_value consumer_exists) pending=$(jetstream_value pending) ack_pending=$(jetstream_value ack_pending)" >&2
  echo "feed_publish_acks=$(feed_metric feed_jetstream_publish_success_total) recorder_persisted=$(recorder_metric recorder_messages_persisted_total) recorder_ack_failures=$(recorder_metric recorder_jetstream_ack_failures_total)" >&2
  compose logs --no-color --tail 40 nats recorder feed-ingestor >&2 || true
}

fail() {
  echo "RECORDER_LIVE_SMOKE_FAIL: $1" >&2
  compose up -d postgres nats nitro-feed-relay recorder feed-ingestor >/dev/null 2>&1 || true
  diagnostics
  exit 1
}

wait_for_recorder() {
  deadline=$(( $(date +%s) + 90 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if recorder_ready &&
      [ "$(jetstream_value stream_exists)" -eq 1 ] &&
      [ "$(jetstream_value consumer_exists)" -eq 1 ]; then
      return 0
    fi
    sleep 3
  done
  return 1
}

wait_for_feed() {
  deadline=$(( $(date +%s) + 90 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if feed_ready && [ "$(feed_metric feed_jetstream_publish_success_total)" -gt 0 ]; then
      return 0
    fi
    sleep 3
  done
  return 1
}

assert_no_loss_events() {
  logs=$(compose logs --no-color --since "$smoke_started" --tail 1000 nats recorder feed-ingestor 2>/dev/null || true)
  if printf '%s\n' "$logs" | grep -Eiq 'slow consumer|core_nats_message_drop|Core NATS delivery loss|recorder_nats_slow_consumer'; then
    fail "a Core NATS slow-consumer or message-loss event was observed"
  fi
}

case "$observation_seconds:$max_pending" in
  *[!0-9:]*) echo "RECORDER_LIVE_SMOKE_BLOCKED: smoke thresholds must be integers" >&2; exit 1 ;;
esac
if [ "$observation_seconds" -lt 60 ]; then
  echo "RECORDER_LIVE_SMOKE_BLOCKED: observation must be at least 60 seconds" >&2
  exit 1
fi

smoke_started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
compose config >/dev/null || fail "production Compose configuration is invalid"
compose stop feed-ingestor recorder >/dev/null 2>&1 || true
compose up -d postgres nats nitro-feed-relay || fail "PostgreSQL, JetStream, or Nitro relay failed to start"
compose run --rm migration-runner || fail "database migrations failed"
compose up -d recorder || fail "Recorder failed to start"

wait_for_recorder || fail "Recorder, stream, or durable consumer did not become ready"

feed_events_before=$(table_count feed_events)
origins_before=$(table_count origin_transactions)
recorder_persisted_before=$(recorder_metric recorder_messages_persisted_total)
recorder_database_failures_before=$(recorder_metric recorder_database_failures_total)
recorder_decode_failures_before=$(recorder_metric recorder_decode_failures_total)
recorder_ack_failures_before=$(recorder_metric recorder_jetstream_ack_failures_total)

compose up -d feed-ingestor || fail "feed-ingestor failed to start"
wait_for_feed || fail "feed-ingestor did not produce a JetStream persistence acknowledgement"

[ "$(feed_metric feed_publish_failures_total)" -eq 0 ] || fail "feed publish failures appeared before observation"
[ "$(feed_metric feed_jetstream_publish_failures_total)" -eq 0 ] || fail "JetStream publish acknowledgement failures appeared before observation"
[ "$(feed_metric feed_jetstream_stream_unavailable_total)" -eq 0 ] || fail "JetStream stream availability failed before observation"
[ "$(feed_metric feed_decode_failures_total)" -eq 0 ] || fail "feed decode failures appeared before observation"

publish_acks_before=$(feed_metric feed_jetstream_publish_success_total)
last_publish_acks=$publish_acks_before
last_publish_progress=$(date +%s)
peak_pending=0
observation_deadline=$(( $(date +%s) + observation_seconds ))

while [ "$(date +%s)" -lt "$observation_deadline" ]; do
  recorder_ready || fail "Recorder readiness fell during the traffic observation"
  feed_ready || fail "feed-ingestor readiness fell during the traffic observation"

  pending=$(jetstream_value pending)
  ack_pending=$(jetstream_value ack_pending)
  [ "$pending" -le "$max_pending" ] || fail "JetStream consumer pending exceeded $max_pending"
  [ "$ack_pending" -le "$max_ack_pending" ] || fail "JetStream ack_pending exceeded $max_ack_pending"
  if [ "$pending" -gt "$peak_pending" ]; then
    peak_pending=$pending
  fi

  publish_acks=$(feed_metric feed_jetstream_publish_success_total)
  if [ "$publish_acks" -gt "$last_publish_acks" ]; then
    last_publish_acks=$publish_acks
    last_publish_progress=$(date +%s)
  elif [ $(( $(date +%s) - last_publish_progress )) -ge 30 ]; then
    fail "JetStream publish acknowledgements stopped for 30 seconds"
  fi

  [ "$(recorder_metric recorder_database_failures_total)" -eq "$recorder_database_failures_before" ] || fail "Recorder database failures increased"
  [ "$(recorder_metric recorder_decode_failures_total)" -eq "$recorder_decode_failures_before" ] || fail "Recorder decode failures increased"
  [ "$(recorder_metric recorder_jetstream_ack_failures_total)" -eq "$recorder_ack_failures_before" ] || fail "Recorder acknowledgement failures increased"
  [ "$(feed_metric feed_publish_failures_total)" -eq 0 ] || fail "feed publish failures increased"
  [ "$(feed_metric feed_jetstream_publish_failures_total)" -eq 0 ] || fail "JetStream publish acknowledgement failures increased"
  [ "$(feed_metric feed_jetstream_stream_unavailable_total)" -eq 0 ] || fail "JetStream stream became unavailable"
  [ "$(feed_metric feed_decode_failures_total)" -eq 0 ] || fail "feed decode failures increased"
  assert_no_loss_events
  sleep 5
done

publish_acks_after=$(feed_metric feed_jetstream_publish_success_total)
recorder_persisted_after=$(recorder_metric recorder_messages_persisted_total)
feed_events_after=$(table_count feed_events)
origins_after=$(table_count origin_transactions)
pending_after_observation=$(jetstream_value pending)

[ "$publish_acks_after" -gt "$publish_acks_before" ] || fail "JetStream publish acknowledgements did not increase"
[ "$recorder_persisted_after" -gt "$recorder_persisted_before" ] || fail "Recorder persisted-message counter did not increase"
[ "$feed_events_after" -gt "$feed_events_before" ] || fail "feed_events did not increase"
[ "$origins_after" -gt "$origins_before" ] || fail "origin_transactions did not increase"
[ "$pending_after_observation" -le "$max_pending" ] || fail "consumer lag was not bounded after observation"

pending_before_restart=$(jetstream_value pending)
feed_events_before_restart=$(table_count feed_events)
publish_before_restart=$(feed_metric feed_jetstream_publish_success_total)
compose stop recorder || fail "Recorder could not be stopped for replay verification"
sleep 15
feed_ready || fail "feed-ingestor readiness fell while Recorder was stopped"
publish_during_restart=$(feed_metric feed_jetstream_publish_success_total)
pending_during_restart=$(jetstream_value pending)
[ "$publish_during_restart" -gt "$publish_before_restart" ] || fail "feed publication did not continue while Recorder was stopped"
[ "$pending_during_restart" -gt "$pending_before_restart" ] || fail "JetStream did not queue messages while Recorder was stopped"

compose up -d recorder || fail "Recorder could not be restarted for replay verification"
wait_for_recorder || fail "Recorder did not recover after restart"

replay_deadline=$(( $(date +%s) + 120 ))
replay_proven=0
while [ "$(date +%s)" -lt "$replay_deadline" ]; do
  recorder_ready || fail "Recorder readiness fell during restart replay"
  feed_ready || fail "feed-ingestor readiness fell during restart replay"
  pending=$(jetstream_value pending)
  ack_pending=$(jetstream_value ack_pending)
  feed_events_now=$(table_count feed_events)
  persisted_now=$(recorder_metric recorder_messages_persisted_total)
  [ "$pending" -le "$max_pending" ] || fail "consumer lag exceeded the bounded threshold during replay"
  [ "$ack_pending" -le "$max_ack_pending" ] || fail "ack_pending exceeded its bound during replay"
  if [ "$pending" -lt "$pending_during_restart" ] &&
    [ "$feed_events_now" -gt "$feed_events_before_restart" ] &&
    [ "$persisted_now" -gt 0 ]; then
    replay_proven=1
    break
  fi
  sleep 3
done
[ "$replay_proven" -eq 1 ] || fail "queued JetStream messages were not replayed and persisted after restart"

final_publish_acks=$(feed_metric feed_jetstream_publish_success_total)
compose stop feed-ingestor || fail "feed-ingestor could not be stopped for the loss-accounting drain"
drain_deadline=$(( $(date +%s) + 120 ))
drained=0
while [ "$(date +%s)" -lt "$drain_deadline" ]; do
  recorder_ready || fail "Recorder readiness fell while draining the durable consumer"
  if [ "$(jetstream_value pending)" -eq 0 ] && [ "$(jetstream_value ack_pending)" -eq 0 ]; then
    drained=1
    break
  fi
  sleep 3
done
[ "$drained" -eq 1 ] || fail "durable consumer did not drain after publication stopped"
feed_events_drained=$(table_count feed_events)
feed_event_growth=$(( feed_events_drained - feed_events_before ))
[ "$feed_event_growth" -ge "$final_publish_acks" ] || fail "acknowledged publications are not fully represented by persisted or replayed feed events"

[ "$(duplicate_group_count origin_transactions tx_hash)" -eq 0 ] || fail "origin transaction uniqueness was violated after replay"
[ "$(duplicate_group_count feed_events 'sequence_number, tx_hash')" -eq 0 ] || fail "feed event uniqueness was violated after replay"
[ "$(recorder_metric recorder_database_failures_total)" -eq 0 ] || fail "Recorder database failures appeared after restart"
[ "$(recorder_metric recorder_decode_failures_total)" -eq 0 ] || fail "Recorder decode failures appeared after restart"
[ "$(recorder_metric recorder_jetstream_ack_failures_total)" -eq 0 ] || fail "Recorder acknowledgement failures appeared after restart"
assert_no_loss_events

compose up -d feed-ingestor || fail "feed-ingestor could not be restarted after the controlled drain"
wait_for_feed || fail "feed-ingestor did not recover after the controlled drain"

echo "RECORDER_LIVE_SMOKE_PASS: durable JetStream publication, bounded batching, restart replay, and PostgreSQL uniqueness observed"
echo "publish_acks=$publish_acks_before->$final_publish_acks feed_events=$feed_events_before->$feed_events_drained origin_transactions=$origins_before->$origins_after peak_pending=$peak_pending restart_pending=$pending_before_restart->$pending_during_restart"
