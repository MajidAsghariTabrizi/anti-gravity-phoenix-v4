#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=$repo_dir/compose.fork.yml

fail() {
  echo "fork-sandbox-run: $1" >&2
  exit 1
}

require_env() {
  eval "value=\${$1-}"
  [ -n "$value" ] || fail "$1 is required"
}

for name in \
  FORK_ANVIL_DIGEST FORK_POSTGRES_PASSWORD FORK_UPSTREAM_RPC_URL FORK_BLOCK_NUMBER \
  FORK_SHADOW_DECISION_ID FORK_ALLOWED_TOKENS FORK_ALLOWED_POOLS FORK_ALLOWED_ROUTERS \
  FORK_ALLOWED_PROTOCOLS FORK_TARGET_CONTRACT FORK_TARGET_CODE_HASH FORK_SIMULATION_FROM \
  FORK_MINIMUM_NET_PNL FORK_MAXIMUM_INPUT_AMOUNT FORK_SLIPPAGE_BPS; do
  require_env "$name"
done

[ "${#FORK_ANVIL_DIGEST}" -eq 64 ] ||
  fail "FORK_ANVIL_DIGEST must contain 64 hex characters"
case "$FORK_ANVIL_DIGEST" in
  *[!0-9a-f]*) fail "FORK_ANVIL_DIGEST must be lowercase hexadecimal" ;;
esac

for name in SIGNER_PRIVATE_KEY PRIVATE_KEY MNEMONIC WALLET_ADDRESS EXECUTOR_ADDRESS; do
  eval "value=\${$name-}"
  [ -z "$value" ] || fail "$name must remain empty"
done

command -v docker >/dev/null 2>&1 || fail "docker is required"
compose() {
  docker compose --project-name phoenix-fork-sandbox -f "$compose_file" \
    --profile fork-sandbox "$@"
}

started=false
stop_services() {
  if [ "$started" = true ]; then
    compose stop anvil fork-postgres >/dev/null 2>&1 || true
  fi
}
trap stop_services EXIT HUP INT TERM

compose config --quiet
compose build fork-migrations fork-sandbox
compose up --detach --wait --wait-timeout 120 fork-postgres anvil
started=true
compose run --rm --no-deps fork-migrations
compose run --rm --no-deps fork-sandbox
