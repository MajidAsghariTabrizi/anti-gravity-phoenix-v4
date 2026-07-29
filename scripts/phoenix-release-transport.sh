#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

deny() {
  printf '%s\n' \
    '{"status":"error","phase":"TRANSPORT","code":"COMMAND_REJECTED","evidence":{}}' >&2
  exit 1
}

[ "$(id -u)" -ne 0 ] || deny
[ "$(uname -s)" = Linux ] || deny
original=${SSH_ORIGINAL_COMMAND:-}
[ -n "$original" ] && [ "${#original}" -le 128 ] || deny
case "$original" in
  *[!a-z0-9\ -]*) deny ;;
esac

# Word splitting is intentional after the complete character allowlist above.
# shellcheck disable=SC2086
set -- $original
command_name=${1:-}
case "$command_name:$#" in
  status:1|history:1|emergency-pause:1|reconcile-active-context:1)
    ;;
  receive:2|plan:2|readiness:2|resume:2|retry-pre-mutation:2|retry-rolled-back:2|rollback:2|evidence:2|reconcile-chain-evidence:2)
    case "${2:-}" in
      *[!0-9a-f]*|"") deny ;;
    esac
    [ "${#2}" -eq 40 ] || deny
    ;;
  *)
    deny
    ;;
esac

exec sudo -n /usr/local/sbin/phoenix-release-gateway "$@"
