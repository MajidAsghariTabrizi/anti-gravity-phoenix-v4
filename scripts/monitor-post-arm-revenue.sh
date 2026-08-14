#!/usr/bin/env sh
set -eu
umask 077

release_sha=${1:-}
duration_seconds=${2:-900}
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=$deploy_dir/current-release.env
compose_file=$deploy_dir/compose.prod.yml
overlay_file=$deploy_dir/compose.live-autonomous.yml

fail() {
  echo "POST_ARM_REVENUE_MONITOR_FAILED: $1" >&2
  exit 1
}

case "$release_sha" in
  *[!0-9a-f]*|"") fail "release SHA is invalid" ;;
esac
[ "${#release_sha}" -eq 40 ] || fail "release SHA is invalid"
case "$duration_seconds" in
  *[!0-9]*|"") fail "monitor duration is invalid" ;;
esac
[ "$duration_seconds" -ge 600 ] && [ "$duration_seconds" -le 900 ] ||
  fail "monitor duration must be between 600 and 900 seconds"

[ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$release_sha" ] ||
  fail "release is not the active exact release"
[ "$(tr -d '\r\n' <"$deploy_dir/release-assets.sha")" = "$release_sha" ] ||
  fail "release assets do not match the active exact release"

compose() {
  python3 "$deploy_dir/production_compose.py" \
    --mode LIVE \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    -- "$@"
}

operator_mode_identity() {
  awk -F= '
    $1 == "PHOENIX_MODE" {
      mode_count += 1
      mode = $2
      next
    }
    $1 == "LIVE_EXECUTION" {
      live_count += 1
      live = $2
      next
    }
    $1 == "AUTONOMOUS_EXECUTION" {
      autonomous_count += 1
      autonomous = $2
      next
    }
    END {
      if (mode_count != 1 || live_count != 1 || autonomous_count != 1) {
        exit 2
      }
      printf "%s:%s:%s\n", mode, live, autonomous
    }
  ' "$env_file"
}

require_operator_live_mode() {
  identity=$(operator_mode_identity) ||
    fail "operator LIVE-mode evidence is unavailable"
  [ "$identity" = "LIVE:true:true" ] ||
    fail "operator environment left exact LIVE mode"
}

fail_closed=0
compensate() {
  code=$?
  trap - EXIT
  fail_closed=1
  compose run --rm --no-deps \
    -e PHOENIX_AUTONOMOUS_DISARM_ACK=DISARM_AUTONOMOUS_LIVE_42161 \
    -e PHOENIX_AUTONOMOUS_DISARM_REASON=post_arm_acceptance_failed \
    autonomous-control disarm >/dev/null 2>&1 || fail_closed=0
  compose stop -t 30 live-executor >/dev/null 2>&1 || fail_closed=0
  compose run --rm --no-deps \
    -e PHOENIX_RELEASE_SHA="$release_sha" \
    -e PHOENIX_EXECUTOR_OWNER_PAUSE_ACK=PAUSE_EXECUTOR_AFTER_FAILED_DEPLOY_42161 \
    --entrypoint /usr/local/bin/autonomous-live-control \
    live-executor owner-pause >/dev/null 2>&1 || fail_closed=0
  python3 "$deploy_dir/production_mode.py" shadow --env-file "$env_file" \
    >/dev/null 2>&1 || fail_closed=0
  if [ "$fail_closed" -ne 1 ]; then
    echo "POST_ARM_REVENUE_COMPENSATION_FAILED" >&2
  else
    echo "POST_ARM_REVENUE_FAIL_CLOSED" >&2
  fi
  exit "$code"
}
trap compensate EXIT
trap 'exit 1' HUP INT TERM

require_operator_live_mode
baseline_executor=$(compose ps --status running -q live-executor)
[ -n "$baseline_executor" ] || fail "live-executor is not running"
[ "$(printf '%s\n' "$baseline_executor" | awk 'NF { count += 1 } END { print count + 0 }')" -eq 1 ] ||
  fail "live-executor identity is ambiguous"
baseline_restarts=$(docker inspect -f '{{.RestartCount}}' "$baseline_executor") ||
  fail "live-executor restart evidence is unavailable"

started_at=$(date +%s)
deadline=$((started_at + duration_seconds))
while :; do
  [ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$release_sha" ] ||
    fail "active release changed during monitoring"
  [ "$(tr -d '\r\n' <"$deploy_dir/release-assets.sha")" = "$release_sha" ] ||
    fail "release assets changed during monitoring"
  require_operator_live_mode

  compose run --rm --no-deps \
    -e PHOENIX_RELEASE_SHA="$release_sha" \
    --entrypoint /usr/local/bin/autonomous-live-control \
    live-executor owner-live-preflight
  compose run --rm --no-deps autonomous-control reconciliation-status

  current_executor=$(compose ps --status running -q live-executor)
  [ "$current_executor" = "$baseline_executor" ] ||
    fail "live-executor identity changed during monitoring"
  [ "$(docker inspect -f '{{.RestartCount}}' "$current_executor")" = "$baseline_restarts" ] ||
    fail "live-executor restarted during monitoring"
  require_operator_live_mode

  now=$(date +%s)
  [ "$now" -lt "$deadline" ] || break
  remaining=$((deadline - now))
  interval=60
  [ "$remaining" -ge "$interval" ] || interval=$remaining
  sleep "$interval"
done

trap - EXIT
echo "POST_ARM_REVENUE_MONITOR_OK: release=$release_sha duration_seconds=$duration_seconds"
