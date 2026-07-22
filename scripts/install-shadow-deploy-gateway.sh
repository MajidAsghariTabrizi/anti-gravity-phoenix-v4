#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH PYTHONDONTWRITEBYTECODE=1

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
test_root=${PHOENIX_GATEWAY_TEST_ROOT:-}

fail() {
  echo "SHADOW_DEPLOY_GATEWAY_INSTALL_FAILED: $1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
[ "$(uname -s)" = Linux ] || fail 'Linux is required'

if [ -n "$test_root" ]; then
  [ "${PHOENIX_GATEWAY_TESTING:-}" = 1 ] || fail 'test root requires explicit test mode'
  case "$test_root" in /*) ;; *) fail 'test root must be absolute' ;; esac
  [ -d "$test_root" ] && [ ! -L "$test_root" ] || fail 'test root is unsafe'
else
  test_root=
fi

libexec=$test_root/usr/local/libexec/phoenix-shadow-deploy
sbin=$test_root/usr/local/sbin
sudoers_dir=$test_root/etc/sudoers.d
gateway=$sbin/phoenix-shadow-deploy-gateway
sudoers=$sudoers_dir/phoenix-shadow-deploy

for command_name in awk cmp install mktemp mv sha256sum stat visudo; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "required command is unavailable: $command_name"
done

ensure_directory() {
  path=$1
  mode=$2
  if [ -L "$path" ]; then
    fail 'installation directory must not be a symlink'
  fi
  if [ -e "$path" ] && [ ! -d "$path" ]; then
    fail 'installation directory is not a directory'
  fi
  install -d -m "$mode" -o root -g root "$path"
  [ "$(stat -c '%U:%G:%a' "$path")" = "root:root:${mode#0}" ] ||
    fail 'installation directory ownership or mode is invalid'
}

atomic_install() {
  source=$1
  target=$2
  mode=$3
  [ -f "$source" ] && [ ! -L "$source" ] || fail 'trusted source is missing or unsafe'
  [ "$(stat -c '%h' "$source")" -eq 1 ] || fail 'trusted source has multiple hard links'
  if [ -e "$target" ] || [ -L "$target" ]; then
    [ -f "$target" ] && [ ! -L "$target" ] || fail 'installed target is unsafe'
    [ "$(stat -c '%h' "$target")" -eq 1 ] || fail 'installed target has multiple hard links'
  fi
  parent=$(dirname -- "$target")
  temporary=$(mktemp "$parent/.phoenix-shadow-deploy.XXXXXX") ||
    fail 'atomic install staging failed'
  trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
  install -m "$mode" -o root -g root "$source" "$temporary"
  [ "$(stat -c '%U:%G:%a:%h' "$temporary")" = "root:root:${mode#0}:1" ] ||
    fail 'staged target ownership or mode is invalid'
  mv -f -- "$temporary" "$target"
  trap - EXIT HUP INT TERM
}

ensure_directory "$test_root/usr/local" 0755
ensure_directory "$test_root/usr/local/libexec" 0755
ensure_directory "$libexec" 0750
ensure_directory "$sbin" 0755
ensure_directory "$test_root/etc" 0755
ensure_directory "$sudoers_dir" 0750

atomic_install "$script_dir/phoenix-shadow-deploy-gateway.sh" "$gateway" 0755
for specification in \
  'phoenix_shadow_deploy.py:0700' \
  'release_assets.py:0600' \
  'release_provenance.py:0600' \
  'install-release-assets.sh:0700' \
  'install-production-release-context.sh:0700' \
  'prelive-protected-maintenance.sh:0700' \
  'prelive_protected_maintenance.py:0600' \
  'prelive-protected-maintenance-launch.sh:0700' \
  'prelive-protected-maintenance-unit.sh:0700' \
  'rollback-release.sh:0700'
do
  name=${specification%%:*}
  mode=${specification##*:}
  atomic_install "$script_dir/$name" "$libexec/$name" "$mode"
done

sudoers_candidate=$(mktemp "$sudoers_dir/.phoenix-shadow-deploy.XXXXXX") ||
  fail 'sudoers staging failed'
trap 'rm -f -- "$sudoers_candidate"' EXIT HUP INT TERM
gateway_digest=$(sha256sum "$gateway" | awk '{print $1}')
printf '%s\n' \
  "phoenix ALL=(root) NOPASSWD: sha256:$gateway_digest /usr/local/sbin/phoenix-shadow-deploy-gateway" \
  >"$sudoers_candidate"
chown root:root "$sudoers_candidate"
chmod 0440 "$sudoers_candidate"
visudo -cf "$sudoers_candidate" >/dev/null || fail 'sudoers validation failed'
if [ -e "$sudoers" ] || [ -L "$sudoers" ]; then
  [ -f "$sudoers" ] && [ ! -L "$sudoers" ] || fail 'installed sudoers target is unsafe'
  [ "$(stat -c '%h' "$sudoers")" -eq 1 ] || fail 'installed sudoers target has multiple hard links'
fi
mv -f -- "$sudoers_candidate" "$sudoers"
trap - EXIT HUP INT TERM

if [ -z "$test_root" ]; then
  /usr/bin/python3 -I -B "$libexec/phoenix_shadow_deploy.py" harden-context ||
    fail 'canonical deploy context hardening failed'
  /usr/bin/python3 -I -B - "$gateway" "$libexec" <<'PY' ||
import sys
from pathlib import Path

sys.path.insert(0, sys.argv[2])
import phoenix_shadow_deploy

phoenix_shadow_deploy.verify_installation(Path(sys.argv[1]), Path(sys.argv[2]))
PY
    fail 'installed gateway verification failed'
fi

echo 'SHADOW_DEPLOY_GATEWAY_INSTALL_OK: constrained gateway installed'
