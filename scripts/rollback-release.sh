#!/usr/bin/env sh
set -eu

deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
release_root="${PHOENIX_RELEASE_ROOT:-$deploy_root/releases}"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
current_file="$deploy_dir/current-release"
current_env="$deploy_dir/current-release.env"
current_state="$deploy_dir/current-release.json"
current_context="$deploy_dir/current-release-context.json"
previous_file="$deploy_dir/previous-release"
runtime_dir="$deploy_dir/.runtime"
protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_services='prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'
service_wait_seconds=${PHOENIX_DEPLOY_SERVICE_WAIT_SECONDS:-300}

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
[ -f "$manifest" ] || fail "release manifest is missing"
[ -f "$compose_file" ] || fail "production compose file is missing"
[ -f "$env_file" ] || fail "production environment file is missing"
[ -d "$release_assets_root" ] || fail "immutable rollback release assets are missing"
[ -f "$release_assets_root/release-assets-manifest.json" ] ||
  fail "rollback release-assets manifest is missing"
case "$service_wait_seconds" in
  ''|*[!0-9]*) fail "service wait seconds must be an integer" ;;
esac
[ "$service_wait_seconds" -ge 30 ] && [ "$service_wait_seconds" -le 900 ] ||
  fail "service wait seconds must be from 30 through 900"

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v cmp >/dev/null 2>&1 || fail "cmp is unavailable"
python3 "$deploy_dir/release_assets.py" verify-tree \
  --root "$release_assets_root" \
  --manifest "$release_assets_root/release-assets-manifest.json" \
  --expected-sha "$release_sha" >/dev/null ||
  fail "immutable rollback release assets failed integrity validation"
"$release_assets_root/scripts/bootstrap-production.sh" "$release_sha" ||
  fail "rollback release assets could not be restored"
[ -s "$deploy_dir/release-assets.sha" ] || fail "rollback release-assets marker is missing"
installed_assets_sha=$(tr -d '\r\n' <"$deploy_dir/release-assets.sha")
[ "$installed_assets_sha" = "$release_sha" ] || fail "rollback release-assets marker is invalid"
mkdir -p "$runtime_dir"
chmod 0750 "$runtime_dir"
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
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose \
      --env-file "$env_file" \
      --env-file "$release_env" \
      -f "$compose_file" "$@"
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
if [ -n "$rollback_from" ] && [ "$rollback_from" != "$release_sha" ]; then
  printf '%s\n' "$rollback_from" >"$previous_file"
fi

trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "ROLLBACK_OK: $release_sha"
