#!/bin/sh
# Bounded Atlas liquidation ground-truth collection anchored to the live tip.
#
# The exporter enforces a 20,000-block span against the provider's RAW
# latest (eth_blockNumber), while the observer tail tracks the provider's
# FINALIZED tip + 1 (finality depth ~2,100 blocks). Anchoring the window to
# the observer tail therefore overflows the span cap as soon as the raw tip
# is >999 blocks ahead. This helper:
#
#   1. probes the raw latest via the released, reviewed exporter (a narrow
#      window that cannot exceed the span cap),
#   2. sizes --from-block = raw_latest - 18,000 (2,000 blocks of headroom
#      against a moving tip),
#   3. runs the canonical collect-atlas-liquidation-ground-truth.sh wrapper
#      with --to-block latest.
#
# The probe and the collection run seconds apart, so the 2,000-block headroom
# is far larger than the tip movement in between. The collection itself keeps
# every reviewed evidence guarantee (status-1 only, idempotent inserts,
# bounded span, redacted failures).
#
# Usage: collect-atlas-liquidation-ground-truth-tip-anchored.sh [wrapper args]
# Env overrides are forwarded to the canonical wrapper unchanged.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
wrapper=$script_dir/collect-atlas-liquidation-ground-truth.sh
exporter=$repo_dir/atlas-observer/scripts/export_rpc_transcript.py

if [ -f /opt/phoenix/deploy/atlas-export-rpc-transcript.py ]; then
  # Prefer the deployed, release-bound exporter (exact production code).
  exporter=/opt/phoenix/deploy/atlas-export-rpc-transcript.py
fi

container=${PHOENIX_GT_CONTAINER:-app-rpc-gateway-1}
scratch=$(mktemp -d /tmp/phx-gt-tip-anchored.XXXXXX)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

# The probe window is fixed below the finalized tip and is far smaller than
# the 20,000-block cap, so it cannot fail the span check regardless of tip
# movement. It only reads chain identity and the latest block number.
sudo -n python3 "$exporter" \
  --container "$container" \
  --from-block 496000000 --to-block 496001000 \
  >"$scratch/probe.json" 2>"$scratch/probe.err" || {
  echo "collect-atlas-liquidation-ground-truth-tip-anchored: probe failed" >&2
  exit 1
}

latest_dec=$(
  python3 -c "import json,sys;print(int(json.load(open(sys.argv[1]))['latest_block'],16))" \
    "$scratch/probe.json"
)
from_dec=$((latest_dec - 18000))
if [ "$from_dec" -lt 1 ]; then
  from_dec=1
fi

echo "collect-atlas-liquidation-ground-truth-tip-anchored: latest=$latest_dec from=$from_dec" >&2
exec sudo -n sh "$wrapper" --from-block "$from_dec" --to-block latest "$@"
