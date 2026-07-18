#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
validator="$script_dir/validate-production-env.sh"
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-env-test.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT

valid_env="$tmp_dir/valid.env"
cat >"$valid_env" <<'ENV'
PHOENIX_ENV=production
PHOENIX_MODE=SHADOW
LIVE_EXECUTION=false
CHAIN_ID=42161
POSTGRES_USER=phoenix_app
POSTGRES_PASSWORD=super-secret-password
POSTGRES_DB=phoenix
POSTGRES_DSN=postgres://phoenix_app:super-secret-password@postgres:5432/phoenix
NATS_URL=nats://nats:4222
PHOENIX_FEED_SOURCE=relay
PHOENIX_FEED_RELAY_URL=ws://nitro-feed-relay:9642/feed
PHOENIX_FEED_FIXTURE=
ARBITRUM_SEQUENCER_FEED_URL=wss://arb1-feed.arbitrum.io/feed
ARBITRUM_RPC_URL=https://arbitrum.drpc.org
PARENT_CHAIN_RPC_URL=https://eth.drpc.org
RPC_PROVIDER_URLS=https://credential-bearing-rpc.example/private-token,https://arbitrum.drpc.org
RPC_PROVIDER_WEIGHTS=4,3
RPC_UPSTREAM_CALLS_PER_SECOND=1
RPC_UPSTREAM_CALL_BURST=4
RPC_STATE_REQUESTS_PER_MINUTE=12
RPC_PROVIDER_PROBE_INTERVAL_SECONDS=60
ENGINE_ROUTER_ADDRESSES=0xe592427a0aece92de3edee1f18e0157c05861564,0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45,0xa51afafe0263b40edaef0df8781ea9aa03e381a3
RECORDER_PERSISTENCE_POLICY=money_path_v1
EXECUTOR_ADDRESS=
SIGNER_PRIVATE_KEY=
WALLET_ADDRESS=
ENV

assert_redacted() {
  output="$1"
  case "$output" in
    *super-secret-password*|*private-token*)
      echo "secret material leaked in validator output"
      echo "$output"
      exit 1
      ;;
  esac
}

output=$("$validator" "$valid_env" 2>&1)
assert_redacted "$output"
printf '%s' "$output" | grep -q 'ENV_VALID'

bad_rpc="$tmp_dir/bad-rpc.env"
sed 's/RPC_PROVIDER_WEIGHTS=4,3/RPC_PROVIDER_WEIGHTS=4/' "$valid_env" >"$bad_rpc"
if output=$("$validator" "$bad_rpc" 2>&1); then
  echo "expected RPC provider/priority mismatch to fail"
  exit 1
fi
assert_redacted "$output"
printf '%s' "$output" | grep -q 'RPC_PROVIDER_URLS count must match RPC_PROVIDER_WEIGHTS count'

bad_budget="$tmp_dir/bad-budget.env"
sed 's/RPC_UPSTREAM_CALLS_PER_SECOND=1/RPC_UPSTREAM_CALLS_PER_SECOND=0/' "$valid_env" >"$bad_budget"
if output=$("$validator" "$bad_budget" 2>&1); then
  echo "expected zero upstream call budget to fail"
  exit 1
fi
assert_redacted "$output"
printf '%s' "$output" | grep -q 'RPC_UPSTREAM_CALLS_PER_SECOND must be greater than zero'

bad_postgres="$tmp_dir/bad-postgres.env"
sed 's/POSTGRES_DSN=postgres:\/\/phoenix_app:super-secret-password@postgres:5432\/phoenix/POSTGRES_DSN=postgres:\/\/phoenix_app:different-password@postgres:5432\/phoenix/' "$valid_env" >"$bad_postgres"
if output=$("$validator" "$bad_postgres" 2>&1); then
  echo "expected PostgreSQL DSN mismatch to fail"
  exit 1
fi
assert_redacted "$output"
printf '%s' "$output" | grep -q 'POSTGRES_DSN does not match'

bad_router="$tmp_dir/bad-router.env"
sed 's/0xa51afafe0263b40edaef0df8781ea9aa03e381a3/0x1b81d678ffb9c0263b24a97847620c99d213eb14/' "$valid_env" >"$bad_router"
if output=$("$validator" "$bad_router" 2>&1); then
  echo "expected an unreviewed Engine router to fail"
  exit 1
fi
assert_redacted "$output"
printf '%s' "$output" | grep -q 'ENGINE_ROUTER_ADDRESSES contains an unreviewed router'

for live_only_name in SIGNER_PRIVATE_KEY WALLET_ADDRESS EXECUTOR_ADDRESS; do
  live_only_env="$tmp_dir/nonempty-$live_only_name.env"
  sed "s/^$live_only_name=/$live_only_name=forbidden/" "$valid_env" >"$live_only_env"
  if output=$("$validator" "$live_only_env" 2>&1); then
    echo "expected non-empty $live_only_name to fail"
    exit 1
  fi
  assert_redacted "$output"
  printf '%s' "$output" | grep -q "$live_only_name must be empty in SHADOW production"
done

echo "validate-production-env-tests: ok"
