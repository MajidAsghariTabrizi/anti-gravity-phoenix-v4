#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
loader=$script_dir/atlas_liquidation_ground_truth.py
wrapper=$script_dir/collect-atlas-liquidation-ground-truth.sh
report_sql=$script_dir/sql/atlas-liquidation-ground-truth-report.sql
schema_sql=$repo_dir/live-executor/schema/011_atlas_liquidation_ground_truth.sql
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "atlas-liquidation-ground-truth-tests: $1" >&2
  exit 1
}

if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  fail "python is unavailable"
fi

"$python_command" -m py_compile "$loader" ||
  fail "loader does not compile"

# The join report SQL must be strictly read-only.
for verb in INSERT UPDATE DELETE CREATE ALTER DROP TRUNCATE GRANT REVOKE COPY; do
  if grep -Eq "\b$verb\b" "$report_sql"; then
    fail "report SQL contains forbidden verb: $verb"
  fi
done
grep -F "phoenix.atlas-liquidation-ground-truth-report.v1" "$report_sql" >/dev/null ||
  fail "report SQL does not carry its schema identity"
grep -F "atlas_liquidation_ground_truth" "$report_sql" >/dev/null ||
  fail "report SQL does not reference the ground-truth table"
grep -F "atlas_auction_ingress" "$report_sql" >/dev/null ||
  fail "report SQL does not join auction ingress"
grep -F "atlas_auction_shadow" "$report_sql" >/dev/null ||
  fail "report SQL does not join the shadow table"

# The v11 migration must declare the v11 contract and the table identity.
grep -F "phoenix.live-canary-schema.v11" "$schema_sql" >/dev/null ||
  fail "migration does not declare the v11 schema contract"
grep -F "CREATE TABLE IF NOT EXISTS live_canary.atlas_liquidation_ground_truth" \
  "$schema_sql" >/dev/null ||
  fail "migration does not create the ground-truth table"

write_fixture() {
  cat >"$1"
}

write_fixture "$test_root/clean.ndjson" <<'JSON'
{"schema":"phoenix.atlas-reconciliation.v1","reconciled_at":"2026-08-20T22:00:00Z","auction_id":"auction-1","user_operation_hash":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","transcript_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","public_settlement_found":true,"onchain_transaction":"0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","transaction_block":491300000,"receipt_status":1,"public_liquidations":[{"collateral_asset":"0x912ce59144191c1204e64559fe8253a0e49e6548","debt_asset":"0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2","borrower":"0x1111111111111111111111111111111111111111","debt_to_cover":"50000000","liquidated_collateral_amount":"55000000","liquidator":"0x2222222222222222222222222222222222222222","receive_a_token":true,"log_index":"a"}]}
{"schema":"phoenix.atlas-reconciliation.v1","reconciled_at":"2026-08-20T22:00:00Z","auction_id":"auction-2","user_operation_hash":"0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","transcript_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","public_settlement_found":false,"onchain_transaction":null,"transaction_block":null,"receipt_status":null,"public_liquidations":null}
JSON

"$python_command" "$loader" <"$test_root/clean.ndjson" \
  >"$test_root/rows.sql" 2>"$test_root/summary.txt" ||
  fail "clean ground-truth fixture failed"
grep -q "rows_loaded=1" "$test_root/summary.txt" ||
  fail "loader summary is missing"
grep -q "ON CONFLICT DO NOTHING" "$test_root/rows.sql" ||
  fail "idempotent insert contract is missing"
if grep -Eq "(DROP|CREATE|UPDATE|DELETE)" "$test_root/rows.sql"; then
  fail "rendered rows contain mutation verbs"
fi
"$python_command" "$loader" <"$test_root/clean.ndjson" \
  >"$test_root/rows2.sql" 2>/dev/null || fail "loader is not deterministic"
cmp "$test_root/rows.sql" "$test_root/rows2.sql" ||
  fail "loader output is not deterministic"

"$python_command" - "$test_root/clean.ndjson" "$test_root/violation.ndjson" <<'PY'
import json
import sys
from pathlib import Path

payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()[0])
payload["public_liquidations"][0]["borrower"] = "0xNOT-AN-ADDRESS"
Path(sys.argv[2]).write_text(json.dumps(payload) + "\n", encoding="utf-8")
PY
if "$python_command" "$loader" <"$test_root/violation.ndjson" \
  >"$test_root/violation.sql" 2>"$test_root/violation.err"; then
  fail "malformed borrower unexpectedly passed"
fi
grep -q "borrower_invalid" "$test_root/violation.err" ||
  fail "violation reason is missing"

# Wrapper argument validation must fail closed before any docker/sudo access.
if sh "$wrapper" --from-block not-a-block >/dev/null 2>&1; then
  fail "non-numeric from-block unexpectedly passed"
fi
if sh "$wrapper" --from-block 0 >/dev/null 2>&1; then
  fail "zero from-block unexpectedly passed"
fi
if sh "$wrapper" --from-block 100 --to-block 50 >/dev/null 2>&1; then
  fail "reversed window unexpectedly passed"
fi
if sh "$wrapper" --from-block 1 --to-block 30000 >/dev/null 2>&1; then
  fail "oversized window unexpectedly passed"
fi
if sh "$wrapper" --from-block 1 --to-block latest --output-dir "$test_root/missing" \
  >/dev/null 2>&1; then
  fail "missing output dir unexpectedly passed"
fi
if sh "$wrapper" --unknown-flag >/dev/null 2>&1; then
  fail "unknown argument unexpectedly passed"
fi

# The reconciler must run through an explicit entrypoint override: the
# atlas-observer image entrypoint is the observer binary, and the wrapper's
# docker invocation must therefore pin /usr/local/bin/atlas-reconciler.
grep -q -- "--entrypoint /usr/local/bin/atlas-reconciler" "$wrapper" ||
  fail "reconciler entrypoint override is missing"

echo "atlas-liquidation-ground-truth-tests: all checks passed"
