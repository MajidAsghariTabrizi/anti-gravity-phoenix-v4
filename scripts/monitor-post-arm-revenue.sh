#!/usr/bin/env sh
set -eu
umask 077

release_sha=${1:-}
duration_seconds=${2:-900}
authority_state=${3:-armed}
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
case "$authority_state" in
  armed|disarmed) ;;
  *) fail "monitor authority state is invalid" ;;
esac

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

hunter_health() {
  compose exec -T atlas-observer \
    wget -q -O - http://127.0.0.1:9700/readyz
}

hunter_metrics() {
  compose exec -T atlas-observer \
    wget -q -O - http://127.0.0.1:9700/metrics
}

gateway_metrics() {
  compose exec -T atlas-observer \
    wget -q -O - http://rpc-gateway:9300/metrics
}

runtime_evidence() {
  evidence=""
  for service in rpc-gateway atlas-observer live-executor; do
    container=$(compose ps --status running -q "$service") || return 1
    [ -n "$container" ] || return 1
    [ "$(printf '%s\n' "$container" | awk 'NF { count += 1 } END { print count + 0 }')" -eq 1 ] || return 1
    state=$(docker inspect -f '{{.RestartCount}}:{{.State.OOMKilled}}:{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$container") || return 1
    [ "${state#*:}" != "$state" ] || return 1
    case "$state" in
      *:true:*) return 1 ;;
      *:false:healthy) ;;
      *) return 1 ;;
    esac
    evidence="${evidence}${service}=${container}:${state};"
  done
  printf '%s\n' "$evidence"
}

provider_health_counter_vector() {
  printf '%s' "$1" | python3 -c '
import json, sys
document = json.load(sys.stdin)
keys = (
    "provider_retryable_degradation_total",
    "provider_recovery_attempt_total",
    "provider_circuit_open_total",
    "provider_circuit_skipped_total",
)
values = [document.get(key) for key in keys]
if any(type(value) is not int or value < 0 for value in values):
    raise SystemExit(1)
print(":".join(str(value) for value in values))
' || return 1
}

provider_gateway_counter_vector() {
  printf '%s' "$1" | python3 -c '
import re, sys
text = sys.stdin.read()
def scalar(name):
    matches = re.findall(r"^" + re.escape(name) + r" ([0-9]+)$", text, re.M)
    if len(matches) != 1:
        raise SystemExit(1)
    return int(matches[0])
failure_calls = sum(
    int(value)
    for outcome, value in re.findall(
        r"^rpc_upstream_calls_total\{method=\"[^\"]+\",outcome=\"(timeout|rate_limited|failure)\",provider_slot=\"[^\"]+\"\} ([0-9]+)$",
        text,
        re.M,
    )
)
values = [
    scalar("rpc_state_request_budget_rejected_total"),
    scalar("rpc_upstream_call_budget_rejected_total"),
    scalar("rpc_provider_unavailable_total"),
    scalar("rpc_provider_rate_limited_total"),
    scalar("rpc_provider_cooldown_total"),
    scalar("rpc_provider_disagreement_total"),
    failure_calls,
]
print(":".join(str(value) for value in values))
' || return 1
}

require_hunter_ready() {
  printf '%s' "$1" | python3 -c '
import json, sys
document = json.load(sys.stdin)
required_true = ("ok", "service_health", "hunting_health", "exact_execution_readiness", "atlas_connected")
if any(document.get(key) is not True for key in required_true):
    raise SystemExit(1)
if document.get("rpc_authority_mode") != "single_primary":
    raise SystemExit(1)
if document.get("primary_provider") != "production-nownodes-arbitrum":
    raise SystemExit(1)
if document.get("confirmation_provider") is not None or document.get("provider_quorum") != 1:
    raise SystemExit(1)
if document.get("degraded_reason") not in (None, ""):
    raise SystemExit(1)
if document.get("provider_recovery_state") != "ready":
    raise SystemExit(1)
if document.get("provider_recovery_sample_count") != 3:
    raise SystemExit(1)
if len(document.get("provider_recovery_samples") or []) != 3:
    raise SystemExit(1)
if document.get("provider_current_class_failure_streak") != 0:
    raise SystemExit(1)
' || fail "Single-Primary Exact readiness regressed"
}

require_latency_gauges() {
  printf '%s' "$1" | python3 -c '
import re, sys
text = sys.stdin.read()
def value(name):
    matches = re.findall(r"^" + re.escape(name) + r" ([0-9.eE+-]+)$", text, re.M)
    if len(matches) != 1:
        raise SystemExit(1)
    return float(matches[0])
depth = value("phoenix_exact_queue_depth")
oldest = value("phoenix_exact_oldest_actionable_age_seconds")
available = value("phoenix_exact_worker_permits_available")
if oldest > 1.0:
    raise SystemExit(1)
if depth > 0 and available > 0:
    raise SystemExit(1)
' || fail "Exact queue progress/age SLO regressed"
}

require_latency_histograms() {
  printf '%s\n# PHOENIX_MONITOR_INTERVAL_BOUNDARY\n%s' "$1" "$2" | python3 -c '
import json, math, re, sys
documents = sys.stdin.read().split("\n# PHOENIX_MONITOR_INTERVAL_BOUNDARY\n")
if len(documents) != 2:
    raise SystemExit(1)
baseline, final = documents
def cumulative_histogram(text, name):
    count_match = re.findall(r"^" + re.escape(name) + r"_count ([0-9]+)$", text, re.M)
    if len(count_match) != 1:
        raise SystemExit(1)
    count = int(count_match[0])
    buckets = {}
    pattern = r"^" + re.escape(name) + r"_bucket\{le=\"([^\"]+)\"\} ([0-9]+)$"
    for label, value in re.findall(pattern, text, re.M):
        if label != "+Inf":
            buckets[float(label)] = int(value)
    return count, buckets
def histogram(name):
    baseline_count, baseline_buckets = cumulative_histogram(baseline, name)
    final_count, final_buckets = cumulative_histogram(final, name)
    if final_count <= baseline_count or set(final_buckets) != set(baseline_buckets):
        raise SystemExit(1)
    count = final_count - baseline_count
    buckets = {}
    for boundary, final_value in final_buckets.items():
        baseline_value = baseline_buckets[boundary]
        if final_value < baseline_value:
            raise SystemExit(1)
        buckets[boundary] = final_value - baseline_value
    return count, buckets
def quantile(name, q):
    count, buckets = histogram(name)
    target = math.ceil(count * q)
    for boundary in sorted(buckets):
        if buckets[boundary] >= target:
            return boundary
    raise SystemExit(1)
limits = {
    "phoenix_signal_to_prefilter_seconds": (0.025, 0.050),
    "phoenix_liquidatable_to_exact_enqueue_seconds": (0.020, 0.050),
    "phoenix_exact_queue_wait_seconds": (0.050, 0.250),
    "phoenix_exact_first_rpc_dispatch_seconds": (0.100, 0.250),
    "phoenix_exact_rpc_state_fetch_seconds": (1.000, 2.500),
    "phoenix_exact_compute_seconds": (0.100, 0.250),
    "phoenix_exact_end_to_end_seconds": (2.000, 5.000),
}
evidence = {}
for name, (p95_limit, p99_limit) in limits.items():
    p95 = quantile(name, 0.95)
    p99 = quantile(name, 0.99)
    if p95 > p95_limit or p99 > p99_limit:
        raise SystemExit(1)
    count, buckets = histogram(name)
    evidence[name] = {"count": count, "p95_upper_bound_seconds": p95, "p99_upper_bound_seconds": p99}
count, buckets = histogram("phoenix_exact_end_to_end_seconds")
if buckets.get(5.0, -1) != count:
    raise SystemExit(1)
print("POST_ARM_LATENCY_SLO_OK: " + json.dumps(evidence, sort_keys=True, separators=(",", ":")))
' || fail "Exact latency histogram SLO regressed"
}

require_disarmed_controls() {
  printf '%s' "$1" | python3 -c '
import json, sys
document = json.load(sys.stdin)
global_control = document.get("global") or {}
if global_control.get("armed") is not False or global_control.get("kill_switch") is not True:
    raise SystemExit(1)
if global_control.get("execution_mode") != "disarmed":
    raise SystemExit(1)
route = document.get("route") or {}
if route.get("enabled") is not False or route.get("kill_switch") is not True:
    raise SystemExit(1)
lanes = {row.get("lane"): row for row in (document.get("revenue_lanes") or [])}
if set(lanes) != {"aave_liquidation", "atlas_solver"}:
    raise SystemExit(1)
if any(row.get("armed") is not False or row.get("kill_switch") is not True for row in lanes.values()):
    raise SystemExit(1)
economic = document.get("economic") or {}
if economic.get("phase") != "DISARMED_EVIDENCE":
    raise SystemExit(1)
provider = document.get("provider_execution_authority") or {}
if provider.get("exact_execution_ready") is not True or provider.get("sample_count") != 3:
    raise SystemExit(1)
' || fail "disarmed revenue/Generic/provider controls regressed"
}

require_armed_controls() {
  printf '%s' "$1" | python3 -c '
import json, sys
document = json.load(sys.stdin)
global_control = document.get("global") or {}
if global_control.get("armed") is not False or global_control.get("kill_switch") is not True:
    raise SystemExit(1)
if global_control.get("execution_mode") != "disarmed":
    raise SystemExit(1)
route = document.get("route") or {}
if route.get("enabled") is not False or route.get("kill_switch") is not True:
    raise SystemExit(1)
lanes = {row.get("lane"): row for row in (document.get("revenue_lanes") or [])}
if set(lanes) != {"aave_liquidation", "atlas_solver"}:
    raise SystemExit(1)
for row in lanes.values():
    if row.get("armed") is not True or row.get("kill_switch") is not False:
        raise SystemExit(1)
    if row.get("maximum_input_amount") != "10000000000000000":
        raise SystemExit(1)
    if row.get("retained_profit_floor") != "1000000000000":
        raise SystemExit(1)
economic = document.get("economic") or {}
if economic.get("phase") != "DISARMED_EVIDENCE":
    raise SystemExit(1)
if economic.get("current_size_level") != "MAX_REVIEWED":
    raise SystemExit(1)
if economic.get("current_input_wei") != "10000000000000000":
    raise SystemExit(1)
provider = document.get("provider_execution_authority") or {}
if provider.get("exact_execution_ready") is not True or provider.get("sample_count") != 3:
    raise SystemExit(1)
' || fail "armed revenue/Generic/economic/provider controls regressed"
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
baseline_runtime_evidence=$(runtime_evidence) || fail "exact runtime identity/restart/OOM evidence is unavailable"
baseline_hunter_health=$(hunter_health) || fail "hunter readiness evidence is unavailable"
require_hunter_ready "$baseline_hunter_health"
baseline_hunter_metrics=$(hunter_metrics) || fail "hunter metrics evidence is unavailable"
baseline_gateway_metrics=$(gateway_metrics) || fail "gateway metrics evidence is unavailable"
baseline_provider_health=$(provider_health_counter_vector "$baseline_hunter_health") ||
  fail "provider health counter evidence is unavailable"
baseline_provider_gateway=$(provider_gateway_counter_vector "$baseline_gateway_metrics") ||
  fail "provider gateway counter evidence is unavailable"
baseline_primary_exact=$(printf '%s' "$baseline_hunter_health" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("last_primary_exact_at") or "")') ||
  fail "primary Exact sample evidence is unavailable"
[ -n "$baseline_primary_exact" ] || fail "primary Exact sample evidence is empty"

started_at=$(date +%s)
deadline=$((started_at + duration_seconds))
while :; do
  [ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$release_sha" ] ||
    fail "active release changed during monitoring"
  [ "$(tr -d '\r\n' <"$deploy_dir/release-assets.sha")" = "$release_sha" ] ||
    fail "release assets changed during monitoring"
  require_operator_live_mode

  if [ "$authority_state" = "armed" ]; then
    compose run --rm --no-deps \
      -e PHOENIX_RELEASE_SHA="$release_sha" \
      --entrypoint /usr/local/bin/autonomous-live-control \
      live-executor owner-live-preflight
  else
    compose run --rm --no-deps autonomous-control owner-configured-preflight
  fi
  current_control_status=$(compose run --rm --no-deps autonomous-control status) ||
    fail "revenue control status is unavailable"
  if [ "$authority_state" = "armed" ]; then
    require_armed_controls "$current_control_status"
  else
    require_disarmed_controls "$current_control_status"
  fi
  compose run --rm --no-deps autonomous-control reconciliation-status

  current_hunter_health=$(hunter_health) || fail "hunter readiness evidence is unavailable"
  require_hunter_ready "$current_hunter_health"
  current_hunter_metrics=$(hunter_metrics) || fail "hunter metrics evidence is unavailable"
  current_gateway_metrics=$(gateway_metrics) || fail "gateway metrics evidence is unavailable"
  require_latency_gauges "$current_hunter_metrics"
  [ "$(provider_health_counter_vector "$current_hunter_health")" = "$baseline_provider_health" ] ||
    fail "hunter provider failure/recovery counters regressed"
  [ "$(provider_gateway_counter_vector "$current_gateway_metrics")" = "$baseline_provider_gateway" ] ||
    fail "gateway provider/budget/error counters regressed"

  current_executor=$(compose ps --status running -q live-executor)
  [ "$current_executor" = "$baseline_executor" ] ||
    fail "live-executor identity changed during monitoring"
  [ "$(docker inspect -f '{{.RestartCount}}' "$current_executor")" = "$baseline_restarts" ] ||
    fail "live-executor restarted during monitoring"
  [ "$(runtime_evidence)" = "$baseline_runtime_evidence" ] ||
    fail "exact runtime identity/restart/OOM evidence regressed"
  require_operator_live_mode

  now=$(date +%s)
  [ "$now" -lt "$deadline" ] || break
  remaining=$((deadline - now))
  interval=60
  [ "$remaining" -ge "$interval" ] || interval=$remaining
  sleep "$interval"
done

final_primary_exact=$(printf '%s' "$current_hunter_health" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("last_primary_exact_at") or "")') ||
  fail "final primary Exact sample evidence is unavailable"
[ -n "$final_primary_exact" ] && [ "$final_primary_exact" != "$baseline_primary_exact" ] ||
  fail "primary Exact samples did not advance during monitoring"
require_latency_histograms "$baseline_hunter_metrics" "$current_hunter_metrics"

trap - EXIT
echo "POST_ARM_REVENUE_MONITOR_OK: release=$release_sha duration_seconds=$duration_seconds authority_state=$authority_state"
