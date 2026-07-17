#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
workflow=$repo_root/.github/workflows/deploy-prelive-protected-maintenance.yml
launcher=$script_dir/prelive-protected-maintenance-launch.sh
unit_runner=$script_dir/prelive-protected-maintenance-unit.sh
maintenance=$script_dir/prelive-protected-maintenance.sh

fail() {
  echo "prelive-protected-maintenance-supervisor-tests: $1" >&2
  exit 1
}

for required in "$workflow" "$launcher" "$unit_runner" "$maintenance"; do
  [ -s "$required" ] || fail "required file is missing: $required"
done

grep -F 'prelive-protected-maintenance-launch.sh' "$workflow" >/dev/null ||
  fail 'workflow does not launch the reviewed transient-unit helper'
grep -F "systemctl show '\$unit_name'" "$workflow" >/dev/null ||
  fail 'workflow does not poll transient-unit status'
grep -F 'protected-maintenance-evidence.tar.gz' "$workflow" >/dev/null ||
  fail 'workflow does not retrieve final maintenance evidence'
grep -F 'unit_started" -eq 1' "$workflow" >/dev/null ||
  fail 'workflow cleanup does not protect an active detached unit'
grep -F 'chown root:root "$stage_dir"' "$launcher" >/dev/null ||
  fail 'launcher does not lock the maintenance stage against SSH-user writes'
grep -F 'chmod 0700 "$stage_dir"' "$launcher" >/dev/null ||
  fail 'launcher does not make the maintenance stage root-private'
if grep -F \
  "sudo /bin/sh '\$remote_stage/prelive-protected-maintenance.sh'" \
  "$workflow" >/dev/null
then
  fail 'workflow still runs maintenance inside the SSH session'
fi

for property in \
  'Type=oneshot' \
  'RemainAfterExit=yes' \
  'TimeoutStartSec=2400' \
  'TimeoutStopSec=300' \
  'KillMode=control-group' \
  'UMask=0077'
do
  grep -F "$property" "$launcher" >/dev/null ||
    fail "transient unit property is missing: $property"
done

tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-maintenance-supervisor.XXXXXX")
success_stage=
failure_stage=
detached_stage=
passwordless_sudo=0
if command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
  passwordless_sudo=1
fi
cleanup() {
  rm -rf -- "$tmp_root" "$success_stage" "$failure_stage" 2>/dev/null || true
  if [ "$passwordless_sudo" -eq 1 ]; then
    sudo -n rm -rf -- "$detached_stage"
  else
    rm -rf -- "$detached_stage" 2>/dev/null || true
  fi
}
trap cleanup EXIT HUP INT TERM

success_stage=$(mktemp -d "/tmp/phoenix-protected-maintenance-success.XXXXXX")
failure_stage=$(mktemp -d "/tmp/phoenix-protected-maintenance-failure.XXXXXX")
detached_stage=$(mktemp -d "/tmp/phoenix-protected-maintenance-detached.XXXXXX")

cp "$unit_runner" "$success_stage/prelive-protected-maintenance-unit.sh"
cat >"$success_stage/prelive-protected-maintenance.sh" <<'SH'
#!/usr/bin/env sh
set -eu
echo "PROTECTED_MAINTENANCE_OK: release=1111111111111111111111111111111111111111 rollback=2222222222222222222222222222222222222222 evidence=/opt/phoenix/evidence/protected-maintenance/fixture-success"
SH
chmod +x \
  "$success_stage/prelive-protected-maintenance-unit.sh" \
  "$success_stage/prelive-protected-maintenance.sh"
/bin/sh "$success_stage/prelive-protected-maintenance-unit.sh" \
  "$success_stage" 1 2 3 4 5 6 7 8
[ "$(tr -d '\r\n' <"$success_stage/maintenance.exit")" = 0 ] ||
  fail 'successful unit runner did not record exit zero'
grep -F 'PROTECTED_MAINTENANCE_OK:' "$success_stage/maintenance.result" >/dev/null ||
  fail 'successful unit runner did not retain final result evidence'

rollback_marker=$failure_stage/rollback.marker
export ROLLBACK_MARKER="$rollback_marker"
cp "$unit_runner" "$failure_stage/prelive-protected-maintenance-unit.sh"
cat >"$failure_stage/prelive-protected-maintenance.sh" <<'SH'
#!/usr/bin/env sh
set -eu
mutation_started=1
finalized=0
rollback_on_failure() {
  code=$?
  trap - EXIT HUP INT TERM
  if [ "$mutation_started" -eq 1 ] && [ "$finalized" -ne 1 ]; then
    printf 'rollback\n' >"$ROLLBACK_MARKER"
  fi
  exit "$code"
}
trap rollback_on_failure EXIT
trap 'exit 1' HUP INT TERM
exit 42
SH
chmod +x \
  "$failure_stage/prelive-protected-maintenance-unit.sh" \
  "$failure_stage/prelive-protected-maintenance.sh"
if /bin/sh "$failure_stage/prelive-protected-maintenance-unit.sh" \
  "$failure_stage" 1 2 3 4 5 6 7 8
then
  fail 'internal maintenance failure was hidden by the unit runner'
fi
[ -s "$rollback_marker" ] ||
  fail 'internal maintenance failure did not invoke rollback'
[ "$(tr -d '\r\n' <"$failure_stage/maintenance.exit")" = 42 ] ||
  fail 'internal maintenance failure status was not retained'
[ ! -e "$failure_stage/maintenance.result" ] ||
  fail 'failed maintenance produced false completion evidence'

if [ "$(uname -s)" = Linux ] &&
  command -v setsid >/dev/null 2>&1 &&
  [ "$passwordless_sudo" -eq 1 ]
then
  fake_bin=$tmp_root/bin
  mkdir -p "$fake_bin"
  for staged_name in \
    prelive_protected_maintenance.py \
    prelive-protected-maintenance.sh \
    install-release-assets.sh \
    install-production-release-context.sh \
    prelive-protected-maintenance-launch.sh \
    prelive-protected-maintenance-unit.sh \
    rollback-release.sh
  do
    cp "$script_dir/$staged_name" "$detached_stage/$staged_name"
  done
  cat >"$detached_stage/prelive-protected-maintenance.sh" <<'SH'
#!/usr/bin/env sh
set -eu
sleep 2
echo "PROTECTED_MAINTENANCE_OK: release=1111111111111111111111111111111111111111 rollback=2222222222222222222222222222222222222222 evidence=/opt/phoenix/evidence/protected-maintenance/fixture-detached"
SH
  for input_number in 1 2 3 4 5 6; do
    printf 'fixture-%s\n' "$input_number" \
      >"$detached_stage/input-$input_number"
  done
  chmod +x \
    "$detached_stage/prelive-protected-maintenance-unit.sh" \
    "$detached_stage/prelive-protected-maintenance.sh"
  cat >"$fake_bin/systemctl" <<'SH'
#!/usr/bin/env sh
set -eu
if [ "${1:-}" = show ]; then
  echo not-found
  exit 0
fi
exit 1
SH
  cat >"$fake_bin/systemd-run" <<'SH'
#!/usr/bin/env sh
set -eu
while [ "$#" -gt 0 ]; do
  case "$1" in
    --unit=*|--description=*|--property=*|--quiet) shift ;;
    *) break ;;
  esac
done
setsid "$@" >"$FAKE_UNIT_OUTPUT" 2>&1 &
printf '%s\n' "$!" >"$FAKE_UNIT_PID"
SH
  chmod +x "$fake_bin/systemctl" "$fake_bin/systemd-run"
  fake_unit_pid=$tmp_root/unit.pid
  fake_unit_output=$tmp_root/unit.log
  export FAKE_UNIT_PID="$fake_unit_pid"
  export FAKE_UNIT_OUTPUT="$fake_unit_output"
  (
    sudo -n env \
      PATH="$fake_bin:$PATH" \
      FAKE_UNIT_PID="$fake_unit_pid" \
      FAKE_UNIT_OUTPUT="$fake_unit_output" \
      /bin/sh "$launcher" \
        phoenix-protected-maintenance-12345-1 \
        "$detached_stage" \
        1111111111111111111111111111111111111111 \
        2222222222222222222222222222222222222222 \
        "$detached_stage/input-1" \
        "$detached_stage/input-2" \
        "$detached_stage/input-3" \
        "$detached_stage/input-4" \
        "$detached_stage/input-5" \
        "$detached_stage/input-6" >/dev/null
    sleep 30
  ) &
  transport_pid=$!
  deadline=$(( $(date +%s) + 10 ))
  while [ ! -s "$fake_unit_pid" ] && [ "$(date +%s)" -lt "$deadline" ]; do
    sleep 1
  done
  [ -s "$fake_unit_pid" ] || fail 'fake transient unit did not start'
  [ "$(stat -c '%U:%G:%a' "$detached_stage")" = root:root:700 ] ||
    fail 'detached maintenance stage was not locked root-private'
  kill -HUP "$transport_pid" >/dev/null 2>&1 || true
  wait "$transport_pid" >/dev/null 2>&1 || true
  deadline=$(( $(date +%s) + 10 ))
  while ! sudo -n test -s "$detached_stage/maintenance.exit" &&
    [ "$(date +%s)" -lt "$deadline" ]
  do
    sleep 1
  done
  [ "$(sudo -n cat "$detached_stage/maintenance.exit" | tr -d '\r\n')" = 0 ] ||
    fail 'SSH/HUP simulation terminated the detached maintenance unit'
  sudo -n grep -F 'fixture-detached' \
    "$detached_stage/maintenance.result" >/dev/null ||
    fail 'detached maintenance completion evidence is missing'
else
  echo 'prelive-protected-maintenance-supervisor-tests: HUP integration skipped (Linux setsid and passwordless sudo required)'
fi

echo 'prelive-protected-maintenance-supervisor-tests: ok'
