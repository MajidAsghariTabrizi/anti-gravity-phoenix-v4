#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
workflow=$repo_root/.github/workflows/deploy-shadow.yml
gateway=$script_dir/phoenix-shadow-deploy-gateway.sh
helper=$script_dir/phoenix_shadow_deploy.py
installer=$script_dir/install-shadow-deploy-gateway.sh

fail() {
  echo "shadow-deploy-gateway-tests: $1" >&2
  exit 1
}

sudoers_prefix='phoenix ALL=(root) NOPASSWD: sha256:'
sudoers_suffix=' /usr/local/sbin/phoenix-shadow-deploy-gateway'
grep -F '"phoenix ALL=(root) NOPASSWD: sha256:$gateway_digest /usr/local/sbin/phoenix-shadow-deploy-gateway"' \
  "$installer" >/dev/null ||
  fail 'exact gateway-only sudoers contract is missing'
sudoers_line=$(grep -F 'NOPASSWD: sha256:$gateway_digest' "$installer")
case "$sudoers_line" in
  *'/bin/sh'*|*'/bin/bash'*|*'SETENV'*|*' * '*|*'/tmp/'*|*'/opt/phoenix/deploy/'*)
    fail 'sudoers contract grants a broad or mutable command'
    ;;
esac
grep -F 'visudo -cf "$sudoers_candidate"' "$installer" >/dev/null ||
  fail 'sudoers candidate is not validated before installation'

for token in \
  'root:root' \
  'stat -c '\''%h'\''' \
  'mv -f -- "$temporary" "$target"' \
  '/usr/local/libexec/phoenix-shadow-deploy' \
  '/usr/local/sbin/phoenix-shadow-deploy-gateway'
do
  grep -F "$token" "$installer" >/dev/null ||
    fail "installer safety contract is missing: $token"
done

grep -F 'helper=/usr/local/libexec/phoenix-shadow-deploy/phoenix_shadow_deploy.py' \
  "$gateway" >/dev/null || fail 'gateway does not use the fixed trusted helper'
grep -F '#!/bin/sh' "$gateway" >/dev/null ||
  fail 'privileged gateway interpreter is not pinned'
grep -F 'exec /usr/bin/env -i' "$gateway" >/dev/null ||
  fail 'gateway does not clear inherited execution environment'
grep -F '/usr/bin/python3 -I -B "$helper" gateway "$@"' "$gateway" >/dev/null ||
  fail 'gateway entrypoint is not isolated and bounded to the installed helper'
grep -F 'release_provenance.validate_deploy_pair(' "$helper" >/dev/null ||
  fail 'gateway does not invoke the canonical deploy-pair validator'
if grep -E 'EXPECTED_IMAGES|feed-ingestor|(^|[^-])recorder([^a-z-]|$)' "$helper" >/dev/null; then
  fail 'gateway duplicates release-component definitions'
fi
grep -F '"/usr/bin/systemd-run"' "$helper" >/dev/null ||
  fail 'gateway does not use the bounded systemd launch path'
grep -F '"--no-block"' "$helper" >/dev/null ||
  fail 'systemd launch can deadlock while the start-side lock is held'
grep -F '"--property=Type=oneshot"' "$helper" >/dev/null ||
  fail 'systemd deployment unit is not Type=oneshot'
grep -F '"--property=RemainAfterExit=yes"' "$helper" >/dev/null ||
  fail 'systemd result evidence is not retained'
grep -F 'SYSTEMD_TIMEOUT_SECONDS = 2400' "$helper" >/dev/null ||
  fail 'systemd deployment timeout is not bounded'
grep -F 'validate_immutable_tree(candidate_tree' "$helper" >/dev/null ||
  fail 'candidate immutable tree is not revalidated before deployment'
grep -F 'str(LIBEXEC_DIR / "rollback-release.sh")' "$helper" >/dev/null ||
  fail 'rollback is not anchored to the installed root helper'

if grep -F 'sudo /bin/sh' "$workflow" >/dev/null; then
  fail 'deploy workflow retains a privileged shell'
fi
if grep -E 'sudo(-n)?[[:space:]]+(env|cp|install|docker|systemctl|systemd-run)' \
  "$workflow" >/dev/null
then
  fail 'deploy workflow grants a privileged utility directly'
fi
sudo_count=$(grep -c 'sudo -n "$gateway"' "$workflow")
[ "$sudo_count" -eq 1 ] ||
  fail 'every privileged workflow call must use only the fixed gateway'
grep -F 'gateway_remote start "${gateway_args[@]}"' "$workflow" >/dev/null ||
  fail 'workflow does not start the bounded gateway operation'
if grep -E "scripts/.*\.(sh|py).*\$remote_stage|/tmp/.*\.(sh|py)" "$workflow" >/dev/null; then
  fail 'deploy workflow stages or executes mutable code'
fi
for staged_name in \
  release-manifest.json \
  release-provenance.json \
  rollback-manifest.json \
  rollback-provenance.json \
  release-assets-manifest.json \
  release-assets-checksums.txt \
  'phoenix-release-assets-${RELEASE_SHA}.tar.gz'
do
  grep -F "$staged_name" "$workflow" >/dev/null ||
    fail "required immutable stage input is missing: $staged_name"
done
grep -F 'remote_stage="/tmp/phoenix-shadow-deploy-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}-${RELEASE_SHA}"' \
  "$workflow" >/dev/null || fail 'workflow stage identity is not deterministic'
grep -F 'gateway_remote evidence "${gateway_args[@]}"' "$workflow" >/dev/null ||
  fail 'workflow does not retrieve sanitized gateway evidence'
grep -F 'gateway_remote cleanup "${gateway_args[@]}"' "$workflow" >/dev/null ||
  fail 'workflow does not request bounded successful-stage cleanup'

if [ "$(uname -s)" != Linux ] ||
  ! command -v sudo >/dev/null 2>&1 ||
  ! sudo -n true >/dev/null 2>&1 ||
  ! command -v visudo >/dev/null 2>&1
then
  echo 'shadow-deploy-gateway-tests: installer fixture skipped (Linux passwordless sudo and visudo required)'
  echo 'shadow-deploy-gateway-tests: ok'
  exit 0
fi

fixture=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-shadow-gateway.XXXXXX")
cleanup() {
  sudo rm -rf -- "$fixture"
}
trap cleanup EXIT HUP INT TERM
sudo env \
  PHOENIX_GATEWAY_TESTING=1 \
  PHOENIX_GATEWAY_TEST_ROOT="$fixture" \
  /bin/sh "$installer" >/dev/null
sudo env \
  PHOENIX_GATEWAY_TESTING=1 \
  PHOENIX_GATEWAY_TEST_ROOT="$fixture" \
  /bin/sh "$installer" >/dev/null

installed_gateway=$fixture/usr/local/sbin/phoenix-shadow-deploy-gateway
installed_libexec=$fixture/usr/local/libexec/phoenix-shadow-deploy
installed_sudoers=$fixture/etc/sudoers.d/phoenix-shadow-deploy
[ "$(sudo stat -c '%U:%G:%a:%h' "$installed_gateway")" = root:root:755:1 ] ||
  fail 'installed gateway ownership, mode, or link count is invalid'
[ "$(sudo stat -c '%U:%G:%a' "$installed_libexec")" = root:root:750 ] ||
  fail 'installed libexec ownership or mode is invalid'
[ "$(sudo stat -c '%U:%G:%a:%h' "$installed_sudoers")" = root:root:440:1 ] ||
  fail 'installed sudoers ownership, mode, or link count is invalid'
gateway_digest=$(sudo sha256sum "$installed_gateway" | awk '{print $1}')
expected_sudoers="${sudoers_prefix}${gateway_digest}${sudoers_suffix}"
[ "$(sudo cat "$installed_sudoers")" = "$expected_sudoers" ] ||
  fail 'installed sudoers content differs from the exact contract'
sudo visudo -cf "$installed_sudoers" >/dev/null ||
  fail 'installed sudoers fixture is invalid'

echo 'shadow-deploy-gateway-tests: ok'
