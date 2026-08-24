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
command_timeout_seconds="${PHOENIX_HEALTH_COMMAND_TIMEOUT_SECONDS:-15}"
expected_mode="${PHOENIX_HEALTH_EXPECTED_MODE:-}"
allow_stopped_standby="${PHOENIX_HEALTH_ALLOW_STOPPED_STANDBY:-false}"
allow_legacy_atlas_binary="${PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY:-false}"
compose_runner=${PHOENIX_COMPOSE_RUNNER:-$deploy_dir/production_compose.py}
if [ ! -f "$compose_runner" ] && [ -f "$script_dir/production_compose.py" ]; then
  compose_runner=$script_dir/production_compose.py
fi
[ -f "$compose_runner" ] ||
  { echo "HEALTH_FAIL: canonical-compose-runner"; exit 1; }
release_components=${PHOENIX_RELEASE_COMPONENTS:-$script_dir/release_components.py}
if [ ! -f "$release_components" ] && [ -f "$deploy_dir/release_components.py" ]; then
  release_components=$deploy_dir/release_components.py
fi
[ -f "$release_components" ] ||
  { echo "HEALTH_FAIL: release-components"; exit 1; }

case "$expected_mode" in
  ""|LIVE|SHADOW|DISARMED_EVIDENCE) ;;
  *) echo "HEALTH_FAIL: invalid expected mode"; exit 1 ;;
esac
case "$allow_stopped_standby" in
  true|false) ;;
  *) echo "HEALTH_FAIL: invalid stopped-standby allowance"; exit 1 ;;
esac
case "$allow_legacy_atlas_binary" in
  true|false) ;;
  *) echo "HEALTH_FAIL: invalid legacy Atlas allowance"; exit 1 ;;
esac

[ -f "$release_env" ] ||
  { echo "HEALTH_FAIL: missing release env $release_env"; exit 1; }
case "$command_timeout_seconds" in
  ""|*[!0-9]*) echo "HEALTH_FAIL: invalid command timeout"; exit 1 ;;
esac
[ "$command_timeout_seconds" -ge 1 ] && [ "$command_timeout_seconds" -le 60 ] ||
  { echo "HEALTH_FAIL: invalid command timeout"; exit 1; }
[ -x /usr/bin/timeout ] ||
  { echo "HEALTH_FAIL: command timeout unavailable"; exit 1; }

set -a
# shellcheck disable=SC1090
. "$env_file"
# shellcheck disable=SC1090
. "$release_env"
set +a
[ -n "${PHOENIX_RELEASE_SHA:-}" ] ||
  { echo "HEALTH_FAIL: release-sha"; exit 1; }
release_manifest="${PHOENIX_RELEASE_MANIFEST:-$deploy_dir/manifests/$PHOENIX_RELEASE_SHA.json}"
[ -f "$release_manifest" ] ||
  { echo "HEALTH_FAIL: release-manifest"; exit 1; }

health_mode=${expected_mode:-${PHOENIX_MODE:-SHADOW}}
compose_mode=SHADOW
if [ "$health_mode" = LIVE ] || [ "$health_mode" = DISARMED_EVIDENCE ]; then
  compose_mode=LIVE
fi

compose() {
  if [ "$compose_mode" = LIVE ]; then
    /usr/bin/timeout --signal=TERM --kill-after=2s \
      "${command_timeout_seconds}s" python3 "$compose_runner" \
      --mode LIVE \
      --env-file "$env_file" \
      --release-env "$release_env" \
      --compose-file "$compose_file" \
      --overlay-file "$overlay_file" \
      --project-directory "$project_directory" \
      -- "$@"
  else
    /usr/bin/timeout --signal=TERM --kill-after=2s \
      "${command_timeout_seconds}s" python3 "$compose_runner" \
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

health_services=$(python3 "$release_components" topology \
  --manifest "$release_manifest" --mode "$health_mode" \
  --field health_services) ||
  { echo "HEALTH_FAIL: release-topology"; exit 1; }
for service in $health_services; do
  case "$service" in
    postgres)
      check postgres compose exec -T postgres /bin/sh -c \
        'pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB"'
      ;;
    nats)
      check nats compose exec -T nats wget -q -O - http://127.0.0.1:8222/healthz
      ;;
    nitro-feed-relay)
      check nitro-feed-relay compose exec -T nitro-feed-relay /bin/sh -c \
        "grep -Eq ':25AA[[:space:]].*[[:space:]]0A[[:space:]]' /proc/net/tcp /proc/net/tcp6"
      ;;
    rpc-gateway)
      check rpc-gateway compose exec -T rpc-gateway wget -q -O - http://127.0.0.1:9300/readyz
      ;;
    feed-ingestor)
      check feed-ingestor compose exec -T feed-ingestor wget -q -O - http://127.0.0.1:9100/readyz
      ;;
    phoenix-engine)
      check phoenix-engine compose exec -T phoenix-engine wget -q -O - http://127.0.0.1:9200/readyz
      ;;
    shadow-dispatcher)
      check shadow-dispatcher compose exec -T shadow-dispatcher wget -q -O - http://127.0.0.1:9500/readyz
      ;;
    recorder)
      check recorder compose exec -T recorder wget -q -O - http://127.0.0.1:9400/readyz
      ;;
    atlas-observer)
      check atlas-observer compose exec -T atlas-observer /bin/sh -c \
        'actual=$(readlink /proc/1/exe) && { [ "$actual" = /usr/local/bin/atlas-aave-hunter ] || { [ "$1" = true ] && [ "$actual" = /usr/local/bin/atlas-observer ]; }; } && wget -q -O - http://127.0.0.1:9700/readyz' \
        atlas-health "$allow_legacy_atlas_binary"
      ;;
    prometheus)
      check prometheus compose exec -T prometheus wget -q -O - http://127.0.0.1:9090/-/ready
      ;;
    dashboard)
      check dashboard compose exec -T dashboard python -c \
        "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8501/_stcore/health', timeout=2)"
      ;;
    phoenix-telegram-ops)
      check phoenix-telegram-ops compose exec -T phoenix-telegram-ops python3 -c \
        "import urllib.request; urllib.request.urlopen('http://127.0.0.1:9750/readyz', timeout=2)"
      ;;
    *)
      echo "HEALTH_FAIL: unsupported-topology-service-$service"
      exit 1
      ;;
  esac
done
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
  if [ "$allow_stopped_standby" = true ]; then
    [ -z "$(compose ps -q live-executor | awk 'NF { print; exit }')" ] ||
      { echo "HEALTH_FAIL: live-executor-started-before-standby-phase"; exit 1; }
  else
    check live-executor-hunting-standby compose exec -T live-executor /bin/sh -c \
      '[ "$LIVE_EXECUTOR_HUNTING_STANDBY" = true ] && [ "$LIVE_EXECUTOR_ARMED" = false ] && [ "$LIVE_EXECUTOR_KILL_SWITCH" = true ] && kill -0 1'
  fi
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
