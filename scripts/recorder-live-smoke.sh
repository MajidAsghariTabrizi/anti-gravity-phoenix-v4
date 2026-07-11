#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=${PHOENIX_COMPOSE_FILE:-$repo_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$repo_dir/deploy/current-release.env}

if [ ! -f "$release_env" ] && [ -f "$repo_dir/current-release.env" ]; then
  release_env="$repo_dir/current-release.env"
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "RECORDER_LIVE_SMOKE_BLOCKED: docker is unavailable" >&2
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
set +a

compose() {
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose --env-file "$env_file" --env-file "$release_env" -f "$compose_file" "$@"
}

nats_value() {
  payload=$1
  field=$2
  value=$(printf '%s\n' "$payload" | sed -n "s/.*\"$field\":[[:space:]]*\([0-9][0-9]*\).*/\1/p" | head -n 1)
  printf '%s\n' "${value:-0}"
}

table_count() {
  table=$1
  value=$(compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atc "SELECT count(*) FROM public.$table" 2>/dev/null | tr -d '[:space:]')
  case "$value" in
    ''|*[!0-9]*) echo 0 ;;
    *) echo "$value" ;;
  esac
}

recorder_subscription_active() {
  connz=$(compose exec -T nats wget -q -O - 'http://127.0.0.1:8222/connz?subs=1' 2>/dev/null || true)
  printf '%s\n' "$connz" | grep -Eq '"name"[[:space:]]*:[[:space:]]*"phoenix-recorder"' &&
    printf '%s\n' "$connz" | grep -q 'phoenix.feed.tx'
}

diagnostics() {
  echo "RECORDER_LIVE_SMOKE_DIAGNOSTICS: bounded status and last 40 service log lines follow" >&2
  compose ps postgres nats recorder nitro-feed-relay feed-ingestor >&2 || true
  compose logs --no-color --tail 40 recorder feed-ingestor >&2 || true
}

compose config >/dev/null
compose up -d postgres nats nitro-feed-relay
compose run --rm migration-runner
compose stop feed-ingestor >/dev/null 2>&1 || true
compose up -d recorder

ready_deadline=$(( $(date +%s) + 90 ))
while [ "$(date +%s)" -lt "$ready_deadline" ]; do
  if compose exec -T recorder wget -q -O - http://127.0.0.1:9400/readyz >/dev/null 2>&1 &&
    recorder_subscription_active; then
    break
  fi
  sleep 3
done

if ! compose exec -T recorder wget -q -O - http://127.0.0.1:9400/readyz >/dev/null 2>&1; then
  echo "RECORDER_LIVE_SMOKE_FAIL: Recorder did not become ready before feed-ingestor startup" >&2
  diagnostics
  exit 1
fi
if ! recorder_subscription_active; then
  echo "RECORDER_LIVE_SMOKE_FAIL: phoenix-recorder subscription to phoenix.feed.tx was not observed" >&2
  diagnostics
  exit 1
fi

feed_events_before=$(table_count feed_events)
origins_before=$(table_count origin_transactions)
varz_before=$(compose exec -T nats wget -q -O - http://127.0.0.1:8222/varz 2>/dev/null || true)
out_msgs_before=$(nats_value "$varz_before" out_msgs)

compose up -d feed-ingestor

deadline=$(( $(date +%s) + 120 ))
feed_events_after=$feed_events_before
origins_after=$origins_before
out_msgs_after=$out_msgs_before
while [ "$(date +%s)" -lt "$deadline" ]; do
  feed_events_after=$(table_count feed_events)
  origins_after=$(table_count origin_transactions)
  varz=$(compose exec -T nats wget -q -O - http://127.0.0.1:8222/varz 2>/dev/null || true)
  out_msgs_after=$(nats_value "$varz" out_msgs)

  case "$feed_events_before:$origins_before:$out_msgs_before:$feed_events_after:$origins_after:$out_msgs_after" in
    *[!0-9:]*)
      feed_events_after=$feed_events_before
      origins_after=$origins_before
      out_msgs_after=$out_msgs_before
      ;;
  esac

  if [ "$out_msgs_after" -gt "$out_msgs_before" ] &&
    [ "$feed_events_after" -gt "$feed_events_before" ] &&
    [ "$origins_after" -gt "$origins_before" ]; then
    echo "RECORDER_LIVE_SMOKE_PASS: Core NATS delivery and PostgreSQL persistence observed"
    echo "feed_events=$feed_events_before->$feed_events_after origin_transactions=$origins_before->$origins_after nats_out_msgs=$out_msgs_before->$out_msgs_after"
    exit 0
  fi
  sleep 3
done

echo "RECORDER_LIVE_SMOKE_FAIL: required NATS and PostgreSQL evidence was not observed within 120 seconds" >&2
echo "feed_events=$feed_events_before->$feed_events_after origin_transactions=$origins_before->$origins_after nats_out_msgs=$out_msgs_before->$out_msgs_after" >&2
diagnostics
exit 1
