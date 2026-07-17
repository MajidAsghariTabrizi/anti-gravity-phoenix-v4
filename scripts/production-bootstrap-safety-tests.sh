#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
bootstrap=$script_dir/bootstrap-production.sh
provisioner=$script_dir/provision-production-host.sh
context_installer=$script_dir/install-production-release-context.sh
release_installer=$script_dir/install-release-assets.sh
maintenance=$script_dir/prelive-protected-maintenance.sh
rollback=$script_dir/rollback-release.sh

fail() {
  echo "production-bootstrap-safety-tests: $1" >&2
  exit 1
}

for required in \
  "$bootstrap" \
  "$provisioner" \
  "$context_installer" \
  "$release_installer" \
  "$maintenance" \
  "$rollback"
do
  [ -s "$required" ] || fail "required file is missing: $required"
done

grep -F 'provision-production-host.sh' "$bootstrap" >/dev/null ||
  fail 'bootstrap does not delegate first-host provisioning'
grep -F 'install-production-release-context.sh' "$bootstrap" >/dev/null ||
  fail 'bootstrap does not delegate release-context installation'
grep -F 'install-production-release-context.sh' "$release_installer" >/dev/null ||
  fail 'release installer does not use the scoped context installer'
if grep -F 'bootstrap-production.sh' "$release_installer" >/dev/null; then
  fail 'release installation still invokes general bootstrap'
fi
if grep -F 'bootstrap-production.sh' "$maintenance" >/dev/null; then
  fail 'protected maintenance still invokes general bootstrap'
fi
if grep -F 'bootstrap-production.sh' "$rollback" >/dev/null; then
  fail 'normal rollback still invokes general bootstrap'
fi
grep -F '/bin/sh "$context_installer" "$rollback_sha"' "$maintenance" >/dev/null ||
  fail 'maintenance rollback does not use the scoped context installer'
grep -F '/bin/sh "$context_installer" "$release_sha"' "$rollback" >/dev/null ||
  fail 'normal rollback does not use the scoped context installer'
if grep -F '$deploy_root/data' "$context_installer" >/dev/null; then
  fail 'release-context installation references persistent data'
fi
if grep -E 'chown[[:space:]]+-R[^#]*/opt/phoenix/data|chmod[[:space:]]+-R[^#]*/opt/phoenix/data' \
  "$bootstrap" "$provisioner" "$context_installer" "$release_installer" "$maintenance" \
  >/dev/null
then
  fail 'a release or maintenance script recursively mutates protected data'
fi

if [ "$(uname -s)" != Linux ] ||
  ! command -v sudo >/dev/null 2>&1 ||
  ! sudo -n true >/dev/null 2>&1
then
  echo 'production-bootstrap-safety-tests: integration skipped (Linux passwordless sudo required)'
  exit 0
fi

for command_name in docker python3 sha256sum stat find; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "integration command is unavailable: $command_name"
done
sudo docker version >/dev/null 2>&1 ||
  fail 'Docker is required for the production installer fixture'
sudo docker compose version >/dev/null 2>&1 ||
  fail 'Docker Compose is required for the production installer fixture'

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-bootstrap-safety.XXXXXX")
cleanup() {
  sudo rm -rf -- "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

owner_user=$(id -un)
owner_group=$(id -gn)
host_root=$tmp_dir/host
postgres_dir=$host_root/data/postgres
nats_fixture=$host_root/data/nats-jetstream-volume
mkdir -p \
  "$postgres_dir/base/16384" \
  "$postgres_dir/global" \
  "$postgres_dir/pg_wal" \
  "$nats_fixture/messages"
printf '16\n' >"$postgres_dir/PG_VERSION"
printf 'control\n' >"$postgres_dir/global/pg_control"
printf 'filenode\n' >"$postgres_dir/global/pg_filenode.map"
printf 'pid\n' >"$postgres_dir/postmaster.pid"
printf 'fsm\n' >"$postgres_dir/base/16384/fixture_fsm"
printf 'jetstream\n' >"$nats_fixture/messages/fixture.blk"
printf 'phoenix_nats_jetstream|local|local|fixture-labels|fixture-options\n' \
  >"$nats_fixture/volume.metadata"
chmod 0700 "$postgres_dir"
chmod 0710 "$postgres_dir/base" "$postgres_dir/base/16384"
chmod 0750 "$postgres_dir/global" "$postgres_dir/pg_wal"
chmod 0600 \
  "$postgres_dir/PG_VERSION" \
  "$postgres_dir/global/pg_control" \
  "$postgres_dir/global/pg_filenode.map" \
  "$postgres_dir/postmaster.pid" \
  "$postgres_dir/base/16384/fixture_fsm"
chmod 0750 "$nats_fixture" "$nats_fixture/messages"
chmod 0640 \
  "$nats_fixture/messages/fixture.blk" \
  "$nats_fixture/volume.metadata"

valid_env=$tmp_dir/phoenix.env
cat >"$valid_env" <<'ENV'
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
EXECUTOR_ADDRESS=
SIGNER_PRIVATE_KEY=
WALLET_ADDRESS=
PUBLIC_TRANSACTION_SUBMISSION=
PRIVATE_RELAY_SUBMISSION=
TRANSACTION_BROADCAST_URL=
ENV
sudo chown root:root "$valid_env"
sudo chmod 0600 "$valid_env"

snapshot_metadata() {
  snapshot_output=$1
  (
    cd "$host_root"
    find data/postgres data/nats-jetstream-volume \
      -printf '%P|%u|%g|%m|%y\n' |
      LC_ALL=C sort
    find data/postgres data/nats-jetstream-volume -type f \
      -exec sha256sum {} \; |
      sed "s|$host_root/||" |
      LC_ALL=C sort
  ) >"$snapshot_output"
}

before=$tmp_dir/protected.before
after=$tmp_dir/protected.after
snapshot_metadata "$before"

sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'first-host provisioning changed existing protected metadata or contents'

sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" "" "$repo_root" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'release-context installation changed protected metadata or contents'

compose_target=$host_root/deploy/compose.prod.yml
compose_backup=$tmp_dir/compose.prod.yml.backup
mv "$compose_target" "$compose_backup"
ln -s "$postgres_dir/global/pg_control" "$compose_target"
if sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" "" "$repo_root" >/dev/null 2>&1
then
  fail 'release-context installation followed a protected-data symlink'
fi
rm -f -- "$compose_target"
mv "$compose_backup" "$compose_target"
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'failed symlink redirect changed protected metadata or contents'

sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$bootstrap" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'idempotent bootstrap changed protected metadata or contents'

rollback_sha=2222222222222222222222222222222222222222
rollback_asset_dir=$tmp_dir/rollback-assets
python3 "$script_dir/release_assets.py" build \
  --repo-root "$repo_root" \
  --release-sha "$rollback_sha" \
  --output-dir "$rollback_asset_dir" \
  --contract-artifact "$repo_root/fork-sandbox/abi/PhoenixExecutor.json" \
  >/dev/null
sudo env \
  PHOENIX_RELEASE_ROOT="$host_root/releases" \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  PHOENIX_CONTEXT_INSTALLER="$context_installer" \
  /bin/sh "$release_installer" \
    "$rollback_sha" \
    "$rollback_asset_dir/phoenix-release-assets-$rollback_sha.tar.gz" \
    "$rollback_asset_dir/release-assets-manifest.json" \
    "$rollback_asset_dir/release-assets-checksums.txt" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'rollback release installation changed protected metadata or contents'

release_sha=3333333333333333333333333333333333333333
asset_dir=$tmp_dir/candidate-assets
python3 "$script_dir/release_assets.py" build \
  --repo-root "$repo_root" \
  --release-sha "$release_sha" \
  --output-dir "$asset_dir" \
  --contract-artifact "$repo_root/fork-sandbox/abi/PhoenixExecutor.json" \
  >/dev/null
sudo env \
  PHOENIX_RELEASE_ROOT="$host_root/releases" \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  PHOENIX_CONTEXT_INSTALLER="$context_installer" \
  /bin/sh "$release_installer" \
    "$release_sha" \
    "$asset_dir/phoenix-release-assets-$release_sha.tar.gz" \
    "$asset_dir/release-assets-manifest.json" \
    "$asset_dir/release-assets-checksums.txt" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'maintenance-success install changed protected metadata or contents'

sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" \
    "$rollback_sha" "$host_root/releases/$rollback_sha" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'explicit rollback context restore changed protected metadata or contents'

failure_driver=$tmp_dir/maintenance-failure-driver.sh
cat >"$failure_driver" <<'SH'
#!/usr/bin/env sh
set -eu
restore_rollback() {
  failure_status=$?
  trap - EXIT
  env \
    PHOENIX_DEPLOY_ROOT="$DEPLOY_ROOT" \
    PHOENIX_ENV_FILE="$ENV_FILE" \
    PHOENIX_OWNER_USER="$OWNER_USER" \
    PHOENIX_OWNER_GROUP="$OWNER_GROUP" \
    /bin/sh "$CONTEXT_INSTALLER" "$ROLLBACK_SHA" "$ROLLBACK_ROOT"
  exit "$failure_status"
}
trap restore_rollback EXIT
env \
  PHOENIX_DEPLOY_ROOT="$DEPLOY_ROOT" \
  PHOENIX_ENV_FILE="$ENV_FILE" \
  PHOENIX_OWNER_USER="$OWNER_USER" \
  PHOENIX_OWNER_GROUP="$OWNER_GROUP" \
  /bin/sh "$CONTEXT_INSTALLER" "$CANDIDATE_SHA" "$CANDIDATE_ROOT"
exit 42
SH
chmod 0755 "$failure_driver"
if sudo env \
  CONTEXT_INSTALLER="$context_installer" \
  DEPLOY_ROOT="$host_root" \
  ENV_FILE="$valid_env" \
  OWNER_USER="$owner_user" \
  OWNER_GROUP="$owner_group" \
  CANDIDATE_SHA="$release_sha" \
  CANDIDATE_ROOT="$host_root/releases/$release_sha" \
  ROLLBACK_SHA="$rollback_sha" \
  ROLLBACK_ROOT="$host_root/releases/$rollback_sha" \
  /bin/sh "$failure_driver" >/dev/null
then
  fail 'internal failure fixture did not retain its failure status'
fi
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'failure-triggered rollback changed protected metadata or contents'

unsafe_root=$tmp_dir/unsafe-host
mkdir -p "$unsafe_root/data/postgres"
printf '16\n' >"$unsafe_root/data/postgres/PG_VERSION"
sudo chown root:root "$unsafe_root/data/postgres"
sudo chown "$owner_user:$owner_group" "$unsafe_root/data/postgres/PG_VERSION"
sudo chmod 0700 "$unsafe_root/data/postgres"
if sudo env \
  PHOENIX_DEPLOY_ROOT="$unsafe_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null 2>&1
then
  fail 'unsafe PostgreSQL ownership was silently repaired'
fi
[ "$(stat -c '%U:%G' "$unsafe_root/data/postgres")" = root:root ] ||
  fail 'unsafe PostgreSQL ownership was modified after fail-closed validation'

echo 'production-bootstrap-safety-tests: ok'
