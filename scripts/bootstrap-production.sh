#!/usr/bin/env sh
set -eu

release_sha=${1:-}
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
owner_user=${PHOENIX_OWNER_USER:-phoenix}
owner_group=${PHOENIX_OWNER_GROUP:-phoenix}

fail() {
  echo "BOOTSTRAP_FAILED: $1" >&2
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

arch=$(uname -m)
case "$arch" in
  x86_64|amd64) ;;
  *) fail "unsupported architecture: $arch" ;;
esac

if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  [ "${ID:-}" = ubuntu ] ||
    echo "WARNING: Ubuntu 24.04 LTS is the supported target"
  [ "${VERSION_ID:-}" = 24.04 ] ||
    echo "WARNING: Ubuntu 24.04 LTS is the supported target"
fi

if ! command -v docker >/dev/null 2>&1 ||
  ! docker compose version >/dev/null 2>&1
then
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io docker-compose-v2
fi

if ! id "$owner_user" >/dev/null 2>&1; then
  [ "$owner_user:$owner_group" = phoenix:phoenix ] ||
    fail 'custom production owner must already exist'
  useradd \
    --system \
    --home-dir "$deploy_root" \
    --create-home \
    --shell /usr/sbin/nologin \
    phoenix
fi

case "$deploy_root:$env_file" in
  /*:/*) ;;
  *) fail 'deployment root and environment file must be absolute' ;;
esac

env_dir=$(dirname -- "$env_file")
if [ -L "$env_dir" ]; then
  fail 'production environment directory must not be a symlink'
elif [ -e "$env_dir" ]; then
  [ -d "$env_dir" ] || fail 'production environment parent is not a directory'
else
  install -d -m 0750 -o root -g root "$env_dir"
fi
[ -f "$env_file" ] ||
  fail "create $env_file as root:root 0600 before starting production"

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
provisioner=$script_dir/provision-production-host.sh
context_installer=$script_dir/install-production-release-context.sh
[ -f "$provisioner" ] && [ ! -L "$provisioner" ] ||
  fail 'first-host provisioner is unavailable'
[ -f "$context_installer" ] && [ ! -L "$context_installer" ] ||
  fail 'release-context installer is unavailable'

if ! PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner"
then
  fail 'host provisioning stage failed'
fi

if ! PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" "$release_sha" "$repo_root"
then
  fail 'release context installation stage failed'
fi

echo "BOOTSTRAP_OK: first-host provisioning and release-context installation completed"
echo "FIREWALL_EXPECTATION: expose SSH only as intended; dashboard and Prometheus bind to 127.0.0.1"
echo "SHADOW_DEFAULT: PHOENIX_MODE=SHADOW and LIVE_EXECUTION=false are required"
