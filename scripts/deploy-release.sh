#!/usr/bin/env sh
set -eu

release_sha="${1:-}"
deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
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
rollback_script="${PHOENIX_ROLLBACK_SCRIPT:-$deploy_dir/rollback-release.sh}"
release_assets_file="$deploy_dir/release-assets.sha"
protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_services='prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'
service_wait_seconds=${PHOENIX_DEPLOY_SERVICE_WAIT_SECONDS:-300}

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
[ -s "$release_assets_file" ] || fail "exact release assets are not installed"
installed_assets_sha=$(tr -d '\r\n' <"$release_assets_file")
[ "$installed_assets_sha" = "$release_sha" ] || fail "installed release assets do not match release SHA"
case "$service_wait_seconds" in
  ''|*[!0-9]*) fail "service wait seconds must be an integer" ;;
esac
[ "$service_wait_seconds" -ge 30 ] && [ "$service_wait_seconds" -le 900 ] ||
  fail "service wait seconds must be from 30 through 900"

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v cmp >/dev/null 2>&1 || fail "cmp is unavailable"
[ -f "$rollback_script" ] && [ ! -L "$rollback_script" ] ||
  fail "rollback script is missing or unsafe"
mkdir -p "$runtime_dir"
chmod 0700 "$runtime_dir"
python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env" || fail "release manifest validation failed"
chmod 0640 "$release_env"

"$deploy_dir/validate-production-env.sh" "$env_file"

set -a
# shellcheck disable=SC1090
. "$env_file"
set +a
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
context_candidate="$state_dir/release-context.json"
context_rendered="$state_dir/context.compose.json"
context_metadata="$state_dir/context.metadata.json"
protected_before="$state_dir/protected.before.tsv"
protected_after="$state_dir/protected.after.tsv"
owner_plan="$runtime_dir/owner-plan-$release_sha.json"

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

capture_protected_ids "$protected_before" || fail "protected services are not ready before deployment"

rollback_on_failure() {
  code=$?
  trap - EXIT
  if [ "$code" -ne 0 ] && [ "$mutation_started" -eq 1 ]; then
    echo "DEPLOY_FAILED: invoking rollback"
    PHOENIX_CURRENT_LIVE_RELEASE_ENV="$release_env" \
      "$rollback_script" || echo "ROLLBACK_FAILED"
  fi
  rm -rf "$state_dir"
  exit "$code"
}
mutation_started=0
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
        --entrypoint /usr/local/bin/autonomous-live-control \
        live-executor owner-plan >"$owner_plan" ||
        fail "executor owner plan could not be materialized"
      chmod 0640 "$owner_plan"
      cat "$owner_plan"
      echo "EXTERNAL_OWNER_AUTHORIZATION_REQUIRED: $owner_plan"
      exit 1
      ;;
    *) fail "read-only autonomous preflight failed" ;;
  esac
fi
if [ -s "$current_file" ]; then
  cp "$current_file" "$previous_file"
fi
python3 "$deploy_dir/production_mode.py" live --env-file "$env_file" ||
  fail "autonomous production mode could not be installed"
mutation_started=1
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
compose run --rm --no-deps \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor migrate
compose run --rm --no-deps migration-runner
for service in $optional_services; do
  compose up -d --no-deps "$service"
  wait_service_healthy "$service" || fail "optional service did not become healthy: $service"
done
compose run --rm --no-deps \
  -e PHOENIX_AUTONOMOUS_ACTIVATION_ACK=ACTIVATE_AUTONOMOUS_LIVE_42161 \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor activate
compose up -d --no-deps live-executor
wait_service_healthy live-executor ||
  fail "autonomous LIVE executor did not become healthy"
compose run --rm --no-deps \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor status
capture_protected_ids "$protected_after" || fail "protected services are not ready after deployment"
cmp "$protected_before" "$protected_after" >/dev/null || fail "protected service identity changed during deployment"
PHOENIX_RELEASE_ENV="$release_env" "$deploy_dir/production-healthcheck.sh"

printf '%s\n' "$release_sha" >"$pointer_candidate"
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
install_active_file "$pointer_candidate" "$current_file" 0640

trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "DEPLOY_OK: $release_sha"
