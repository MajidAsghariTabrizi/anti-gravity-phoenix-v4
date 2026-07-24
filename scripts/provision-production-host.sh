#!/usr/bin/env sh
set -eu
umask 027

deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
owner_user=${PHOENIX_OWNER_USER:-phoenix}
owner_group=${PHOENIX_OWNER_GROUP:-phoenix}
postgres_dir=$deploy_root/data/postgres
prometheus_dir=$deploy_root/data/prometheus
prometheus_runtime_uid=65534
prometheus_runtime_gid=65534

fail() {
  echo "PRODUCTION_PROVISION_FAILED: $1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
[ "$(uname -s)" = Linux ] || fail 'Linux is required'
case "$deploy_root" in
  /*) ;;
  *) fail 'deployment root must be absolute' ;;
esac
id "$owner_user" >/dev/null 2>&1 || fail 'production owner user is unavailable'
for command_name in chown chmod find install stat; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "required command is unavailable: $command_name"
done

ensure_owned_directory() {
  provision_path=$1
  provision_mode=$2
  if [ -L "$provision_path" ]; then
    fail "provisioning path must not be a symlink: $provision_path"
  fi
  if [ -e "$provision_path" ]; then
    [ -d "$provision_path" ] ||
      fail "provisioning path is not a directory: $provision_path"
  fi
  install -d -m "$provision_mode" -o "$owner_user" -g "$owner_group" \
    "$provision_path" ||
    fail "directory ownership or mode could not be enforced: $provision_path"
  provision_metadata=$(stat -c '%U:%G:%a' "$provision_path") ||
    fail "directory metadata is unavailable: $provision_path"
  [ "$provision_metadata" = "$owner_user:$owner_group:${provision_mode#0}" ] ||
    fail "directory ownership or mode is invalid: $provision_path"
}

ensure_postgres_directory() {
  if [ -L "$postgres_dir" ]; then
    fail "provisioning path must not be a symlink: $postgres_dir"
  fi
  if [ -e "$postgres_dir" ]; then
    [ -d "$postgres_dir" ] ||
      fail "provisioning path is not a directory: $postgres_dir"
    first_entry=$(find "$postgres_dir" -mindepth 1 -maxdepth 1 -print -quit) ||
      fail 'PostgreSQL data directory inspection failed'
    [ -z "$first_entry" ] || return
  fi
  install -d -m 0750 -o "$owner_user" -g "$owner_group" "$postgres_dir" ||
    fail 'empty PostgreSQL data directory could not be provisioned'
}

provision_prometheus_directory() {
  if [ -L "$prometheus_dir" ]; then
    fail "Prometheus data path must not be a symlink: $prometheus_dir"
  fi
  if [ -e "$prometheus_dir" ]; then
    [ -d "$prometheus_dir" ] ||
      fail "Prometheus data path is not a directory: $prometheus_dir"
  else
    install -d -m 0750 \
      -o "$prometheus_runtime_uid" -g "$prometheus_runtime_gid" \
      "$prometheus_dir" ||
      fail 'Prometheus data directory could not be created'
  fi

  unsafe_entry=$(find "$prometheus_dir" -xdev -type l -print -quit)
  [ -z "$unsafe_entry" ] ||
    fail 'Prometheus data directory must not contain symlinks'
  unsafe_entry=$(
    find "$prometheus_dir" -xdev -type f -links +1 -print -quit
  )
  [ -z "$unsafe_entry" ] ||
    fail 'Prometheus data directory must not contain hard-linked files'
  unsafe_entry=$(
    find "$prometheus_dir" -xdev ! -type d ! -type f -print -quit
  )
  [ -z "$unsafe_entry" ] ||
    fail 'Prometheus data directory contains an unsupported file type'

  prometheus_device=$(stat -c '%d' "$prometheus_dir") ||
    fail 'Prometheus data device identity is unavailable'
  if ! find "$prometheus_dir" -xdev -mindepth 1 -exec sh -c '
    expected_device=$1
    shift
    for candidate; do
      [ "$(stat -c "%d" "$candidate")" = "$expected_device" ] || exit 1
    done
  ' sh "$prometheus_device" {} +
  then
    fail 'Prometheus data directory must not contain nested mounts'
  fi

  find "$prometheus_dir" -xdev \
    -exec chown "$prometheus_runtime_uid:$prometheus_runtime_gid" {} + ||
    fail 'Prometheus data ownership could not be applied'
  find "$prometheus_dir" -xdev -type d -exec chmod 0750 {} + ||
    fail 'Prometheus directory mode could not be applied'
  find "$prometheus_dir" -xdev -type f -exec chmod 0640 {} + ||
    fail 'Prometheus file mode could not be applied'

  unsafe_entry=$(
    find "$prometheus_dir" -xdev \
      \( ! -uid "$prometheus_runtime_uid" -o ! -gid "$prometheus_runtime_gid" \) \
      -print -quit
  )
  [ -z "$unsafe_entry" ] ||
    fail 'Prometheus data ownership could not be enforced'
  unsafe_entry=$(
    find "$prometheus_dir" -xdev -type d ! -perm 0750 -print -quit
  )
  [ -z "$unsafe_entry" ] ||
    fail 'Prometheus directory mode could not be enforced'
  unsafe_entry=$(
    find "$prometheus_dir" -xdev -type f ! -perm 0640 -print -quit
  )
  [ -z "$unsafe_entry" ] ||
    fail 'Prometheus file mode could not be enforced'
}

validate_existing_postgres() {
  first_entry=$(find "$postgres_dir" -mindepth 1 -maxdepth 1 -print -quit)
  [ -n "$first_entry" ] || return

  [ -f "$postgres_dir/PG_VERSION" ] && [ ! -L "$postgres_dir/PG_VERSION" ] ||
    fail 'non-empty PostgreSQL data directory is missing a regular PG_VERSION'
  postgres_owner=$(stat -c '%u:%g' "$postgres_dir") ||
    fail 'PostgreSQL directory ownership is unavailable'
  marker_owner=$(stat -c '%u:%g' "$postgres_dir/PG_VERSION") ||
    fail 'PG_VERSION ownership is unavailable'
  [ "$postgres_owner" = "$marker_owner" ] ||
    fail 'PostgreSQL directory owner does not match PG_VERSION'
  case "$postgres_owner" in
    0:*|*:0) fail 'non-empty PostgreSQL data must not be owned by root' ;;
  esac
  if [ "$owner_user" = phoenix ]; then
    phoenix_identity=$(id -u "$owner_user"):$(id -g "$owner_user")
    [ "$postgres_owner" != "$phoenix_identity" ] ||
      fail 'non-empty PostgreSQL data must not be owned by the Phoenix service user'
  fi

  postgres_mode=$(stat -c '%a' "$postgres_dir") ||
    fail 'PostgreSQL directory mode is unavailable'
  case "$postgres_mode" in
    [0-7][0-7][0-7]) ;;
    *) fail 'PostgreSQL directory mode is not a safe three-digit mode' ;;
  esac
  case "$postgres_mode" in
    ?[2367]?|??[2367])
      fail 'PostgreSQL directory must not be group- or world-writable'
      ;;
  esac

  for critical_path in \
    PG_VERSION \
    global/pg_control \
    global/pg_filenode.map \
    postmaster.pid
  do
    candidate=$postgres_dir/$critical_path
    [ -e "$candidate" ] || continue
    [ ! -L "$candidate" ] ||
      fail "PostgreSQL critical path must not be a symlink: $critical_path"
    candidate_owner=$(stat -c '%u:%g' "$candidate") ||
      fail "PostgreSQL critical path ownership is unavailable: $critical_path"
    [ "$candidate_owner" = "$postgres_owner" ] ||
      fail "PostgreSQL critical path owner differs: $critical_path"
  done
}

ensure_owned_directory "$deploy_root" 0750
ensure_owned_directory "$deploy_root/data" 0750
ensure_postgres_directory
ensure_owned_directory "$deploy_root/data/feed" 0750
ensure_owned_directory "$deploy_root/logs" 0750
ensure_owned_directory "$deploy_root/evidence" 0755
ensure_owned_directory "$deploy_root/evidence/dashboard" 0755
validate_existing_postgres
provision_prometheus_directory

echo "PRODUCTION_PROVISION_OK: protected data preserved; Prometheus runtime ownership enforced"
