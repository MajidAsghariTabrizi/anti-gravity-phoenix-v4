#!/usr/bin/env sh
set -eu

deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
release_root="${PHOENIX_RELEASE_ROOT:-$deploy_root/releases}"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
overlay_file="$deploy_dir/compose.live-autonomous.yml"
current_file="$deploy_dir/current-release"
current_env="$deploy_dir/current-release.env"
live_release_env="${PHOENIX_CURRENT_LIVE_RELEASE_ENV:-$current_env}"
current_state="$deploy_dir/current-release.json"
current_context="$deploy_dir/current-release-context.json"
release_assets_file="$deploy_dir/release-assets.sha"
candidate_release_assets_file="$deploy_dir/candidate-release-assets.sha"
previous_file="$deploy_dir/previous-release"
runtime_dir="${PHOENIX_DEPLOY_RUNTIME_DIR:-$deploy_dir/.deploy-runtime}"
protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_services='prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'
service_wait_seconds=${PHOENIX_DEPLOY_SERVICE_WAIT_SECONDS:-300}
reconciliation_seconds=${PHOENIX_ROLLBACK_RECONCILIATION_SECONDS:-180}

fail() {
  echo "ROLLBACK_FAILED: $1"
  exit 1
}

[ -s "$previous_file" ] || fail "previous release is missing"
release_sha=$(tr -d '\r\n' <"$previous_file")
case "$release_sha" in
  *[!0-9a-f]*|"") fail "previous release SHA is invalid" ;;
esac
[ "${#release_sha}" -eq 40 ] || fail "previous release SHA is invalid"

release_assets_root="$release_root/$release_sha"
manifest="$deploy_dir/manifests/$release_sha.json"
release_env="$deploy_dir/manifests/$release_sha.env"
release_metadata="$deploy_dir/manifests/$release_sha.render.json"
release_state="$deploy_dir/manifests/$release_sha.state.json"
context_installer="${PHOENIX_CONTEXT_INSTALLER:-$deploy_dir/install-production-release-context.sh}"
[ -f "$manifest" ] || fail "release manifest is missing"
[ -f "$compose_file" ] || fail "production compose file is missing"
[ -f "$env_file" ] || fail "production environment file is missing"
[ -d "$release_assets_root" ] || fail "immutable rollback release assets are missing"
[ -f "$release_assets_root/release-assets-manifest.json" ] ||
  fail "rollback release-assets manifest is missing"
[ -f "$context_installer" ] && [ ! -L "$context_installer" ] ||
  fail "release-context installer is missing or unsafe"
case "$service_wait_seconds" in
  ''|*[!0-9]*) fail "service wait seconds must be an integer" ;;
esac
[ "$service_wait_seconds" -ge 30 ] && [ "$service_wait_seconds" -le 900 ] ||
  fail "service wait seconds must be from 30 through 900"
case "$reconciliation_seconds" in
  ''|*[!0-9]*) fail "reconciliation seconds must be an integer" ;;
esac
[ "$reconciliation_seconds" -ge 30 ] && [ "$reconciliation_seconds" -le 900 ] ||
  fail "reconciliation seconds must be from 30 through 900"

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v cmp >/dev/null 2>&1 || fail "cmp is unavailable"
mkdir -p "$runtime_dir"
chmod 0700 "$runtime_dir"

current_live_compose() {
  python3 "$deploy_dir/production_compose.py" \
    --mode LIVE \
    --env-file "$env_file" \
    --release-env "$live_release_env" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    -- "$@"
}

if [ -f "$overlay_file" ] && [ -s "$live_release_env" ]; then
  live_executor_id=$(current_live_compose ps -a -q live-executor | awk 'NF { print; exit }')
  current_live_compose config --services |
    grep -F -x autonomous-control >/dev/null ||
    fail "signerless autonomous control service is unavailable"
  current_live_compose run --rm --no-deps \
    -e PHOENIX_AUTONOMOUS_DISARM_ACK=DISARM_AUTONOMOUS_LIVE_42161 \
    -e PHOENIX_AUTONOMOUS_DISARM_REASON=operator_rollback \
    autonomous-control disarm ||
    fail "autonomous controls could not enter DISARMED_FAILURE"
  if [ -n "$live_executor_id" ]; then
    reconciliation_deadline=$(( $(date +%s) + reconciliation_seconds ))
    reconciled=0
    while [ "$(date +%s)" -lt "$reconciliation_deadline" ]; do
      if current_live_compose run --rm --no-deps \
        autonomous-control reconciliation-status >/dev/null 2>&1
      then
        reconciled=1
        break
      fi
      sleep 3
    done
    if [ "$reconciled" -eq 0 ]; then
      echo "ROLLBACK_NOTICE: receipt reconciliation timeout elapsed"
    fi
    current_live_compose stop -t 30 live-executor ||
      fail "autonomous LIVE executor could not be stopped"
  fi
  current_live_compose stop -t 30 economic-monitor >/dev/null 2>&1 || true
  current_live_compose stop -t 30 economic-supervisor >/dev/null 2>&1 || true
fi
python3 "$deploy_dir/production_mode.py" shadow --env-file "$env_file" ||
  fail "SHADOW production mode could not be restored"

python3 "$deploy_dir/release_assets.py" verify-tree \
  --root "$release_assets_root" \
  --manifest "$release_assets_root/release-assets-manifest.json" \
  --expected-sha "$release_sha" >/dev/null ||
  fail "immutable rollback release assets failed integrity validation"
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
  /bin/sh "$context_installer" "$release_sha" "$release_assets_root" ||
  fail "rollback release assets could not be restored"
[ -s "$candidate_release_assets_file" ] || fail "rollback release-assets marker is missing"
installed_assets_sha=$(tr -d '\r\n' <"$candidate_release_assets_file")
[ "$installed_assets_sha" = "$release_sha" ] || fail "rollback release-assets marker is invalid"
python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env" || fail "release manifest validation failed"
chmod 0640 "$release_env"

"$deploy_dir/validate-production-env.sh" "$env_file"

state_dir=$(mktemp -d "$runtime_dir/rollback-$release_sha.XXXXXX") ||
  fail "temporary rollback state could not be created"
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

"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$rendered_candidate" \
  --metadata-output "$metadata_candidate" >/dev/null ||
  fail "canonical production rendering failed"

compose() {
  python3 "$deploy_dir/production_compose.py" \
    --mode SHADOW \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --compose-file "$compose_file" \
    -- "$@"
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

verify_active_release_coherence() {
  expected_sha=$1
  expected_previous_sha=$2
  [ "$(tr -d '\r\n' <"$current_file")" = "$expected_sha" ] || return 1
  [ "$(tr -d '\r\n' <"$release_assets_file")" = "$expected_sha" ] || return 1
  [ "$(tr -d '\r\n' <"$previous_file")" = "$expected_previous_sha" ] ||
    return 1
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

rollback_from=
if [ -s "$current_file" ]; then
  rollback_from=$(tr -d '\r\n' <"$current_file")
fi

capture_protected_ids "$protected_before" || fail "protected services are not ready before rollback"
compose pull
for service in $optional_services; do
  compose up -d --no-deps "$service"
  wait_service_healthy "$service" || fail "optional service did not become healthy during rollback: $service"
done
capture_protected_ids "$protected_after" || fail "protected services are not ready after rollback"
cmp "$protected_before" "$protected_after" >/dev/null || fail "protected service identity changed during rollback"
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$release_env" \
PHOENIX_HEALTH_EXPECTED_MODE=SHADOW \
  "$deploy_dir/production-healthcheck.sh"

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
expected_previous_sha=$release_sha
if [ -n "$rollback_from" ] && [ "$rollback_from" != "$release_sha" ]; then
  printf '%s\n' "$rollback_from" >"$previous_file"
  expected_previous_sha=$rollback_from
fi
verify_active_release_coherence "$release_sha" "$expected_previous_sha" ||
  fail "rollback release pointers are incoherent after promotion"
rm -f "$candidate_release_assets_file"

trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "ROLLBACK_OK: $release_sha"
