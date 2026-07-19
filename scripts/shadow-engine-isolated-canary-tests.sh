#!/usr/bin/env sh
# Sourced canary functions consume these globals and test doubles indirectly.
# shellcheck disable=SC2016,SC2034,SC2317
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi

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
route_config_payload=
env_file=$(mktemp)
release_env=$(mktemp)
trap 'rm -f "$test_log" "$preflight_output" "$env_file" "$release_env"; rm -rf "$test_state"' EXIT HUP INT TERM

compose() {
  if [ "${1:-}" = 'config' ]; then
    printf '%s\n' "$route_config_payload"
    return 0
  fi
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

ENGINE_ROUTE_REGISTRY_JSON='[{"route_id":"expected","legs":[{"pool_id":"token-a:token-b:500"}]}]'
printf 'ENGINE_ROUTE_REGISTRY_JSON=%s\n' "$ENGINE_ROUTE_REGISTRY_JSON" >"$env_file"
route_config_payload='{"services":{"phoenix-engine":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{\"route_id\":\"expected\",\"legs\":[{\"pool_id\":\"token-a:token-b:500\"}]}]"}},"recorder":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{\"route_id\":\"expected\",\"legs\":[{\"pool_id\":\"token-a:token-b:500\"}]}]"}}}}'
isolated_canary_state_dir=$(mktemp -d)
isolated_canary_route_registry_preflight || fail 'valid rendered route registry failed preflight'

route_config_payload='{"services":{"phoenix-engine":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{route_id:expected}]"}},"recorder":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{route_id:expected}]"}}}}'
if isolated_canary_route_registry_preflight; then
  fail 'malformed rendered route registry passed preflight'
fi

route_config_payload='{"services":{"phoenix-engine":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{\"route_id\":\"different\"}]"}},"recorder":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{\"route_id\":\"different\"}]"}}}}'
if isolated_canary_route_registry_preflight; then
  fail 'structurally different rendered route registry passed preflight'
fi

isolated_canary_container_is_healthy() {
  return 0
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
rm -f "$test_log"
: >"$test_log"
if (isolated_canary_dependency_preflight) >"$preflight_output" 2>&1; then
  fail 'different route registry unexpectedly passed startup preflight'
fi
grep -F 'route registry rendering invalid' "$preflight_output" >/dev/null ||
  fail 'different route registry did not produce a bounded preflight error'
if grep -E '^up([[:space:]]|$)' "$test_log" >/dev/null; then
  fail 'route registry preflight failure started Engine or RPC Gateway'
fi

route_config_payload='{"services":{"phoenix-engine":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{\"route_id\":\"expected\",\"legs\":[{\"pool_id\":\"token-a:token-b:500\"}]}]"}},"recorder":{"environment":{"ENGINE_ROUTE_REGISTRY_JSON":"[{\"route_id\":\"expected\",\"legs\":[{\"pool_id\":\"token-a:token-b:500\"}]}]"}}}}'
isolated_canary_state_dir=$(mktemp -d)
isolated_canary_container_is_healthy() {
  [ "$1" != 'feed-ingestor' ]
}
rm -f "$preflight_output"
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

service_metric_count() {
  printf '0\n'
}

engine_test_logs=
engine_test_state='exited|missing|0'
compose() {
  case "${1:-}" in
    ps) printf 'container-%s\n' "$4" ;;
    logs) printf '%s\n' "$engine_test_logs" ;;
    *) printf '%s\n' "$*" >>"$test_log" ;;
  esac
}
docker() {
  [ "${1:-}" = inspect ] || return 1
  printf '%s\n' "$engine_test_state"
}

isolated_canary_state_dir=$(mktemp -d)
: >"$isolated_canary_state_dir/engine-started"
if isolated_canary_watch_target; then
  fail 'exited Engine did not stop the watcher'
fi
[ "$(cat "$isolated_canary_state_dir/watcher-result")" = engine-exited ] ||
  fail 'exited Engine produced the wrong bounded failure'

isolated_canary_state_dir=$(mktemp -d)
: >"$isolated_canary_state_dir/engine-started"
engine_test_state='running|healthy|1'
if isolated_canary_watch_target; then
  fail 'Engine restart loop did not stop the watcher'
fi
[ "$(cat "$isolated_canary_state_dir/watcher-result")" = restart-loop ] ||
  fail 'Engine restart loop produced the wrong bounded failure'

isolated_canary_state_dir=$(mktemp -d)
: >"$isolated_canary_state_dir/engine-started"
engine_test_state='exited|missing|0'
engine_test_logs='invalid Engine route registry'
if isolated_canary_watch_target; then
  fail 'invalid route registry did not stop the watcher'
fi
[ "$(cat "$isolated_canary_state_dir/watcher-result")" = invalid-route-registry ] ||
  fail 'invalid route registry produced the wrong bounded failure'

if canary_is_enabled 0; then
  fail 'zero canary limit unexpectedly enabled'
fi
[ "${PHOENIX_MODE:-SHADOW}" = SHADOW ] || fail 'SHADOW default changed'
grep -F '[ "${LIVE_EXECUTION:-}" = "false" ]' "$repo_dir/scripts/shadow-engine-live-smoke.sh" >/dev/null ||
  fail 'LIVE safety check changed'

echo 'shadow-engine-isolated-canary-tests: ok'
