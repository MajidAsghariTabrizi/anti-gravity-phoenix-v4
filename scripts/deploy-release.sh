#!/usr/bin/env sh
set -eu

release_sha="${1:-}"
deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
manifest="$deploy_dir/manifests/$release_sha.json"
release_env="$deploy_dir/manifests/$release_sha.env"
release_metadata="$deploy_dir/manifests/$release_sha.render.json"
release_state="$deploy_dir/manifests/$release_sha.state.json"
current_file="$deploy_dir/current-release"
current_env="$deploy_dir/current-release.env"
current_state="$deploy_dir/current-release.json"
current_context="$deploy_dir/current-release-context.json"
previous_file="$deploy_dir/previous-release"
runtime_dir="$deploy_dir/.runtime"

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
[ -f "$env_file" ] || fail "missing production environment file"

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
mkdir -p "$runtime_dir"
chmod 0750 "$runtime_dir"
python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env" || fail "release manifest validation failed"
chmod 0640 "$release_env"

"$deploy_dir/validate-production-env.sh" "$env_file"

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

if [ -s "$current_file" ]; then
  cp "$current_file" "$previous_file"
fi

rollback_on_failure() {
  code=$?
  trap - EXIT
  if [ "$code" -ne 0 ]; then
    echo "DEPLOY_FAILED: invoking rollback"
    "$deploy_dir/rollback-release.sh" || echo "ROLLBACK_FAILED"
  fi
  rm -rf "$state_dir"
  exit "$code"
}
trap rollback_on_failure EXIT

compose pull
compose run --rm migration-runner
compose up -d
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

trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "DEPLOY_OK: $release_sha"
