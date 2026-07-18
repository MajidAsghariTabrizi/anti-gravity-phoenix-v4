#!/usr/bin/env sh
set -eu
umask 027

release_sha=${1:-}
source_root=${2:-}
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
owner_user=${PHOENIX_OWNER_USER:-phoenix}
owner_group=${PHOENIX_OWNER_GROUP:-phoenix}
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

fail() {
  echo "RELEASE_CONTEXT_INSTALL_FAILED: $1" >&2
  exit 1
}

if [ -n "$release_sha" ]; then
  case "$release_sha" in
    *[!0-9a-f]*) fail 'release SHA must be 40 lowercase hex characters' ;;
  esac
  [ "${#release_sha}" -eq 40 ] ||
    fail 'release SHA must be 40 lowercase hex characters'
fi

[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
[ "$(uname -s)" = Linux ] || fail 'Linux is required'
case "$deploy_root:$env_file" in
  /*:/*) ;;
  *) fail 'deployment root and environment file must be absolute' ;;
esac
id "$owner_user" >/dev/null 2>&1 || fail 'production owner user is unavailable'
for command_name in chown chmod docker install mktemp readlink stat; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "required command is unavailable: $command_name"
done
[ -d "$deploy_root" ] && [ ! -L "$deploy_root" ] ||
  fail 'deployment root is missing or unsafe'

if [ -z "$source_root" ]; then
  source_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
else
  [ -d "$source_root" ] && [ ! -L "$source_root" ] ||
    fail 'release source root is unavailable'
  source_root=$(readlink -f "$source_root") ||
    fail 'release source root is invalid'
fi

[ -f "$env_file" ] && [ ! -L "$env_file" ] ||
  fail 'production environment file is missing or unsafe'
env_owner=$(stat -c '%U:%G' "$env_file")
env_mode=$(stat -c '%a' "$env_file")
[ "$env_owner" = root:root ] ||
  fail 'production environment file must be root:root'
[ "$env_mode" = 600 ] || fail 'production environment file must be mode 600'

install_source() {
  source_path=$1
  target_path=$2
  target_mode=$3
  [ -f "$source_path" ] && [ ! -L "$source_path" ] ||
    fail "release context source is missing or unsafe: $source_path"
  case "$target_path" in
    "$deploy_dir"/*) ;;
    *) fail "release context target escapes deployment directory: $target_path" ;;
  esac
  if [ -L "$target_path" ]; then
    fail "release context target must not be a symlink: $target_path"
  fi
  if [ -e "$target_path" ]; then
    [ -f "$target_path" ] ||
      fail "release context target is not a regular file: $target_path"
    [ "$(stat -c '%h' "$target_path")" -eq 1 ] ||
      fail "release context target has multiple hard links: $target_path"
  fi
  if [ -e "$target_path" ] &&
    [ "$(readlink -f "$source_path")" = "$(readlink -f "$target_path")" ]
  then
    return
  fi
  install -m "$target_mode" -o "$owner_user" -g "$owner_group" \
    "$source_path" "$target_path"
}

ensure_deploy_directory() {
  context_path=$1
  if [ -L "$context_path" ]; then
    fail "release context directory must not be a symlink: $context_path"
  fi
  if [ -e "$context_path" ]; then
    [ -d "$context_path" ] ||
      fail "release context path is not a directory: $context_path"
  fi
  install -d -m 0750 -o "$owner_user" -g "$owner_group" "$context_path"
}

for context_directory in \
  "$deploy_dir" \
  "$deploy_dir/manifests" \
  "$deploy_dir/.runtime" \
  "$deploy_dir/prometheus" \
  "$deploy_dir/sql" \
  "$deploy_dir/schemas" \
  "$deploy_dir/routes" \
  "$deploy_dir/contracts"
do
  ensure_deploy_directory "$context_directory"
done

install_source "$source_root/compose.prod.yml" "$deploy_dir/compose.prod.yml" 0640
install_source \
  "$source_root/deploy/nats-server.conf" "$deploy_dir/nats-server.conf" 0644
install_source \
  "$source_root/prometheus/prometheus.yml" \
  "$deploy_dir/prometheus/prometheus.yml" 0644
install_source \
  "$source_root/dashboard/snapshot_model.py" "$deploy_dir/snapshot_model.py" 0640
install_source \
  "$source_root/fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json" \
  "$deploy_dir/routes/arbitrum_uniswap_v3_pool_proofs.json" 0640

for sql_name in \
  shadow-profitability-report.sql \
  shadow-route-discovery-enrichment.sql \
  prelive-money-path-report.sql \
  prelive-dashboard-source.sql
do
  install_source \
    "$source_root/scripts/sql/$sql_name" "$deploy_dir/sql/$sql_name" 0640
done

for schema_name in \
  prelive-money-path-summary.schema.json \
  prelive-shadow-control-evidence.schema.json \
  phoenix-release-assets.schema.json
do
  install_source \
    "$source_root/schemas/$schema_name" "$deploy_dir/schemas/$schema_name" 0640
done

for script_name in \
  production_context.py \
  render-production-compose.sh \
  verify-compose-route-registry.py \
  validate-production-release-context.sh \
  validate-production-env.sh \
  production-healthcheck.sh \
  shadow-engine-isolated-canary.sh \
  shadow-positive-route-evidence.sh \
  shadow-profitability-report.sh \
  shadow_profitability_report.py \
  shadow-route-discovery.sh \
  shadow_route_discovery.py \
  prelive-money-path-report.sh \
  prelive_money_path_report.py \
  prelive_dashboard_snapshot.py \
  prelive_dashboard_live.py \
  prelive_shadow_control.py \
  prelive-shadow-control.sh \
  release_assets.py \
  verify_dashboard_compose.py \
  deploy-release.sh
do
  install_source \
    "$source_root/scripts/$script_name" "$deploy_dir/$script_name" 0750
done

# These reviewed safety scripts may be newer than an immutable rollback tree.
for safety_script in \
  install-release-assets.sh \
  install-production-release-context.sh \
  prelive-protected-maintenance.sh \
  prelive_protected_maintenance.py \
  prelive-protected-maintenance-launch.sh \
  prelive-protected-maintenance-unit.sh \
  rollback-release.sh
do
  install_source "$script_dir/$safety_script" "$deploy_dir/$safety_script" 0750
done

if [ -n "$release_sha" ]; then
  install_source \
    "$source_root/release-assets-manifest.json" \
    "$deploy_dir/release-assets-manifest.json" 0640
  install_source \
    "$source_root/contracts/PhoenixExecutor.compiled.json" \
    "$deploy_dir/contracts/PhoenixExecutor.compiled.json" 0640
fi

"$deploy_dir/validate-production-env.sh" "$env_file"
docker version >/dev/null
docker compose version >/dev/null

if [ -n "$release_sha" ]; then
  marker=$(mktemp "$deploy_dir/.release-assets.XXXXXX") ||
    fail 'release marker staging failed'
  printf '%s\n' "$release_sha" >"$marker"
  chown "$owner_user:$owner_group" "$marker"
  chmod 0640 "$marker"
  mv "$marker" "$deploy_dir/release-assets.sha"
fi

echo "RELEASE_CONTEXT_INSTALL_OK: canonical deploy context updated without persistent-data mutation"
