#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
gateway_installer=$script_dir/install-shadow-deploy-gateway.sh

fail() {
  echo "production-release-context-compatibility-tests: $1" >&2
  exit 1
}

if [ "$(uname -s)" != Linux ] ||
  ! command -v sudo >/dev/null 2>&1 ||
  ! sudo -n true >/dev/null 2>&1 ||
  ! command -v visudo >/dev/null 2>&1
then
  echo 'production-release-context-compatibility-tests: integration skipped (Linux passwordless sudo and visudo required)'
  exit 0
fi

for command_name in cmp python3 stat tar; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "integration command is unavailable: $command_name"
done

tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-release-context-compatibility.XXXXXX")
cleanup() {
  sudo rm -rf -- "$tmp_root"
}
trap cleanup EXIT HUP INT TERM

owner_user=$(id -un)
owner_group=$(id -gn)
gateway_root=$tmp_root/gateway
fake_bin=$tmp_root/bin
env_file=$tmp_root/phoenix.env
mkdir -p "$gateway_root" "$fake_bin"

cat >"$fake_bin/docker" <<'SH'
#!/usr/bin/env sh
exit 0
SH
chmod 0755 "$fake_bin/docker"

cat >"$env_file" <<'ENV'
PHOENIX_ENV=production
PHOENIX_MODE=SHADOW
LIVE_EXECUTION=false
CHAIN_ID=42161
POSTGRES_USER=phoenix_app
POSTGRES_PASSWORD=ci-only-password
POSTGRES_DB=phoenix
POSTGRES_DSN=postgres://phoenix_app:ci-only-password@postgres:5432/phoenix
NATS_URL=nats://nats:4222
PHOENIX_FEED_SOURCE=relay
PHOENIX_FEED_RELAY_URL=ws://nitro-feed-relay:9642/feed
PHOENIX_FEED_FIXTURE=
ARBITRUM_SEQUENCER_FEED_URL=wss://arb1-feed.arbitrum.io/feed
ARBITRUM_RPC_URL=https://arbitrum.example.invalid
PARENT_CHAIN_RPC_URL=https://ethereum.example.invalid
RPC_PROVIDER_URLS=https://provider-one.example.invalid,https://provider-two.example.invalid
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
PUBLIC_TRANSACTION_SUBMISSION=
PRIVATE_RELAY_SUBMISSION=
TRANSACTION_BROADCAST_URL=
ENV
sudo chown root:root "$env_file"
sudo chmod 0600 "$env_file"

sudo env \
  PHOENIX_GATEWAY_TESTING=1 \
  PHOENIX_GATEWAY_TEST_ROOT="$gateway_root" \
  /bin/sh "$gateway_installer" >/dev/null

trusted_dir=$gateway_root/usr/local/libexec/phoenix-shadow-deploy
context_installer=$trusted_dir/install-production-release-context.sh
trusted_registry=$trusted_dir/release-components.json
trusted_healthcheck=$trusted_dir/production-healthcheck.sh
runtime_path=$fake_bin:/usr/sbin:/usr/bin:/sbin:/bin

run_installer() {
  deploy_root=$1
  release_sha=$2
  source_root=$3
  mkdir -p "$deploy_root"
  sudo env \
    PATH="$runtime_path" \
    PHOENIX_DEPLOY_ROOT="$deploy_root" \
    PHOENIX_ENV_FILE="$env_file" \
    PHOENIX_OWNER_USER="$owner_user" \
    PHOENIX_OWNER_GROUP="$owner_group" \
    /bin/sh "$context_installer" "$release_sha" "$source_root" >/dev/null
}

install_trusted_registry() {
  sudo install -m 0600 -o root -g root \
    "$repo_root/release-components.json" "$trusted_registry"
}

printf 'trusted fallback must not be selected for a modern release\n' \
  >"$tmp_root/not-a-registry"
sudo install -m 0600 -o root -g root \
  "$tmp_root/not-a-registry" "$trusted_registry"
modern_root=$tmp_root/modern-host
run_installer "$modern_root" "" "$repo_root"
sudo cmp "$repo_root/release-components.json" \
  "$modern_root/deploy/release-components.json" >/dev/null ||
  fail 'modern release did not use its own component registry'
sudo cmp "$trusted_healthcheck" \
  "$modern_root/deploy/production-healthcheck.sh" >/dev/null ||
  fail 'modern context did not receive the trusted production healthcheck'

legacy_sha=4444444444444444444444444444444444444444
asset_dir=$tmp_root/release-assets
python3 "$script_dir/release_assets.py" build \
  --repo-root "$repo_root" \
  --release-sha "$legacy_sha" \
  --output-dir "$asset_dir" \
  --contract-artifact "$repo_root/fork-sandbox/abi/PhoenixExecutor.json" \
  >/dev/null
tar -xzf "$asset_dir/phoenix-release-assets-$legacy_sha.tar.gz" -C "$tmp_root"
legacy_root=$tmp_root/phoenix-release-$legacy_sha
rm -f -- "$legacy_root/release-components.json"
cat >"$legacy_root/scripts/production-healthcheck.sh" <<'SH'
#!/usr/bin/env sh
wget -q -O - http://127.0.0.1:8547/livenessprobe
SH
chmod 0755 "$legacy_root/scripts/production-healthcheck.sh"

install_trusted_registry
legacy_host=$tmp_root/legacy-host
run_installer "$legacy_host" "$legacy_sha" "$legacy_root"
sudo cmp "$trusted_registry" \
  "$legacy_host/deploy/release-components.json" >/dev/null ||
  fail 'legacy release did not use the exact trusted component registry'
sudo cmp "$trusted_healthcheck" \
  "$legacy_host/deploy/production-healthcheck.sh" >/dev/null ||
  fail 'rollback context did not receive the corrected trusted healthcheck'
if sudo grep -F '8547' "$legacy_host/deploy/production-healthcheck.sh" >/dev/null; then
  fail 'rollback context retained the legacy invalid healthcheck'
fi

expect_legacy_fallback_failure() {
  case_name=$1
  unsafe_host=$tmp_root/unsafe-$case_name
  mkdir -p "$unsafe_host"
  if sudo env \
    PATH="$runtime_path" \
    PHOENIX_DEPLOY_ROOT="$unsafe_host" \
    PHOENIX_ENV_FILE="$env_file" \
    PHOENIX_OWNER_USER="$owner_user" \
    PHOENIX_OWNER_GROUP="$owner_group" \
    /bin/sh "$context_installer" "" "$legacy_root" >/dev/null 2>&1
  then
    fail "unsafe trusted component fallback was accepted: $case_name"
  fi
}

sudo chown "$owner_user:$owner_group" "$trusted_registry"
expect_legacy_fallback_failure ownership
install_trusted_registry

sudo chmod 0640 "$trusted_registry"
expect_legacy_fallback_failure mode
install_trusted_registry

sudo rm -f -- "$trusted_registry"
sudo ln -s "$repo_root/release-components.json" "$trusted_registry"
expect_legacy_fallback_failure symlink
sudo rm -f -- "$trusted_registry"
install_trusted_registry

sudo ln "$trusted_registry" "$trusted_dir/release-components.link"
expect_legacy_fallback_failure hard-link
sudo rm -f -- "$trusted_dir/release-components.link"
install_trusted_registry

sudo rm -f -- "$trusted_registry"
expect_legacy_fallback_failure missing
install_trusted_registry

unsafe_source=$tmp_root/unsafe-source
cp -R "$legacy_root" "$unsafe_source"
ln -s "$repo_root/release-components.json" \
  "$unsafe_source/release-components.json"
unsafe_source_host=$tmp_root/unsafe-source-host
mkdir -p "$unsafe_source_host"
if sudo env \
  PATH="$runtime_path" \
  PHOENIX_DEPLOY_ROOT="$unsafe_source_host" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" "" "$unsafe_source" >/dev/null 2>&1
then
  fail 'unsafe present release registry incorrectly selected the trusted fallback'
fi

echo 'production-release-context-compatibility-tests: ok'
