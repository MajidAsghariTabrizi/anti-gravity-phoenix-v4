#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
compose_file=$repo_dir/compose.prod.yml
tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-compose-route.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi

route_json='[{"route_id":"arb1-weth-usdc-uni500-uni3000-canary","token_in":"0x82af49447d8a07e3bd95bd0d56f35241523fbab1","token_out":"0xaf88d065e77c8cc2239327c5edb3a432268e5831","amount_in":"1000000000000000","legs":[{"pool_id":"0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:500","protocol":"UniswapV3","fee":500},{"pool_id":"0x82af49447d8a07e3bd95bd0d56f35241523fbab1:0xaf88d065e77c8cc2239327c5edb3a432268e5831:3000","protocol":"UniswapV3","fee":3000}],"max_hops":2}]'

run_compose() {
  if [ -n "${PHOENIX_COMPOSE_BIN:-}" ]; then
    "$PHOENIX_COMPOSE_BIN" "$@"
  else
    docker compose "$@"
  fi
}

write_env() {
  target=$1
  include_route=$2
  cat >"$target" <<'ENV'
POSTGRES_USER=phoenix_app
POSTGRES_PASSWORD=ci-placeholder-not-production
POSTGRES_DB=phoenix
POSTGRES_DSN=postgres://phoenix_app:ci-placeholder-not-production@postgres:5432/phoenix
PHOENIX_FEED_RELAY_URL=ws://nitro-feed-relay:9642/feed
ARBITRUM_SEQUENCER_FEED_URL=wss://sequencer-feed-placeholder.invalid
RPC_PROVIDER_URLS=https://rpc-placeholder.invalid
ENGINE_ROUTER_ADDRESSES=0xe592427a0aece92de3edee1f18e0157c05861564
FEED_INGESTOR_IMAGE=ghcr.io/majidasgharitabrizi/feed-ingestor@sha256:0000000000000000000000000000000000000000000000000000000000000000
PHOENIX_ENGINE_IMAGE=ghcr.io/majidasgharitabrizi/phoenix-engine@sha256:0000000000000000000000000000000000000000000000000000000000000000
RPC_GATEWAY_IMAGE=ghcr.io/majidasgharitabrizi/rpc-gateway@sha256:0000000000000000000000000000000000000000000000000000000000000000
RECORDER_IMAGE=ghcr.io/majidasgharitabrizi/recorder@sha256:0000000000000000000000000000000000000000000000000000000000000000
DASHBOARD_IMAGE=ghcr.io/majidasgharitabrizi/dashboard@sha256:0000000000000000000000000000000000000000000000000000000000000000
ENV
  if [ "$include_route" = yes ]; then
    printf 'ENGINE_ROUTE_REGISTRY_JSON=%s\n' "$route_json" >>"$target"
  fi
}

render() {
  env_file=$1
  output=$2
  set -a
  # shellcheck disable=SC1090
  . "$env_file"
  set +a
  (
    unset ENGINE_ROUTE_REGISTRY_JSON
    PHOENIX_ENV_FILE="$env_file" run_compose --env-file "$env_file" \
      -f "$compose_file" config --format json >"$output"
  )
}

render_with_inherited_override() {
  env_file=$1
  output=$2
  set -a
  # shellcheck disable=SC1090
  . "$env_file"
  set +a
  PHOENIX_ENV_FILE="$env_file" run_compose --env-file "$env_file" \
    -f "$compose_file" config --format json >"$output"
}

route_env=$tmp_dir/route.env
route_config=$tmp_dir/route-config.json
broken_config=$tmp_dir/broken-config.json
write_env "$route_env" yes
render_with_inherited_override "$route_env" "$broken_config"
if python3 "$script_dir/verify-compose-route-registry.py" \
  --compose-config "$broken_config" --expected-env-file "$route_env" >/dev/null 2>&1; then
  echo 'inherited shell override unexpectedly preserved route JSON' >&2
  exit 1
fi
render "$route_env" "$route_config"
python3 "$script_dir/verify-compose-route-registry.py" \
  --compose-config "$route_config" --expected-env-file "$route_env"

empty_env=$tmp_dir/empty.env
empty_config=$tmp_dir/empty-config.json
write_env "$empty_env" no
render "$empty_env" "$empty_config"
python3 "$script_dir/verify-compose-route-registry.py" \
  --compose-config "$empty_config" --expected-env-file "$empty_env" --allow-empty

grep -F 'unset ENGINE_ROUTE_REGISTRY_JSON' "$script_dir/shadow-engine-live-smoke.sh" >/dev/null

echo 'compose-route-registry-tests: ok'
