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
  echo "NITRO_LIVE_SMOKE_BLOCKED: docker is unavailable" >&2
  exit 1
fi
if [ ! -f "$compose_file" ]; then
  echo "NITRO_LIVE_SMOKE_BLOCKED: compose file is missing" >&2
  exit 1
fi
if [ ! -f "$env_file" ]; then
  echo "NITRO_LIVE_SMOKE_BLOCKED: production environment file is missing" >&2
  exit 1
fi
if [ ! -f "$release_env" ]; then
  echo "NITRO_LIVE_SMOKE_BLOCKED: deploy/current-release.env is missing" >&2
  exit 1
fi

export PHOENIX_ENV_FILE="$env_file"
export PHOENIX_RELEASE_ENV="$release_env"

compose() {
  docker compose --env-file "$env_file" --env-file "$release_env" -f "$compose_file" "$@"
}

metric_value() {
  metrics_payload="$1"
  metric_name="$2"
  value=$(printf '%s\n' "$metrics_payload" | awk -v name="$metric_name" '$1 == name { print $2; exit }')
  printf '%s\n' "${value:-0}"
}

nats_in_msgs_value() {
  varz_payload="$1"
  value=$(printf '%s\n' "$varz_payload" | awk -F: '/"in_msgs"/ { gsub(/[^0-9]/, "", $2); print $2; exit }')
  printf '%s\n' "${value:-0}"
}

relay_health_value() {
  container_id=$(compose ps -q nitro-feed-relay 2>/dev/null || true)
  if [ -z "$container_id" ]; then
    echo "missing"
    return
  fi
  docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$container_id" 2>/dev/null || echo "unknown"
}

compose config >/dev/null
compose up -d nats nitro-feed-relay
compose up -d feed-ingestor

deadline=$(( $(date +%s) + 90 ))
relay_health=unknown
connections=0
messages=0
normalized=0
publish_success=0
nats_in_msgs=0
readiness=0

while [ "$(date +%s)" -lt "$deadline" ]; do
  relay_health=$(relay_health_value)
  metrics_payload=$(compose exec -T feed-ingestor wget -q -O - http://127.0.0.1:9100/metrics 2>/dev/null || true)
  varz_payload=$(compose exec -T nats wget -q -O - http://127.0.0.1:8222/varz 2>/dev/null || true)
  connections=$(metric_value "$metrics_payload" feed_connections_total)
  messages=$(metric_value "$metrics_payload" feed_messages_total)
  normalized=$(metric_value "$metrics_payload" feed_normalized_transactions_total)
  publish_success=$(metric_value "$metrics_payload" feed_publish_success_total)
  readiness=$(metric_value "$metrics_payload" feed_readiness)
  nats_in_msgs=$(nats_in_msgs_value "$varz_payload")

  case "$connections:$messages:$normalized:$publish_success:$nats_in_msgs:$readiness" in
    *[!0-9:]*)
      connections=0
      messages=0
      normalized=0
      publish_success=0
      nats_in_msgs=0
      readiness=0
      ;;
  esac

  if [ "$relay_health" = "healthy" ] &&
    [ "$connections" -gt 0 ] &&
    [ "$messages" -gt 0 ] &&
    [ "$normalized" -gt 0 ] &&
    [ "$publish_success" -gt 0 ] &&
    [ "$nats_in_msgs" -gt 0 ] &&
    [ "$readiness" -eq 1 ]; then
    echo "NITRO_LIVE_SMOKE_PASS: relay, decoder, NATS publication, and readiness evidence observed"
    echo "relay_health=$relay_health feed_connections_total=$connections feed_messages_total=$messages feed_normalized_transactions_total=$normalized feed_publish_success_total=$publish_success nats_in_msgs=$nats_in_msgs feed_readiness=$readiness"
    exit 0
  fi

  sleep 3
done

echo "NITRO_LIVE_SMOKE_FAIL: required live production evidence was not observed within 90 seconds" >&2
echo "relay_health=$relay_health feed_connections_total=$connections feed_messages_total=$messages feed_normalized_transactions_total=$normalized feed_publish_success_total=$publish_success nats_in_msgs=$nats_in_msgs feed_readiness=$readiness" >&2
echo "NITRO_LIVE_SMOKE_DIAGNOSTICS: bounded service status and last 40 relay/ingestor log lines follow" >&2
compose ps nats nitro-feed-relay feed-ingestor >&2 || true
compose logs --no-color --tail 40 nitro-feed-relay feed-ingestor >&2 || true
exit 1
