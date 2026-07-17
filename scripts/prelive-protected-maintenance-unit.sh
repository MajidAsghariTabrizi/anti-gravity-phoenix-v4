#!/usr/bin/env sh
set -u
umask 077

stage_dir=${1:-}
shift || true

fail() {
  echo "PROTECTED_MAINTENANCE_UNIT_FAILED: $1" >&2
  exit 1
}

[ "$#" -eq 8 ] || fail 'exactly eight maintenance arguments are required'
case "$stage_dir" in
  /tmp/phoenix-protected-maintenance-*) ;;
  *) fail 'maintenance stage path is invalid' ;;
esac
[ -d "$stage_dir" ] && [ ! -L "$stage_dir" ] ||
  fail 'maintenance stage directory is unavailable'

maintenance_script=$stage_dir/prelive-protected-maintenance.sh
log_file=$stage_dir/maintenance.log
exit_file=$stage_dir/maintenance.exit
result_file=$stage_dir/maintenance.result
exit_tmp=$stage_dir/.maintenance.exit.tmp
result_tmp=$stage_dir/.maintenance.result.tmp

[ -f "$maintenance_script" ] && [ ! -L "$maintenance_script" ] ||
  fail 'maintenance runtime is unavailable'
rm -f -- "$log_file" "$exit_file" "$result_file" "$exit_tmp" "$result_tmp"

set +e
/bin/sh "$maintenance_script" "$@" >"$log_file" 2>&1
maintenance_status=$?
set -e

if [ "$maintenance_status" -eq 0 ]; then
  result_line=$(grep '^PROTECTED_MAINTENANCE_OK: ' "$log_file" | tail -n 1)
  if [ -z "$result_line" ]; then
    echo "PROTECTED_MAINTENANCE_UNIT_FAILED: completion evidence is missing" \
      >>"$log_file"
    maintenance_status=125
  else
    printf '%s\n' "$result_line" >"$result_tmp"
    chmod 0600 "$result_tmp"
    mv "$result_tmp" "$result_file"
  fi
fi

printf '%s\n' "$maintenance_status" >"$exit_tmp"
chmod 0600 "$exit_tmp"
mv "$exit_tmp" "$exit_file"
exit "$maintenance_status"
