#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
analyzer=$script_dir/prelive_money_path_report.py
workflow=$script_dir/prelive-money-path-report.sh
sql=$script_dir/sql/prelive-money-path-report.sql
schema=$repo_dir/schemas/prelive-money-path-summary.schema.json
source_fixture=$repo_dir/fixtures/reports/prelive_money_path_source.json
metrics_fixture=$repo_dir/fixtures/reports/prelive_money_path_metrics.json
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "prelive-money-path-report-tests: $1" >&2
  exit 1
}

if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  fail "python is unavailable"
fi

run_report() {
  "$python_command" "$analyzer" \
    --source "$1" \
    --metrics-input "$2" \
    --format "${3:-json}" \
    --window-hours 24
}

"$python_command" -m py_compile "$analyzer" || fail "analyzer does not compile"
report=$test_root/report.json
run_report "$source_fixture" "$metrics_fixture" >"$report" || fail "canonical report failed"

"$python_command" - "$report" <<'PY' || fail "canonical report contract failed"
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
raw = path.read_bytes()
assert len(raw) < 2 * 1024 * 1024
report = json.loads(raw)
assert report["schema_version"] == "phoenix.prelive.money-path-summary.v1"
assert report["metric_counter_scope"] == "process_lifetime"
assert report["mode"] == "SHADOW"
assert report["live_execution"] is False
assert report["execution_eligible"] is False
assert report["execution_request_created"] is False
assert report["technical"]["scrape_health"] == {
    "feed-ingestor": "1",
    "phoenix-engine": "1",
    "recorder": "1",
    "rpc-gateway": "1",
    "shadow-dispatcher": "1",
}
assert report["business"]["profitability_funnel"] == {
    "configured_route_matches": "30",
    "feed_inputs": "100",
    "fork_passed": "5",
    "normalized_transactions": "80",
    "official_router_inputs": "50",
    "primary_profitable": "7",
    "shadow_accepted": "5",
    "supported_exact_input_inputs": "40",
}
assert report["business"]["profitability"]["sum_expected_net_pnl"] == "5000"
assert report["technical"]["fork"]["simulations_total"] == "6"
assert len(report["metric_series"]) < 2048

forbidden_keys = {
    "address",
    "block_hash",
    "candidate_key",
    "instance",
    "pool",
    "provider_id",
    "provider_url",
    "route_id",
    "source_event_identity",
    "transaction_hash",
    "tx_hash",
}

def inspect(value):
    if isinstance(value, dict):
        assert not (set(value) & forbidden_keys)
        for child in value.values():
            inspect(child)
    elif isinstance(value, list):
        for child in value:
            inspect(child)
    elif isinstance(value, str):
        assert "://" not in value

inspect(report)
PY

run_report "$source_fixture" "$metrics_fixture" >"$test_root/report-second.json" ||
  fail "second deterministic report failed"
cmp "$report" "$test_root/report-second.json" >/dev/null || fail "JSON output is not deterministic"

run_report "$source_fixture" "$metrics_fixture" text >"$test_root/report.txt" ||
  fail "text report failed"
grep -F "Technical Report" "$test_root/report.txt" >/dev/null || fail "technical heading is missing"
grep -F "Business Report" "$test_root/report.txt" >/dev/null || fail "business heading is missing"
grep -F "Runtime counter scope: process lifetime" "$test_root/report.txt" >/dev/null ||
  fail "runtime counter scope is missing"
grep -F "Realization status: not realized; SHADOW evidence only" "$test_root/report.txt" >/dev/null ||
  fail "SHADOW realization disclaimer is missing"

if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoProfile -Command '
    $json = Get-Content -Raw -LiteralPath $args[0]
    if (-not ($json | Test-Json -SchemaFile $args[1] -ErrorAction Stop)) { exit 1 }
  ' "$report" "$schema" || fail "JSON Schema validation failed"
fi

"$python_command" - "$source_fixture" "$metrics_fixture" "$test_root" <<'PY'
import json
from pathlib import Path
import sys

source_path, metrics_path, root_path = map(Path, sys.argv[1:])
root = Path(root_path)
source = json.loads(source_path.read_text(encoding="utf-8"))
metrics = json.loads(metrics_path.read_text(encoding="utf-8"))

unsafe = dict(source)
unsafe["live_execution"] = True
(root / "unsafe-source.json").write_text(json.dumps(unsafe), encoding="utf-8")

identity = dict(source)
identity["tx_hash"] = "0x" + "a" * 64
(root / "identity-source.json").write_text(json.dumps(identity), encoding="utf-8")

missing = json.loads(json.dumps(metrics))
missing["data"]["result"] = [
    sample
    for sample in missing["data"]["result"]
    if not (sample["metric"]["__name__"] == "up" and sample["metric"]["job"] == "rpc-gateway")
]
(root / "missing-scrape.json").write_text(json.dumps(missing), encoding="utf-8")

bad_label = json.loads(json.dumps(metrics))
sample = next(
    item
    for item in bad_label["data"]["result"]
    if item["metric"]["__name__"] == "phoenix_profitability_rejections_total"
)
sample["metric"]["reason"] = "arbitrary-user-value"
(root / "bad-label.json").write_text(json.dumps(bad_label), encoding="utf-8")

wrong_job = json.loads(json.dumps(metrics))
sample = next(
    item
    for item in wrong_job["data"]["result"]
    if item["metric"]["__name__"] == "feed_messages_total"
)
sample["metric"]["job"] = "rpc-gateway"
(root / "wrong-job.json").write_text(json.dumps(wrong_job), encoding="utf-8")

oversized = {"status": "success", "data": {"resultType": "vector", "result": []}}
for index in range(2049):
    oversized["data"]["result"].append(
        {
            "metric": {
                "__name__": "feed_message_kind_total",
                "job": "feed-ingestor",
                "instance": "feed-ingestor:9100",
                "classification": "unsupported",
                "kind": str(index % 256),
                "layer": "l1" if index < 1024 else "l2",
            },
            "value": [1784160000, "1"],
        }
    )
(root / "oversized-series.json").write_text(json.dumps(oversized), encoding="utf-8")
PY

for invalid_source in unsafe-source.json identity-source.json; do
  if run_report "$test_root/$invalid_source" "$metrics_fixture" >/dev/null 2>&1; then
    fail "$invalid_source was accepted"
  fi
done
for invalid_metrics in missing-scrape.json bad-label.json wrong-job.json oversized-series.json; do
  if run_report "$source_fixture" "$test_root/$invalid_metrics" >/dev/null 2>&1; then
    fail "$invalid_metrics was accepted"
  fi
done

grep -F "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY" "$sql" >/dev/null ||
  fail "SQL report is not explicitly read-only"
grep -F "LIMIT :reason_limit" "$sql" >/dev/null || fail "SQL reason output is not bounded"
if grep -Eiq '(^|[^a-z_])(insert|update|delete|alter|create|drop|truncate|copy)([^a-z_]|$)' "$sql"; then
  fail "SQL report contains a mutating statement"
fi
grep -F "validate-production-env.sh" "$workflow" >/dev/null ||
  fail "production workflow does not validate SHADOW safety"
grep -F "render-production-compose.sh" "$workflow" >/dev/null ||
  fail "production workflow does not validate the digest-pinned context"
if grep -E '(cat|printenv|env)[[:space:]]+.*phoenix\.env' "$workflow" >/dev/null; then
  fail "production workflow can print the environment file"
fi
if [ "$(grep -c 'sample_limit: 2048' "$repo_dir/prometheus/prometheus.yml")" -ne 5 ]; then
  fail "Prometheus scrape cardinality is not bounded for every money-path service"
fi

echo "prelive-money-path-report-tests: ok"
