#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
analyzer=$script_dir/shadow_route_discovery.py
workflow=$script_dir/shadow-route-discovery.sh
decoded=$repo_dir/fixtures/reports/shadow_route_discovery_decoded.ndjson
enrichment=$repo_dir/fixtures/reports/shadow_route_discovery_enrichment.ndjson
proofs=$repo_dir/fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json
sql=$script_dir/sql/shadow-route-discovery-enrichment.sql
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "shadow-route-discovery-tests: $1" >&2
  exit 1
}

if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  fail "python is unavailable"
fi

run_analyzer() {
  "$python_command" "$analyzer" \
    --decoded "$1" \
    --enrichment "$2" \
    --pool-proofs "$3" \
    --format "${4:-json}" \
    --limit "${5:-10000}" \
    --top 10
}

report=$test_root/report.json
run_analyzer "$decoded" "$enrichment" "$proofs" >"$report" || fail "canonical fixture analysis failed"
"$python_command" - "$report" <<'PY' || fail "canonical report contract failed"
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
assert report["schema_version"] == "phoenix.shadow.route-ranking.v1"
assert report["chain_id"] == 42161
assert report["mode"] == "SHADOW"
assert report["live_execution"] is False
assert report["execution_eligible"] is False
assert report["execution_request_created"] is False
assert report["production_registry_mutated"] is False
assert report["realization_status"] == "not realized"
assert report["input_summary"]["candidate_routes"] == 12
assert report["input_summary"]["trusted_supported_rows"] == 0
assert report["selected_shadow_route_count"] == 0
assert len(report["top_routes"]) == 10
top = report["top_routes"][0]
assert top["fee_path"] == [500, 3000]
assert len(top["component_scores"]) == 20
assert top["canonical_pool_addresses"] == [
    "0xc6962004f452be9203591991d15f6b388e09e8d0",
    "0xc473e2aee3441bf9240be85eb122abb059a3b57c",
]
assert top["suggested_route_json"]["strategy"]["max_gas_price_wei"] == "1000000000000"
assert "untrusted_or_synthetic_history" in top["unsafe_or_unsupported_reasons"]
assert top["shadow_activation_eligible"] is False
assert "feed_gap_overlap_unavailable_not_persisted" in report["global_warnings"]
reverse = next(
    route
    for route in report["top_routes"]
    if route["fee_path"] == [3000, 500]
    and route["canonical_token_path"][0] == "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
)
assert reverse["component_scores"]["expected_net_pnl"]["status"] == "unavailable"
assert reverse["canonical_pool_addresses"] == [
    "0xc473e2aee3441bf9240be85eb122abb059a3b57c",
    "0xc6962004f452be9203591991d15f6b388e09e8d0",
]
PY

reversed_decoded=$test_root/reversed-decoded.ndjson
reversed_enrichment=$test_root/reversed-enrichment.ndjson
"$python_command" - "$decoded" "$reversed_decoded" "$enrichment" "$reversed_enrichment" <<'PY'
from pathlib import Path
import sys

for source, target in ((sys.argv[1], sys.argv[2]), (sys.argv[3], sys.argv[4])):
    lines = Path(source).read_text(encoding="utf-8").splitlines()
    Path(target).write_text("\n".join(reversed(lines)) + "\n", encoding="utf-8")
PY
run_analyzer "$reversed_decoded" "$reversed_enrichment" "$proofs" >"$test_root/reversed.json" ||
  fail "reordered fixture analysis failed"
cmp "$report" "$test_root/reversed.json" >/dev/null || fail "ranking is not deterministic"

run_analyzer "$decoded" "$enrichment" "$proofs" text >"$test_root/report.txt" ||
  fail "text report failed"
grep -F 'Top 10 Routes' "$test_root/report.txt" >/dev/null || fail "top-route text heading is missing"
grep -F 'Selected SHADOW Routes' "$test_root/report.txt" >/dev/null || fail "selected-route text heading is missing"
grep -F 'Realization status: not realized' "$test_root/report.txt" >/dev/null || fail "realization disclaimer is missing"

"$python_command" - "$script_dir" <<'PY' || fail "Ethereum Keccak vectors failed"
import importlib.util
from pathlib import Path
import sys

path = Path(sys.argv[1]) / "shadow_route_discovery.py"
spec = importlib.util.spec_from_file_location("shadow_route_discovery", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
assert module.keccak256(b"").hex() == "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
assert module.keccak256(b"abc").hex() == "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
PY

"$python_command" - "$decoded" "$test_root" <<'PY'
import json
from pathlib import Path
import sys

rows = [json.loads(line) for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()]
root = Path(sys.argv[2])

trusted = []
for index in range(20):
    row = dict(rows[0] if index % 2 == 0 else rows[2])
    row["transaction_hash"] = "0x" + format(index + 1, "064x")
    row["source_sequence"] = 1000 + index
    row["recorded_at"] = f"2026-01-01T00:00:{index:02d}Z"
    row["source_block_number"] = 100 + index % 5
    row["source_block_hash"] = "0x" + format(100 + index % 5, "064x")
    row["trusted_persisted_source"] = True
    row["production_evidence"] = False
    trusted.append(row)
(root / "trusted.ndjson").write_text(
    "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in trusted),
    encoding="utf-8",
)

multi = dict(rows[0])
multi["decoded_token_path"] = [
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
]
multi["decoded_fee_path"] = [500, 3000]
multi["decoded_pool_ids"] = [
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:500",
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:3000",
]
multi["affected_configured_pool_ids"] = list(multi["decoded_pool_ids"])
(root / "multi-hop.ndjson").write_text(json.dumps(multi, separators=(",", ":")) + "\n", encoding="utf-8")

load = []
for index in range(2000):
    row = dict(rows[index % 4])
    row["transaction_hash"] = "0x" + format(index + 1, "064x")
    row["source_sequence"] = 10000 + index
    load.append(row)
(root / "load.ndjson").write_text(
    "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in load),
    encoding="utf-8",
)

duplicate = dict(rows[0])
duplicate["source_sequence"] = 9999
duplicate["recorded_at"] = "2026-01-02T00:00:00Z"
(root / "deduplicated.ndjson").write_text(
    "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows + [duplicate]),
    encoding="utf-8",
)
conflict = dict(duplicate)
conflict["input_amount"] = "9999999"
(root / "conflicting-duplicate.ndjson").write_text(
    "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows + [conflict]),
    encoding="utf-8",
)
router_mismatch = dict(rows[0])
router_mismatch["router_address"] = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45"
(root / "router-mismatch.ndjson").write_text(
    json.dumps(router_mismatch, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY

run_analyzer "$test_root/trusted.ndjson" "$enrichment" "$proofs" >"$test_root/trusted.json" ||
  fail "trusted-history policy fixture failed"
"$python_command" - "$test_root/trusted.json" <<'PY' || fail "top-three SHADOW selection policy failed"
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
assert report["selected_shadow_route_count"] == 1
assert len(report["selected_shadow_routes"]) <= 3
assert report["top_routes"][0]["shadow_activation_eligible"] is True
assert report["top_routes"][0]["unsafe_or_unsupported_reasons"] == []
assert report["live_execution"] is False
assert report["execution_request_created"] is False
PY

availability_only=$test_root/availability.ndjson
tail -n 1 "$enrichment" >"$availability_only"
run_analyzer "$test_root/multi-hop.ndjson" "$availability_only" "$proofs" >"$test_root/multi-hop.json" ||
  fail "multi-hop evidence fixture failed"
"$python_command" - "$test_root/multi-hop.json" <<'PY' || fail "multi-hop volume was over-credited"
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
assert report["top_routes"][0]["component_scores"]["volume_proxy"]["status"] == "unavailable"
assert report["top_routes"][0]["component_scores"]["volume_proxy"]["raw"] is None
PY

run_analyzer "$test_root/load.ndjson" "$enrichment" "$proofs" >/dev/null ||
  fail "bounded 2000-row load fixture failed"
run_analyzer "$test_root/deduplicated.ndjson" "$enrichment" "$proofs" >"$test_root/deduplicated.json" ||
  fail "duplicate transaction deduplication failed"
"$python_command" - "$report" "$test_root/deduplicated.json" <<'PY' || fail "deduplication changed ranking evidence"
import json
import sys

baseline = json.load(open(sys.argv[1], encoding="utf-8"))
deduplicated = json.load(open(sys.argv[2], encoding="utf-8"))
assert deduplicated["top_routes"] == baseline["top_routes"]
assert deduplicated["input_summary"]["decoded_rows"] == baseline["input_summary"]["decoded_rows"]
assert deduplicated["input_summary"]["duplicate_decoded_rows_removed"] == 1
PY
if run_analyzer "$test_root/conflicting-duplicate.ndjson" "$enrichment" "$proofs" >/dev/null 2>&1; then
  fail "conflicting duplicate transaction evidence was accepted"
fi
if run_analyzer "$test_root/router-mismatch.ndjson" "$enrichment" "$proofs" >/dev/null 2>&1; then
  fail "reviewed router identity mismatch was accepted"
fi

"$python_command" - "$enrichment" "$test_root/cross-asset.ndjson" <<'PY'
import json
from pathlib import Path
import sys

lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
other_asset = json.loads(lines[0])
other_asset["candidate_key"] = "33333333-3333-8333-8333-333333333333"
other_asset["token_path"] = [
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
    "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
    "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
]
other_asset["expected_net_pnl"] = "999999999999999999999999999999"
other_asset["severe_net_pnl"] = "999999999999999999999999999998"
other_asset["minimum_required_net_pnl"] = "1"
other_asset["primary_profitability_status"] = "meets_minimum"
Path(sys.argv[2]).write_text(
    "\n".join(lines[:-1] + [json.dumps(other_asset, separators=(",", ":")), lines[-1]]) + "\n",
    encoding="utf-8",
)
PY
run_analyzer "$decoded" "$test_root/cross-asset.ndjson" "$proofs" >"$test_root/cross-asset.json" ||
  fail "cross-settlement-asset fixture failed"
"$python_command" - "$test_root/cross-asset.json" <<'PY' || fail "financial units were mixed"
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
weth = next(
    route
    for route in report["top_routes"]
    if route["fee_path"] == [500, 3000]
    and route["canonical_token_path"][0] == "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
)
usdc = next(
    route
    for route in report["top_routes"]
    if route["fee_path"] == [500, 3000]
    and route["canonical_token_path"][0] == "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
)
assert weth["component_scores"]["expected_net_pnl"]["raw"] == {
    "settlement_asset": "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
    "conservative_lower_median": "95",
}
assert usdc["component_scores"]["expected_net_pnl"]["raw"] == {
    "settlement_asset": "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
    "conservative_lower_median": "999999999999999999999999999999",
}
assert weth["component_scores"]["candidate_frequency"]["raw"]["complete_facts"] == 2
assert usdc["suggested_route_json"] is None
PY

"$python_command" - "$enrichment" "$test_root/stale.ndjson" "$test_root/adverse.ndjson" <<'PY'
import json
from pathlib import Path
import sys

lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
stale = json.loads(lines[1])
stale["evaluated_at_unix_ms"] = str(int(stale["detected_at_unix_ms"]) + 2001)
Path(sys.argv[2]).write_text(
    "\n".join([lines[0], json.dumps(stale, separators=(",", ":"))] + lines[2:]) + "\n",
    encoding="utf-8",
)
adverse = json.loads(lines[1])
adverse["severe_net_pnl"] = "-1"
Path(sys.argv[3]).write_text(
    "\n".join([lines[0], json.dumps(adverse, separators=(",", ":"))] + lines[2:]) + "\n",
    encoding="utf-8",
)
PY
run_analyzer "$test_root/trusted.ndjson" "$test_root/stale.ndjson" "$proofs" >"$test_root/stale.json" ||
  fail "stale-evidence fixture failed"
run_analyzer "$test_root/trusted.ndjson" "$test_root/adverse.ndjson" "$proofs" >"$test_root/adverse.json" ||
  fail "adverse severe-evidence fixture failed"
"$python_command" - "$test_root/stale.json" "$test_root/adverse.json" <<'PY' || fail "absolute activation gates failed"
import json
import sys

stale = json.load(open(sys.argv[1], encoding="utf-8"))["top_routes"][0]
adverse = json.load(open(sys.argv[2], encoding="utf-8"))["top_routes"][0]
assert "state_freshness_above_policy" in stale["unsafe_or_unsupported_reasons"]
assert stale["shadow_activation_eligible"] is False
assert "non_positive_severe_pnl_evidence" in adverse["unsafe_or_unsupported_reasons"]
assert adverse["shadow_activation_eligible"] is False
PY

"$python_command" - "$enrichment" "$test_root/zero-liquidity.ndjson" "$test_root/stale-checkpoint.ndjson" "$test_root/duplicate-fact.ndjson" <<'PY'
import json
from pathlib import Path
import sys

lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
zero = json.loads(lines[2])
zero["liquidity"] = "0"
Path(sys.argv[2]).write_text(
    "\n".join(lines[:2] + [json.dumps(zero, separators=(",", ":"))] + lines[3:]) + "\n",
    encoding="utf-8",
)
old = json.loads(lines[2])
old["block_number"] = "99"
Path(sys.argv[3]).write_text(
    "\n".join(lines[:2] + [json.dumps(old, separators=(",", ":"))] + lines[3:]) + "\n",
    encoding="utf-8",
)
Path(sys.argv[4]).write_text("\n".join(lines[:-1] + [lines[0], lines[-1]]) + "\n", encoding="utf-8")
PY
run_analyzer "$test_root/trusted.ndjson" "$test_root/zero-liquidity.ndjson" "$proofs" >"$test_root/zero-liquidity.json" ||
  fail "zero-liquidity fixture failed"
run_analyzer "$test_root/trusted.ndjson" "$test_root/stale-checkpoint.ndjson" "$proofs" >"$test_root/stale-checkpoint.json" ||
  fail "stale-checkpoint fixture failed"
"$python_command" - "$test_root/zero-liquidity.json" "$test_root/stale-checkpoint.json" <<'PY' || fail "liquidity activation gates failed"
import json
import sys

zero = json.load(open(sys.argv[1], encoding="utf-8"))["top_routes"][0]
stale = json.load(open(sys.argv[2], encoding="utf-8"))["top_routes"][0]
assert "zero_liquidity_checkpoint" in zero["unsafe_or_unsupported_reasons"]
assert zero["shadow_activation_eligible"] is False
assert "stale_liquidity_checkpoint" in stale["unsafe_or_unsupported_reasons"]
assert stale["shadow_activation_eligible"] is False
PY
if run_analyzer "$decoded" "$test_root/duplicate-fact.ndjson" "$proofs" >/dev/null 2>&1; then
  fail "duplicate profitability evidence was accepted"
fi

printf '{"transaction_hash":"0x%064d","transaction_hash":"0x%064d"}\n' 0 0 >"$test_root/duplicate.jsonl"
if run_analyzer "$test_root/duplicate.jsonl" "$enrichment" "$proofs" >/dev/null 2>&1; then
  fail "duplicate JSON keys were accepted"
fi

"$python_command" - "$enrichment" "$test_root/float.ndjson" "$test_root/unsafe.ndjson" <<'PY'
import json
from pathlib import Path
import sys

lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
first = json.loads(lines[0])
first["expected_net_pnl"] = 1.5
Path(sys.argv[2]).write_text(json.dumps(first) + "\n" + "\n".join(lines[1:]) + "\n", encoding="utf-8")
first = json.loads(lines[0])
first["execution_eligible"] = True
Path(sys.argv[3]).write_text(json.dumps(first) + "\n" + "\n".join(lines[1:]) + "\n", encoding="utf-8")
PY
if run_analyzer "$decoded" "$test_root/float.ndjson" "$proofs" >/dev/null 2>&1; then
  fail "floating-point financial evidence was accepted"
fi
if run_analyzer "$decoded" "$test_root/unsafe.ndjson" "$proofs" >/dev/null 2>&1; then
  fail "executable enrichment evidence was accepted"
fi

"$python_command" - "$proofs" "$test_root/invalid-proofs.json" "$test_root/unpinned-proofs.json" <<'PY'
import json
from pathlib import Path
import sys

proofs = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
proofs["pools"][0]["pool_address"] = "0x" + "0" * 40
Path(sys.argv[2]).write_text(json.dumps(proofs), encoding="utf-8")
proofs = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
proofs["source"]["commit"] = "0" * 40
Path(sys.argv[3]).write_text(json.dumps(proofs), encoding="utf-8")
PY
if run_analyzer "$decoded" "$enrichment" "$test_root/invalid-proofs.json" >/dev/null 2>&1; then
  fail "invalid CREATE2 pool proof was accepted"
fi
if run_analyzer "$decoded" "$enrichment" "$test_root/unpinned-proofs.json" >/dev/null 2>&1; then
  fail "unpinned official proof source was accepted"
fi
if run_analyzer "$decoded" "$enrichment" "$proofs" json 4 >/dev/null 2>&1; then
  fail "decoded row-bound overflow was silently truncated"
fi
printf '{"record_type":"overflow"}\n' >"$test_root/overflow.ndjson"
if run_analyzer "$decoded" "$test_root/overflow.ndjson" "$proofs" >/dev/null 2>&1; then
  fail "PostgreSQL evidence overflow was accepted"
fi

grep -F 'BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY' "$sql" >/dev/null ||
  fail "enrichment SQL is not repeatable-read and read-only"
if grep -Ei '^[[:space:]]*(INSERT|UPDATE|DELETE|TRUNCATE|ALTER|DROP|CREATE)[[:space:]]' "$sql" >/dev/null; then
  fail "enrichment SQL contains a mutation"
fi
if grep -E 'compose[[:space:]]+(up|down|stop|start|restart|pull|build|rm)|--remove-orphans' "$workflow" >/dev/null; then
  fail "workflow contains a service or release mutation"
fi
grep -F 'validate-production-env.sh' "$workflow" >/dev/null || fail "production env validation is missing"
grep -F 'render-production-compose.sh' "$workflow" >/dev/null || fail "digest-pinned render validation is missing"
grep -F 'scan-postgres' "$workflow" >/dev/null || fail "production decoder scan is missing"
grep -F 'execution_eligible' "$analyzer" >/dev/null || fail "execution safety output is missing"
grep -F 'unavailable_not_persisted' "$analyzer" >/dev/null || fail "feed-gap unavailability is not explicit"
grep -F 'sh ./scripts/shadow-route-discovery-tests.sh' "$repo_dir/.github/workflows/ci.yml" >/dev/null ||
  fail "route discovery tests are not wired into CI"
grep -F 'python3 -m py_compile scripts/shadow_route_discovery.py' "$repo_dir/.github/workflows/ci.yml" >/dev/null ||
  fail "route discovery syntax validation is not wired into CI"

echo 'shadow-route-discovery-tests: ok'
