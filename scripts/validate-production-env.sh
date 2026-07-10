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

ok() {
  echo "[OK] $1"
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

csv_count() {
  value="$1"
  if [ -z "$value" ]; then
    echo 0
    return
  fi
  count=1
  rest="$value"
  while :; do
    case "$rest" in
      *,*)
        count=$((count + 1))
        rest=${rest#*,}
        ;;
      *) break ;;
    esac
  done
  echo "$count"
}

trim_value() {
  printf '%s' "$1" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//'
}

validate_positive_integer() {
  name="$1"
  value="$2"
  case "$value" in
    ''|*[!0-9]*)
      fail "$name must be a positive integer"
      return
      ;;
  esac
  if [ "$value" -le 0 ]; then
    fail "$name must be greater than zero"
  fi
}

validate_rpc_providers() {
  url_count=$(csv_count "${RPC_PROVIDER_URLS:-}")
  priority_count=$(csv_count "${RPC_PROVIDER_WEIGHTS:-}")
  if [ "$url_count" -eq 0 ]; then
    fail "RPC_PROVIDER_URLS must contain at least one provider"
    return
  fi
  if [ "$url_count" -ne "$priority_count" ]; then
    fail "RPC_PROVIDER_URLS count must match RPC_PROVIDER_WEIGHTS count"
    return
  fi

  index=0
  rest_urls="${RPC_PROVIDER_URLS:-}"
  rest_priorities="${RPC_PROVIDER_WEIGHTS:-}"
  while :; do
    case "$rest_urls" in
      *,*)
        url=$(trim_value "${rest_urls%%,*}")
        rest_urls=${rest_urls#*,}
        ;;
      *)
        url=$(trim_value "$rest_urls")
        rest_urls=
        ;;
    esac
    case "$rest_priorities" in
      *,*)
        priority=$(trim_value "${rest_priorities%%,*}")
        rest_priorities=${rest_priorities#*,}
        ;;
      *)
        priority=$(trim_value "$rest_priorities")
        rest_priorities=
        ;;
    esac

    if [ -z "$url" ]; then
      fail "RPC provider URL at index $index is empty"
    fi
    case "$url" in
      http://*|https://*) ;;
      *) fail "RPC provider URL at index $index must be http(s)" ;;
    esac
    validate_positive_integer "RPC provider priority at index $index" "$priority"

    index=$((index + 1))
    if [ -z "$rest_urls" ] && [ -z "$rest_priorities" ]; then
      break
    fi
  done
}

validate_postgres_consistency() {
  dsn_without_scheme=${POSTGRES_DSN#postgres://}
  if [ "$dsn_without_scheme" = "$POSTGRES_DSN" ]; then
    fail "POSTGRES_DSN must use postgres://"
    return
  fi
  credentials=${dsn_without_scheme%%@*}
  after_at=${dsn_without_scheme#*@}
  if [ "$credentials" = "$dsn_without_scheme" ] || [ "$after_at" = "$dsn_without_scheme" ]; then
    fail "POSTGRES_DSN must include credentials and host"
    return
  fi
  dsn_user=${credentials%%:*}
  dsn_password=${credentials#*:}
  if [ "$dsn_user" = "$credentials" ] || [ -z "$dsn_user" ] || [ -z "$dsn_password" ]; then
    fail "POSTGRES_DSN credentials are malformed"
    return
  fi
  db_path=${after_at#*/}
  dsn_db=${db_path%%\?*}
  if [ "$db_path" = "$after_at" ] || [ -z "$dsn_db" ]; then
    fail "POSTGRES_DSN database name is missing"
    return
  fi
  if [ "$dsn_user" != "${POSTGRES_USER:-}" ] ||
    [ "$dsn_password" != "${POSTGRES_PASSWORD:-}" ] ||
    [ "$dsn_db" != "${POSTGRES_DB:-}" ]; then
    fail "POSTGRES_DSN does not match POSTGRES_USER, POSTGRES_PASSWORD, and POSTGRES_DB"
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
  ARBITRUM_SEQUENCER_FEED_URL \
  ARBITRUM_RPC_URL \
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
require_shape ARBITRUM_SEQUENCER_FEED_URL '^wss?://.+'
require_shape ARBITRUM_RPC_URL '^https?://.+'
require_shape PARENT_CHAIN_RPC_URL '^https?://.+'
require_shape RPC_GLOBAL_RPS '^[0-9]+$'
validate_positive_integer RPC_GLOBAL_RPS "${RPC_GLOBAL_RPS:-}"
validate_rpc_providers
validate_postgres_consistency

if [ -n "${SIGNER_PRIVATE_KEY:-}" ]; then
  warn "SIGNER_PRIVATE_KEY is LIVE-only and is not required for SHADOW startup"
fi

if [ -n "${EXECUTOR_ADDRESS:-}" ] && ! printf '%s' "$EXECUTOR_ADDRESS" | grep -Eq '^0x[0-9a-fA-F]{40}$'; then
  fail "EXECUTOR_ADDRESS has invalid shape"
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi

ok "Phoenix mode: SHADOW"
ok "Live execution disabled"
ok "Chain ID: 42161"
ok "Feed source: relay"
ok "Sequencer feed configured"
ok "$(csv_count "${RPC_PROVIDER_URLS:-}") RPC providers configured"
ok "RPC priority values valid"
ok "Global RPC budget: ${RPC_GLOBAL_RPS} RPS"
ok "Parent-chain RPC configured"
ok "Arbitrum RPC configured"
ok "PostgreSQL configuration consistent"
ok "LIVE-only signer configuration not required in SHADOW"
echo "ENV_VALID: required SHADOW variables are present and shaped correctly"
