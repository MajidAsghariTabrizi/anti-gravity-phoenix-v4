#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
release_env="${PHOENIX_RELEASE_ENV:-$deploy_dir/current-release.env}"
compose_file="${PHOENIX_COMPOSE_FILE:-$deploy_dir/compose.prod.yml}"
overlay_file="${PHOENIX_COMPOSE_OVERLAY_FILE:-$deploy_dir/compose.live-autonomous.yml}"
project_directory="${PHOENIX_COMPOSE_PROJECT_DIRECTORY:-$deploy_dir}"
retries="${PHOENIX_HEALTH_RETRIES:-20}"
sleep_seconds="${PHOENIX_HEALTH_SLEEP_SECONDS:-3}"
expected_mode="${PHOENIX_HEALTH_EXPECTED_MODE:-}"
compose_runner=${PHOENIX_COMPOSE_RUNNER:-$deploy_dir/production_compose.py}
if [ ! -f "$compose_runner" ] && [ -f "$script_dir/production_compose.py" ]; then
  compose_runner=$script_dir/production_compose.py
fi
[ -f "$compose_runner" ] ||
  { echo "HEALTH_FAIL: canonical-compose-runner"; exit 1; }

case "$expected_mode" in
  ""|LIVE|SHADOW|DISARMED_EVIDENCE) ;;
  *) echo "HEALTH_FAIL: invalid expected mode"; exit 1 ;;
esac

[ -f "$release_env" ] ||
  { echo "HEALTH_FAIL: missing release env $release_env"; exit 1; }

set -a
# shellcheck disable=SC1090
. "$env_file"
# shellcheck disable=SC1090
. "$release_env"
set +a

health_mode=${expected_mode:-${PHOENIX_MODE:-SHADOW}}
compose_mode=SHADOW
if [ "$health_mode" = LIVE ] || [ "$health_mode" = DISARMED_EVIDENCE ]; then
  compose_mode=LIVE
fi

compose() {
  if [ "$compose_mode" = LIVE ]; then
    python3 "$compose_runner" \
      --mode LIVE \
      --env-file "$env_file" \
      --release-env "$release_env" \
      --compose-file "$compose_file" \
      --overlay-file "$overlay_file" \
      --project-directory "$project_directory" \
      -- "$@"
  else
    python3 "$compose_runner" \
      --mode SHADOW \
      --env-file "$env_file" \
      --release-env "$release_env" \
      --compose-file "$compose_file" \
      --project-directory "$project_directory" \
      -- "$@"
  fi
}

check() {
  name="$1"
  shift
  attempt=1
  while [ "$attempt" -le "$retries" ]; do
    if "$@" >/dev/null 2>&1; then
      echo "HEALTH_OK: $name"
      return 0
    fi
    attempt=$((attempt + 1))
    sleep "$sleep_seconds"
  done
  echo "HEALTH_FAIL: $name"
  return 1
}

check postgres compose exec -T postgres /bin/sh -c 'pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB"'
check nats compose exec -T nats wget -q -O - http://127.0.0.1:8222/healthz
check nitro-feed-relay compose exec -T nitro-feed-relay /bin/sh -c \
  "grep -Eq ':25AA[[:space:]].*[[:space:]]0A[[:space:]]' /proc/net/tcp /proc/net/tcp6"
check rpc-gateway compose exec -T rpc-gateway wget -q -O - http://127.0.0.1:9300/readyz
check feed-ingestor compose exec -T feed-ingestor wget -q -O - http://127.0.0.1:9100/readyz
check phoenix-engine compose exec -T phoenix-engine wget -q -O - http://127.0.0.1:9200/readyz
check recorder compose exec -T recorder wget -q -O - http://127.0.0.1:9400/readyz
check prometheus compose exec -T prometheus wget -q -O - http://127.0.0.1:9090/-/ready
check dashboard compose exec -T dashboard python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8501/_stcore/health', timeout=2)"
if [ "$health_mode" = LIVE ]; then
  if [ -z "$expected_mode" ]; then
    [ "${PHOENIX_MODE:-}" = LIVE ] &&
      [ "${LIVE_EXECUTION:-}" = true ] &&
      [ "${AUTONOMOUS_EXECUTION:-}" = true ] ||
      { echo "HEALTH_FAIL: autonomous-live-mode"; exit 1; }
  fi
  check live-executor compose exec -T live-executor /bin/sh -c 'kill -0 1'
  check autonomous-live-mode compose exec -T phoenix-engine /bin/sh -c \
    '[ "$PHOENIX_MODE" = LIVE ] && [ "$LIVE_EXECUTION" = true ] && [ "$AUTONOMOUS_EXECUTION" = true ]'
  check autonomous-controls compose exec -T live-executor \
    /usr/local/bin/autonomous-live-control status
  check event-metrics compose exec -T phoenix-engine wget -q -O - \
    http://127.0.0.1:9200/metrics
elif [ "$health_mode" = DISARMED_EVIDENCE ]; then
  [ -z "$(compose ps -q live-executor | awk 'NF { print; exit }')" ] ||
    { echo "HEALTH_FAIL: live-executor-running-while-disarmed"; exit 1; }
  check disarmed-evidence-mode compose exec -T phoenix-engine /bin/sh -c \
    '[ "$PHOENIX_MODE" = LIVE ] && [ "$LIVE_EXECUTION" = true ] && [ "$AUTONOMOUS_EXECUTION" = true ]'
  check disarmed-controls compose run --rm --no-deps autonomous-control status
  check event-metrics compose exec -T phoenix-engine wget -q -O - \
    http://127.0.0.1:9200/metrics
else
  check shadow-mode compose exec -T phoenix-engine /bin/sh -c \
    '[ "$PHOENIX_MODE" = SHADOW ] && [ "$LIVE_EXECUTION" = false ]'
fi

echo "PRODUCTION_HEALTH_OK"
