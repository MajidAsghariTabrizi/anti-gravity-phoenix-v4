#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
libexec=/usr/local/libexec/phoenix-autonomous-live-deploy
gateway=/usr/local/sbin/phoenix-autonomous-live-deploy-gateway
sudoers=/etc/sudoers.d/phoenix-autonomous-live-deploy

fail() {
  echo "AUTONOMOUS_LIVE_GATEWAY_INSTALL_FAILED: $1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail root_required
[ "$(uname -s)" = Linux ] || fail linux_required
id phoenix >/dev/null 2>&1 || fail phoenix_user_missing
for command in awk chown install mktemp mv sha256sum stat visudo; do
  command -v "$command" >/dev/null 2>&1 || fail "command_missing_$command"
done
install -d -m 0750 -o root -g root "$libexec"
install -d -m 0755 -o root -g root /usr/local/sbin
install -d -m 0750 -o root -g root /etc/sudoers.d
install -m 0755 -o root -g root \
  "$script_dir/phoenix-autonomous-live-deploy-gateway.sh" "$gateway"
for specification in \
  'release_assets.py:0600' \
  'release_components.py:0600' \
  'release_provenance.py:0600' \
  'install-release-assets.sh:0700' \
  'install-production-release-context.sh:0700' \
  'production-healthcheck.sh:0700' \
  'production_mode.py:0700' \
  'rollback-release.sh:0700'
do
  name=${specification%%:*}
  mode=${specification##*:}
  install -m "$mode" -o root -g root "$script_dir/$name" "$libexec/$name"
done
install -m 0600 -o root -g root \
  "$script_dir/../release-components.json" "$libexec/release-components.json"
candidate=$(mktemp /etc/sudoers.d/.phoenix-autonomous-live.XXXXXX)
trap 'rm -f "$candidate"' EXIT HUP INT TERM
digest=$(sha256sum "$gateway" | awk '{print $1}')
printf '%s\n' \
  "phoenix ALL=(root) NOPASSWD: sha256:$digest $gateway" >"$candidate"
chown root:root "$candidate"
chmod 0440 "$candidate"
visudo -cf "$candidate" >/dev/null || fail sudoers_invalid
mv "$candidate" "$sudoers"
trap - EXIT HUP INT TERM
echo "AUTONOMOUS_LIVE_GATEWAY_INSTALL_OK"
