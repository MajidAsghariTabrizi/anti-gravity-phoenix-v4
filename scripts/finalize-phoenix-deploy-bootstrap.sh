#!/usr/bin/env sh
set -eu
umask 077

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

deploy_user=phoenix-deploy
bootstrap_user=codex-bootstrap
deploy_dropin=/etc/ssh/sshd_config.d/89-phoenix-deploy.conf
bootstrap_dropin=/etc/ssh/sshd_config.d/90-codex-bootstrap.conf
bootstrap_sudoers=/etc/sudoers.d/90-phoenix-codex-bootstrap
backup_root=/run/phoenix-bootstrap-revocation

fail() {
  printf 'PHOENIX_BOOTSTRAP_REVOCATION_FAILED: %s\n' "$1" >&2
  exit 1
}

[ "$(id -u)" -eq 0 ] || fail root_required
[ "$(uname -s)" = Linux ] || fail linux_required
id phoenix >/dev/null 2>&1 || fail administration_user_missing
id "$deploy_user" >/dev/null 2>&1 || fail deployment_user_missing
[ -x /usr/local/sbin/phoenix-release-transport ] ||
  fail permanent_transport_missing
[ -x /usr/local/sbin/phoenix-release-gateway ] ||
  fail permanent_gateway_missing
sudo -l -U "$deploy_user" 2>/dev/null |
  grep -F /usr/local/sbin/phoenix-release-gateway >/dev/null ||
  fail permanent_sudo_policy_missing
sudo -l -U "$deploy_user" 2>/dev/null |
  grep -F 'NOPASSWD: ALL' >/dev/null &&
  fail permanent_sudo_policy_unrestricted

install -d -m 0700 -o root -g root "$backup_root"
candidate=$(mktemp /etc/ssh/sshd_config.d/.phoenix-final.XXXXXX)
cat >"$candidate" <<'EOF'
AllowUsers phoenix phoenix-deploy

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
chown root:root "$candidate"
chmod 0644 "$candidate"
mv "$candidate" "$deploy_dropin"

if [ -e "$bootstrap_dropin" ]; then
  [ -f "$bootstrap_dropin" ] && [ ! -L "$bootstrap_dropin" ] ||
    fail bootstrap_dropin_unsafe
  mv "$bootstrap_dropin" "$backup_root/90-codex-bootstrap.conf"
fi
if [ -e "$bootstrap_sudoers" ]; then
  [ -f "$bootstrap_sudoers" ] && [ ! -L "$bootstrap_sudoers" ] ||
    fail bootstrap_sudoers_unsafe
  mv "$bootstrap_sudoers" "$backup_root/90-phoenix-codex-bootstrap"
fi
if ! sshd -t; then
  [ ! -e "$backup_root/90-codex-bootstrap.conf" ] ||
    mv "$backup_root/90-codex-bootstrap.conf" "$bootstrap_dropin"
  [ ! -e "$backup_root/90-phoenix-codex-bootstrap" ] ||
    mv "$backup_root/90-phoenix-codex-bootstrap" "$bootstrap_sudoers"
  fail sshd_configuration_invalid
fi
systemctl reload ssh || systemctl reload sshd || fail ssh_reload_failed

if id "$bootstrap_user" >/dev/null 2>&1; then
  pkill -KILL -u "$bootstrap_user" >/dev/null 2>&1 || true
  userdel --remove "$bootstrap_user"
fi
rm -f \
  "$backup_root/90-codex-bootstrap.conf" \
  "$backup_root/90-phoenix-codex-bootstrap"
rmdir "$backup_root"

sshd -t || fail final_sshd_configuration_invalid
effective_allow_users=$(
  sshd -T | awk '$1 == "allowusers" { for (i=2; i<=NF; i++) print $i }' |
    sort -u |
    tr '\n' ' '
)
[ "$effective_allow_users" = "phoenix phoenix-deploy " ] ||
  fail final_allow_users_invalid

printf '%s\n' \
  '{"schema":"phoenix.bootstrap-revocation.v1","status":"ok","removed_user":"codex-bootstrap","allow_users":["phoenix","phoenix-deploy"]}'
