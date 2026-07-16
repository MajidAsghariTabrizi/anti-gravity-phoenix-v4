#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
workflow=$script_dir/prelive-shadow-control.sh
test_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-shadow-control-tests.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "prelive-shadow-control-tests: $1" >&2
  exit 1
}

if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  fail 'python is unavailable'
fi
PHOENIX_PYTHON_BIN=$python_command
export PHOENIX_PYTHON_BIN

for mode_duration in '15m 900' '1h 3600' '6h 21600' '24h 86400' 'continuous null'; do
  mode=${mode_duration%% *}
  expected=${mode_duration#* }
  "$workflow" plan "$mode" >"$test_root/plan.json" || fail "plan failed for $mode"
  "$python_command" - "$test_root/plan.json" "$mode" "$expected" <<'PY' || fail "plan contract failed for $mode"
import json
import sys

plan = json.load(open(sys.argv[1], encoding="utf-8"))
expected = None if sys.argv[3] == "null" else int(sys.argv[3])
assert plan["mode"] == sys.argv[2]
assert plan["duration_seconds"] == expected
assert plan["protected_services"] == [
    "nitro-feed-relay", "feed-ingestor", "nats", "postgres", "recorder"
]
assert plan["start_order"] == [
    "prometheus", "rpc-gateway", "shadow-dispatcher", "phoenix-engine", "dashboard"
]
assert plan["stop_order"] == list(reversed(plan["start_order"]))
assert plan["safety"] == {
    "mode": "SHADOW",
    "live_execution": False,
    "execution_eligible": False,
    "execution_request_created": False,
    "submission_methods_allowed": False,
}
PY
done

if "$workflow" plan 5m >"$test_root/invalid.out" 2>"$test_root/invalid.err"; then
  fail 'invalid mode was accepted'
fi
grep -F 'mode must be 15m, 1h, 6h, 24h, or continuous' "$test_root/invalid.err" >/dev/null || fail 'invalid mode error was not explicit'

if grep -Ei 'docker[[:space:]]+compose[[:space:]]+down|compose[[:space:]]+down|remove-orphans|docker[[:space:]]+(system[[:space:]]+)?prune|stream[[:space:]]+delete|consumer[[:space:]]+(delete|reset)' "$workflow" >/dev/null; then
  fail 'destructive control-plane command is present'
fi
if grep -E 'compose[[:space:]]+(up|stop|restart|rm)[^#]*(nitro-feed-relay|feed-ingestor|[[:space:]]nats|postgres|recorder)' "$workflow" >/dev/null; then
  fail 'protected service appears in a lifecycle command'
fi
grep -F 'compose up -d --no-deps "$service"' "$workflow" >/dev/null || fail 'optional starts are not explicit --no-deps operations'
grep -F 'compose stop $stop_services' "$workflow" >/dev/null || fail 'optional stop list is absent'
grep -F '[ "$(date +%s)" -ge "$deadline" ]' "$workflow" >/dev/null || fail 'finite duration completion guard is absent'
grep -F "trap control_signal HUP INT TERM" "$workflow" >/dev/null || fail 'signal handling is absent'
grep -F 'PHOENIX_DASHBOARD_EVIDENCE_START="$database_clock_baseline"' "$workflow" >/dev/null || fail 'Dashboard SQL is not clipped to the control baseline'
grep -F -- '--execution-request-count-before "$execution_request_count_before"' "$workflow" >/dev/null || fail 'execution request baseline is absent from evidence assembly'
grep -F -- '--execution-request-count-after "$execution_request_count_after"' "$workflow" >/dev/null || fail 'execution request final count is absent from evidence assembly'

command_log=$test_root/commands.log
: >"$command_log"
PHOENIX_SHADOW_CONTROL_LIBRARY_ONLY=1
export PHOENIX_SHADOW_CONTROL_LIBRARY_ONLY
# shellcheck disable=SC1090
. "$workflow"
compose() {
  printf '%s\n' "$*" >>"$command_log"
}
wait_service_healthy() {
  return 0
}
start_optional_runtime || fail 'optional lifecycle start failed under harness'
stop_optional_runtime || fail 'optional lifecycle stop failed under harness'

cat >"$test_root/expected.log" <<'EOF'
up -d --no-deps prometheus
up -d --no-deps rpc-gateway
up -d --no-deps shadow-dispatcher
up -d --no-deps phoenix-engine
up -d --no-deps dashboard
stop dashboard phoenix-engine shadow-dispatcher rpc-gateway prometheus
EOF
cmp "$test_root/expected.log" "$command_log" >/dev/null || fail 'optional lifecycle command sequence changed'

"$python_command" -m unittest scripts.tests.test_prelive_shadow_control -v \
  >/dev/null || fail 'Python control-plane contract tests failed'
"$python_command" -m unittest scripts.tests.test_prelive_dashboard_live -v \
  >/dev/null || fail 'Python live Dashboard collector tests failed'
"$python_command" "$script_dir/prelive_dashboard_live.py" validate-source \
  --input "$repo_dir/fixtures/dashboard/live-source.json" >/dev/null || fail 'Dashboard source fixture validation failed'
"$python_command" "$script_dir/prelive_shadow_control.py" validate-evidence \
  --input "$repo_dir/fixtures/control-plane/valid-evidence.json" >/dev/null || fail 'evidence fixture validation failed'

echo 'prelive-shadow-control-tests: ok'
