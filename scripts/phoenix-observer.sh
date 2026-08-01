#!/usr/bin/env bash
set -u
export LC_ALL=C
export LANG=C

NOW_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
CRITICAL_SERVICES="rpc-gateway phoenix-engine live-executor feed-ingestor recorder shadow-dispatcher atlas-observer postgres nats prometheus dashboard"

section() {
  printf '\n============================================================\n'
  printf '%s\n' "$1"
  printf '============================================================\n'
}

service_id() {
  docker ps -aq \
    --filter "label=com.docker.compose.service=$1" |
    awk 'NF { print; exit }'
}

health_value() {
  docker inspect \
    --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
    "$1" 2>/dev/null || printf 'inspect_failed'
}

redact_stream() {
  sed -E \
    -e 's#(https?|wss?|postgres(ql)?|nats)://[^[:space:]"]+#<REDACTED_URL>#g' \
    -e 's#((password|passwd|secret|token|authorization|private[_-]?key)[[:space:]]*[:=][[:space:]]*)[^[:space:]"]+#\1<REDACTED>#Ig'
}

evaluate_live_executor_expectation() {
  python3 -c '
import json
import sys

SCHEMA = "phoenix.live-executor-expectation.v1"
EXECUTABLE_PHASES = {
    "CANARY_READY",
    "LIVE_CANARY_MIN",
    "LIVE_SCALE_L1",
    "LIVE_SCALE_L2",
    "LIVE_SCALE_L3",
    "LIVE_SCALE_L4",
    "LIVE_SCALE_L5",
    "LIVE_MAX_REVIEWED",
}


def emit(expected_state, observed_state, result, reason, checks):
    print(json.dumps({
        "checks": checks,
        "expected_state": expected_state,
        "observed_state": observed_state,
        "reason": reason,
        "result": result,
        "schema": SCHEMA,
    }, sort_keys=True, separators=(",", ":")))


try:
    evidence = json.load(sys.stdin)
except Exception:
    emit("unknown", "unknown", "critical", "bounded_evidence_invalid", {})
    raise SystemExit(0)

if not isinstance(evidence, dict):
    emit("unknown", "unknown", "critical", "bounded_evidence_invalid", {})
    raise SystemExit(0)

observed = evidence.get("observed_state")
phase = evidence.get("economic_phase")
armed = evidence.get("armed")
kill_switch = evidence.get("kill_switch")
route_count = evidence.get("executable_route_count")
contract_paused = evidence.get("contract_paused")
request_count = evidence.get("execution_request_count")
active_attempts = evidence.get("active_attempts")
unresolved = evidence.get("unresolved_submissions")
activation = evidence.get("activation_path")
activation_completed = evidence.get("activation_completed")

types_valid = (
    observed in {"running", "stopped"}
    and isinstance(phase, str)
    and type(armed) is bool
    and type(kill_switch) is bool
    and type(route_count) is int
    and route_count >= 0
    and type(contract_paused) is bool
    and type(request_count) is int
    and request_count >= 0
    and type(active_attempts) is int
    and active_attempts >= 0
    and type(unresolved) is int
    and unresolved >= 0
    and isinstance(activation, dict)
    and set(activation) == {"active", "enabled", "sub_state"}
    and type(activation.get("active")) is bool
    and type(activation.get("enabled")) is bool
    and isinstance(activation.get("sub_state"), str)
    and type(activation_completed) is bool
)
if not types_valid:
    emit("unknown", observed if observed in {"running", "stopped"} else "unknown",
         "critical", "bounded_evidence_invalid", {})
    raise SystemExit(0)

checks = {
    "activation_completed": activation_completed,
    "activation_enabled_waiting": (
        activation["enabled"]
        and activation["active"]
        and activation["sub_state"] == "waiting"
    ),
    "active_attempts_zero": active_attempts == 0,
    "contract_paused": contract_paused,
    "economic_phase_disarmed_evidence": phase == "DISARMED_EVIDENCE",
    "execution_requests_zero": request_count == 0,
    "global_disarmed": armed is False and kill_switch is True,
    "routes_fail_closed": route_count == 0,
    "unresolved_submissions_zero": unresolved == 0,
}
safe_stopped = (
    checks["economic_phase_disarmed_evidence"]
    and checks["global_disarmed"]
    and checks["routes_fail_closed"]
    and checks["contract_paused"]
    and checks["execution_requests_zero"]
    and checks["active_attempts_zero"]
    and checks["unresolved_submissions_zero"]
    and checks["activation_enabled_waiting"]
    and not checks["activation_completed"]
)

if observed == "running":
    if safe_stopped:
        emit("stopped", observed, "critical",
             "live_executor_running_while_disarmed", checks)
    else:
        emit("running", observed, "healthy_observed",
             "executable_runtime_observed", checks)
    raise SystemExit(0)

if activation_completed:
    reason = "activation_completed_executor_missing"
elif phase in EXECUTABLE_PHASES:
    reason = "executable_phase_executor_missing"
elif request_count != 0:
    reason = "execution_request_executor_missing"
elif active_attempts != 0:
    reason = "active_attempt_executor_missing"
elif unresolved != 0:
    reason = "unresolved_submission_executor_missing"
elif armed is not False or kill_switch is not True or route_count != 0:
    reason = "armed_control_executor_missing"
elif contract_paused is not True:
    reason = "unpaused_contract_executor_missing"
elif phase != "DISARMED_EVIDENCE":
    reason = "unexpected_phase_executor_missing"
elif not checks["activation_enabled_waiting"]:
    reason = "activation_path_not_waiting"
elif safe_stopped:
    emit("stopped", observed, "healthy_expected",
         "disarmed_evidence_contract_paused", checks)
    raise SystemExit(0)
else:
    reason = "bounded_evidence_invalid"

emit("running", observed, "critical", reason, checks)
'
}

if [ "${1:-}" = "--evaluate-live-executor-expectation" ]; then
  [ "$#" -eq 1 ] || {
    printf '{"expected_state":"unknown","observed_state":"unknown","reason":"arguments_invalid","result":"critical","schema":"phoenix.live-executor-expectation.v1"}\n'
    exit 64
  }
  evaluate_live_executor_expectation
  exit 0
fi

[ "$#" -eq 0 ] || {
  printf 'PHOENIX_OBSERVER_FAILED: arguments_invalid\n' >&2
  exit 64
}

printf 'PHOENIX_AUDIT_BEGIN\n'
printf 'audit_time_utc=%s\n' "$NOW_UTC"
printf 'hostname=%s\n' "$(hostname)"
printf 'observer_version=phoenix-observer.v2-bounded\n'

section "1. EXECUTIVE RELEASE AND CONTROL STATE"

printf 'current_release='
cat /opt/phoenix/deploy/current-release 2>/dev/null || printf 'unavailable\n'

printf 'release_assets_sha='
cat /opt/phoenix/deploy/release-assets.sha 2>/dev/null || printf 'unavailable\n'

printf '\noperator_mode_flags:\n'
grep -E '^(PHOENIX_MODE|LIVE_EXECUTION|AUTONOMOUS_EXECUTION)=' \
  /etc/phoenix/phoenix.env 2>/dev/null ||
  printf 'mode_flags_unavailable\n'

printf '\nrelease_gateway_status:\n'
/usr/local/sbin/phoenix-release-gateway status 2>&1 ||
  printf '{"status":"error","code":"gateway_status_failed"}\n'

printf '\nactive_release_context_summary:\n'
python3 - <<'PY'
import json
from pathlib import Path

path = Path("/opt/phoenix/deploy/current-release-context.json")
if not path.is_file():
    print('{"status":"unavailable","reason":"current_release_context_missing"}')
    raise SystemExit(0)

try:
    data = json.loads(path.read_text(encoding="utf-8"))
except Exception:
    print('{"status":"unavailable","reason":"current_release_context_invalid"}')
    raise SystemExit(0)

interesting = {
    "schema",
    "release_sha",
    "mode",
    "live_execution",
    "autonomous_execution",
    "generated_at",
    "validated_at",
    "build_run_id",
    "source_ci_run_id",
}

def collect(value, out):
    if isinstance(value, dict):
        for key, nested in value.items():
            if key in interesting and key not in out:
                out[key] = nested
            collect(nested, out)
    elif isinstance(value, list):
        for nested in value:
            collect(nested, out)

out = {}
collect(data, out)
print(json.dumps(out, sort_keys=True, indent=2))
PY

section "2. LIVE EXECUTOR / CONTRACT / MONEY-PATH CONTROL"

live_id="$(service_id live-executor)"
live_executor_observed_state="stopped"
if [ -n "$live_id" ]; then
  live_executor_running="$(
    docker inspect --format '{{.State.Running}}' "$live_id" 2>/dev/null ||
      printf unknown
  )"
  if [ "$live_executor_running" = "true" ]; then
    live_executor_observed_state="running"
  fi
fi

release_sha="$(
  tr -d '\r\n' </opt/phoenix/deploy/current-release 2>/dev/null || true
)"
contract_paused="unknown"
case "$release_sha" in
  *[!0-9a-f]*|"") ;;
  *)
    if [ "${#release_sha}" -eq 40 ]; then
      contract_paused="$(
        python3 - "$release_sha" <<'PY'
import json
import sys
from pathlib import Path

path = Path("/var/lib/phoenix-release/releases") / sys.argv[1] / "state.json"
try:
    value = json.loads(path.read_text(encoding="utf-8")).get("contract_paused")
except Exception:
    value = None
if value is True:
    print("true")
elif value is False:
    print("false")
else:
    print("unknown")
PY
      )"
    fi
    ;;
esac

pg_id="$(service_id postgres)"
runtime_evidence='{}'
if [ -n "$pg_id" ]; then
  runtime_evidence="$(
    docker exec -i "$pg_id" sh -lc '
      exec psql -X -qAt -v ON_ERROR_STOP=1 \
        -U "$POSTGRES_USER" -d "$POSTGRES_DB"
    ' <<'SQL' 2>/dev/null || printf '{}'
SELECT json_build_object(
    'economic_phase', economic.phase,
    'armed', global_control.armed,
    'kill_switch', global_control.kill_switch,
    'executable_route_count', (
        SELECT count(*)
        FROM live_canary.autonomous_route_controls
        WHERE enabled OR NOT kill_switch
    ),
    'execution_request_count', (
        SELECT count(*) FROM live_canary.execution_requests
    ),
    'active_attempts', (
        SELECT count(*)
        FROM live_canary.execution_attempts
        WHERE status IN (
            'claimed', 'nonce_allocated', 'submission_unknown',
            'pending', 'timed_out'
        )
    ),
    'unresolved_submissions', (
        SELECT count(*)
        FROM live_canary.execution_attempts
        WHERE status IN ('submission_unknown', 'pending', 'timed_out')
    )
)::text
FROM live_canary.economic_control AS economic
CROSS JOIN live_canary.autonomous_global_control AS global_control
WHERE economic.singleton AND global_control.singleton;
SQL
  )"
fi

activation_enabled="$(
  systemctl is-enabled phoenix-economic-activation.path 2>/dev/null || true
)"
activation_active="$(
  systemctl is-active phoenix-economic-activation.path 2>/dev/null || true
)"
activation_sub_state="$(
  systemctl show phoenix-economic-activation.path \
    -p SubState --value 2>/dev/null || true
)"

live_executor_evidence="$(
  PHOENIX_OBSERVER_RUNTIME_EVIDENCE="$runtime_evidence" \
  PHOENIX_OBSERVER_CONTRACT_PAUSED="$contract_paused" \
  PHOENIX_OBSERVER_ACTIVATION_ENABLED="$activation_enabled" \
  PHOENIX_OBSERVER_ACTIVATION_ACTIVE="$activation_active" \
  PHOENIX_OBSERVER_ACTIVATION_SUB_STATE="$activation_sub_state" \
  PHOENIX_OBSERVER_EXECUTOR_STATE="$live_executor_observed_state" \
    python3 -c '
import json
import os

try:
    value = json.loads(os.environ["PHOENIX_OBSERVER_RUNTIME_EVIDENCE"])
except Exception:
    value = {}
value["contract_paused"] = {
    "true": True,
    "false": False,
}.get(os.environ.get("PHOENIX_OBSERVER_CONTRACT_PAUSED"))
value["activation_path"] = {
    "active": os.environ.get("PHOENIX_OBSERVER_ACTIVATION_ACTIVE") == "active",
    "enabled": os.environ.get("PHOENIX_OBSERVER_ACTIVATION_ENABLED") == "enabled",
    "sub_state": os.environ.get("PHOENIX_OBSERVER_ACTIVATION_SUB_STATE", ""),
}
value["activation_completed"] = value.get("economic_phase") in {
    "CANARY_READY",
    "LIVE_CANARY_MIN",
    "LIVE_SCALE_L1",
    "LIVE_SCALE_L2",
    "LIVE_SCALE_L3",
    "LIVE_SCALE_L4",
    "LIVE_SCALE_L5",
    "LIVE_MAX_REVIEWED",
}
value["observed_state"] = os.environ.get("PHOENIX_OBSERVER_EXECUTOR_STATE")
print(json.dumps(value, sort_keys=True, separators=(",", ":")))
'
)"
live_executor_expectation="$(
  printf '%s\n' "$live_executor_evidence" |
    evaluate_live_executor_expectation
)"
live_executor_expectation_result="$(
  printf '%s\n' "$live_executor_expectation" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("result", "critical"))' \
      2>/dev/null || printf critical
)"
live_executor_expectation_reason="$(
  printf '%s\n' "$live_executor_expectation" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("reason", "bounded_evidence_invalid"))' \
      2>/dev/null || printf bounded_evidence_invalid
)"
printf 'live_executor_expectation=%s\n' "$live_executor_expectation"

if [ -n "$live_id" ]; then
  printf 'live_executor_container=%s\n' "$live_id"
  docker exec "$live_id" \
    /usr/local/bin/autonomous-live-control status 2>&1 |
    redact_stream ||
    printf 'live_executor_status_failed\n'
else
  printf 'live_executor_container=missing\n'
fi

section "3. BUSINESS SNAPSHOT"

python3 - <<'PY'
import json
from datetime import datetime, timezone
from pathlib import Path

path = Path("/opt/phoenix/evidence/dashboard/latest-dashboard.json")
if not path.is_file():
    print(json.dumps({
        "status": "unavailable",
        "path": str(path),
        "reason": "dashboard_snapshot_missing",
    }, indent=2))
    raise SystemExit(0)

try:
    data = json.loads(path.read_text(encoding="utf-8"))
except Exception:
    print(json.dumps({
        "status": "unavailable",
        "path": str(path),
        "reason": "dashboard_snapshot_invalid",
    }, indent=2))
    raise SystemExit(0)

def obj(name):
    value = data.get(name)
    return value if isinstance(value, dict) else {}

def selected(source, keys):
    return {key: source.get(key) for key in keys if key in source}

generated = data.get("generated_at")
age = None
if isinstance(generated, str):
    try:
        parsed = datetime.fromisoformat(generated.replace("Z", "+00:00"))
        age = int((datetime.now(timezone.utc) - parsed.astimezone(timezone.utc)).total_seconds())
    except Exception:
        pass

business = obj("business")
profitability = obj("profitability")
summary = profitability.get("summary")
if not isinstance(summary, dict):
    summary = {}

output = {
    "snapshot": {
        "path": str(path),
        "schema": data.get("schema"),
        "generated_at": generated,
        "age_seconds": age,
        "window_hours": data.get("window_hours"),
        "prelive_schema_warning": str(data.get("schema", "")).startswith("phoenix.prelive."),
    },
    "safety": selected(obj("safety"), (
        "mode",
        "live_execution",
        "execution_eligible",
        "execution_request_created",
        "prelive_lock",
    )),
    "business": selected(business, (
        "sample_count",
        "independently_verified_count",
        "fork_successful_count",
        "nearest_to_profitable_count",
        "active_shadow_routes",
    )),
    "funnel": data.get("funnel", []),
    "profitability_summary": selected(summary, (
        "gross_profit",
        "total_cost",
        "net_pnl",
        "expected_net_pnl",
        "conservative_net_pnl",
        "severe_net_pnl",
        "fork_simulated_net_pnl",
    )),
    "feed": selected(obj("feed"), (
        "completeness_status",
        "gap_count",
        "missing_sequences",
        "reconnects",
        "most_recent_gap_at",
    )),
    "rpc": selected(obj("rpc"), (
        "secondary_requested",
        "agreed",
        "disagreed",
        "state_freshness_seconds",
        "pinned_block_status",
    )),
    "money_path_ingress": selected(obj("money_path_ingress"), (
        "feed_inputs_total",
        "irrelevant_filtered_total",
        "unsupported_interesting_total",
        "relevant_route_inputs_total",
        "persistence_ratio",
        "raw_rows_avoided_total",
        "relevant_transactions_committed_total",
        "relevant_transaction_failures_total",
        "dispatcher_rows_published_total",
        "dispatcher_pending_rows_estimate",
        "dispatcher_oldest_claimable_age_seconds",
        "projected_disk_runway_days",
    )),
    "postgres": selected(obj("postgres"), (
        "readiness",
        "database_size_bytes",
        "growth_bytes_1h",
        "growth_bytes_6h",
        "growth_bytes_24h",
        "projected_disk_headroom_bytes",
        "active_connections",
        "wal_bytes",
        "retention_status",
        "migration_version",
        "migration_checksum",
    )),
    "reliability": selected(obj("reliability"), (
        "retry_attempts",
        "recovered_retries",
        "exhausted_or_quarantined",
        "terminal_integrity_failures",
        "restart_loops",
        "later_message_progress_after_quarantine",
        "protected_service_identity_status",
    )),
    "fork": selected(obj("fork"), (
        "unsigned_plan_count",
        "simulations",
        "success",
        "reverted",
        "gas_used",
        "balance_delta",
        "simulated_net_pnl",
        "absolute_prediction_error",
        "fork_block",
        "contract_guard_failures",
    )),
    "alerts": data.get("alerts", []),
}

print(json.dumps(output, sort_keys=True, indent=2))
PY

section "4. CONTAINER HEALTH, IMAGE AND RESTART STATE"

printf '%-24s %-18s %-10s %-10s %-8s %-12s %s\n' \
  "SERVICE" "CONTAINER" "STATE" "HEALTH" "RESTART" "OOM" "IMAGE"

for cid in $(docker ps -aq --filter label=com.docker.compose.service); do
  docker inspect --format \
    '{{index .Config.Labels "com.docker.compose.service"}}|{{.Name}}|{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.RestartCount}}|{{.State.OOMKilled}}|{{.Config.Image}}' \
    "$cid" 2>/dev/null
done |
  sed 's#^|#unknown|#' |
  sort |
  awk -F'|' '{
    gsub("^/","",$2);
    printf "%-24s %-18s %-10s %-10s %-8s %-12s %s\n",
      $1,$2,$3,$4,$5,$6,$7
  }'

printf '\ncritical_service_failures:\n'
critical_failures=0
for service in $CRITICAL_SERVICES; do
  cid="$(service_id "$service")"
  if [ "$service" = "live-executor" ]; then
    case "$live_executor_expectation_result" in
      healthy_expected)
        printf 'HEALTHY_EXPECTED service=%s state=stopped reason=%s\n' \
          "$service" "$live_executor_expectation_reason"
        continue
        ;;
      critical)
        printf 'CRITICAL service=%s state=%s reason=%s\n' \
          "$service" "$live_executor_observed_state" \
          "$live_executor_expectation_reason"
        critical_failures=$((critical_failures + 1))
        continue
        ;;
    esac
  fi
  if [ -z "$cid" ]; then
    printf 'MISSING service=%s\n' "$service"
    critical_failures=$((critical_failures + 1))
    continue
  fi

  state="$(docker inspect --format '{{.State.Status}}' "$cid" 2>/dev/null || printf unknown)"
  health="$(health_value "$cid")"
  restart="$(docker inspect --format '{{.RestartCount}}' "$cid" 2>/dev/null || printf unknown)"

  if [ "$state" != "running" ] ||
     { [ "$health" != "healthy" ] && [ "$health" != "none" ]; } ||
     { [ "$restart" != "0" ] && [ "$restart" != "unknown" ]; }
  then
    printf 'DEGRADED service=%s state=%s health=%s restarts=%s\n' \
      "$service" "$state" "$health" "$restart"
    critical_failures=$((critical_failures + 1))
  fi
done
printf 'critical_failure_count=%s\n' "$critical_failures"

section "5. SERVER CAPACITY AND RESOURCE PRESSURE"

printf 'uptime:\n'
uptime

printf '\nmemory:\n'
free -h

printf '\ndisk:\n'
df -hT / /opt/phoenix 2>/dev/null | awk '!seen[$7]++'

printf '\ninodes:\n'
df -ih / /opt/phoenix 2>/dev/null | awk '!seen[$6]++'

printf '\ndocker_disk_usage:\n'
docker system df 2>&1

printf '\ncontainer_resource_snapshot:\n'
docker stats --no-stream \
  --format 'table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}\t{{.NetIO}}\t{{.BlockIO}}\t{{.PIDs}}' \
  2>&1

printf '\nload_and_pressure:\n'
cat /proc/loadavg
for file in /proc/pressure/cpu /proc/pressure/memory /proc/pressure/io; do
  if [ -r "$file" ]; then
    printf '%s:\n' "$file"
    cat "$file"
  fi
done

section "6. PROMETHEUS TARGETS AND KEY METRICS"

python3 - <<'PY'
import json
import re
import urllib.parse
import urllib.request

BASE = "http://127.0.0.1:9090"

def get(path):
    with urllib.request.urlopen(BASE + path, timeout=8) as response:
        return json.load(response)

try:
    payload = get("/api/v1/targets?state=active")
    active = payload.get("data", {}).get("activeTargets", [])
    targets = []
    for item in active:
        labels = item.get("labels") or {}
        targets.append({
            "job": labels.get("job"),
            "instance": labels.get("instance"),
            "health": item.get("health"),
            "last_scrape": item.get("lastScrape"),
            "last_error": item.get("lastError") or "",
        })
    print("targets=")
    print(json.dumps(targets, sort_keys=True, indent=2))
except Exception as exc:
    print(json.dumps({"targets_error": type(exc).__name__}))

queries = {
    "up_total": "sum(up)",
    "down_targets": "count(up == 0)",
    "engine_terminal_integrity_total": "sum(phoenix_engine_terminal_integrity_total)",
    "engine_process_exit_total": "sum(phoenix_engine_runtime_exits_total)",
    "engine_terminal_integrity_rate_5m": "sum(rate(phoenix_engine_terminal_integrity_total[5m]))",
    "engine_process_exit_rate_5m": "sum(rate(phoenix_engine_runtime_exits_total[5m]))",
}

results = {}
for label, query in queries.items():
    try:
        path = "/api/v1/query?" + urllib.parse.urlencode({"query": query})
        result = get(path).get("data", {}).get("result", [])
        results[label] = result
    except Exception as exc:
        results[label] = {"error": type(exc).__name__}

print("key_queries=")
print(json.dumps(results, sort_keys=True, indent=2))

try:
    names = get("/api/v1/label/__name__/values").get("data", [])
    pattern = re.compile(
        r"(phoenix|rpc|feed|recorder|dispatcher|jetstream|execution|"
        r"candidate|opportun|transaction|receipt|profit|pnl|integrity|"
        r"quarantine|backlog|reconnect|gap)",
        re.I,
    )
    selected = sorted(name for name in names if pattern.search(name))[:250]
    print("relevant_metric_names=")
    print(json.dumps(selected, indent=2))
except Exception as exc:
    print(json.dumps({"metric_names_error": type(exc).__name__}))
PY

section "7. SERVICE METRIC SAMPLES"

print_metrics() {
  service="$1"
  port="$2"
  cid="$(service_id "$service")"

  printf '\n--- %s metrics ---\n' "$service"

  if [ -z "$cid" ]; then
    printf 'service_missing\n'
    return
  fi

  docker exec "$cid" \
    wget -q -O - "http://127.0.0.1:${port}/metrics" 2>/dev/null |
    grep -v '^#' |
    grep -Ei \
      '(candidate|opportun|profit|pnl|execution|transaction|receipt|revert|error|failure|integrity|quarantine|latency|backlog|gap|reconnect|provider|ready|processed|published|persist|retry|runtime_exit)' |
    head -n 160 ||
    printf 'no_selected_metrics_or_endpoint_unavailable\n'
}

print_metrics feed-ingestor 9100
print_metrics phoenix-engine 9200
print_metrics rpc-gateway 9300
print_metrics recorder 9400
print_metrics shadow-dispatcher 9500
print_metrics atlas-observer 9700

section "8. NATS JETSTREAM"

nats_id="$(service_id nats)"
if [ -n "$nats_id" ]; then
  docker exec "$nats_id" \
    wget -q -O - \
    'http://127.0.0.1:8222/jsz?streams=true&consumers=true' 2>/dev/null |
    python3 -c '
import json
import sys

try:
    data = json.load(sys.stdin)
except Exception:
    print("{\"status\":\"unavailable\",\"reason\":\"nats_jsz_invalid\"}")
    raise SystemExit(0)

stream_details = []
for account in data.get("account_details", []):
    if not isinstance(account, dict):
        continue
    values = account.get("stream_detail", [])
    if isinstance(values, list):
        stream_details.extend(values)

streams = []
for stream in stream_details:
    if not isinstance(stream, dict):
        continue
    state = stream.get("state") or {}
    config = stream.get("config") or {}
    streams.append({
        "name": config.get("name"),
        "messages": state.get("messages"),
        "bytes": state.get("bytes"),
        "first_seq": state.get("first_seq"),
        "last_seq": state.get("last_seq"),
        "consumer_count": state.get("consumer_count"),
    })

print(json.dumps({
    "server_id": data.get("server_id"),
    "memory": data.get("memory"),
    "storage": data.get("storage"),
    "stream_count": data.get("streams"),
    "consumer_count": data.get("consumers"),
    "streams": streams,
}, sort_keys=True, indent=2))
'
else
  printf '{"status":"unavailable","reason":"nats_container_missing"}\n'
fi

section "9. POSTGRESQL BUSINESS AND DATA-PLANE READOUT"

pg_id="$(service_id postgres)"
if [ -n "$pg_id" ]; then
  docker exec -i "$pg_id" sh -lc '
    exec psql \
      -X \
      -v ON_ERROR_STOP=1 \
      -U "$POSTGRES_USER" \
      -d "$POSTGRES_DB"
  ' <<'SQL'
\pset pager off
\pset fieldsep ' | '
\pset null '<null>'

\echo 'DATABASE_SUMMARY'
SELECT
    now() AT TIME ZONE 'UTC' AS observed_at_utc,
    current_database() AS database_name,
    pg_database_size(current_database()) AS database_bytes,
    numbackends AS active_connections,
    xact_commit,
    xact_rollback,
    blks_read,
    blks_hit,
    deadlocks,
    temp_bytes
FROM pg_stat_database
WHERE datname = current_database();

\echo 'RELEVANT_TABLE_ACTIVITY_ESTIMATES'
SELECT
    schemaname || '.' || relname AS table_name,
    n_live_tup AS estimated_live_rows,
    n_dead_tup AS estimated_dead_rows,
    seq_scan,
    idx_scan,
    n_tup_ins,
    n_tup_upd,
    n_tup_del,
    pg_total_relation_size(relid) AS total_bytes,
    last_analyze,
    last_autoanalyze,
    last_vacuum,
    last_autovacuum
FROM pg_stat_user_tables
WHERE relname ~* '(execution|opportun|candidate|transaction|receipt|pnl|profit|loss|approval|decision|route|shadow|event|message|dispatch|record)'
ORDER BY pg_total_relation_size(relid) DESC
LIMIT 50;

\echo 'RELEVANT_INDEX_ACTIVITY'
SELECT
    schemaname || '.' || relname AS table_name,
    indexrelname AS index_name,
    idx_scan,
    idx_tup_read,
    idx_tup_fetch,
    pg_relation_size(indexrelid) AS index_bytes
FROM pg_stat_user_indexes
WHERE relname ~* '(execution|opportun|candidate|transaction|receipt|pnl|profit|loss|approval|decision|route|shadow|event|message|dispatch|record)'
ORDER BY idx_scan DESC, pg_relation_size(indexrelid) DESC
LIMIT 50;

\echo 'RELEVANT_SCHEMA_COLUMNS'
SELECT
    table_schema || '.' || table_name AS table_name,
    string_agg(
        column_name || ':' || data_type,
        ', '
        ORDER BY ordinal_position
    ) AS relevant_columns
FROM information_schema.columns
WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
  AND table_name ~* '(execution|opportun|candidate|transaction|receipt|pnl|profit|loss|approval|decision|route|shadow|event|message|dispatch|record)'
  AND (
      column_name ~* '(status|state|result|decision|outcome|reason|failure|execution|receipt|profit|pnl|amount|gas|tx|hash|created|updated|observed|received|executed|submitted|confirmed|timestamp|event|time|at)$'
      OR column_name IN ('id', 'route_id', 'chain_id')
  )
GROUP BY table_schema, table_name
ORDER BY table_schema, table_name;

\echo 'DATABASE_LOCKS'
SELECT
    mode,
    granted,
    count(*) AS locks
FROM pg_locks
GROUP BY mode, granted
ORDER BY granted DESC, locks DESC;

\echo 'LONG_RUNNING_QUERIES'
SELECT
    pid,
    usename,
    application_name,
    state,
    wait_event_type,
    wait_event,
    age(clock_timestamp(), query_start) AS query_age,
    left(regexp_replace(query, '\s+', ' ', 'g'), 160) AS redacted_query_shape
FROM pg_stat_activity
WHERE datname = current_database()
  AND pid <> pg_backend_pid()
  AND query_start IS NOT NULL
  AND age(clock_timestamp(), query_start) > interval '5 seconds'
ORDER BY query_start
LIMIT 30;
SQL
else
  printf 'postgres_container_missing\n'
fi

section "10. RECENT ERROR / WARN / INTEGRITY SIGNALS"

for service in rpc-gateway phoenix-engine live-executor feed-ingestor recorder shadow-dispatcher; do
  cid="$(service_id "$service")"
  printf '\n--- %s recent signals ---\n' "$service"

  if [ -z "$cid" ]; then
    printf 'service_missing\n'
    continue
  fi

  docker logs --since 60m "$cid" 2>&1 |
    grep -Ei \
      '(error|warn|panic|fatal|revert|failed|failure|integrity|quarantine|timeout|rate.limit|unavailable|dropped|gap|reconnect)' |
    tail -n 30 |
    redact_stream ||
    printf 'no_matching_signals\n'
done

section "11. HOST AND DOCKER WARNINGS"

printf 'docker_daemon_warnings_last_hour:\n'
journalctl -u docker --since '-1 hour' -p warning --no-pager 2>/dev/null |
  tail -n 80 |
  redact_stream ||
  printf 'journal_unavailable_or_no_warnings\n'

printf '\nkernel_oom_and_io_signals:\n'
journalctl -k --since '-6 hours' --no-pager 2>/dev/null |
  grep -Ei '(oom|out of memory|killed process|i/o error|filesystem error|ext4|xfs|nvme|segfault)' |
  tail -n 80 ||
  printf 'no_matching_kernel_signals\n'

section "12. DASHBOARD ENDPOINT HEALTH"

dashboard_http="down"
prometheus_http="down"

if python3 - <<'PY'
import urllib.request
urllib.request.urlopen("http://127.0.0.1:8501/_stcore/health", timeout=4).read()
PY
then
  dashboard_http="up"
fi

if python3 - <<'PY'
import urllib.request
urllib.request.urlopen("http://127.0.0.1:9090/-/ready", timeout=4).read()
PY
then
  prometheus_http="up"
fi

printf 'dashboard_http=%s\n' "$dashboard_http"
printf 'prometheus_http=%s\n' "$prometheus_http"
printf 'dashboard_server_url=http://127.0.0.1:8501\n'
printf 'prometheus_server_url=http://127.0.0.1:9090\n'

section "13. HIGH-LEVEL AUDIT VERDICT"

pointer_a="$(tr -d '\r\n' </opt/phoenix/deploy/current-release 2>/dev/null || true)"
pointer_b="$(tr -d '\r\n' </opt/phoenix/deploy/release-assets.sha 2>/dev/null || true)"
mode="$(sed -n 's/^PHOENIX_MODE=//p' /etc/phoenix/phoenix.env 2>/dev/null | tail -n1)"
live="$(sed -n 's/^LIVE_EXECUTION=//p' /etc/phoenix/phoenix.env 2>/dev/null | tail -n1)"
auto="$(sed -n 's/^AUTONOMOUS_EXECUTION=//p' /etc/phoenix/phoenix.env 2>/dev/null | tail -n1)"

verdict="PASS"
reasons=""

if [ -z "$pointer_a" ] || [ "$pointer_a" != "$pointer_b" ]; then
  verdict="FAIL"
  reasons="${reasons} release_pointer_mismatch"
fi

if [ "$mode" != "LIVE" ] ||
   [ "$live" != "true" ] ||
   [ "$auto" != "true" ]
then
  verdict="FAIL"
  reasons="${reasons} live_control_mismatch"
fi

if [ "$critical_failures" -gt 0 ]; then
  verdict="FAIL"
  reasons="${reasons} critical_service_degraded"
fi

if [ "$dashboard_http" != "up" ] ||
   [ "$prometheus_http" != "up" ]
then
  if [ "$verdict" = "PASS" ]; then
    verdict="WARN"
  fi
  reasons="${reasons} monitoring_endpoint_degraded"
fi

printf 'AUDIT_VERDICT=%s\n' "$verdict"
printf 'AUDIT_REASONS=%s\n' "${reasons:- none}"
printf 'PHOENIX_AUDIT_END\n'
