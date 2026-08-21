#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
exporter=$script_dir/atlas-export-rpc-transcript.py
if [ ! -f "$exporter" ]; then
  exporter=$repo_dir/atlas-observer/scripts/export_rpc_transcript.py
fi
loader=$script_dir/atlas_liquidation_ground_truth.py
if [ ! -f "$loader" ]; then
  loader=$repo_dir/scripts/atlas_liquidation_ground_truth.py
fi
report_sql=$script_dir/sql/atlas-liquidation-ground-truth-report.sql
if [ ! -f "$report_sql" ]; then
  report_sql=$repo_dir/scripts/sql/atlas-liquidation-ground-truth-report.sql
fi
max_span=20000

usage() {
  cat <<'USAGE'
usage: collect-atlas-liquidation-ground-truth.sh
          --from-block <decimal block> --to-block <decimal block | latest>
          [--output-dir <dir>]

Collects reviewed public-chain liquidation ground truth for a bounded block
window: exports the credential-free transcript through the reviewed exporter,
runs the reviewed atlas-reconciler, loads strictly validated rows into
live_canary.atlas_liquidation_ground_truth (append-only, idempotent), and
emits the read-only join report. Evidence only: never authorizes execution.
USAGE
}

fail() {
  echo "collect-atlas-liquidation-ground-truth: $1" >&2
  exit 1
}

from_block=""
to_block="latest"
output_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --from-block)
      [ $# -ge 2 ] || fail "--from-block requires a value"
      from_block=$2
      shift 2
      ;;
    --to-block)
      [ $# -ge 2 ] || fail "--to-block requires a value"
      to_block=$2
      shift 2
      ;;
    --output-dir)
      [ $# -ge 2 ] || fail "--output-dir requires a value"
      output_dir=$2
      shift 2
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

# Argument validation happens before any docker or sudo access.
case "$from_block" in
  ''|*[!0-9]*) fail "--from-block must be a decimal block number" ;;
esac
case "$to_block" in
  latest) ;;
  *[!0-9]*) fail "--to-block must be a decimal block number or 'latest'" ;;
esac
if [ "$from_block" -le 0 ]; then
  fail "--from-block must be positive"
fi
if [ "$to_block" != "latest" ] && [ "$to_block" -lt "$from_block" ]; then
  fail "--to-block must not precede --from-block"
fi
if [ "$to_block" != "latest" ] && [ "$((to_block - from_block + 1))" -gt "$max_span" ]; then
  fail "window exceeds the $max_span-block transcript bound"
fi
if [ -n "$output_dir" ] && [ ! -d "$output_dir" ]; then
  fail "--output-dir is not a directory"
fi

command -v docker >/dev/null 2>&1 || fail "docker is unavailable"
command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
[ -f "$exporter" ] || fail "bounded transcript exporter is missing: $exporter"
[ -f "$loader" ] || fail "ground-truth loader is missing: $loader"
[ -f "$report_sql" ] || fail "ground-truth report SQL is missing: $report_sql"

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

echo "collect-atlas-liquidation-ground-truth: exporting transcript $from_block..$to_block" >&2
if ! sudo -n python3 "$exporter" \
  --container app-rpc-gateway-1 \
  --from-block "$from_block" \
  --to-block "$to_block" >"$scratch/transcript.json" 2>"$scratch/exporter.err"; then
  fail "bounded transcript export failed: $(tail -n 1 "$scratch/exporter.err")"
fi
effective_to=$(python3 -c 'import json,sys;print(int(json.load(open(sys.argv[1]))["latest_block"],16))' "$scratch/transcript.json") ||
  fail "transcript latest block could not be read"
if [ "$to_block" != "latest" ] && [ "$effective_to" -ne "$to_block" ]; then
  fail "transcript latest block $effective_to does not match --to-block $to_block"
fi

echo "collect-atlas-liquidation-ground-truth: staging the reviewed auction ledger" >&2
if ! docker exec app-atlas-observer-1 cat \
  /var/lib/phoenix-atlas-observer/atlas/auctions.ndjson >"$scratch/auctions.ndjson" 2>/dev/null; then
  fail "auction ledger could not be staged"
fi

image=$(docker inspect app-atlas-observer-1 --format '{{.Image}}') ||
  fail "atlas-observer image identity could not be read"
echo "collect-atlas-liquidation-ground-truth: reconciling with image $image" >&2
if ! docker run --rm -i --user "$(id -u):$(id -g)" \
  --entrypoint /usr/local/bin/atlas-reconciler \
  -v "$scratch:/ledger" "$image" \
  --ledger-dir /ledger \
  <"$scratch/transcript.json" >"$scratch/reconciler.out" 2>"$scratch/reconciler.err"; then
  fail "atlas-reconciler failed: $(tail -n 1 "$scratch/reconciler.err")"
fi
cat "$scratch/reconciler.out" >&2
[ -f "$scratch/reconciliation.ndjson" ] ||
  fail "atlas-reconciler produced no reconciliation ledger"

echo "collect-atlas-liquidation-ground-truth: validating and rendering ground-truth rows" >&2
if ! python3 "$loader" <"$scratch/reconciliation.ndjson" >"$scratch/rows.sql" 2>"$scratch/loader.err"; then
  fail "ground-truth load validation failed: $(tail -n 1 "$scratch/loader.err")"
fi
cat "$scratch/loader.err" >&2
grep -q "rows_loaded=" "$scratch/loader.err" ||
  fail "loader summary is missing"

env_file=${PHOENIX_ENV_FILE:-$repo_dir/.env.production}
release_env=${PHOENIX_RELEASE_ENV:-$repo_dir/.env.release}
compose_file=${PHOENIX_COMPOSE_FILE:-$repo_dir/compose.prod.yml}
[ -f "$env_file" ] || fail "production env file is missing: $env_file"
[ -f "$release_env" ] || fail "release environment file is unavailable: $release_env"
[ -f "$compose_file" ] || fail "compose file is missing: $compose_file"

echo "collect-atlas-liquidation-ground-truth: loading rows into live_canary" >&2
docker compose --env-file "$env_file" --env-file "$release_env" \
  -f "$compose_file" exec -T postgres \
  sh -c 'psql -X -q -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
  <"$scratch/rows.sql" || fail "ground-truth rows were rejected by live_canary"

report_scratch=$scratch/report.json
echo "collect-atlas-liquidation-ground-truth: emitting the join report" >&2
docker compose --env-file "$env_file" --env-file "$release_env" \
  -f "$compose_file" exec -T postgres \
  sh -c 'psql -X -qAt -v ON_ERROR_STOP=1 \
    -v window_start_block="$1" -v window_end_block="$2" \
    -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
  sh "$from_block" "$effective_to" \
  <"$report_sql" >"$report_scratch" ||
  fail "ground-truth report failed"
grep -q '"schema" : "phoenix.atlas-liquidation-ground-truth-report.v1"' "$report_scratch" ||
  fail "ground-truth report identity is missing"
if [ -n "$output_dir" ]; then
  cp "$report_scratch" "$output_dir/phoenix-atlas-liquidation-ground-truth-report.json"
else
  cat "$report_scratch"
fi

echo "collect-atlas-liquidation-ground-truth: complete (evidence only, execution authority unchanged)" >&2
