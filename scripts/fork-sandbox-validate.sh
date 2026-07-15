#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)

fail() {
  echo "fork-sandbox-validate: $1" >&2
  exit 1
}

compose_file=$repo_dir/compose.fork.yml
production_compose=$repo_dir/compose.prod.yml
engine_manifest=$repo_dir/phoenix-engine/Cargo.toml
source_dir=$repo_dir/fork-sandbox/src

[ -f "$compose_file" ] || fail "fork-only Compose file is missing"
[ -f "$repo_dir/deploy/fork-sandbox.Dockerfile" ] || fail "sandbox Dockerfile is missing"
[ -f "$repo_dir/migrations/010_fork_simulation_evidence.sql" ] || fail "evidence migration is missing"

grep -F 'profiles: ["fork-sandbox"]' "$compose_file" >/dev/null ||
  fail "every fork service must stay behind the fork-sandbox profile"
[ "$(grep -c 'profiles: \["fork-sandbox"\]' "$compose_file")" -eq 4 ] ||
  fail "unexpected fork service profile count"
grep -F 'network_mode: "service:anvil"' "$compose_file" >/dev/null ||
  fail "sandbox does not share the loopback-only Anvil namespace"
grep -F 'FORK_RPC_URL: http://127.0.0.1:8545' "$compose_file" >/dev/null ||
  fail "sandbox RPC is not loopback-only"
grep -F 'FORK_TARGET_CODE_HASH:' "$compose_file" >/dev/null ||
  fail "reviewed target bytecode hash is not required"
grep -F 'PHOENIX_MODE: SHADOW' "$compose_file" >/dev/null || fail "SHADOW mode is not fixed"
grep -F 'LIVE_EXECUTION: "false"' "$compose_file" >/dev/null ||
  fail "LIVE execution is not fixed false"
grep -F 'ghcr.io/foundry-rs/foundry@sha256:${FORK_ANVIL_DIGEST:' "$compose_file" >/dev/null ||
  fail "Anvil image is not structurally digest-pinned"
if grep -Eq '(^|[[:space:]])ports:' "$compose_file"; then
  fail "fork services must not publish host ports"
fi
if grep -Eq 'fork-sandbox|PHOENIX_FORK_MODE|FORK_RPC_URL' "$production_compose"; then
  fail "production Compose references the fork sandbox"
fi
if grep -Eq 'phoenix-fork-sandbox|fork-sandbox' "$engine_manifest"; then
  fail "production Engine depends on the fork sandbox"
fi

for method in eth_sendRawTransaction eth_sendTransaction anvil_impersonateAccount; do
  if grep -R -F "$method" "$source_dir" >/dev/null; then
    fail "forbidden RPC method is present in sandbox source"
  fi
done
grep -F 'execution_eligible: false' "$source_dir/planner.rs" >/dev/null ||
  fail "unsigned plan does not force execution_eligible false"
grep -F 'execution_request_created: false' "$source_dir/planner.rs" >/dev/null ||
  fail "unsigned plan does not force execution_request_created false"
grep -F 'public_broadcast: false' "$source_dir/runner.rs" >/dev/null ||
  fail "sandbox result does not force public_broadcast false"
grep -F 'signer_used: false' "$source_dir/runner.rs" >/dev/null ||
  fail "sandbox result does not force signer_used false"
grep -F 'compose up --detach --wait --wait-timeout 120 fork-postgres anvil' \
  "$repo_dir/scripts/fork-sandbox-run.sh" >/dev/null ||
  fail "launcher does not wait for isolated dependencies"
if grep -F -- '--abort-on-container-exit' "$repo_dir/scripts/fork-sandbox-run.sh" >/dev/null; then
  fail "launcher can abort on successful migration completion"
fi

echo "fork-sandbox-validate: PASS"
