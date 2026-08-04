#!/usr/bin/env sh
set -eu

release_sha="${1:-}"
deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
release_root="${PHOENIX_RELEASE_ROOT:-$deploy_root/releases}"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
overlay_file="$deploy_dir/compose.live-autonomous.yml"
manifest="$deploy_dir/manifests/$release_sha.json"
release_env="$deploy_dir/manifests/$release_sha.env"
release_metadata="$deploy_dir/manifests/$release_sha.render.json"
release_state="$deploy_dir/manifests/$release_sha.state.json"
current_file="$deploy_dir/current-release"
current_env="$deploy_dir/current-release.env"
current_state="$deploy_dir/current-release.json"
current_context="$deploy_dir/current-release-context.json"
previous_file="$deploy_dir/previous-release"
runtime_dir="${PHOENIX_DEPLOY_RUNTIME_DIR:-$deploy_dir/.deploy-runtime}"
candidate_release_assets_file="$deploy_dir/candidate-release-assets.sha"
release_assets_file="$deploy_dir/release-assets.sha"
service_wait_seconds=${PHOENIX_DEPLOY_SERVICE_WAIT_SECONDS:-300}
recorder_drain_seconds=${PHOENIX_RECORDER_DRAIN_SECONDS:-180}
engine_burn_in_seconds=${PHOENIX_ENGINE_BURN_IN_SECONDS:-120}
release_state_updater=${PHOENIX_RELEASE_STATE_UPDATER:-}
aave_discovery_source=${PHOENIX_AAVE_DISCOVERY_SOURCE:-/var/lib/phoenix-atlas-hunter/evidence/aave-borrow-archive-7736400-489813224-0f204e03/aave-borrow-discovery.json}
aave_discovery_target=${PHOENIX_AAVE_DISCOVERY_TARGET:-/opt/phoenix/evidence/aave-discovery/aave-borrow-discovery.json}
aave_discovery_sha256=3d8513bb05607d9a91fece589872db3f5782953d13e24fc418ea2b4b0aa0239c

fail() {
  echo "DEPLOY_FAILED: $1"
  exit 1
}

state_update() {
  operation=$1
  value=$2
  [ -n "$release_state_updater" ] || return 0
  [ -f "$release_state_updater" ] && [ ! -L "$release_state_updater" ] ||
    fail "release state updater is missing or unsafe"
  /usr/bin/python3 -I -B "$release_state_updater" \
    "$release_sha" "$operation" "$value" >/dev/null ||
    fail "durable release state update failed"
}

mark_phase() {
  state_update phase "$1"
}

install_aave_discovery_seed() {
  [ -f "$aave_discovery_source" ] && [ ! -L "$aave_discovery_source" ] ||
    fail "canonical Aave discovery seed is missing or unsafe"
  source_identity=$(stat -c '%u:%g:%a' "$aave_discovery_source") ||
    fail "canonical Aave discovery seed identity could not be read"
  [ "$source_identity" = "0:0:600" ] ||
    fail "canonical Aave discovery seed identity is invalid"
  source_sha256=$(sha256sum "$aave_discovery_source" | awk '{print $1}') ||
    fail "canonical Aave discovery seed hash could not be read"
  [ "$source_sha256" = "$aave_discovery_sha256" ] ||
    fail "canonical Aave discovery seed hash is invalid"

  aave_discovery_parent=$(dirname "$aave_discovery_target")
  install -d -m 0755 -o root -g root "$aave_discovery_parent" ||
    fail "Aave discovery target directory could not be installed"
  if [ -d "$aave_discovery_target" ] && [ ! -L "$aave_discovery_target" ]; then
    rmdir "$aave_discovery_target" ||
      fail "unsafe nonempty Aave discovery target directory"
  elif [ -e "$aave_discovery_target" ] || [ -L "$aave_discovery_target" ]; then
    [ -f "$aave_discovery_target" ] && [ ! -L "$aave_discovery_target" ] ||
      fail "existing Aave discovery target is unsafe"
    target_identity=$(stat -c '%u:%g:%a' "$aave_discovery_target") ||
      fail "existing Aave discovery target identity could not be read"
    target_sha256=$(sha256sum "$aave_discovery_target" | awk '{print $1}') ||
      fail "existing Aave discovery target hash could not be read"
    if [ "$target_identity" = "0:0:444" ] &&
       [ "$target_sha256" = "$aave_discovery_sha256" ]; then
      return 0
    fi
  fi

  seed_candidate="$aave_discovery_parent/.aave-borrow-discovery.$$.candidate"
  rm -f "$seed_candidate"
  install -m 0444 -o root -g root "$aave_discovery_source" "$seed_candidate" || {
    rm -f "$seed_candidate"
    fail "canonical Aave discovery seed could not be staged"
  }
  candidate_sha256=$(sha256sum "$seed_candidate" | awk '{print $1}') || {
    rm -f "$seed_candidate"
    fail "staged Aave discovery seed hash could not be read"
  }
  [ "$candidate_sha256" = "$aave_discovery_sha256" ] || {
    rm -f "$seed_candidate"
    fail "staged Aave discovery seed hash is invalid"
  }
  mv -f "$seed_candidate" "$aave_discovery_target" || {
    rm -f "$seed_candidate"
    fail "canonical Aave discovery seed could not be activated"
  }
  [ "$(stat -c '%u:%g:%a' "$aave_discovery_target")" = "0:0:444" ] ||
    fail "installed Aave discovery seed identity is invalid"
  [ "$(sha256sum "$aave_discovery_target" | awk '{print $1}')" = "$aave_discovery_sha256" ] ||
    fail "installed Aave discovery seed hash is invalid"
}

verify_runtime_control_phase() {
  expected_phase=$1
  python3 -c '
import json
import sys

expected_phase, expected_release = sys.argv[1:]
status = json.load(sys.stdin)
global_control = status["global"]
route_control = status["route"]
revenue_lanes = status["revenue_lanes"]
economic = status["economic"]
valid = (
    global_control["armed"] is False
    and global_control["kill_switch"] is True
    and global_control["execution_mode"] == "disarmed"
    and route_control is not None
    and route_control["enabled"] is False
    and route_control["kill_switch"] is True
    and len(revenue_lanes) == 2
    and {lane["lane"] for lane in revenue_lanes} == {"atlas_solver", "aave_liquidation"}
    and all(lane["armed"] is False for lane in revenue_lanes)
    and all(lane["kill_switch"] is True for lane in revenue_lanes)
    and economic["phase"] == expected_phase
    and economic["release_sha"] == expected_release
)
raise SystemExit(0 if valid else 1)
' "$expected_phase" "$release_sha"
}

case "$release_sha" in
  *[!0-9a-f]*|"") fail "release SHA must be 40 lowercase hex characters" ;;
esac
[ "${#release_sha}" -eq 40 ] || fail "release SHA must be 40 lowercase hex characters"
[ -f "$manifest" ] || fail "missing release manifest"
[ -f "$compose_file" ] || fail "missing production compose file"
[ -f "$overlay_file" ] || fail "missing autonomous LIVE compose overlay"
[ -f "$env_file" ] || fail "missing production environment file"
[ -s "$candidate_release_assets_file" ] || fail "exact candidate release assets are not installed"
installed_assets_sha=$(tr -d '\r\n' <"$candidate_release_assets_file")
[ "$installed_assets_sha" = "$release_sha" ] || fail "installed release assets do not match release SHA"
[ -s "$current_file" ] || fail "active release pointer is missing"
rollback_sha=$(tr -d '\r\n' <"$current_file")
case "$rollback_sha" in
  *[!0-9a-f]*|"") fail "active release SHA is invalid" ;;
esac
[ "${#rollback_sha}" -eq 40 ] || fail "active release SHA is invalid"
[ -s "$release_assets_file" ] || fail "active release-assets pointer is missing"
[ "$(tr -d '\r\n' <"$release_assets_file")" = "$rollback_sha" ] ||
  fail "active release pointers are incoherent"
rollback_release_root="$release_root/$rollback_sha"
rollback_manifest="$deploy_dir/manifests/$rollback_sha.json"
rollback_script="$rollback_release_root/scripts/rollback-release.sh"
rollback_context_installer="${PHOENIX_CONTEXT_INSTALLER:-$rollback_release_root/scripts/install-production-release-context.sh}"
[ -f "$rollback_script" ] && [ ! -L "$rollback_script" ] ||
  fail "version-matched rollback script is missing or unsafe"
[ -f "$rollback_manifest" ] || fail "active rollback release manifest is missing"
[ -f "$rollback_context_installer" ] && [ ! -L "$rollback_context_installer" ] ||
  fail "version-matched rollback context installer is missing or unsafe"
case "$service_wait_seconds" in
  ''|*[!0-9]*) fail "service wait seconds must be an integer" ;;
esac
[ "$service_wait_seconds" -ge 30 ] && [ "$service_wait_seconds" -le 900 ] ||
  fail "service wait seconds must be from 30 through 900"
case "$recorder_drain_seconds" in
  ''|*[!0-9]*) fail "Recorder drain seconds must be an integer" ;;
esac
[ "$recorder_drain_seconds" -ge 30 ] && [ "$recorder_drain_seconds" -le 900 ] ||
  fail "Recorder drain seconds must be from 30 through 900"
case "$engine_burn_in_seconds" in
  ''|*[!0-9]*) fail "Engine burn-in seconds must be an integer" ;;
esac
[ "$engine_burn_in_seconds" -ge 120 ] && [ "$engine_burn_in_seconds" -le 900 ] ||
  fail "Engine burn-in seconds must be from 120 through 900"

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v cmp >/dev/null 2>&1 || fail "cmp is unavailable"
if [ -e "$runtime_dir" ]; then
  [ -d "$runtime_dir" ] && [ ! -L "$runtime_dir" ] ||
    fail "deployment runtime directory is unsafe"
else
  mkdir -p "$runtime_dir"
fi
chmod 0700 "$runtime_dir"
runtime_metadata=$(stat -c '%u:%g:%a' "$runtime_dir") ||
  fail "deployment runtime metadata is unavailable"
[ "$runtime_metadata" = 0:0:700 ] ||
  fail "deployment runtime directory must be root-only"
python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$manifest" \
  --expected-sha "$release_sha" \
  --route-registry "$deploy_dir/routes/weth_usdc_uniswap_v3.json" \
  --output "$release_env" || fail "release manifest validation failed"
chmod 0640 "$release_env"
protected_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode DISARMED_EVIDENCE \
  --field protected_services) || fail "target topology is invalid"
fixed_protected_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode DISARMED_EVIDENCE \
  --field fixed_protected_services) || fail "target topology is invalid"
start_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode DISARMED_EVIDENCE \
  --field start_services) || fail "target topology is invalid"
pull_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode DISARMED_EVIDENCE \
  --field pull_services) || fail "target topology is invalid"
remove_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --source-manifest "$rollback_manifest" \
  --mode DISARMED_EVIDENCE --field remove_services) ||
  fail "release transition topology is invalid"
absent_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode DISARMED_EVIDENCE \
  --field intentional_absence) || fail "target topology is invalid"

reload_environment() {
  unset PHOENIX_MODE LIVE_EXECUTION AUTONOMOUS_EXECUTION
  set -a
  # shellcheck disable=SC1090
  . "$env_file"
  set +a
}

assert_live_environment() {
  [ "${PHOENIX_MODE:-}" = LIVE ] ||
    fail "atomically reloaded PHOENIX_MODE is not LIVE"
  [ "${LIVE_EXECUTION:-}" = true ] ||
    fail "atomically reloaded LIVE_EXECUTION is not true"
  [ "${AUTONOMOUS_EXECUTION:-}" = true ] ||
    fail "atomically reloaded AUTONOMOUS_EXECUTION is not true"
}

"$deploy_dir/validate-production-env.sh" "$env_file"
reload_environment
if [ -z "${PRODUCTION_RPC_URL:-}" ] || [ -z "${SECONDARY_RPC_URL:-}" ] ||
  [ -z "${LIVE_EXECUTOR_RPC_ALLOWLIST:-}" ]
then
  echo EXTERNAL_RPC_CREDENTIAL_REQUIRED
  exit 1
fi

state_dir=$(mktemp -d "$runtime_dir/deploy-$release_sha.XXXXXX") ||
  fail "temporary release state could not be created"
cleanup_candidate() {
  rm -rf "$state_dir"
}
trap cleanup_candidate EXIT
trap 'exit 1' HUP INT TERM
rendered_candidate="$state_dir/compose.rendered.json"
metadata_candidate="$state_dir/render.metadata.json"
candidate_evidence_env="$state_dir/candidate-evidence.env"
candidate_live_env="$state_dir/candidate-live.env"
state_candidate="$state_dir/release-state.json"
pointer_candidate="$state_dir/current-release"
assets_pointer_candidate="$state_dir/release-assets.sha"
context_candidate="$state_dir/release-context.json"
context_rendered="$state_dir/context.compose.json"
context_metadata="$state_dir/context.metadata.json"
protected_before="$state_dir/protected.before.tsv"
protected_after="$state_dir/protected.after.tsv"
fixed_before="$state_dir/fixed-protected.before.tsv"
fixed_after="$state_dir/fixed-protected.after.tsv"
recorder_drain_snapshot="$state_dir/recorder-drain.json"
protected_transition_started=0

verify_active_release_coherence() {
  expected_sha=$1
  expected_previous_sha=$2
  [ "$(tr -d '\r\n' <"$current_file")" = "$expected_sha" ] || return 1
  [ "$(tr -d '\r\n' <"$release_assets_file")" = "$expected_sha" ] || return 1
  if [ -n "$expected_previous_sha" ]; then
    [ "$(tr -d '\r\n' <"$previous_file")" = "$expected_previous_sha" ] ||
      return 1
  fi
  grep -F -x "PHOENIX_RELEASE_SHA=$expected_sha" "$current_env" >/dev/null ||
    return 1
  python3 -I -B - "$expected_sha" "$current_state" "$current_context" <<'PY'
import json
import sys
from pathlib import Path

expected = sys.argv[1]
for name in sys.argv[2:]:
    try:
        value = json.loads(Path(name).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise SystemExit(1)
    if not isinstance(value, dict) or value.get("release_sha") != expected:
        raise SystemExit(1)
PY
}

compose() {
  python3 "$deploy_dir/production_compose.py" \
    --mode LIVE \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    -- "$@"
}

rollback_release_env="$deploy_dir/manifests/$rollback_sha.env"
[ -s "$rollback_release_env" ] ||
  fail "active rollback release environment is missing"

compose_with_release_env() {
  selected_release_env=$1
  shift
  python3 "$deploy_dir/production_compose.py" \
    --mode LIVE \
    --env-file "$env_file" \
    --release-env "$selected_release_env" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    -- "$@"
}

capture_service_ids() {
  capture_release_env=$1
  capture_services=$2
  output=$3
  : >"$output"
  for service in $capture_services; do
    id=$(compose_with_release_env "$capture_release_env" ps -a -q "$service" |
      awk 'NF { print; exit }')
    if [ -z "$id" ]; then
      echo "PROTECTED_SERVICE_UNAVAILABLE: service=$service state=missing"
      return 1
    fi
    state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id") ||
      state=inspect_failed
    if [ "$state" != 'running|healthy' ]; then
      echo "PROTECTED_SERVICE_UNAVAILABLE: service=$service state=$state"
      return 1
    fi
    printf '%s\t%s\n' "$service" "$id" >>"$output"
  done
}

capture_protected_ids() {
  capture_service_ids "$release_env" "$protected_services" "$1"
}

wait_service_healthy_with_env() {
  healthy_release_env=$1
  service=$2
  deadline=$(( $(date +%s) + service_wait_seconds ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    id=$(compose_with_release_env "$healthy_release_env" ps -a -q "$service" |
      awk 'NF { print; exit }')
    if [ -n "$id" ]; then
      state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id" 2>/dev/null || true)
      [ "$state" = 'running|healthy' ] && return 0
    fi
    sleep 3
  done
  return 1
}

wait_service_healthy() {
  wait_service_healthy_with_env "$release_env" "$1"
}

capture_service_failure_evidence() {
  failure_service=$1
  failure_id=$(compose ps -a -q "$failure_service" |
    awk 'NF { print; exit }')
  if [ -z "$failure_id" ]; then
    echo "SERVICE_STARTUP_EVIDENCE: service=$failure_service state=missing"
    return 0
  fi
  failure_state=$(docker inspect --format \
    '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.RestartCount}}|{{.State.OOMKilled}}|{{.State.ExitCode}}|{{.State.Error}}' \
    "$failure_id" 2>/dev/null || true)
  echo "SERVICE_STARTUP_EVIDENCE: service=$failure_service state=$failure_state"
  echo "SERVICE_STARTUP_LOG_TAIL_BEGIN: service=$failure_service lines=80"
  docker logs --timestamps --tail 80 "$failure_id" 2>&1 || true
  echo "SERVICE_STARTUP_LOG_TAIL_END: service=$failure_service"
}

remove_source_only_services() {
  for service in $remove_services; do
    compose_with_release_env "$rollback_release_env" stop -t 30 "$service" \
      >/dev/null 2>&1 || true
    compose_with_release_env "$rollback_release_env" rm -f "$service" \
      >/dev/null || return 1
  done
}

assert_intentional_absence() {
  for service in $absent_services; do
    [ -z "$(compose ps -a -q "$service" | awk 'NF { print; exit }')" ] ||
      return 1
  done
}

release_env_value() {
  value_file=$1
  value_name=$2
  awk -F= -v expected="$value_name" '
    $1 == expected {
      print substr($0, length($1) + 2)
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$value_file"
}

wait_recorder_drain() {
  drain_release_env=$1
  drain_helper="$deploy_dir/prelive_protected_maintenance.py"
  [ -f "$drain_helper" ] && [ ! -L "$drain_helper" ] ||
    return 1
  drain_deadline=$(( $(date +%s) + recorder_drain_seconds ))
  while [ "$(date +%s)" -lt "$drain_deadline" ]; do
    if compose_with_release_env "$drain_release_env" exec -T nats \
      wget -q -O - \
        'http://127.0.0.1:8222/jsz?streams=true&consumers=true&config=true' \
        >"$recorder_drain_snapshot"
    then
      drain_state=$(
        python3 -B "$drain_helper" consumer-state \
          --jetstream "$recorder_drain_snapshot"
      ) || drain_state=
      drain_pending=$(printf '%s\n' "$drain_state" | awk '{ print $1 }')
      drain_ack_pending=$(printf '%s\n' "$drain_state" | awk '{ print $2 }')
      case "$drain_pending:$drain_ack_pending" in
        *[!0-9:]*|:*|*:|*::*) ;;
        *)
          if [ "$drain_pending" -eq 0 ] && [ "$drain_ack_pending" -eq 0 ]; then
            return 0
          fi
          ;;
      esac
    fi
    sleep 3
  done
  return 1
}

transition_mutable_protected() {
  source_release_env=$1
  target_release_env=$2
  source_feed=$(release_env_value "$source_release_env" FEED_INGESTOR_IMAGE) ||
    return 1
  target_feed=$(release_env_value "$target_release_env" FEED_INGESTOR_IMAGE) ||
    return 1
  source_recorder=$(release_env_value "$source_release_env" RECORDER_IMAGE) ||
    return 1
  target_recorder=$(release_env_value "$target_release_env" RECORDER_IMAGE) ||
    return 1
  if [ "$source_feed" = "$target_feed" ] &&
    [ "$source_recorder" = "$target_recorder" ]
  then
    return 0
  fi

  protected_transition_started=1
  transition_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  compose_with_release_env "$source_release_env" stop -t 30 feed-ingestor \
    >/dev/null || return 1
  wait_recorder_drain "$source_release_env" || return 1
  if [ "$source_recorder" != "$target_recorder" ]; then
    compose_with_release_env "$target_release_env" up -d --no-deps \
      --force-recreate recorder >/dev/null || return 1
    wait_service_healthy_with_env "$target_release_env" recorder || return 1
  fi
  if [ "$source_feed" = "$target_feed" ]; then
    compose_with_release_env "$target_release_env" up -d --no-deps \
      feed-ingestor >/dev/null || return 1
  else
    compose_with_release_env "$target_release_env" up -d --no-deps \
      --force-recreate feed-ingestor >/dev/null || return 1
  fi
  wait_service_healthy_with_env "$target_release_env" feed-ingestor || return 1
  transition_log="$state_dir/protected-transition.log"
  compose_with_release_env "$target_release_env" logs --no-color \
    --since "$transition_started_at" nats recorder feed-ingestor \
    >"$transition_log" 2>&1 || return 1
  if grep -Eiq \
      'slow consumer|core_nats_message_drop|Core NATS delivery loss|recorder_nats_slow_consumer' \
      "$transition_log"
  then
    return 1
  fi
  return 0
}

validate_live_rpc_inputs() {
  python3 -I -B - \
    "$RPC_PROVIDER_URLS" "$RPC_PROVIDER_WEIGHTS" \
    "$PRODUCTION_RPC_URL" "$SECONDARY_RPC_URL" <<'PY'
import sys

urls = [item.strip() for item in sys.argv[1].split(",")]
priorities = [item.strip() for item in sys.argv[2].split(",")]

if urls != [sys.argv[3], sys.argv[4]] or len(set(urls)) != 2:
    raise SystemExit(1)
if len(priorities) != 2 or any(
    not value.isdigit() or int(value) <= 0
    for value in priorities
):
    raise SystemExit(1)
PY
}

production_environment_identity() {
  identity_path=$1
  [ -f "$identity_path" ] && [ ! -L "$identity_path" ] || return 1
  identity_metadata=$(stat -c '%d:%i:%u:%g:%a:%h:%s' "$identity_path") ||
    return 1
  identity_digest=$(sha256sum "$identity_path" | awk 'NR == 1 { print $1 }') ||
    return 1
  case "$identity_digest" in
    *[!0-9a-f]*|"") return 1 ;;
  esac
  [ "${#identity_digest}" -eq 64 ] || return 1
  printf '%s:%s\n' "$identity_metadata" "$identity_digest"
}

validate_live_rpc_rendering() {
  python3 -I -B - \
    "$rendered_candidate" "$PRODUCTION_RPC_URL" "$SECONDARY_RPC_URL" <<'PY'
import json
import sys
from pathlib import Path

try:
    rendered = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    environment = rendered["services"]["rpc-gateway"]["environment"]
    urls = [item.strip() for item in environment["RPC_PROVIDER_URLS"].split(",")]
    priorities = [
        item.strip() for item in environment["RPC_PROVIDER_WEIGHTS"].split(",")
    ]
except (OSError, UnicodeError, json.JSONDecodeError, KeyError, TypeError, AttributeError):
    raise SystemExit(1)
if urls != [sys.argv[2], sys.argv[3]] or len(set(urls)) != 2:
    raise SystemExit(1)
if len(priorities) != 2 or any(not value.isdigit() or int(value) <= 0 for value in priorities):
    raise SystemExit(1)
PY
}

engine_terminal_integrity_total() {
  metrics_output=$(
    compose exec -T phoenix-engine wget -q -O - http://127.0.0.1:9200/metrics
  ) || return 1
  metric_value=$(
    printf '%s\n' "$metrics_output" |
      awk '$1 == "phoenix_engine_terminal_integrity_total" { print $2; found=1 } END { if (!found) exit 1 }'
  ) || return 1
  case "$metric_value" in
    ''|*[!0-9]*) return 1 ;;
  esac
  printf '%s\n' "$metric_value"
}

engine_process_fatal_integrity_total() {
  metrics_output=$(
    compose exec -T phoenix-engine wget -q -O - http://127.0.0.1:9200/metrics
  ) || return 1
  metric_value=$(
    printf '%s\n' "$metrics_output" |
      awk '$1 == "phoenix_engine_runtime_exits_total{class=\"integrity_failure\"}" { print $2; found=1 } END { if (!found) exit 1 }'
  ) || return 1
  case "$metric_value" in
    ''|*[!0-9]*) return 1 ;;
  esac
  printf '%s\n' "$metric_value"
}

mark_engine_burn_in_started() {
  container_id=$1
  restart_count=$2
  terminal_integrity=$3
  process_fatal_integrity=$4
  [ -n "$release_state_updater" ] || return 0
  /usr/bin/python3 -I -B "$release_state_updater" \
    "$release_sha" engine-baseline ENGINE_BURN_IN_STARTED \
    --container-id "$container_id" \
    --restart-count "$restart_count" \
    --terminal-integrity "$terminal_integrity" \
    --process-fatal-integrity "$process_fatal_integrity" >/dev/null ||
    fail "durable Engine burn-in baseline update failed"
}

run_live_engine_burn_in() {
  engine_id=$(compose ps -a -q phoenix-engine | awk 'NF { print; exit }')
  rpc_id=$(compose ps -a -q rpc-gateway | awk 'NF { print; exit }')
  [ -n "$engine_id" ] && [ -n "$rpc_id" ] || return 1
  engine_restart_count=$(
    docker inspect --format '{{.RestartCount}}' "$engine_id"
  ) || return 1
  terminal_integrity_baseline=$(engine_terminal_integrity_total) || return 1
  process_fatal_integrity_baseline=$(
    engine_process_fatal_integrity_total
  ) || return 1
  mark_engine_burn_in_started \
    "$engine_id" "$engine_restart_count" "$terminal_integrity_baseline" \
    "$process_fatal_integrity_baseline"
  burn_in_deadline=$(( $(date +%s) + engine_burn_in_seconds ))
  while [ "$(date +%s)" -lt "$burn_in_deadline" ]; do
    sleep 5
    [ "$(compose ps -a -q phoenix-engine | awk 'NF { print; exit }')" = "$engine_id" ] ||
      return 1
    [ "$(compose ps -a -q rpc-gateway | awk 'NF { print; exit }')" = "$rpc_id" ] ||
      return 1
    [ -z "$(compose ps -q live-executor | awk 'NF { print; exit }')" ] || return 1
    engine_state=$(
      docker inspect --format \
        '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.RestartCount}}' \
        "$engine_id"
    ) || return 1
    [ "$engine_state" = "running|healthy|$engine_restart_count" ] || return 1
    rpc_state=$(
      docker inspect --format \
        '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
        "$rpc_id"
    ) || return 1
    [ "$rpc_state" = "running|healthy" ] || return 1
    compose exec -T phoenix-engine wget -q -O - \
      http://127.0.0.1:9200/readyz >/dev/null || return 1
    compose exec -T rpc-gateway wget -q -O - \
      http://127.0.0.1:9300/readyz >/dev/null || return 1
    [ "$(engine_terminal_integrity_total)" = "$terminal_integrity_baseline" ] ||
      return 1
    [ "$(engine_process_fatal_integrity_total)" = \
      "$process_fatal_integrity_baseline" ] || return 1
  done
  echo "LIVE_ENGINE_BURN_IN_OK: ${engine_burn_in_seconds}s"
}

install_active_file() {
  source_file=$1
  target_file=$2
  target_mode=$3
  active_tmp=$(mktemp "$runtime_dir/active.XXXXXX") || return 1
  if ! cp "$source_file" "$active_tmp" ||
    ! chmod "$target_mode" "$active_tmp" ||
    ! mv "$active_tmp" "$target_file"
  then
    rm -f "$active_tmp"
    return 1
  fi
}

verify_active_release_coherence "$rollback_sha" "" ||
  fail "active release pointers are incoherent before deployment"
active_environment_identity_before=$(production_environment_identity "$env_file") ||
  fail "active production environment identity is unavailable before preflight"
cp "$env_file" "$candidate_evidence_env" ||
  fail "candidate evidence environment could not be copied"
chmod 0600 "$candidate_evidence_env"
[ "$(stat -c '%u:%g:%a:%h' "$candidate_evidence_env")" = 0:0:600:1 ] ||
  fail "candidate evidence environment metadata is unsafe"
python3 "$deploy_dir/production_mode.py" shadow \
  --env-file "$candidate_evidence_env" ||
  fail "candidate evidence environment could not be materialized"
"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --overlay-file "$overlay_file" \
  --expected-mode DISARMED_EVIDENCE \
  --env-file "$candidate_evidence_env" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$rendered_candidate" \
  --metadata-output "$metadata_candidate" >/dev/null ||
  fail "preflight production rendering failed"
rm -f "$candidate_evidence_env"
validate_live_rpc_inputs ||
  fail "preflight LIVE RPC provider and priority configuration is invalid"
cp "$env_file" "$candidate_live_env" ||
  fail "candidate LIVE environment could not be copied"
chmod 0600 "$candidate_live_env"
[ "$(stat -c '%u:%g:%a:%h' "$candidate_live_env")" = 0:0:600:1 ] ||
  fail "candidate LIVE environment metadata is unsafe"
python3 "$deploy_dir/production_mode.py" live --env-file "$candidate_live_env" ||
  fail "candidate LIVE environment could not be materialized"
"$deploy_dir/validate-production-env.sh" "$candidate_live_env"
"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --overlay-file "$overlay_file" \
  --expected-mode LIVE \
  --env-file "$candidate_live_env" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$rendered_candidate" \
  --metadata-output "$metadata_candidate" >/dev/null ||
  fail "candidate LIVE overlay rendering failed"
validate_live_rpc_rendering ||
  fail "candidate LIVE RPC provider and priority rendering is invalid"
rm -f "$candidate_live_env"
active_environment_identity_after=$(production_environment_identity "$env_file") ||
  fail "active production environment identity is unavailable after preflight"
[ "$active_environment_identity_after" = "$active_environment_identity_before" ] ||
  fail "active production environment changed during candidate preflight"
capture_protected_ids "$protected_before" || fail "protected services are not ready before deployment"
capture_service_ids "$release_env" "$fixed_protected_services" "$fixed_before" ||
  fail "fixed protected services are not ready before deployment"
mark_phase CANDIDATE_LIVE_RENDER_VERIFIED

rollback_on_failure() {
  code=$?
  trap - EXIT
  if [ "$code" -ne 0 ]; then
    state_update failure deployment_failed >/dev/null 2>&1 || true
  fi
  if [ "$code" -ne 0 ] && [ "$mutation_started" -eq 1 ]; then
    state_update rollback ROLLBACK_STARTED >/dev/null 2>&1 || true
    repause_ok=1
    compose stop -t 30 live-executor >/dev/null 2>&1 || true
    set +e
    disarm_output=$(compose run --rm --no-deps \
      -e PHOENIX_AUTONOMOUS_DISARM_ACK=DISARM_AUTONOMOUS_LIVE_42161 \
      -e PHOENIX_AUTONOMOUS_DISARM_REASON=deployment_rollback \
      autonomous-control disarm 2>&1)
    disarm_code=$?
    set -e
    printf '%s\n' "$disarm_output"
    if [ "$disarm_code" -ne 0 ]; then
      repause_ok=0
      echo "DEPLOY_COMPENSATION_FAILED: disarmed-failure state was not proven"
    fi
    echo "DEPLOY_FAILED: invoking rollback"
    set +e
    rollback_output=$(
      {
        if [ "$protected_transition_started" -eq 1 ]; then
          transition_mutable_protected "$release_env" "$rollback_release_env"
        fi &&
        PHOENIX_DEPLOY_ROOT="$deploy_root" \
        PHOENIX_ENV_FILE="$env_file" \
          /bin/sh "$rollback_context_installer" \
            "$rollback_sha" "$rollback_release_root" &&
        PHOENIX_DEPLOY_ROOT="$deploy_root" \
        PHOENIX_RELEASE_ROOT="$release_root" \
        PHOENIX_ENV_FILE="$env_file" \
        PHOENIX_CURRENT_LIVE_RELEASE_ENV="$release_env" \
        PHOENIX_CONTEXT_INSTALLER="$rollback_context_installer" \
        PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY=true \
          /bin/sh "$rollback_script"
      } 2>&1
    )
    rollback_code=$?
    set -e
    if [ "$rollback_code" -eq 0 ]; then
      if ! verify_active_release_coherence "$rollback_sha" "$rollback_sha"; then
        echo "ROLLBACK_INCOMPLETE: active release pointers are incoherent"
        state_update rollback ROLLBACK_FAILED >/dev/null 2>&1 || true
      elif [ "$repause_ok" -eq 1 ]; then
        printf '%s\n' "$rollback_output"
        state_update rollback ROLLED_BACK >/dev/null 2>&1 || true
      else
        echo "ROLLBACK_INCOMPLETE: executor re-pause was not proven"
        state_update rollback ROLLBACK_FAILED >/dev/null 2>&1 || true
      fi
    else
      printf '%s\n' "$rollback_output"
      echo "ROLLBACK_FAILED"
      state_update rollback ROLLBACK_FAILED >/dev/null 2>&1 || true
    fi
  fi
  rm -rf "$state_dir"
  exit "$code"
}
mutation_started=0
trap rollback_on_failure EXIT

# Word splitting is intentional for manifest-derived validated service names.
# shellcheck disable=SC2086
compose pull $pull_services
state_update mutation mutation_started
mutation_started=1
if [ -s "$current_file" ]; then
  cp "$current_file" "$previous_file"
fi
python3 "$deploy_dir/production_mode.py" live --env-file "$env_file" ||
  fail "autonomous production mode could not be installed"
reload_environment
assert_live_environment
"$deploy_dir/validate-production-env.sh" "$env_file"
"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --overlay-file "$overlay_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$rendered_candidate" \
  --metadata-output "$metadata_candidate" >/dev/null ||
  fail "canonical production rendering failed"
validate_live_rpc_rendering ||
  fail "rendered LIVE RPC provider and priority configuration is invalid"
transition_mutable_protected "$rollback_release_env" "$release_env" ||
  fail "mutable protected services could not transition without loss"
compose stop -t 30 live-executor >/dev/null 2>&1 || true
compose run --rm --no-deps autonomous-control migrate
compose run --rm --no-deps migration-runner
mark_phase MIGRATIONS_APPLIED
mark_phase EVIDENCE_MODE_INSTALLED
compose run --rm --no-deps \
  -e PHOENIX_RELEASE_SHA="$release_sha" \
  -e PHOENIX_DISARMED_DEPLOY_ACK=INSTALL_DISARMED_EVIDENCE_RELEASE_42161 \
  autonomous-control disarmed-deploy
mark_phase DISARMED_CONTROL_INSTALLED
install_aave_discovery_seed
remove_source_only_services ||
  fail "source-only services could not be removed before target activation"
for service in $start_services; do
  case "$service" in
    rpc-gateway|phoenix-engine) continue ;;
    economic-monitor)
      compose up -d --no-deps --force-recreate "$service"
      ;;
    *)
      compose up -d --no-deps "$service"
      ;;
  esac
  if ! wait_service_healthy "$service"; then
    capture_service_failure_evidence "$service"
    fail "optional service did not become healthy: $service"
  fi
done
compose up -d --no-deps rpc-gateway
wait_service_healthy rpc-gateway ||
  fail "rpc-gateway did not become healthy before Engine burn-in"
mark_phase RPC_GATEWAY_HEALTHY
compose up -d --no-deps phoenix-engine
wait_service_healthy phoenix-engine ||
  fail "phoenix-engine did not become healthy before Engine burn-in"
mark_phase ENGINE_HEALTHY
run_live_engine_burn_in ||
  fail "disarmed evidence Engine burn-in failed"
mark_phase ENGINE_BURN_IN_PASSED
mark_phase POST_DISARMED_VERIFYING
assert_intentional_absence ||
  fail "a service required to be absent is present after target activation"
capture_protected_ids "$protected_after" || fail "protected services are not ready after deployment"
capture_service_ids "$release_env" "$fixed_protected_services" "$fixed_after" ||
  fail "fixed protected services are not ready after deployment"
cmp "$fixed_before" "$fixed_after" >/dev/null ||
  fail "fixed protected service identity changed during deployment"
if [ "$protected_transition_started" -eq 0 ]; then
  cmp "$protected_before" "$protected_after" >/dev/null ||
    fail "protected service identity changed without an image transition"
fi
reload_environment
assert_live_environment
(
  unset PHOENIX_MODE LIVE_EXECUTION AUTONOMOUS_EXECUTION
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_RELEASE_ENV="$release_env" \
  PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE \
  PHOENIX_HEALTH_ALLOW_STOPPED_STANDBY=true \
    "$deploy_dir/production-healthcheck.sh"
)
mark_phase POST_DISARMED_VERIFIED
control_status=$(compose run --rm --no-deps autonomous-control status)
printf '%s\n' "$control_status" |
  verify_runtime_control_phase DISARMED_DEPLOY ||
  fail "runtime controls are not fail-closed before evidence-start"
[ -z "$(compose ps -q live-executor | awk 'NF { print; exit }')" ] ||
  fail "live-executor started before evidence-start"
compose run --rm --no-deps \
  -e PHOENIX_RELEASE_SHA="$release_sha" \
  -e PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161 \
  autonomous-control evidence-start
evidence_status=$(compose run --rm --no-deps autonomous-control status)
printf '%s\n' "$evidence_status" |
  verify_runtime_control_phase DISARMED_EVIDENCE ||
  fail "runtime did not enter fail-closed DISARMED_EVIDENCE"
[ -z "$(compose ps -q live-executor | awk 'NF { print; exit }')" ] ||
  fail "live-executor started during evidence-start"
mark_phase DISARMED_EVIDENCE_STARTED

# Keep the signer-isolated process continuously connected. Durable per-lane
# controls are still disarmed, all lane kill switches remain engaged, and the
# on-chain executor remains paused at this release phase.
compose up -d --no-deps live-executor
wait_service_healthy live-executor ||
  fail "live-executor hunting standby did not become healthy"
control_status=$(compose run --rm --no-deps autonomous-control status)
printf '%s\n' "$control_status" |
  verify_runtime_control_phase DISARMED_EVIDENCE ||
  fail "hunting standby changed fail-closed runtime controls"
mark_phase HUNTING_STANDBY_STARTED

printf '%s\n' "$release_sha" >"$pointer_candidate"
printf '%s\n' "$release_sha" >"$assets_pointer_candidate"
python3 "$deploy_dir/production_context.py" write-state \
  --manifest "$manifest" \
  --release-env "$release_env" \
  --render-metadata "$metadata_candidate" \
  --compose-config "$rendered_candidate" \
  --output "$state_candidate"

set +e
context_validation_output=$(
  "$deploy_dir/validate-production-release-context.sh" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --release-manifest "$manifest" \
    --current-release "$pointer_candidate" \
    --release-state "$state_candidate" \
    --inspect-running \
    --rendered-output "$context_rendered" \
    --metadata-output "$context_metadata" \
    --output "$context_candidate" 2>&1
)
context_validation_code=$?
set -e
if [ "$context_validation_code" -ne 0 ]; then
  context_validation_evidence=$(
    printf '%s\n' "$context_validation_output" |
      grep -E '^\{"code":"[A-Z][A-Z0-9_]{2,63}",.*"status":"error"\}$' |
      tail -n 1 || true
  )
  if [ -z "$context_validation_evidence" ]; then
    context_validation_evidence='{"code":"PRODUCTION_CONTEXT_VALIDATION_FAILED","status":"error"}'
  fi
  printf '%s\n' "$context_validation_evidence"
  fail "production release context validation failed"
fi
install_active_file "$metadata_candidate" "$release_metadata" 0640
install_active_file "$state_candidate" "$release_state" 0640
install_active_file "$release_env" "$current_env" 0640
install_active_file "$state_candidate" "$current_state" 0640
install_active_file "$context_candidate" "$current_context" 0640
install_active_file "$assets_pointer_candidate" "$release_assets_file" 0640
install_active_file "$pointer_candidate" "$current_file" 0640
verify_active_release_coherence "$release_sha" "$rollback_sha" ||
  fail "candidate release pointers are incoherent after promotion"
mark_phase COMPLETED
rm -f "$candidate_release_assets_file"

trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "DEPLOY_OK: $release_sha"
