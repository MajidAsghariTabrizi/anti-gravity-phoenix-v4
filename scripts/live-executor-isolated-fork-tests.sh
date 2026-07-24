#!/usr/bin/env sh
set -eu

for command in forge cast anvil cargo python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'required isolated-fork command is unavailable: %s\n' "$command" >&2
    exit 1
  fi
done

tmp_dir="$(mktemp -d)"
anvil_pid=""
proxy_pid=""
cleanup() {
  if [ -n "$proxy_pid" ]; then
    kill "$proxy_pid" >/dev/null 2>&1 || true
    wait "$proxy_pid" >/dev/null 2>&1 || true
  fi
  if [ -n "$anvil_pid" ]; then
    kill "$anvil_pid" >/dev/null 2>&1 || true
    wait "$anvil_pid" >/dev/null 2>&1 || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

wallet_json="$tmp_dir/wallet.json"
cast wallet new --json >"$wallet_json"
chmod 600 "$wallet_json"
test_key="$(
  python3 - "$wallet_json" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
if isinstance(value, list):
    value = value[0]
for name in ("private_key", "privateKey", "private_key_hex"):
    candidate = value.get(name)
    if isinstance(candidate, str) and candidate:
        print(candidate)
        raise SystemExit(0)
raise SystemExit("cast wallet output did not contain a private key")
PY
)"
test_wallet="$(cast wallet address --private-key "$test_key")"

anvil_port="${PHOENIX_TEST_ANVIL_PORT:-18545}"
rpc_url="http://127.0.0.1:${anvil_port}"
anvil --silent --host 127.0.0.1 --port "$anvil_port" --chain-id 42161 \
  >"$tmp_dir/anvil.log" 2>&1 &
anvil_pid="$!"

attempt=0
until cast chain-id --rpc-url "$rpc_url" >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    printf 'isolated Anvil did not become ready\n' >&2
    exit 1
  fi
  sleep 0.1
done

cast rpc --rpc-url "$rpc_url" anvil_setBalance \
  "$test_wallet" 0x56bc75e2d63100000 >/dev/null

deployment_json="$tmp_dir/deployment.json"
(
  cd contracts
  forge create src/PhoenixExecutor.sol:PhoenixExecutor \
    --rpc-url "$rpc_url" \
    --private-key "$test_key" \
    --broadcast \
    --json \
    --constructor-args "$test_wallet" "$test_wallet" \
    >"$deployment_json"
)
executor_address="$(
  python3 - "$deployment_json" <<'PY'
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
for name in ("deployedTo", "deployed_to"):
    candidate = value.get(name)
    if isinstance(candidate, str) and candidate:
        print(candidate.lower())
        raise SystemExit(0)
raise SystemExit("forge deployment output did not contain the contract address")
PY
)"

proxy_port="${PHOENIX_TEST_QUOTE_PROXY_PORT:-18546}"
proxy_url="http://127.0.0.1:${proxy_port}"
python3 scripts/anvil_quote_proxy.py \
  --listen-port "$proxy_port" \
  --upstream "$rpc_url" >"$tmp_dir/quote-proxy.log" 2>&1 &
proxy_pid="$!"
attempt=0
until python3 - "$proxy_url" <<'PY' >/dev/null 2>&1
import json
import sys
import urllib.request

request = urllib.request.Request(
    sys.argv[1],
    data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []}).encode(),
    headers={"Content-Type": "application/json"},
)
with urllib.request.urlopen(request, timeout=1) as response:
    assert json.load(response)["result"] == "0xa4b1"
PY
do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 50 ]; then
    printf 'isolated quote proxy did not become ready\n' >&2
    exit 1
  fi
  sleep 0.1
done

read -r block_number block_hash executor_code_hash <<EOF
$(python3 - "$rpc_url" "$executor_address" <<'PY'
import hashlib
import json
import sys
import urllib.request

def rpc(method, params):
    request = urllib.request.Request(
        sys.argv[1],
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=2) as response:
        return json.load(response)["result"]

block_number_hex = rpc("eth_blockNumber", [])
block = rpc("eth_getBlockByNumber", [block_number_hex, False])
code = bytes.fromhex(rpc("eth_getCode", [sys.argv[2], "latest"])[2:])
print(int(block_number_hex, 16), block["hash"].lower(), hashlib.sha256(code).hexdigest())
PY
)
EOF

test_dsn="${PHOENIX_TEST_POSTGRES_DSN:-}"
[ -n "$test_dsn" ] || {
  printf 'PHOENIX_TEST_POSTGRES_DSN is required for autonomous E2E\n' >&2
  exit 1
}
POSTGRES_DSN="$test_dsn" \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- migrate
POSTGRES_DSN="$test_dsn" \
LIVE_EXECUTOR_MAX_INPUT_AMOUNT=10000000000000000 \
LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI=10000000000000000 \
PHOENIX_AUTONOMOUS_ACTIVATION_ACK=ACTIVATE_AUTONOMOUS_LIVE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- activate

PHOENIX_TEST_POSTGRES_DSN="$test_dsn" \
PHOENIX_TEST_NATS_URL="${PHOENIX_TEST_NATS_URL:-nats://127.0.0.1:4222}" \
PHOENIX_TEST_QUOTE_PROXY_RPC_URL="$proxy_url" \
PHOENIX_TEST_ISOLATED_FORK_SIGNER_KEY="$test_key" \
PHOENIX_TEST_EXECUTOR_ADDRESS="$executor_address" \
PHOENIX_TEST_EXECUTOR_CODE_HASH="$executor_code_hash" \
PHOENIX_TEST_WALLET_ADDRESS="$(printf '%s' "$test_wallet" | tr '[:upper:]' '[:lower:]')" \
PHOENIX_TEST_BLOCK_NUMBER="$block_number" \
PHOENIX_TEST_BLOCK_HASH="$block_hash" \
  cargo test --locked --manifest-path autonomous-live-e2e/Cargo.toml \
    --test autonomous_live_e2e -- --nocapture --test-threads=1

PHOENIX_TEST_ISOLATED_FORK_RPC_URL="$rpc_url" \
PHOENIX_TEST_ISOLATED_FORK_CONFIRM=CONFIRMED_LOCAL_ANVIL \
PHOENIX_TEST_ISOLATED_FORK_SIGNER_KEY="$test_key" \
PHOENIX_TEST_EXECUTOR_ADDRESS="$executor_address" \
  cargo test --locked --manifest-path live-executor/Cargo.toml \
    --test isolated_fork -- --nocapture
