#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH PYTHONDONTWRITEBYTECODE=1

fail() {
  printf '%s\n' \
    '{"status":"error","phase":"GATEWAY","code":"GATEWAY_WRAPPER_FAILED","evidence":{}}' >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail
[ "$(uname -s)" = Linux ] || fail
[ "$#" -ge 1 ] && [ "$#" -le 3 ] || fail
command -v flock >/dev/null 2>&1 || fail
[ -f /usr/local/libexec/phoenix-release/phoenix_release/cli.py ] || fail

if [ "$1" = reconcile-chain-evidence ]; then
  exec /usr/bin/flock -n /run/lock/phoenix-release.lock \
    /usr/bin/env PHOENIX_RELEASE_LOCK_HELD=1 \
    /usr/bin/python3 -I -B \
    /usr/local/libexec/phoenix-release/phoenix_release/cli.py "$@"
fi

if [ "$1" = enter-post-recovery-live-mode ]; then
  exec /usr/bin/flock -n /run/lock/phoenix-release.lock \
    /usr/bin/flock -n /run/lock/phoenix-economic-activation.lock \
    /usr/bin/python3 -I -B \
    /usr/local/libexec/phoenix-release/phoenix_release/cli.py "$@"
fi

exec /usr/bin/flock -w 30 /run/lock/phoenix-release.lock \
  /usr/bin/python3 -I -B \
  /usr/local/libexec/phoenix-release/phoenix_release/cli.py "$@"
