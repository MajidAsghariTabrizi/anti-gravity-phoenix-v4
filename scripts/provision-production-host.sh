#!/usr/bin/env sh
set -eu
umask 027

deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
owner_user=${PHOENIX_OWNER_USER:-phoenix}
owner_group=${PHOENIX_OWNER_GROUP:-phoenix}
postgres_dir=$deploy_root/data/postgres

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
for command_name in find install stat; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "required command is unavailable: $command_name"
done

ensure_directory() {
  provision_path=$1
  provision_mode=$2
  if [ -L "$provision_path" ]; then
    fail "provisioning path must not be a symlink: $provision_path"
  fi
  if [ -e "$provision_path" ]; then
    [ -d "$provision_path" ] ||
      fail "provisioning path is not a directory: $provision_path"
    return
  fi
  install -d -m "$provision_mode" -o "$owner_user" -g "$owner_group" \
    "$provision_path"
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

ensure_directory "$deploy_root" 0750
ensure_directory "$deploy_root/data" 0750
ensure_directory "$postgres_dir" 0750
ensure_directory "$deploy_root/data/prometheus" 0750
ensure_directory "$deploy_root/data/feed" 0750
ensure_directory "$deploy_root/logs" 0750
ensure_directory "$deploy_root/evidence" 0755
ensure_directory "$deploy_root/evidence/dashboard" 0755
validate_existing_postgres

echo "PRODUCTION_PROVISION_OK: existing persistent directories were not modified"
