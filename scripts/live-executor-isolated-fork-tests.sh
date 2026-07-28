#!/usr/bin/env sh
set -eu

for command in forge cast anvil cargo python3 psql; do
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
  --block-base-fee-per-gas 1 --gas-price 1 \
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
PHOENIX_RELEASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
PHOENIX_ENGINE_IMAGE=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
LIVE_EXECUTOR_EXECUTOR_CODE_HASH="$executor_code_hash" \
LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI=10000000000000000 \
PHOENIX_DISARMED_DEPLOY_ACK=INSTALL_DISARMED_EVIDENCE_RELEASE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- disarmed-deploy

if POSTGRES_DSN="$test_dsn" \
  PHOENIX_RELEASE_SHA=cccccccccccccccccccccccccccccccccccccccc \
  PHOENIX_ENGINE_IMAGE=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- evidence-start >/dev/null 2>&1
then
  printf 'evidence-start accepted the wrong release SHA\n' >&2
  exit 1
fi
if POSTGRES_DSN="$test_dsn" \
  PHOENIX_RELEASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  PHOENIX_ENGINE_IMAGE=sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- evidence-start >/dev/null 2>&1
then
  printf 'evidence-start accepted the wrong Engine digest\n' >&2
  exit 1
fi

POSTGRES_DSN="$test_dsn" \
PHOENIX_RELEASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
PHOENIX_ENGINE_IMAGE=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- evidence-start
if POSTGRES_DSN="$test_dsn" \
  PHOENIX_RELEASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  PHOENIX_ENGINE_IMAGE=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- evidence-start >/dev/null 2>&1
then
  printf 'repeated evidence-start was not safely rejected\n' >&2
  exit 1
fi

evidence_atomic="$(
  psql -X -qAt "$test_dsn" -c "
    SELECT (
      economic.phase = 'DISARMED_EVIDENCE'
      AND NOT legacy.armed AND legacy.kill_switch
      AND NOT global.armed AND global.kill_switch
      AND global.execution_mode = 'disarmed'
      AND NOT route.enabled AND route.kill_switch
      AND transition.from_phase = 'DISARMED_DEPLOY'
      AND transition.to_phase = 'DISARMED_EVIDENCE'
      AND transition.control_epoch = economic.control_epoch
      AND transition.transitioned_at = economic.updated_at
    )::text
    FROM live_canary.economic_control economic
    CROSS JOIN live_canary.control legacy
    CROSS JOIN live_canary.autonomous_global_control global
    JOIN live_canary.autonomous_route_controls route
      ON route.route_fingerprint = economic.route_fingerprint
    JOIN live_canary.economic_transitions transition
      ON transition.control_epoch = economic.control_epoch
     AND transition.to_phase = 'DISARMED_EVIDENCE'
    WHERE economic.singleton AND legacy.singleton AND global.singleton"
)"
[ "$evidence_atomic" = true ] || {
  printf 'DISARMED_EVIDENCE transition and ledger were not atomic and fail-closed\n' >&2
  exit 1
}

POSTGRES_DSN="$test_dsn" \
PHOENIX_AUTONOMOUS_DISARM_ACK=DISARM_AUTONOMOUS_LIVE_42161 \
PHOENIX_AUTONOMOUS_DISARM_REASON=isolated_evidence_rollback \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- disarm
[ "$(psql -X -qAt "$test_dsn" -c \
  "SELECT phase FROM live_canary.economic_control WHERE singleton")" = DISARMED_FAILURE ] || {
  printf 'rollback from DISARMED_EVIDENCE did not end in DISARMED_FAILURE\n' >&2
  exit 1
}

POSTGRES_DSN="$test_dsn" \
PHOENIX_RELEASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
PHOENIX_ENGINE_IMAGE=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
LIVE_EXECUTOR_EXECUTOR_CODE_HASH="$executor_code_hash" \
LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI=10000000000000000 \
PHOENIX_DISARMED_DEPLOY_ACK=INSTALL_DISARMED_EVIDENCE_RELEASE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- disarmed-deploy
POSTGRES_DSN="$test_dsn" \
PHOENIX_RELEASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
PHOENIX_ENGINE_IMAGE=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
PHOENIX_EVIDENCE_START_ACK=START_DISARMED_EVIDENCE_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- evidence-start
sleep 2

read -r economic_epoch global_epoch route_epoch evidence_started_at <<EOF
$(psql -X -qAt "$test_dsn" -F ' ' -c \
  "SELECT economic.control_epoch, global.control_epoch, route.control_epoch,
          to_char(economic.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')
   FROM live_canary.economic_control economic
   CROSS JOIN live_canary.autonomous_global_control global
   JOIN live_canary.autonomous_route_controls route
     ON route.route_fingerprint = 'arbitrum-weth-usdc-uniswap-v3-500-3000-v1'
   WHERE economic.singleton AND global.singleton
     AND economic.phase = 'DISARMED_EVIDENCE'")
EOF
readiness_file="$tmp_dir/canary-readiness.json"
authorization_file="$tmp_dir/automation-authorization.json"
python3 - "$readiness_file" "$authorization_file" \
  "$economic_epoch" "$global_epoch" "$route_epoch" "$executor_code_hash" \
  "$evidence_started_at" <<'PY'
import datetime
import hashlib
import json
import sys


def timestamp(value):
    return value.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def bind_hash(value, field, domain, schema):
    body = dict(value)
    body.pop(field)
    canonical = json.dumps(body, sort_keys=True, separators=(",", ":"))
    prefix = f"phoenix.canonical-json.v1:{domain}:{schema}\n"
    return hashlib.sha256((prefix + canonical).encode()).hexdigest()


now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
evidence_started_at = datetime.datetime.fromisoformat(sys.argv[7].replace("Z", "+00:00"))
binding = {
    "release_sha": "a" * 40,
    "engine_image_digest": "sha256:" + "b" * 64,
    "route_fingerprint": "arbitrum-weth-usdc-uniswap-v3-500-3000-v1",
    "route_universe_hash": "84adac686635535486e06e44fcaf90c812dc27273affc5bffc4eebd6c164928c",
    "route_policy_hash": "d7aff21eb025696208c646631772a45c241fc2971ef0c9866646d12dca12d476",
    "risk_policy_hash": "d7aff21eb025696208c646631772a45c241fc2971ef0c9866646d12dca12d476",
    "economic_control_epoch": int(sys.argv[3]),
    "global_control_epoch": int(sys.argv[4]),
    "route_control_epoch": int(sys.argv[5]),
    "executor_code_hash": sys.argv[6],
    "contract_identity_hash": "c" * 64,
    "wallet_gas_reserve_wei": 2,
    "gas_reserve_floor_wei": 1,
    "current_daily_loss_wei": 0,
    "daily_loss_limit_wei": 10000000000000000,
    "observed_from": evidence_started_at.isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "observed_until": timestamp(now - datetime.timedelta(seconds=1)),
    "created_at": timestamp(now),
    "expires_at": timestamp(now + datetime.timedelta(minutes=10)),
    "candidate_evidence_hashes": ["d" * 64],
}
evidence = {
    "supported_observations": 100,
    "valid_acceptance_bps": 9990,
    "process_fatal_integrity_exits": 0,
    "quarantine_progress_proven": True,
    "consumer_pending_bounded": True,
    "ack_pending_bounded": True,
    "stale_outbox_rows": 0,
    "primary_rpc_healthy": True,
    "secondary_rpc_healthy": True,
    "rpc_providers_independent": True,
    "eligible_rpc_disagreements": 0,
    "maximum_state_age_blocks": 1,
    "maximum_quote_age_ms": 2000,
    "maximum_candidate_age_ms": 3000,
    "fork_attempts": 100,
    "fork_passes": 95,
    "prediction_error_bps": 1000,
    "secondary_skips": 0,
    "fork_skips": 0,
    "execution_requests": 0,
    "active_attempts": 0,
    "positive_independent_fork_candidates": 1,
}
readiness = {
    "schema_version": "phoenix.canary-readiness.v1",
    "readiness_id": "11111111-1111-4111-8111-111111111111",
    "binding": binding,
    "evidence": evidence,
    "readiness_hash": "0" * 64,
}
readiness["readiness_hash"] = bind_hash(
    readiness,
    "readiness_hash",
    "canary-readiness",
    "phoenix.canary-readiness.v1",
)
authorization = {
    "schema_version": "phoenix.automation-authorization.v1",
    "authorization_id": "22222222-2222-4222-8222-222222222222",
    "authorization": {
        "route_fingerprint": binding["route_fingerprint"],
        "route_policy_hash": binding["route_policy_hash"],
        "maximum_reviewed_input_wei": 10000000000000000,
        "executor_code_hash": sys.argv[6],
        "release_family": "isolated-test",
        "one_transaction_at_a_time": True,
        "reviewed_ladder_only": True,
        "automatic_disarm_required": True,
        "expires_at": timestamp(now + datetime.timedelta(minutes=10)),
    },
    "authorization_hash": "0" * 64,
}
authorization["authorization_hash"] = bind_hash(
    authorization,
    "authorization_hash",
    "automation-authorization",
    "phoenix.automation-authorization.v1",
)
for path, value in ((sys.argv[1], readiness), (sys.argv[2], authorization)):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
PY
chmod 600 "$readiness_file" "$authorization_file"
POSTGRES_DSN="$test_dsn" \
PHOENIX_CANARY_READINESS_FILE="$readiness_file" \
PHOENIX_CANARY_READINESS_ACK=CREATE_HASH_BOUND_CANARY_READINESS_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- create-readiness
POSTGRES_DSN="$test_dsn" \
PHOENIX_AUTOMATION_AUTHORIZATION_FILE="$authorization_file" \
PHOENIX_AUTOMATION_AUTHORIZATION_ACK=INSTALL_BOUNDED_AUTOMATION_AUTHORIZATION_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- install-authorization
POSTGRES_DSN="$test_dsn" \
LIVE_EXECUTOR_MAX_INPUT_AMOUNT=10000000000000000 \
LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI=10000000000000000 \
PHOENIX_CANARY_READINESS_ID=11111111-1111-4111-8111-111111111111 \
PHOENIX_AUTOMATION_AUTHORIZATION_ID=22222222-2222-4222-8222-222222222222 \
PHOENIX_AUTONOMOUS_ACTIVATION_ACK=ACTIVATE_READY_MIN_CANARY_42161 \
  cargo run --locked --quiet --manifest-path live-executor/Cargo.toml \
    --bin autonomous-live-control -- activate-ready-canary

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
