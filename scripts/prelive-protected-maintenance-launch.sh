#!/usr/bin/env sh
set -eu
umask 077

unit_name=${1:-}
stage_dir=${2:-}
shift 2 || true

fail() {
  echo "PROTECTED_MAINTENANCE_LAUNCH_FAILED: $1" >&2
  exit 1
}

[ "$#" -eq 8 ] || fail 'exactly eight maintenance arguments are required'
[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
case "$unit_name" in
  *[!A-Za-z0-9_.-]*) fail 'transient unit name is invalid' ;;
esac
unit_identity=${unit_name#phoenix-protected-maintenance-}
[ "$unit_identity" != "$unit_name" ] ||
  fail 'transient unit name is invalid'
unit_run_id=${unit_identity%-*}
unit_attempt=${unit_identity##*-}
case "$unit_run_id:$unit_attempt" in
  *[!0-9:]*|:*|*:|*::*)
    fail 'transient unit name is invalid'
    ;;
esac
case "$stage_dir" in
  /tmp/phoenix-protected-maintenance-*) ;;
  *) fail 'maintenance stage path is invalid' ;;
esac
[ -d "$stage_dir" ] && [ ! -L "$stage_dir" ] ||
  fail 'maintenance stage directory is unavailable'

for command_name in chmod chown systemctl systemd-run; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "required command is unavailable: $command_name"
done

# Remove the SSH account's write access before systemd reads staged inputs.
chown root:root "$stage_dir" ||
  fail 'maintenance stage ownership could not be locked'
chmod 0700 "$stage_dir" ||
  fail 'maintenance stage permissions could not be locked'

for staged_path in "$3" "$4" "$5" "$6" "$7" "$8"; do
  case "$staged_path" in
    "$stage_dir/"*) ;;
    *) fail 'maintenance input is outside the locked stage' ;;
  esac
  [ "${staged_path#"$stage_dir"/}" = "${staged_path##*/}" ] ||
    fail 'maintenance input must be a direct child of the locked stage'
  [ -f "$staged_path" ] && [ ! -L "$staged_path" ] ||
    fail 'maintenance input is missing or unsafe'
done
for staged_name in \
  prelive_protected_maintenance.py \
  prelive-protected-maintenance.sh \
  install-release-assets.sh \
  install-production-release-context.sh \
  prelive-protected-maintenance-launch.sh \
  prelive-protected-maintenance-unit.sh \
  rollback-release.sh
do
  staged_path=$stage_dir/$staged_name
  [ -f "$staged_path" ] && [ ! -L "$staged_path" ] ||
    fail "maintenance runtime is missing or unsafe: $staged_name"
done
for staged_path in "$stage_dir"/*; do
  [ -f "$staged_path" ] && [ ! -L "$staged_path" ] ||
    fail 'maintenance stage contains a non-regular entry'
  chown root:root "$staged_path" ||
    fail 'maintenance stage file ownership could not be locked'
  chmod 0600 "$staged_path" ||
    fail 'maintenance stage file permissions could not be locked'
done

unit_marker=$stage_dir/maintenance.unit
existing_load_state=$(
  systemctl show "$unit_name.service" --property=LoadState --value 2>/dev/null ||
    true
)
if [ -n "$existing_load_state" ] && [ "$existing_load_state" != not-found ]; then
  [ -f "$unit_marker" ] &&
    [ "$(tr -d '\r\n' <"$unit_marker")" = "$unit_name.service" ] ||
    fail 'existing transient unit does not match this maintenance stage'
  echo "PROTECTED_MAINTENANCE_UNIT_STARTED: $unit_name.service"
  exit 0
fi

printf '%s\n' "$unit_name.service" >"$unit_marker"
chmod 0600 "$unit_marker"
if ! systemd-run \
  --unit="$unit_name" \
  --description="Phoenix protected maintenance $unit_name" \
  --property=Type=oneshot \
  --property=RemainAfterExit=yes \
  --property=TimeoutStartSec=2400 \
  --property=TimeoutStopSec=300 \
  --property=KillMode=control-group \
  --property=UMask=0077 \
  --property=StandardOutput=journal \
  --property=StandardError=journal \
  --quiet \
  /bin/sh "$stage_dir/prelive-protected-maintenance-unit.sh" "$stage_dir" "$@"
then
  rm -f -- "$unit_marker"
  fail 'transient maintenance unit could not be started'
fi

echo "PROTECTED_MAINTENANCE_UNIT_STARTED: $unit_name.service"
