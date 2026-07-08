#!/usr/bin/env sh
set -eu

env_file="${1:-/etc/phoenix/phoenix.env}"

if [ ! -f "$env_file" ]; then
  echo "MISSING_ENV_FILE: $env_file"
  exit 1
fi

set -a
# shellcheck disable=SC1090
. "$env_file"
set +a

failed=0

fail() {
  echo "ENV_INVALID: $1"
  failed=1
}

warn() {
  echo "ENV_WARNING: $1"
}

require_var() {
  name="$1"
  eval "value=\${$name:-}"
  if [ -z "$value" ]; then
    fail "$name is required"
  fi
}

require_shape() {
  name="$1"
  pattern="$2"
  eval "value=\${$name:-}"
  if [ -n "$value" ] && ! printf '%s' "$value" | grep -Eq "$pattern"; then
    fail "$name has invalid shape"
  fi
}

for name in \
  PHOENIX_ENV \
  PHOENIX_MODE \
  LIVE_EXECUTION \
  CHAIN_ID \
  POSTGRES_USER \
  POSTGRES_PASSWORD \
  POSTGRES_DB \
  POSTGRES_DSN \
  NATS_URL \
  PHOENIX_FEED_SOURCE \
  PHOENIX_FEED_RELAY_URL \
  PARENT_CHAIN_RPC_URL \
  RPC_PROVIDER_URLS \
  RPC_PROVIDER_WEIGHTS \
  RPC_GLOBAL_RPS
do
  require_var "$name"
done

[ "${PHOENIX_ENV:-}" = "production" ] || fail "PHOENIX_ENV must be production"
[ "${PHOENIX_MODE:-}" = "SHADOW" ] || fail "PHOENIX_MODE must be SHADOW"
[ "${LIVE_EXECUTION:-}" = "false" ] || fail "LIVE_EXECUTION must be false for shadow production"
[ "${CHAIN_ID:-}" = "42161" ] || fail "CHAIN_ID must be 42161"
[ "${PHOENIX_FEED_SOURCE:-}" = "relay" ] || fail "PHOENIX_FEED_SOURCE must be relay"

if [ -n "${PHOENIX_FEED_FIXTURE:-}" ]; then
  fail "PHOENIX_FEED_FIXTURE must not be set in production"
fi

if [ "${POSTGRES_USER:-}" = "phoenix" ]; then
  fail "POSTGRES_USER must not use the local default"
fi

if [ "${POSTGRES_PASSWORD:-}" = "phoenix" ] || [ "${POSTGRES_PASSWORD:-}" = "REPLACE_ME" ]; then
  fail "POSTGRES_PASSWORD must not use a local or placeholder value"
fi

case "${POSTGRES_DSN:-}" in
  *REPLACE_ME*|*placeholder*|*example*) fail "POSTGRES_DSN contains a placeholder marker" ;;
esac

require_shape POSTGRES_DSN '^postgres://[^:]+:[^@]+@[^/]+/.+'
require_shape NATS_URL '^nats://[^:]+:[0-9]+$'
require_shape PHOENIX_FEED_RELAY_URL '^ws://[^/]+(:[0-9]+)?/.+'
require_shape PARENT_CHAIN_RPC_URL '^https?://.+'
require_shape RPC_GLOBAL_RPS '^[0-9]+$'

if [ -n "${SIGNER_PRIVATE_KEY:-}" ]; then
  warn "SIGNER_PRIVATE_KEY is LIVE-only and is not required for SHADOW startup"
fi

if [ -n "${EXECUTOR_ADDRESS:-}" ] && ! printf '%s' "$EXECUTOR_ADDRESS" | grep -Eq '^0x[0-9a-fA-F]{40}$'; then
  fail "EXECUTOR_ADDRESS has invalid shape"
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi

echo "ENV_VALID: required SHADOW variables are present and shaped correctly"
