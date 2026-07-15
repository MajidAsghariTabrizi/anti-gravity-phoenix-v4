#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
fixture=$repo_dir/fixtures/reports/shadow_profitability_rows.ndjson
analyzer=$script_dir/shadow_profitability_report.py

if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  echo "python is required for profitability report tests" >&2
  exit 1
fi

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-profitability-tests.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

"$python_command" "$analyzer" --format json --limit 3 <"$fixture" >"$tmp_dir/first.json"
"$python_command" - "$fixture" >"$tmp_dir/reversed.ndjson" <<'PY'
import pathlib
import sys

rows = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
sys.stdout.write("\n".join(reversed(rows)) + "\n")
PY
"$python_command" "$analyzer" --format json --limit 3 \
  <"$tmp_dir/reversed.ndjson" >"$tmp_dir/second.json"
cmp "$tmp_dir/first.json" "$tmp_dir/second.json"

"$python_command" - "$tmp_dir/first.json" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
sections = {
    "candidate_funnel",
    "counts_by_route",
    "rejection_reasons",
    "profitability_distribution",
    "nearest_to_profitable",
    "cost_breakdown",
    "rpc_failure_contribution",
    "stale_state_contribution",
    "route_expected_pnl",
    "model_comparison",
    "sensitivity",
    "data_completeness",
}
assert sections <= report.keys()
assert report["financial_basis"] == "SHADOW expected"
assert report["realization_status"] == "not realized"
assert report["candidate_funnel"] == {
    "candidates_observed": 3,
    "complete_evaluations": 2,
    "incomplete_candidates": 1,
    "accepted": 0,
    "rejected": 2,
    "meets_minimum": 1,
    "below_minimum": 1,
}
distributions = report["profitability_distribution"]["by_settlement_asset"]
assert [item["expected_net_pnl"]["sum"] for item in distributions] == ["575", "1460"]
assert report["nearest_to_profitable"][0]["candidates"][0]["gap_to_minimum"] == "25"
assert report["rpc_failure_contribution"]["failure_evidence_candidates"] == 1
assert report["rpc_failure_contribution"]["candidate_counts_by_independent_verification_status"] == [
    {"independent_verification_status": "disagreed", "candidates": 1},
    {"independent_verification_status": "historical_null", "candidates": 1},
    {"independent_verification_status": "not_requested", "candidates": 1},
]
assert report["stale_state_contribution"]["stale_state_reserve_by_settlement_asset"] == [
    {"settlement_asset": "0x1111111111111111111111111111111111111111", "sum": "25"}
]
assert report["data_completeness"]["financial_aggregates_exclude_incomplete"] is True
PY

"$python_command" "$analyzer" --format text --limit 3 <"$fixture" >"$tmp_dir/report.txt"
grep -F "Financial basis: SHADOW expected" "$tmp_dir/report.txt" >/dev/null
grep -F "Realization status: not realized" "$tmp_dir/report.txt" >/dev/null
for heading in \
  "Candidate Funnel" \
  "Counts By Route" \
  "Rejection Reasons" \
  "Profitability Distribution" \
  "Nearest To Profitable" \
  "Cost Breakdown" \
  "RPC Failure Contribution" \
  "Stale-State Contribution" \
  "Route-Level Expected PnL" \
  "Model Comparison" \
  "Conservative/Severe Sensitivity" \
  "Data Completeness"
do
  grep -F "$heading" "$tmp_dir/report.txt" >/dev/null
done

if "$python_command" "$analyzer" --format json --limit 2 \
  <"$fixture" >"$tmp_dir/over-limit.out" 2>"$tmp_dir/over-limit.err"
then
  echo "report analyzer accepted rows beyond its bound" >&2
  exit 1
fi

"$python_command" - "$fixture" >"$tmp_dir/float.ndjson" <<'PY'
import json
import pathlib
import sys

row = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()[0])
row["expected_net_pnl"] = 1.5
print(json.dumps(row, separators=(",", ":")))
PY
if "$python_command" "$analyzer" --format json --limit 1 \
  <"$tmp_dir/float.ndjson" >"$tmp_dir/float.out" 2>"$tmp_dir/float.err"
then
  echo "report analyzer accepted floating-point finance" >&2
  exit 1
fi

"$python_command" - "$fixture" >"$tmp_dir/unsafe.ndjson" <<'PY'
import json
import pathlib
import sys

row = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()[0])
row["execution_eligible"] = True
print(json.dumps(row, separators=(",", ":")))
PY
if "$python_command" "$analyzer" --format json --limit 1 \
  <"$tmp_dir/unsafe.ndjson" >"$tmp_dir/unsafe.out" 2>"$tmp_dir/unsafe.err"
then
  echo "report analyzer accepted executable evidence" >&2
  exit 1
fi

"$python_command" - "$fixture" >"$tmp_dir/arithmetic.ndjson" <<'PY'
import json
import pathlib
import sys

row = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()[0])
row["total_cost"] = "424"
print(json.dumps(row, separators=(",", ":")))
PY
if "$python_command" "$analyzer" --format json --limit 1 \
  <"$tmp_dir/arithmetic.ndjson" >"$tmp_dir/arithmetic.out" 2>"$tmp_dir/arithmetic.err"
then
  echo "report analyzer accepted inconsistent profitability arithmetic" >&2
  exit 1
fi

"$python_command" - "$fixture" >"$tmp_dir/self-verified.ndjson" <<'PY'
import json
import pathlib
import sys

row = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()[1])
row["secondary_provider_id"] = row["primary_provider_id"]
print(json.dumps(row, separators=(",", ":")))
PY
if "$python_command" "$analyzer" --format json --limit 1 \
  <"$tmp_dir/self-verified.ndjson" >"$tmp_dir/self-verified.out" 2>"$tmp_dir/self-verified.err"
then
  echo "report analyzer accepted same-provider self-verification" >&2
  exit 1
fi

"$python_command" - "$fixture" >"$tmp_dir/wrong-route.ndjson" <<'PY'
import json
import pathlib
import sys

row = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()[1])
row["secondary_route_config_hash"] = "0" * 64
print(json.dumps(row, separators=(",", ":")))
PY
if "$python_command" "$analyzer" --format json --limit 1 \
  <"$tmp_dir/wrong-route.ndjson" >"$tmp_dir/wrong-route.out" 2>"$tmp_dir/wrong-route.err"
then
  echo "report analyzer accepted mismatched route evidence" >&2
  exit 1
fi

first_row=$(head -n 1 "$fixture")
printf '{"candidate_key":"duplicate",%s\n' "${first_row#\{}" >"$tmp_dir/duplicate.ndjson"
if "$python_command" "$analyzer" --format json --limit 1 \
  <"$tmp_dir/duplicate.ndjson" >"$tmp_dir/duplicate.out" 2>"$tmp_dir/duplicate.err"
then
  echo "report analyzer accepted duplicate JSON keys" >&2
  exit 1
fi

sh "$script_dir/shadow-profitability-report.sh" --help >/dev/null
echo "shadow profitability report tests passed"
