#!/usr/bin/env sh
# Literal grep patterns and variables consumed by the sourced workflow are intentional.
# shellcheck disable=SC2016,SC2034,SC2329
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH='' cd -- "$script_dir/.." && pwd)
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
grep -F 'PHOENIX_DASHBOARD_EVIDENCE_START="$runtime_sampling_baseline"' "$workflow" >/dev/null || fail 'Dashboard SQL is not clipped to the post-restart sampling baseline'
# shellcheck disable=SC2016
grep -F -- '--database-clock-baseline "$database_clock_baseline"' "$workflow" >/dev/null || fail 'initial preflight baseline is not preserved for final evidence'
engine_ready_line=$(grep -nF 'wait_service_healthy phoenix-engine' "$workflow" | tail -n 1 | cut -d: -f1)
# shellcheck disable=SC2016
sampling_baseline_line=$(grep -nF 'runtime_sampling_baseline=$(database_clock_utc)' "$workflow" | cut -d: -f1)
initial_sample_line=$(grep -nF "collect_sample || fail 'initial bounded evidence sample failed'" "$workflow" | cut -d: -f1)
[ "$engine_ready_line" -lt "$sampling_baseline_line" ] &&
  [ "$sampling_baseline_line" -lt "$initial_sample_line" ] ||
  fail 'runtime sampling baseline is not captured after the final Engine restart and before sampling'
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

# Invoked indirectly by the sourced workflow.
# shellcheck disable=SC2329
compose() {
  printf '2026-07-16T12:00:00Z\n'
}
# Consumed by database_clock_utc from the sourced workflow.
# shellcheck disable=SC2034
POSTGRES_USER=phoenix
# shellcheck disable=SC2034
POSTGRES_DB=phoenix
sampling_clock=$(database_clock_utc) || fail 'canonical PostgreSQL sampling clock was rejected'
[ "$sampling_clock" = 2026-07-16T12:00:00Z ] || fail 'sampling clock changed'
# Invoked indirectly by the sourced workflow.
# shellcheck disable=SC2329
compose() {
  printf 'not-a-timestamp\n'
}
if database_clock_utc >/dev/null 2>&1; then
  fail 'malformed PostgreSQL sampling clock was accepted'
fi

identity_state=$test_root/identity-state
mkdir -p "$identity_state"
state_dir=$identity_state
fake_docker=$test_root/fake-docker
cat >"$fake_docker" <<'EOF'
#!/usr/bin/env sh
printf '%s|sha256:%064d|2026-07-16T11:00:00Z|2026-07-16T11:00:01Z|0|[]\n' "$4" 1
EOF
chmod 0700 "$fake_docker"
# Consumed by capture_protected_identity from the sourced workflow.
# shellcheck disable=SC2034
docker_bin=$fake_docker
container_id() {
  printf '%064d\n' 1
}
compose() {
  cat "$repo_dir/fixtures/control-plane/jetstream.json"
}
identity_output=$identity_state/identity.requested.json
output=$identity_output
capture_protected_identity "$output" || fail 'nested protected identity capture failed'
[ "$output" = "$identity_output" ] || fail 'nested capture clobbered the caller destination'
[ -s "$identity_output" ] || fail 'identity output was not written to the requested path'
[ -s "$identity_state/jetstream.identity.json" ] || fail 'JetStream identity capture used the caller path'
if ! "$python_command" - "$identity_output" "$identity_state/jetstream.identity.json" <<'PY'
import json
import sys

identity = json.load(open(sys.argv[1], encoding="utf-8"))
jetstream = json.load(open(sys.argv[2], encoding="utf-8"))
assert identity["schema_version"] == "phoenix.prelive.protected-identity.v1"
assert len(jetstream["streams"]) == 2
assert len(jetstream["consumers"]) == 2
PY
then
  fail 'nested capture outputs are incomplete'
fi

fake_script_dir=$test_root/fake-positive
mkdir -p "$fake_script_dir"
cat >"$fake_script_dir/shadow-positive-route-evidence.sh" <<'EOF'
#!/usr/bin/env sh
case "${PHOENIX_FAKE_POSITIVE_RESULT:-}" in
  found)
    echo 'dial https://rpc.invalid/private'
    echo 'POSTGRES_DSN=postgres://phoenix:placeholder@postgres/phoenix'
    echo 'wallet=0x1111111111111111111111111111111111111111'
    echo "opaque ${PHOENIX_TEST_SECRET:-missing}"
    echo 'POSITIVE_ROUTE_EVIDENCE_FOUND'
    ;;
  not_found)
    echo 'POSITIVE_ROUTE_EVIDENCE_NOT_FOUND'
    ;;
  *)
    echo 'unexpected fake mode'
    exit 7
    ;;
esac
EOF
chmod 0700 "$fake_script_dir/shadow-positive-route-evidence.sh"
script_dir=$fake_script_dir
state_dir=$test_root/positive-state
evidence_root=$test_root/evidence
mkdir -p "$state_dir" "$evidence_root"
# Consumed by run_positive_route_evidence from the sourced workflow.
# shellcheck disable=SC2034
compose_file=$test_root/compose.yml
# shellcheck disable=SC2034
env_file=$test_root/phoenix.env
# shellcheck disable=SC2034
release_env=$test_root/release.env
# shellcheck disable=SC2034
release_manifest=$test_root/release.json
# shellcheck disable=SC2034
positive_timeout=30
PHOENIX_TEST_SECRET=runtime-test-only-value
export PHOENIX_TEST_SECRET
PHOENIX_FAKE_POSITIVE_RESULT=found
export PHOENIX_FAKE_POSITIVE_RESULT
attempt_output=$(run_positive_route_evidence) || fail 'positive attempt harness failed'
attempt_log=$(find "$evidence_root/positive-route-attempts" -type f -name '*.log' | head -n 1)
[ -n "$attempt_log" ] && [ -s "$attempt_log" ] || fail 'positive attempt log was not retained'
[ "$(stat -c '%a' "$attempt_log")" = 600 ] || fail 'positive attempt log is not mode 0600'
grep -F 'terminal_reason=evidence_found' "$attempt_log" >/dev/null ||
  fail 'positive attempt terminal reason is missing'
printf '%s' "$attempt_output" | grep -F "path=$attempt_log" >/dev/null ||
  fail 'positive attempt path was not printed to the control journal'
if grep -E 'runtime-test-only-value|rpc\.invalid|postgres://|0x1111111111111111111111111111111111111111' "$attempt_log" >/dev/null; then
  fail 'positive attempt log retained sensitive runtime material'
fi

PHOENIX_FAKE_POSITIVE_RESULT=not_found
export PHOENIX_FAKE_POSITIVE_RESULT
if failed_attempt_output=$(run_positive_route_evidence); then
  fail 'not-found positive attempt unexpectedly succeeded'
fi
attempt_count=$(find "$evidence_root/positive-route-attempts" -type f -name '*.log' | wc -l | tr -d '[:space:]')
[ "$attempt_count" -eq 2 ] || fail 'positive attempt logs are not uniquely retained'
printf '%s' "$failed_attempt_output" | grep -F 'reason=evidence_not_found' >/dev/null ||
  fail 'failed positive attempt reason was not printed'
grep -R -F 'terminal_reason=evidence_not_found' "$evidence_root/positive-route-attempts" >/dev/null ||
  fail 'failed positive attempt terminal reason was not retained'
script_dir=$repo_dir/scripts

"$python_command" -m unittest scripts.tests.test_prelive_shadow_control -v \
  >/dev/null || fail 'Python control-plane contract tests failed'
"$python_command" -m unittest scripts.tests.test_prelive_dashboard_live -v \
  >/dev/null || fail 'Python live Dashboard collector tests failed'
"$python_command" "$script_dir/prelive_dashboard_live.py" validate-source \
  --input "$repo_dir/fixtures/dashboard/live-source.json" >/dev/null || fail 'Dashboard source fixture validation failed'
"$python_command" "$script_dir/prelive_shadow_control.py" validate-evidence \
  --input "$repo_dir/fixtures/control-plane/valid-evidence.json" >/dev/null || fail 'evidence fixture validation failed'

echo 'prelive-shadow-control-tests: ok'
