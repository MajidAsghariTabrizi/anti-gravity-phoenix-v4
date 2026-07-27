#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
transport=$repo_root/scripts/phoenix-release-transport.sh
gateway=$repo_root/scripts/phoenix-release-gateway.sh
installer=$repo_root/scripts/install-phoenix-release-platform.sh
finalizer=$repo_root/scripts/finalize-phoenix-deploy-bootstrap.sh

fail() {
  printf 'PHOENIX_RELEASE_PLATFORM_TEST_FAILED: %s\n' "$1" >&2
  exit 1
}

for command in \
  'sh -c id' \
  'status;id' \
  'receive ../escape' \
  'resume aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaZ' \
  'history extra'
do
  output=$(
    SSH_ORIGINAL_COMMAND=$command /bin/sh "$transport" 2>&1 || true
  )
  printf '%s\n' "$output" | grep -F '"code":"COMMAND_REJECTED"' >/dev/null ||
    fail "arbitrary_command_not_rejected"
done

grep -F 'NOPASSWD: ALL' "$installer" >/dev/null 2>&1 &&
  fail unrestricted_sudo_present
grep -F 'AllowUsers phoenix phoenix-deploy' "$finalizer" >/dev/null ||
  fail final_allow_users_missing
grep -F 'userdel --remove "$bootstrap_user"' "$finalizer" >/dev/null ||
  fail bootstrap_user_removal_missing
grep -F 'restrict,command="%s"' "$installer" >/dev/null ||
  fail forced_command_missing
for setting in \
  'AllowAgentForwarding no' \
  'AllowTcpForwarding no' \
  'X11Forwarding no' \
  'PermitTTY no' \
  'PermitTunnel no' \
  'PermitUserRC no'
do
  grep -F "$setting" "$installer" >/dev/null ||
    fail "sshd_restriction_missing"
done

gateway_output=$(/bin/sh "$gateway" status 2>&1 || true)
if [ "$(id -u)" -ne 0 ]; then
  printf '%s\n' "$gateway_output" |
    grep -F '"code":"GATEWAY_WRAPPER_FAILED"' >/dev/null ||
    fail non_root_gateway_not_rejected
fi

/usr/bin/python3 -I -B \
  "$repo_root/scripts/phoenix_release/cli.py" --help >/dev/null ||
  fail isolated_python_entrypoint_failed

printf '%s\n' PHOENIX_RELEASE_PLATFORM_TEST_OK
