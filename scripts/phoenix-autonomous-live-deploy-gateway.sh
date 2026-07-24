#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH PYTHONDONTWRITEBYTECODE=1

libexec=/usr/local/libexec/phoenix-autonomous-live-deploy
deploy_root=/opt/phoenix
deploy_dir=$deploy_root/deploy
release_root=$deploy_root/releases
env_file=/etc/phoenix/phoenix.env
lock_file=/run/lock/phoenix-autonomous-live-deploy.lock

fail() {
  echo "AUTONOMOUS_LIVE_DEPLOY_GATEWAY_FAILED: $1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail root_required
[ "$(uname -s)" = Linux ] || fail linux_required
for command in flock install mktemp rm stat; do
  command -v "$command" >/dev/null 2>&1 || fail "command_missing_$command"
done
[ "$#" -eq 6 ] || fail argument_count_invalid
release_sha=$1
rollback_sha=$2
build_run_id=$3
rollback_build_run_id=$4
github_run_id=$5
github_run_attempt=$6
for value in "$release_sha" "$rollback_sha"; do
  case "$value" in *[!0-9a-f]*|"") fail release_sha_invalid ;; esac
  [ "${#value}" -eq 40 ] || fail release_sha_invalid
done
[ "$release_sha" != "$rollback_sha" ] || fail release_sha_pair_invalid
for value in \
  "$build_run_id" "$rollback_build_run_id" "$github_run_id" "$github_run_attempt"
do
  case "$value" in *[!0-9]*|"") fail run_identity_invalid ;; esac
done

stage="/tmp/phoenix-autonomous-live-deploy-$github_run_id-$github_run_attempt-$release_sha"
[ -d "$stage" ] && [ ! -L "$stage" ] || fail stage_invalid
[ "$(stat -c '%U:%G:%a' "$stage")" = phoenix:phoenix:700 ] || fail stage_permissions_invalid
archive="$stage/phoenix-release-assets-$release_sha.tar.gz"
release_manifest="$stage/release-manifest.json"
release_provenance="$stage/release-provenance.json"
rollback_manifest="$stage/rollback-manifest.json"
rollback_provenance="$stage/rollback-provenance.json"
asset_manifest="$stage/release-assets-manifest.json"
checksums="$stage/release-assets-checksums.txt"
for path in \
  "$archive" "$release_manifest" "$release_provenance" \
  "$rollback_manifest" "$rollback_provenance" "$asset_manifest" "$checksums"
do
  [ -f "$path" ] && [ ! -L "$path" ] || fail staged_file_invalid
  [ "$(stat -c '%U:%G:%a:%h' "$path")" = phoenix:phoenix:600:1 ] ||
    fail staged_file_permissions_invalid
done
[ -f "$env_file" ] && [ ! -L "$env_file" ] ||
  fail production_env_invalid
[ "$(stat -c '%U:%G:%a:%h' "$env_file")" = root:root:600:1 ] ||
  fail production_env_invalid
[ -s "$deploy_dir/current-release" ] || fail current_release_invalid
[ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$rollback_sha" ] ||
  fail current_release_mismatch
[ -s "$deploy_dir/release-assets.sha" ] ||
  fail current_release_invalid
[ "$(tr -d '\r\n' <"$deploy_dir/release-assets.sha")" = "$rollback_sha" ] ||
  fail current_release_mismatch
[ -d "$release_root/$rollback_sha" ] && [ ! -L "$release_root/$rollback_sha" ] ||
  fail rollback_release_invalid

exec 9>"$lock_file"
flock -n 9 || fail deployment_in_progress

/usr/bin/python3 -I -B "$libexec/release_provenance.py" validate-deploy-pair \
  --release-manifest "$release_manifest" \
  --release-provenance "$release_provenance" \
  --release-sha "$release_sha" \
  --build-run-id "$build_run_id" \
  --rollback-manifest "$rollback_manifest" \
  --rollback-provenance "$rollback_provenance" \
  --rollback-sha "$rollback_sha" \
  --rollback-build-run-id "$rollback_build_run_id" ||
  fail release_provenance_invalid
/usr/bin/python3 -I -B "$libexec/release_assets.py" verify \
  --archive "$archive" \
  --manifest "$asset_manifest" \
  --checksums "$checksums" \
  --expected-sha "$release_sha" >/dev/null ||
  fail release_assets_invalid
/usr/bin/python3 -I -B "$libexec/release_assets.py" verify-tree \
  --root "$release_root/$rollback_sha" \
  --manifest "$release_root/$rollback_sha/release-assets-manifest.json" \
  --expected-sha "$rollback_sha" >/dev/null ||
  fail rollback_release_invalid

current_validator=$deploy_dir/validate-production-release-context.sh
current_manifest=$deploy_dir/manifests/$rollback_sha.json
current_release_env=$deploy_dir/current-release.env
current_state=$deploy_dir/current-release.json
for path in \
  "$current_validator" "$deploy_dir/compose.prod.yml" "$current_manifest" \
  "$current_release_env" "$current_state"
do
  [ -f "$path" ] && [ ! -L "$path" ] || fail active_release_context_invalid
  [ "$(stat -c '%u:%h' "$path")" = 0:1 ] || fail active_release_context_invalid
done
preflight_root=$(mktemp -d /run/phoenix-autonomous-live-preflight.XXXXXX) ||
  fail active_release_context_invalid
trap 'rm -rf -- "$preflight_root"' EXIT HUP INT TERM
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
  /bin/sh "$current_validator" \
    --compose-file "$deploy_dir/compose.prod.yml" \
    --env-file "$env_file" \
    --release-env "$current_release_env" \
    --release-manifest "$current_manifest" \
    --current-release "$deploy_dir/current-release" \
    --release-state "$current_state" \
    --inspect-running \
    --rendered-output "$preflight_root/rendered.json" \
    --metadata-output "$preflight_root/metadata.json" \
    --output "$preflight_root/context.json" >/dev/null ||
  fail active_release_context_invalid

PHOENIX_RELEASE_ROOT="$release_root" \
PHOENIX_CONTEXT_INSTALLER="$libexec/install-production-release-context.sh" \
  /bin/sh "$libexec/install-release-assets.sh" \
    "$release_sha" "$archive" "$asset_manifest" "$checksums" ||
  fail release_install_failed
install -m 0640 -o root -g phoenix \
  "$release_manifest" "$deploy_dir/manifests/$release_sha.json"
install -m 0640 -o root -g phoenix \
  "$release_provenance" "$deploy_dir/manifests/$release_sha.provenance.json"

PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_RELEASE_ROOT="$release_root" \
PHOENIX_ENV_FILE="$env_file" \
  /bin/sh "$deploy_dir/deploy-release.sh" "$release_sha" ||
  fail deployment_failed
[ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$release_sha" ] ||
  fail active_release_mismatch

rm -rf -- "$preflight_root"
trap - EXIT HUP INT TERM
rm -f -- \
  "$archive" "$release_manifest" "$release_provenance" \
  "$rollback_manifest" "$rollback_provenance" "$asset_manifest" "$checksums"
rmdir "$stage"
printf '{"active_release":"%s","rollback_release":"%s","schema":"phoenix.autonomous-live-deploy-evidence.v1","status":"ok"}\n' \
  "$release_sha" "$rollback_sha"
