#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
analyzer=$script_dir/atlas_shadow_validation_report.py
workflow=$script_dir/atlas-shadow-validation.sh
sql=$script_dir/sql/shadow-atlas-validation.sql
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "shadow-atlas-validation-tests: $1" >&2
  exit 1
}

if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  fail "python is unavailable"
fi

"$python_command" -m py_compile "$analyzer" ||
  fail "analyzer does not compile"

# The validation SQL must stay strictly read-only: every mutation verb is
# forbidden, and the shadow table plus window variables must be referenced.
for verb in INSERT UPDATE DELETE CREATE ALTER DROP TRUNCATE GRANT REVOKE COPY; do
  if grep -Eq "\b$verb\b" "$sql"; then
    fail "validation SQL contains forbidden verb: $verb"
  fi
done
grep -F "live_canary.atlas_auction_shadow" "$sql" >/dev/null ||
  fail "validation SQL does not reference the shadow table"
grep -F ":'window_start'" "$sql" >/dev/null ||
  fail "validation SQL does not reference window_start"
grep -F ":'window_end'" "$sql" >/dev/null ||
  fail "validation SQL does not reference window_end"
grep -F "atlas_solver_requests" "$sql" >/dev/null ||
  fail "validation SQL does not check the solver request invariant"
grep -F "execution_requests" "$sql" >/dev/null ||
  fail "validation SQL does not check the execution request invariant"

run_analyzer() {
  "$python_command" "$analyzer" "$@"
}

write_fixture() {
  cat >"$1"
}

write_fixture "$test_root/clean.json" <<'JSON'
{"schema":"phoenix.atlas-shadow-validation.v1",
 "window":{"start":"2026-08-20T21:18:00Z","end":"2026-08-21T21:18:00Z"},
 "coverage":{"relevant_ingress":3000,"shadow_evaluated":2970},
 "callback_simulation":{"attempted":120,"passed":119},
 "bid_ability":{"evaluated_rows":2970,"eligible_rows":9,
                "eligible_with_maximum_bid":9,"rejected_rows":2961},
 "value_proxy":{"expected_net_after_bid_sum":"123456789",
                "conservative_net_after_bid_sum":"110000000"},
 "zero_invariants":{"atlas_solver_requests_total":0,"execution_requests_total":0,
   "active_attempts":0,"unresolved_submissions":0,
   "eligible_rows_with_rejection_reason":0,
   "eligible_rows_without_maximum_bid":0}}
JSON

run_analyzer --format json <"$test_root/clean.json" >"$test_root/report.json" ||
  fail "canonical validation report failed"
"$python_command" - "$test_root/report.json" <<'PY' || fail "report contract failed"
import json
from pathlib import Path
import sys

report = json.loads(Path(sys.argv[1]).read_bytes())
assert report["schema"] == "phoenix.atlas-shadow-validation.v1"
assert report["coverage"]["svr_coverage_bp"] == 9900
assert report["callback_simulation"]["success_bp"] == 9917
assert report["bid_ability"]["eligible_with_maximum_bid"] == 9
assert report["value_proxy"]["expected_net_after_bid_sum"] == "123456789"
assert report["mode"] == "SHADOW"
assert report["financial_authority"] == "CLOSED"
assert report["zero_invariants"]["active_attempts"] == 0
assert report["warnings"] == []
PY

run_analyzer <"$test_root/clean.json" >"$test_root/summary.txt" ||
  fail "text summary failed"
grep -F "Zero invariants: all zero" "$test_root/summary.txt" >/dev/null ||
  fail "zero invariant summary line is missing"
grep -F "not realized; SHADOW evidence only" "$test_root/summary.txt" >/dev/null ||
  fail "SHADOW realization disclaimer is missing"

build_violation_fixture() {
  "$python_command" - "$test_root/clean.json" "$test_root/violation.json" <<'PY'
import json
from pathlib import Path
import sys

payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
payload["zero_invariants"]["active_attempts"] = 1
Path(sys.argv[2]).write_text(json.dumps(payload), encoding="utf-8")
PY
}
build_violation_fixture || fail "violation fixture could not be produced"
if run_analyzer <"$test_root/violation.json" >"$test_root/violation.out" \
    2>"$test_root/violation.err"; then
  fail "invariant violation unexpectedly passed"
fi
grep -F "zero_invariants_violated" "$test_root/violation.err" >/dev/null ||
  fail "violation reason is missing"

# Wrapper argument validation must fail closed before any docker access.
if sh "$workflow" --format xml >/dev/null 2>&1; then
  fail "invalid format unexpectedly passed"
fi
if sh "$workflow" --window-start 2026-08-21T00:00:00Z \
  --window-end 2026-08-20T00:00:00Z >/dev/null 2>&1; then
  fail "reversed window unexpectedly passed"
fi
if sh "$workflow" --window-start "2026-08-20 00:00:00" \
  --window-end 2026-08-21T00:00:00Z >/dev/null 2>&1; then
  fail "non-ISO window start unexpectedly passed"
fi
if sh "$workflow" --window-start 2026-07-01T00:00:00Z \
  --window-end 2026-08-21T00:00:00Z >/dev/null 2>&1; then
  fail "oversized window unexpectedly passed"
fi
if sh "$workflow" --unknown-flag >/dev/null 2>&1; then
  fail "unknown argument unexpectedly passed"
fi

echo "shadow-atlas-validation-tests: all checks passed"
