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
cleanup() {
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

PHOENIX_TEST_ISOLATED_FORK_RPC_URL="$rpc_url" \
PHOENIX_TEST_ISOLATED_FORK_CONFIRM=CONFIRMED_LOCAL_ANVIL \
PHOENIX_TEST_ISOLATED_FORK_SIGNER_KEY="$test_key" \
PHOENIX_TEST_EXECUTOR_ADDRESS="$executor_address" \
  cargo test --locked --manifest-path live-executor/Cargo.toml \
    --test isolated_fork -- --nocapture
