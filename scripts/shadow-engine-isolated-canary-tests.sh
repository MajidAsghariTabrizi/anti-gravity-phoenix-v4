#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)

# shellcheck disable=SC1091
. "$script_dir/shadow-engine-canary-control.sh"
# shellcheck disable=SC1091
. "$script_dir/shadow-engine-isolated-canary.sh"

fail() {
  echo "shadow-engine-isolated-canary-tests: $1" >&2
  exit 1
}

test_log=$(mktemp)
test_state=$(mktemp -d)
preflight_output=
trap 'rm -f "$test_log" "$preflight_output"; rm -rf "$test_state"' EXIT HUP INT TERM

compose() {
  if [ "${1:-}" = 'ps' ]; then
    printf 'container-%s\n' "$4"
    return 0
  fi
  printf '%s\n' "$*" >>"$test_log"
}

service_metric_count() {
  printf '500\n'
}

isolated_canary_state_dir=$test_state
isolated_canary_watcher_pid=
canary_input_limit=500
evidence_timeout=5
canary_poll_interval=0.01

isolated_canary_start_and_watch || fail 'isolated start/watch failed'
grep -F 'up -d --no-deps --force-recreate rpc-gateway phoenix-engine' "$test_log" >/dev/null ||
  fail 'isolated start did not use the exact explicit Compose command'
if grep -E '^(up|stop).*(nitro-feed-relay|nats|postgres|migration-runner|feed-ingestor|recorder|shadow-dispatcher|prometheus|dashboard)' "$test_log" >/dev/null; then
  fail 'isolated start/threshold cleanup touched a protected service'
fi

isolated_canary_cleanup_optional_runtime || fail 'isolated cleanup failed'
grep -F 'stop phoenix-engine rpc-gateway' "$test_log" >/dev/null ||
  fail 'cleanup did not stop exactly Engine and RPC Gateway'

if grep -E '^[[:space:]]*compose[[:space:]]+up([[:space:]]|$)' "$script_dir/shadow-engine-isolated-canary.sh" |
  grep -v -- '--no-deps --force-recreate rpc-gateway phoenix-engine' >/dev/null; then
  fail 'isolated canary contains an unscoped or non-isolated Compose up'
fi
if grep -E 'compose[[:space:]]+(pull|rm|down)|--remove-orphans|migration-runner|feed-ingestor|recorder|postgres|nats|prometheus|dashboard|shadow-dispatcher' \
  "$script_dir/shadow-engine-isolated-canary.sh" |
  grep -E 'compose[[:space:]]+(up|stop|pull|rm|down)|--remove-orphans' >/dev/null; then
  fail 'isolated canary contains a forbidden runtime mutation'
fi

isolated_canary_container_is_healthy() {
  [ "$1" != 'feed-ingestor' ]
}

isolated_canary_image_is_local_and_pinned() {
  return 0
}

service_ready() {
  return 0
}

postgres_ready() {
  return 0
}

engine_js_value() {
  case "$1" in
    stream_exists|consumer_exists) printf '1\n' ;;
    *) printf '0\n' ;;
  esac
}

preflight_output=$(mktemp)
if (isolated_canary_dependency_preflight) >"$preflight_output" 2>&1; then
  fail 'unhealthy dependency preflight unexpectedly passed'
fi
grep -F 'dependency not ready: feed-ingestor' "$preflight_output" >/dev/null ||
  fail 'unhealthy dependency did not identify only the failing service'
grep -F 'stop phoenix-engine rpc-gateway' "$test_log" >/dev/null ||
  fail 'preflight failure did not stop only the optional runtime'
if grep -E '^(up|stop).*(nitro-feed-relay|nats|postgres|migration-runner|feed-ingestor|recorder|shadow-dispatcher|prometheus|dashboard)' "$test_log" >/dev/null; then
  fail 'preflight failure touched a protected service'
fi

isolated_canary_state_dir=$(mktemp -d)
snapshot_generation=0
docker() {
  [ "${1:-}" = inspect ] || return 1
  snapshot_service=${4#container-}
  if [ "$snapshot_generation" -eq 0 ]; then
    printf 'id-%s|image-%s|digest-%s|created-%s|started-%s|0|true\n' \
      "$snapshot_service" "$snapshot_service" "$snapshot_service" "$snapshot_service" "$snapshot_service"
  else
    printf 'id-%s|changed-image-%s|digest-%s|created-%s|started-%s|1|true\n' \
      "$snapshot_service" "$snapshot_service" "$snapshot_service" "$snapshot_service" "$snapshot_service"
  fi
}

isolated_canary_record_snapshot || fail 'protected snapshot could not be recorded'
snapshot_generation=1
if isolated_canary_verify_snapshot; then
  fail 'protected image/restart-count changes were not detected'
fi
[ -n "$isolated_canary_changed_service" ] || fail 'snapshot change did not identify a service'

if canary_is_enabled 0; then
  fail 'zero canary limit unexpectedly enabled'
fi
[ "${PHOENIX_MODE:-SHADOW}" = SHADOW ] || fail 'SHADOW default changed'
grep -F '[ "${LIVE_EXECUTION:-}" = "false" ]' "$repo_dir/scripts/shadow-engine-live-smoke.sh" >/dev/null ||
  fail 'LIVE safety check changed'

echo 'shadow-engine-isolated-canary-tests: ok'
