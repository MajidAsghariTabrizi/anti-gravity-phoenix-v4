#!/bin/sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH PYTHONDONTWRITEBYTECODE=1

helper=/usr/local/libexec/phoenix-shadow-deploy/phoenix_shadow_deploy.py
libexec=/usr/local/libexec/phoenix-shadow-deploy
gateway=/usr/local/sbin/phoenix-shadow-deploy-gateway

if [ "$(id -u)" -ne 0 ]; then
  echo 'PHOENIX_SHADOW_DEPLOY_GATEWAY_ERROR: root_required' >&2
  exit 1
fi

if [ ! -d "$libexec" ] || [ -L "$libexec" ] ||
  [ "$(stat -c '%U:%G:%a' "$libexec" 2>/dev/null || true)" != root:root:750 ] ||
  [ -L "$gateway" ] ||
  [ "$(stat -c '%U:%G:%a:%h' "$gateway" 2>/dev/null || true)" != root:root:755:1 ]
then
  echo 'PHOENIX_SHADOW_DEPLOY_GATEWAY_ERROR: trusted_installation_invalid' >&2
  exit 1
fi

for specification in \
  'phoenix_shadow_deploy.py:700' \
  'release_assets.py:600' \
  'release-components.json:600' \
  'release_components.py:600' \
  'release_provenance.py:600' \
  'install-release-assets.sh:700' \
  'install-production-release-context.sh:700' \
  'production_mode.py:600' \
  'production-healthcheck.sh:700' \
  'prelive-protected-maintenance.sh:700' \
  'prelive_protected_maintenance.py:600' \
  'prelive-protected-maintenance-launch.sh:700' \
  'prelive-protected-maintenance-unit.sh:700' \
  'rollback-release.sh:700'
do
  name=${specification%%:*}
  mode=${specification##*:}
  path=$libexec/$name
  if [ ! -f "$path" ] || [ -L "$path" ] ||
    [ "$(stat -c '%U:%G:%a:%h' "$path" 2>/dev/null || true)" != "root:root:$mode:1" ]
  then
    echo 'PHOENIX_SHADOW_DEPLOY_GATEWAY_ERROR: trusted_installation_invalid' >&2
    exit 1
  fi
done

if [ ! -f "$helper" ] || [ -L "$helper" ]; then
  echo 'PHOENIX_SHADOW_DEPLOY_GATEWAY_ERROR: trusted_helper_unavailable' >&2
  exit 1
fi

exec /usr/bin/env -i \
  HOME=/root \
  LANG=C \
  LC_ALL=C \
  PATH="$PATH" \
  PYTHONDONTWRITEBYTECODE=1 \
  /usr/bin/python3 -I -B "$helper" gateway "$@"
