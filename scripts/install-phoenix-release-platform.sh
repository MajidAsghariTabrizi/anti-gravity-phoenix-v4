#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
source_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
public_key_file=
release_sha=
reuse_existing_key=0
deploy_user=phoenix-deploy
libexec=/usr/local/libexec/phoenix-release
gateway=/usr/local/sbin/phoenix-release-gateway
transport=/usr/local/sbin/phoenix-release-transport
sudoers=/etc/sudoers.d/phoenix-release
authorized_keys=/home/$deploy_user/.ssh/authorized_keys
sshd_dropin=/etc/ssh/sshd_config.d/89-phoenix-deploy.conf

fail() {
  printf 'PHOENIX_RELEASE_PLATFORM_INSTALL_FAILED: %s\n' "$1" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --release-sha)
      [ "$#" -ge 2 ] || fail release_sha_missing
      release_sha=$2
      shift 2
      ;;
    --reuse-existing-key)
      reuse_existing_key=1
      shift
      ;;
    --public-key-file)
      [ "$#" -ge 2 ] || fail public_key_file_missing
      public_key_file=$2
      shift 2
      ;;
    -*)
      fail argument_invalid
      ;;
    *)
      # Preserve the original one-positional-key-file installation contract.
      [ -z "$public_key_file" ] || fail argument_invalid
      public_key_file=$1
      shift
      ;;
  esac
done

[ "$(id -u)" -eq 0 ] || fail root_required
[ "$(uname -s)" = Linux ] || fail linux_required
case "$release_sha" in
  *[!0-9a-f]*|"") fail release_sha_invalid ;;
esac
[ "${#release_sha}" -eq 40 ] || fail release_sha_invalid
[ "$reuse_existing_key" -eq 0 ] || [ -z "$public_key_file" ] ||
  fail key_source_ambiguous
for command in chown chmod find getent install mkdir mktemp mv python3 sha256sum \
  sshd systemctl useradd usermod visudo
do
  command -v "$command" >/dev/null 2>&1 || fail "command_missing_$command"
done

if [ "$reuse_existing_key" -eq 1 ]; then
  [ -f "$authorized_keys" ] && [ ! -L "$authorized_keys" ] ||
    fail existing_public_key_invalid
  [ "$(wc -l <"$authorized_keys" | tr -d ' ')" = 1 ] ||
    fail existing_public_key_invalid
  authorized_line=$(tr -d '\r\n' <"$authorized_keys")
  authorized_prefix='restrict,command="/usr/local/sbin/phoenix-release-transport" '
  case "$authorized_line" in
    "$authorized_prefix"ssh-ed25519\ *)
      public_key=${authorized_line#"$authorized_prefix"}
      ;;
    *)
      fail existing_public_key_invalid
      ;;
  esac
else
  [ -f "$public_key_file" ] && [ ! -L "$public_key_file" ] ||
    fail public_key_file_invalid
  public_key=$(tr -d '\r\n' <"$public_key_file")
fi
[ "${#public_key}" -le 1024 ] || fail public_key_invalid
set -f
# Word splitting is intentional for strict two-field public-key validation.
# shellcheck disable=SC2086
set -- $public_key
[ "$#" -eq 2 ] && [ "$1" = ssh-ed25519 ] || fail public_key_invalid
case "$2" in
  *[!A-Za-z0-9+/=]*|"") fail public_key_invalid ;;
esac
public_key="$1 $2"

if ! getent passwd "$deploy_user" >/dev/null 2>&1; then
  useradd --create-home --user-group --shell /bin/sh "$deploy_user"
fi
usermod --lock "$deploy_user"
deploy_group=$(id -gn "$deploy_user")
deploy_home=$(getent passwd "$deploy_user" | awk -F: '{print $6}')
[ "$deploy_home" = "/home/$deploy_user" ] || fail deploy_home_invalid

install -d -m 0755 -o root -g root /usr/local/sbin
install -d -m 0755 -o root -g root /usr/local/libexec
install -d -m 0755 -o root -g root "$libexec"
install -d -m 0755 -o root -g root "$libexec/phoenix_release"
install -d -m 0700 -o root -g root /var/lib/phoenix-release
install -d -m 0700 -o root -g root /var/lib/phoenix-release/incoming
install -d -m 0700 -o root -g root /var/lib/phoenix-release/releases

install -m 0755 -o root -g root "$script_dir/phoenix-release-gateway.sh" "$gateway"
install -m 0755 -o root -g root "$script_dir/phoenix-release-transport.sh" "$transport"
for name in __init__.py model.py controller.py gateway.py cli.py phase_update.py; do
  install -m 0644 -o root -g root \
    "$script_dir/phoenix_release/$name" "$libexec/phoenix_release/$name"
done
for specification in \
  'release_assets.py:0644' \
  'release_components.py:0644' \
  'release_platform.py:0644' \
  'release_provenance.py:0644' \
  'activate-economic-canary.sh:0644' \
  'deploy-release.sh:0644' \
  'install-release-assets.sh:0644' \
  'install-production-release-context.sh:0644' \
  'production_compose.py:0644' \
  'production_context.py:0644' \
  'production_mode.py:0644' \
  'production-healthcheck.sh:0644' \
  'rehearse-production-release.sh:0644' \
  'render-production-compose.sh:0644' \
  'validate-production-env.sh:0644' \
  'validate-production-release-context.sh:0644' \
  'prelive-protected-maintenance.sh:0644' \
  'prelive_protected_maintenance.py:0644' \
  'prelive-protected-maintenance-launch.sh:0644' \
  'prelive-protected-maintenance-unit.sh:0644' \
  'rollback-release.sh:0644'
do
  name=${specification%%:*}
  mode=${specification##*:}
  install -m "$mode" -o root -g root "$script_dir/$name" "$libexec/$name"
done
install -m 0644 -o root -g root \
  "$script_dir/../release-components.json" "$libexec/release-components.json"

manifest_candidate=$(mktemp "$libexec/.platform-manifest.XXXXXX")
trap 'rm -f "$manifest_candidate"' EXIT HUP INT TERM
python3 "$script_dir/release_platform.py" create \
  --source-root "$source_root" \
  --release-sha "$release_sha" \
  --output "$manifest_candidate" ||
  fail platform_manifest_create_failed
install -m 0644 -o root -g root \
  "$manifest_candidate" "$libexec/platform-manifest.json"
rm -f "$manifest_candidate"
trap - EXIT HUP INT TERM

install -d -m 0700 -o "$deploy_user" -g "$deploy_group" "$deploy_home/.ssh"
key_candidate=$(mktemp "$deploy_home/.ssh/.authorized_keys.XXXXXX")
trap 'rm -f "$key_candidate"' EXIT HUP INT TERM
printf 'restrict,command="%s" %s\n' "$transport" "$public_key" >"$key_candidate"
chown "$deploy_user:$deploy_group" "$key_candidate"
chmod 0600 "$key_candidate"
mv "$key_candidate" "$authorized_keys"
trap - EXIT HUP INT TERM

sudo_candidate=$(mktemp /etc/sudoers.d/.phoenix-release.XXXXXX)
gateway_digest=$(sha256sum "$gateway" | awk 'NR == 1 { print $1 }')
printf '%s\n' \
  "$deploy_user ALL=(root) NOPASSWD: sha256:$gateway_digest $gateway" \
  >"$sudo_candidate"
chown root:root "$sudo_candidate"
chmod 0440 "$sudo_candidate"
visudo -cf "$sudo_candidate" >/dev/null || fail sudoers_invalid
mv "$sudo_candidate" "$sudoers"

sshd_candidate=$(mktemp /etc/ssh/sshd_config.d/.phoenix-deploy.XXXXXX)
cat >"$sshd_candidate" <<'EOF'
AllowUsers phoenix codex-bootstrap phoenix-deploy

Match User phoenix-deploy
    AuthenticationMethods publickey
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    AllowAgentForwarding no
    AllowTcpForwarding no
    X11Forwarding no
    PermitTTY no
    PermitTunnel no
    PermitUserRC no
    ForceCommand /usr/local/sbin/phoenix-release-transport
EOF
chown root:root "$sshd_candidate"
chmod 0644 "$sshd_candidate"
mv "$sshd_candidate" "$sshd_dropin"
sshd -t || fail sshd_configuration_invalid
systemctl reload ssh || systemctl reload sshd || fail ssh_reload_failed

[ "$(stat -c '%U:%G:%a' "$gateway")" = root:root:755 ] ||
  fail gateway_metadata_invalid
[ "$(stat -c '%U:%G:%a' "$transport")" = root:root:755 ] ||
  fail transport_metadata_invalid
if find "$libexec" -type f -perm /022 -print | grep . >/dev/null 2>&1; then
  fail libexec_writable_by_deployment_identity
fi
python3 "$libexec/release_platform.py" verify \
  --installed-root / \
  --expected-sha "$release_sha" >/dev/null ||
  fail installed_platform_identity_invalid
sudo -l -U "$deploy_user" >/dev/null || fail sudo_policy_invalid

printf '%s\n' \
  "{\"schema\":\"phoenix.release-platform-install.v1\",\"status\":\"ok\",\"protocol_version\":\"phoenix-release.v1\",\"deploy_user\":\"phoenix-deploy\",\"release_sha\":\"$release_sha\"}"
