#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=${PHOENIX_COMPOSE_FILE:-$repo_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}

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

export PHOENIX_ENV_FILE="$env_file"

compose() {
  docker compose --env-file "$env_file" -f "$compose_file" "$@"
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

compose up -d nats nitro-feed-relay
compose up -d feed-ingestor

deadline=$(( $(date +%s) + 90 ))
connections=0
messages=0
nats_in_msgs=0

while [ "$(date +%s)" -lt "$deadline" ]; do
  metrics_payload=$(compose exec -T feed-ingestor wget -q -O - http://127.0.0.1:9100/metrics 2>/dev/null || true)
  varz_payload=$(compose exec -T nats wget -q -O - http://127.0.0.1:8222/varz 2>/dev/null || true)
  connections=$(metric_value "$metrics_payload" feed_connections_total)
  messages=$(metric_value "$metrics_payload" feed_messages_total)
  nats_in_msgs=$(nats_in_msgs_value "$varz_payload")

  case "$connections:$messages:$nats_in_msgs" in
    *[!0-9:]*)
      connections=0
      messages=0
      nats_in_msgs=0
      ;;
  esac

  if [ "$connections" -gt 0 ] && [ "$messages" -gt 0 ] && [ "$nats_in_msgs" -gt 0 ]; then
    echo "NITRO_LIVE_SMOKE_PASS: transport and NATS evidence observed; readiness was not evaluated"
    echo "feed_connections_total=$connections feed_messages_total=$messages nats_in_msgs=$nats_in_msgs"
    exit 0
  fi

  sleep 3
done

echo "NITRO_LIVE_SMOKE_FAIL: required live evidence was not observed within 90 seconds; readiness is not claimed" >&2
echo "feed_connections_total=$connections feed_messages_total=$messages nats_in_msgs=$nats_in_msgs" >&2
exit 1
