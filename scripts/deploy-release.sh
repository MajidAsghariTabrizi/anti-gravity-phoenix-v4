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
owner_authorization=/etc/phoenix/authorizations/executor-owner-bootstrap.json
candidate_release_assets_file="$deploy_dir/candidate-release-assets.sha"
release_assets_file="$deploy_dir/release-assets.sha"
protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_services='prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'
service_wait_seconds=${PHOENIX_DEPLOY_SERVICE_WAIT_SECONDS:-300}
engine_burn_in_seconds=${PHOENIX_ENGINE_BURN_IN_SECONDS:-120}

fail() {
  echo "DEPLOY_FAILED: $1"
  exit 1
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
rollback_script="$rollback_release_root/scripts/rollback-release.sh"
rollback_context_installer="$rollback_release_root/scripts/install-production-release-context.sh"
[ -f "$rollback_script" ] && [ ! -L "$rollback_script" ] ||
  fail "version-matched rollback script is missing or unsafe"
[ -f "$rollback_context_installer" ] && [ ! -L "$rollback_context_installer" ] ||
  fail "version-matched rollback context installer is missing or unsafe"
case "$service_wait_seconds" in
  ''|*[!0-9]*) fail "service wait seconds must be an integer" ;;
esac
[ "$service_wait_seconds" -ge 30 ] && [ "$service_wait_seconds" -le 900 ] ||
  fail "service wait seconds must be from 30 through 900"
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
  --output "$release_env" || fail "release manifest validation failed"
chmod 0640 "$release_env"

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
[ -n "${LIVE_EXECUTOR_SIGNER_FILE:-}" ] &&
  [ -f "$LIVE_EXECUTOR_SIGNER_FILE" ] &&
  [ ! -L "$LIVE_EXECUTOR_SIGNER_FILE" ] || {
    echo EXTERNAL_SIGNER_FILE_REQUIRED
    exit 1
  }
signer_metadata=$(stat -c '%u:%g:%a:%h' "$LIVE_EXECUTOR_SIGNER_FILE") ||
  fail "signer file metadata is unavailable"
case "$signer_metadata" in
  65532:65532:400:1|65532:65532:440:1) ;;
  *) fail "signer file ownership, mode, or link count is unsafe" ;;
esac
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
state_candidate="$state_dir/release-state.json"
pointer_candidate="$state_dir/current-release"
assets_pointer_candidate="$state_dir/release-assets.sha"
context_candidate="$state_dir/release-context.json"
context_rendered="$state_dir/context.compose.json"
context_metadata="$state_dir/context.metadata.json"
protected_before="$state_dir/protected.before.tsv"
protected_after="$state_dir/protected.after.tsv"
owner_plan="$runtime_dir/owner-plan-$release_sha.json"
owner_configure_evidence="$runtime_dir/owner-configure-$release_sha.json"
owner_configured_preflight_evidence="$runtime_dir/owner-configured-preflight-$release_sha.json"
owner_unpause_evidence="$runtime_dir/owner-unpause-$release_sha.json"
owner_pause_evidence="$runtime_dir/owner-pause-$release_sha.json"
consumed_owner_authorization=
owner_unpause_attempted=0

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
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose \
      --env-file "$env_file" \
      --env-file "$release_env" \
      -f "$compose_file" \
      -f "$overlay_file" \
      --profile live-autonomous "$@"
}

capture_protected_ids() {
  output=$1
  : >"$output"
  for service in $protected_services; do
    id=$(compose ps -a -q "$service" | awk 'NF { print; exit }')
    [ -n "$id" ] || return 1
    state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id") || return 1
    [ "$state" = 'running|healthy' ] || return 1
    printf '%s\t%s\n' "$service" "$id" >>"$output"
  done
}

wait_service_healthy() {
  service=$1
  deadline=$(( $(date +%s) + service_wait_seconds ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    id=$(compose ps -a -q "$service" | awk 'NF { print; exit }')
    if [ -n "$id" ]; then
      state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id" 2>/dev/null || true)
      [ "$state" = 'running|healthy' ] && return 0
    fi
    sleep 3
  done
  return 1
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

run_live_engine_burn_in() {
  engine_id=$(compose ps -a -q phoenix-engine | awk 'NF { print; exit }')
  rpc_id=$(compose ps -a -q rpc-gateway | awk 'NF { print; exit }')
  [ -n "$engine_id" ] && [ -n "$rpc_id" ] || return 1
  engine_restart_count=$(
    docker inspect --format '{{.RestartCount}}' "$engine_id"
  ) || return 1
  terminal_integrity_baseline=$(engine_terminal_integrity_total) || return 1
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

validate_owner_authorization() {
  [ -f "$owner_authorization" ] && [ ! -L "$owner_authorization" ] ||
    fail "executor owner authorization is missing or unsafe"
  authorization_size=$(stat -c '%s' "$owner_authorization") ||
    fail "executor owner authorization size is unavailable"
  [ "$authorization_size" -gt 0 ] && [ "$authorization_size" -le 2048 ] ||
    fail "executor owner authorization size is invalid"
  authorization_metadata=$(stat -c '%u:%g:%a:%h' "$owner_authorization") ||
    fail "executor owner authorization metadata is unavailable"
  [ "$authorization_metadata" = 0:0:600:1 ] ||
    fail "executor owner authorization metadata is unsafe"
  python3 -I -B - "$owner_authorization" "$release_sha" <<'PY' ||
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
release_sha = sys.argv[2]
try:
    raw = path.read_bytes()
    text = raw.decode("utf-8")
    value = json.loads(text)
except (OSError, UnicodeDecodeError, json.JSONDecodeError):
    raise SystemExit(1)
if not isinstance(value, dict):
    raise SystemExit(1)
if set(value) != {"schema", "release_sha", "chain_id", "acknowledgement"}:
    raise SystemExit(1)
if value != {
    "schema": "phoenix.executor-owner-bootstrap-authorization.v1",
    "release_sha": release_sha,
    "chain_id": 42161,
    "acknowledgement": "BOOTSTRAP_EXECUTOR_OWNER_42161",
}:
    raise SystemExit(1)
PY
    fail "executor owner authorization content is invalid"
}

consume_owner_authorization() {
  authorization_device=$(stat -c '%d' "$owner_authorization") ||
    fail "executor owner authorization filesystem is unavailable"
  runtime_device=$(stat -c '%d' "$runtime_dir") ||
    fail "deployment runtime filesystem is unavailable"
  [ "$authorization_device" = "$runtime_device" ] ||
    fail "executor owner authorization cannot be consumed atomically"
  consumed_authorization_dir=$(mktemp -d \
    "$runtime_dir/owner-authorization-consumed-$release_sha.XXXXXX") ||
    fail "consumed executor owner authorization directory could not be created"
  chmod 0700 "$consumed_authorization_dir"
  consumed_owner_authorization="$consumed_authorization_dir/authorization.json"
  mv -n "$owner_authorization" "$consumed_owner_authorization" ||
    fail "executor owner authorization could not be consumed"
  [ ! -e "$owner_authorization" ] && [ -f "$consumed_owner_authorization" ] ||
    fail "executor owner authorization was not consumed exactly once"
  consumed_metadata=$(stat -c '%u:%g:%a:%h' "$consumed_owner_authorization") ||
    fail "consumed executor owner authorization metadata is unavailable"
  [ "$consumed_metadata" = 0:0:600:1 ] ||
    fail "consumed executor owner authorization metadata is unsafe"
}

verify_active_release_coherence "$rollback_sha" "" ||
  fail "active release pointers are incoherent before deployment"
"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --overlay-file "$overlay_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$rendered_candidate" \
  --metadata-output "$metadata_candidate" >/dev/null ||
  fail "preflight production rendering failed"
validate_live_rpc_rendering ||
  fail "preflight LIVE RPC provider and priority configuration is invalid"
capture_protected_ids "$protected_before" || fail "protected services are not ready before deployment"

rollback_on_failure() {
  code=$?
  trap - EXIT
  if [ "$code" -ne 0 ] && [ "$mutation_started" -eq 1 ]; then
    repause_ok=1
    compose stop -t 30 live-executor >/dev/null 2>&1 || true
    if [ "$owner_unpause_attempted" -eq 1 ]; then
      if [ "$owner_unpaused" -eq 1 ]; then
        echo "DEPLOY_FAILED: compensating applied owner-unpause"
      else
        echo "DEPLOY_FAILED: compensating attempted owner-unpause"
      fi
      set +e
      pause_output=$(compose run --rm --no-deps \
        -e PHOENIX_RELEASE_SHA="$release_sha" \
        -e PHOENIX_EXECUTOR_OWNER_PAUSE_ACK=PAUSE_EXECUTOR_AFTER_FAILED_DEPLOY_42161 \
        --entrypoint /usr/local/bin/autonomous-live-control \
        live-executor owner-pause 2>&1)
      pause_code=$?
      set -e
      printf '%s\n' "$pause_output"
      if [ "$pause_code" -eq 0 ]; then
        printf '%s\n' "$pause_output" >"$owner_pause_evidence"
        chmod 0600 "$owner_pause_evidence"
        echo "DEPLOY_COMPENSATION_OK: executor paused before rollback"
      else
        repause_ok=0
        echo "DEPLOY_COMPENSATION_FAILED: executor re-pause failed"
      fi
    fi
    echo "DEPLOY_FAILED: invoking rollback"
    set +e
    rollback_output=$(
      {
        PHOENIX_DEPLOY_ROOT="$deploy_root" \
        PHOENIX_ENV_FILE="$env_file" \
          /bin/sh "$rollback_context_installer" \
            "$rollback_sha" "$rollback_release_root" &&
        PHOENIX_DEPLOY_ROOT="$deploy_root" \
        PHOENIX_RELEASE_ROOT="$release_root" \
        PHOENIX_ENV_FILE="$env_file" \
        PHOENIX_CURRENT_LIVE_RELEASE_ENV="$release_env" \
        PHOENIX_CONTEXT_INSTALLER="$rollback_context_installer" \
          /bin/sh "$rollback_script"
      } 2>&1
    )
    rollback_code=$?
    set -e
    if [ "$rollback_code" -eq 0 ]; then
      if ! verify_active_release_coherence "$rollback_sha" "$rollback_sha"; then
        echo "ROLLBACK_INCOMPLETE: active release pointers are incoherent"
      elif [ "$repause_ok" -eq 1 ]; then
        printf '%s\n' "$rollback_output"
      else
        echo "ROLLBACK_INCOMPLETE: executor re-pause was not proven"
      fi
    else
      printf '%s\n' "$rollback_output"
      echo "ROLLBACK_FAILED"
    fi
  fi
  rm -rf "$state_dir"
  exit "$code"
}
mutation_started=0
owner_unpaused=0
owner_bootstrap_started=0
trap rollback_on_failure EXIT

compose pull
set +e
preflight_output=$(compose run --rm --no-deps \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor preflight 2>&1)
preflight_code=$?
set -e
printf '%s\n' "$preflight_output"
if [ "$preflight_code" -ne 0 ]; then
  case "$preflight_output" in
    *"wallet has no native gas balance"*)
      echo EXTERNAL_GAS_FUNDING_REQUIRED
      exit 1
      ;;
    *"executor configuration is not LIVE-ready"*)
      compose run --rm --no-deps \
        -e PHOENIX_RELEASE_SHA="$release_sha" \
        --entrypoint /usr/local/bin/autonomous-live-control \
        live-executor owner-plan >"$owner_plan" ||
        fail "executor owner plan could not be materialized"
      chmod 0640 "$owner_plan"
      cat "$owner_plan"
      if [ ! -e "$owner_authorization" ]; then
        echo "EXTERNAL_OWNER_AUTHORIZATION_REQUIRED: $owner_plan"
        exit 1
      fi
      validate_owner_authorization
      consume_owner_authorization
      owner_bootstrap_started=1
      set +e
      owner_configure_output=$(compose run --rm --no-deps \
        -e PHOENIX_RELEASE_SHA="$release_sha" \
        -e PHOENIX_EXECUTOR_OWNER_BOOTSTRAP_ACK=BOOTSTRAP_EXECUTOR_OWNER_42161 \
        --entrypoint /usr/local/bin/autonomous-live-control \
        live-executor owner-configure 2>&1)
      owner_configure_code=$?
      set -e
      printf '%s\n' "$owner_configure_output"
      [ "$owner_configure_code" -eq 0 ] ||
        fail "executor owner configuration failed"
      printf '%s\n' "$owner_configure_output" >"$owner_configure_evidence"
      chmod 0600 "$owner_configure_evidence"
      set +e
      owner_configured_preflight_output=$(compose run --rm --no-deps \
        -e PHOENIX_RELEASE_SHA="$release_sha" \
        --entrypoint /usr/local/bin/autonomous-live-control \
        live-executor owner-configured-preflight 2>&1)
      owner_configured_preflight_code=$?
      set -e
      printf '%s\n' "$owner_configured_preflight_output"
      [ "$owner_configured_preflight_code" -eq 0 ] ||
        fail "configured executor preflight failed"
      printf '%s\n' "$owner_configured_preflight_output" \
        >"$owner_configured_preflight_evidence"
      chmod 0600 "$owner_configured_preflight_evidence"
      ;;
    *) fail "read-only autonomous preflight failed" ;;
  esac
fi
if [ -s "$current_file" ]; then
  cp "$current_file" "$previous_file"
fi
mutation_started=1
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
compose stop -t 30 live-executor >/dev/null 2>&1 || true
compose run --rm --no-deps \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor migrate
compose run --rm --no-deps migration-runner
for service in $optional_services; do
  case "$service" in
    rpc-gateway|phoenix-engine) continue ;;
  esac
  compose up -d --no-deps "$service"
  wait_service_healthy "$service" || fail "optional service did not become healthy: $service"
done
compose up -d --no-deps rpc-gateway
wait_service_healthy rpc-gateway ||
  fail "rpc-gateway did not become healthy before Engine burn-in"
compose up -d --no-deps phoenix-engine
wait_service_healthy phoenix-engine ||
  fail "phoenix-engine did not become healthy before Engine burn-in"
run_live_engine_burn_in ||
  fail "autonomous LIVE Engine burn-in failed"
compose run --rm --no-deps \
  -e PHOENIX_AUTONOMOUS_ACTIVATION_ACK=ACTIVATE_AUTONOMOUS_LIVE_42161 \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor activate
set +e
owner_unpause_attempted=1
owner_unpause_output=$(compose run --rm --no-deps \
  -e PHOENIX_RELEASE_SHA="$release_sha" \
  -e PHOENIX_EXECUTOR_OWNER_UNPAUSE_ACK=UNPAUSE_CONFIGURED_EXECUTOR_42161 \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor owner-unpause 2>&1)
owner_unpause_code=$?
set -e
printf '%s\n' "$owner_unpause_output"
if [ "$owner_bootstrap_started" -eq 1 ] &&
  printf '%s\n' "$owner_unpause_output" |
    grep -F '"status": "applied"' >/dev/null
then
  owner_unpaused=1
fi
[ "$owner_unpause_code" -eq 0 ] || fail "executor owner unpause failed"
printf '%s\n' "$owner_unpause_output" >"$owner_unpause_evidence"
chmod 0600 "$owner_unpause_evidence"
compose run --rm --no-deps \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor preflight
compose up -d --no-deps live-executor
wait_service_healthy live-executor ||
  fail "autonomous LIVE executor did not become healthy"
compose run --rm --no-deps \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor status
capture_protected_ids "$protected_after" || fail "protected services are not ready after deployment"
cmp "$protected_before" "$protected_after" >/dev/null || fail "protected service identity changed during deployment"
reload_environment
assert_live_environment
(
  unset PHOENIX_MODE LIVE_EXECUTION AUTONOMOUS_EXECUTION
  PHOENIX_RELEASE_ENV="$release_env" "$deploy_dir/production-healthcheck.sh"
)

printf '%s\n' "$release_sha" >"$pointer_candidate"
printf '%s\n' "$release_sha" >"$assets_pointer_candidate"
python3 "$deploy_dir/production_context.py" write-state \
  --manifest "$manifest" \
  --release-env "$release_env" \
  --render-metadata "$metadata_candidate" \
  --compose-config "$rendered_candidate" \
  --output "$state_candidate"

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
  --output "$context_candidate" >/dev/null

install_active_file "$metadata_candidate" "$release_metadata" 0640
install_active_file "$state_candidate" "$release_state" 0640
install_active_file "$release_env" "$current_env" 0640
install_active_file "$state_candidate" "$current_state" 0640
install_active_file "$context_candidate" "$current_context" 0640
install_active_file "$assets_pointer_candidate" "$release_assets_file" 0640
install_active_file "$pointer_candidate" "$current_file" 0640
verify_active_release_coherence "$release_sha" "$rollback_sha" ||
  fail "candidate release pointers are incoherent after promotion"
rm -f "$candidate_release_assets_file"

trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "DEPLOY_OK: $release_sha"
