#!/usr/bin/env sh
# Literal contract patterns must retain their dollar signs.
# shellcheck disable=SC2016
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
bootstrap=$script_dir/bootstrap-production.sh
provisioner=$script_dir/provision-production-host.sh
context_installer=$script_dir/install-production-release-context.sh
release_installer=$script_dir/install-release-assets.sh
maintenance=$script_dir/prelive-protected-maintenance.sh
rollback=$script_dir/rollback-release.sh
compose_file=$repo_root/compose.prod.yml

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
grep -F "fail 'host provisioning stage failed'" "$bootstrap" >/dev/null ||
  fail 'bootstrap lacks a stage-specific host provisioning diagnostic'
grep -F "fail 'release context installation stage failed'" "$bootstrap" >/dev/null ||
  fail 'bootstrap lacks a stage-specific release-context diagnostic'
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
grep -F 'user: "65534:65534"' "$compose_file" >/dev/null ||
  fail 'Prometheus runtime UID/GID is not explicit in production Compose'
grep -F '"$deploy_dir/prometheus/prometheus.yml" 0644' "$context_installer" >/dev/null ||
  fail 'installed Prometheus configuration is not mode 0644'
grep -F '"$deploy_dir/compose.live-canary.yml" 0640' "$context_installer" >/dev/null ||
  fail 'live-canary Compose overlay does not use the existing Compose mode policy'
grep -F '"$deploy_dir/live-executor/schema/001_live_canary.sql" 0640' \
  "$context_installer" >/dev/null ||
  fail 'live-canary base schema does not use the existing data-file mode policy'
grep -F '"$deploy_dir/live-executor/schema/002_approval_evidence.sql" 0640' \
  "$context_installer" >/dev/null ||
  fail 'live-canary approval schema does not use the existing data-file mode policy'
if grep -E '(^|[[:space:]])psql([[:space:]]|$)|migration-runner' \
  "$context_installer" "$release_installer" >/dev/null
then
  fail 'release installation must package schemas without applying them'
fi
grep -F 'prometheus_runtime_uid=65534' "$provisioner" >/dev/null ||
  fail 'Prometheus provisioning UID is not explicit'
grep -F 'prometheus_runtime_gid=65534' "$provisioner" >/dev/null ||
  fail 'Prometheus provisioning GID is not explicit'
grep -F 'directory ownership or mode could not be enforced:' "$provisioner" >/dev/null ||
  fail 'fresh-host directory provisioning lacks a stage-specific diagnostic'
if grep -E 'chown[[:space:]]+-R.*data/postgres|chmod[[:space:]]+-R.*data/postgres' \
  "$provisioner" >/dev/null
then
  fail 'first-host provisioning recursively mutates PostgreSQL data'
fi
grep -F '[ -z "$first_entry" ] || return 0' "$provisioner" >/dev/null ||
  fail 'non-empty PostgreSQL preservation does not return success explicitly'
grep -F '[ -n "$first_entry" ] || return 0' "$provisioner" >/dev/null ||
  fail 'empty PostgreSQL validation does not return success explicitly'

if [ "$(uname -s)" != Linux ] ||
  ! command -v sudo >/dev/null 2>&1 ||
  ! sudo -n true >/dev/null 2>&1
then
  echo 'production-bootstrap-safety-tests: integration skipped (Linux passwordless sudo required)'
  exit 0
fi

for command_name in docker python3 setpriv sha256sum stat find; do
  command -v "$command_name" >/dev/null 2>&1 ||
    fail "integration command is unavailable: $command_name"
done
sudo docker version >/dev/null 2>&1 ||
  fail 'Docker is required for the production installer fixture'
sudo docker compose version >/dev/null 2>&1 ||
  fail 'Docker Compose is required for the production installer fixture'

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-bootstrap-safety.XXXXXX")
fresh_fixture_created=false
fresh_env_created=false
v5_fixture_created=false
v5_etc_created=false
cleanup() {
  if [ "$fresh_fixture_created" = true ]; then
    sudo rm -rf -- /opt/phoenix
  fi
  if [ "$fresh_env_created" = true ]; then
    sudo rm -f -- /etc/phoenix/phoenix.env
  fi
  if [ "$v5_fixture_created" = true ]; then
    sudo rm -rf -- /opt/phoenix-v5
    sudo rm -f -- /etc/phoenix/phoenix-v5.env
  fi
  if [ "$v5_etc_created" = true ]; then
    sudo rmdir /etc/phoenix 2>/dev/null || true
  fi
  sudo rm -rf -- "$tmp_dir"
}
trap cleanup EXIT HUP INT TERM

owner_user=$(id -un)
owner_group=$(id -gn)
host_root=$tmp_dir/host
postgres_dir=$host_root/data/postgres
nats_fixture=$host_root/data/nats-jetstream-volume
prometheus_dir=$host_root/data/prometheus
prometheus_payload=$prometheus_dir/chunks/fixture.block
mkdir -p \
  "$postgres_dir/base/16384" \
  "$postgres_dir/global" \
  "$postgres_dir/pg_wal" \
  "$nats_fixture/messages" \
  "$prometheus_dir/chunks"
printf '16\n' >"$postgres_dir/PG_VERSION"
printf 'control\n' >"$postgres_dir/global/pg_control"
printf 'filenode\n' >"$postgres_dir/global/pg_filenode.map"
printf 'pid\n' >"$postgres_dir/postmaster.pid"
printf 'fsm\n' >"$postgres_dir/base/16384/fixture_fsm"
printf 'jetstream\n' >"$nats_fixture/messages/fixture.blk"
printf 'phoenix_nats_jetstream|local|local|fixture-labels|fixture-options\n' \
  >"$nats_fixture/volume.metadata"
printf 'preserved-prometheus-block\n' >"$prometheus_payload"
chmod 0755 "$tmp_dir" "$host_root" "$host_root/data"
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
chmod 0750 "$prometheus_dir" "$prometheus_dir/chunks"
chmod 0640 \
  "$nats_fixture/messages/fixture.blk" \
  "$nats_fixture/volume.metadata" \
  "$prometheus_payload"
prometheus_payload_sha=$(sha256sum "$prometheus_payload")

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
RECORDER_PERSISTENCE_POLICY=money_path_v1
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

snapshot_tree() {
  snapshot_root=$1
  snapshot_output=$2
  sudo /bin/sh -c '
    set -eu
    cd "$1"
    {
      find . -xdev -printf "%P|%u|%g|%m|%y|%s\n" | LC_ALL=C sort
      find . -xdev -type f -exec sha256sum {} \; | LC_ALL=C sort
    } >"$2"
  ' sh "$snapshot_root" "$snapshot_output" ||
    fail "tree snapshot failed: $snapshot_root"
}

prometheus_runtime_write_probe() {
  probe_dir=$1
  sudo /bin/sh -c '
    set -eu
    cd "$1"
    exec setpriv --reuid=65534 --regid=65534 --clear-groups \
      /bin/sh -c ": >.runtime-write-probe && rm -f -- .runtime-write-probe"
  ' sh "$probe_dir"
}

prometheus_runtime_read_probe() {
  probe_file=$1
  probe_parent=$(dirname -- "$probe_file")
  probe_name=$(basename -- "$probe_file")
  sudo /bin/sh -c '
    set -eu
    cd "$1"
    exec setpriv --reuid=65534 --regid=65534 --clear-groups \
      /bin/sh -c "test -r \"\$1\"" sh "$2"
  ' sh "$probe_parent" "$probe_name"
}

assert_live_canary_context() {
  context_source=$1
  context_target=$2
  for relative in \
    compose.live-canary.yml \
    live-executor/schema/001_live_canary.sql \
    live-executor/schema/002_approval_evidence.sql
  do
    [ -f "$context_target/$relative" ] && [ ! -L "$context_target/$relative" ] ||
      fail "installed live-canary asset is missing or unsafe: $relative"
    sudo cmp "$context_source/$relative" "$context_target/$relative" >/dev/null ||
      fail "installed live-canary asset bytes differ: $relative"
    [ "$(sudo stat -c '%a' "$context_target/$relative")" = 640 ] ||
      fail "installed live-canary asset mode differs: $relative"
  done
}

before=$tmp_dir/protected.before
after=$tmp_dir/protected.after
snapshot_metadata "$before"

if prometheus_runtime_write_probe "$prometheus_dir" 2>/dev/null; then
  fail 'Prometheus fixture unexpectedly started writable by the runtime identity'
fi
sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'first-host provisioning changed existing protected metadata or contents'
[ "$(sudo stat -c '%u:%g:%a' "$prometheus_dir")" = 65534:65534:750 ] ||
  fail 'Prometheus data directory runtime ownership or mode is incorrect'
[ "$(sudo stat -c '%u:%g:%a' "$prometheus_dir/chunks")" = 65534:65534:750 ] ||
  fail 'nested Prometheus directory runtime ownership or mode is incorrect'
[ "$(sudo stat -c '%u:%g:%a' "$prometheus_payload")" = 65534:65534:640 ] ||
  fail 'Prometheus data file runtime ownership or mode is incorrect'
[ "$prometheus_payload_sha" = "$(sudo sha256sum "$prometheus_payload")" ] ||
  fail 'Prometheus provisioning changed existing data contents'
prometheus_runtime_write_probe "$prometheus_dir" ||
  fail 'Prometheus data directory is not writable by the runtime identity'

prometheus_config=$host_root/deploy/prometheus/prometheus.yml
mkdir -p "$host_root/deploy/prometheus"
printf 'stale-unreadable-config\n' >"$prometheus_config"
chmod 0755 "$host_root/deploy" "$host_root/deploy/prometheus"
chmod 0640 "$prometheus_config"
[ "$(stat -c '%a' "$prometheus_config")" = 640 ] ||
  fail 'Prometheus configuration regression fixture is not mode 0640'
if prometheus_runtime_read_probe "$prometheus_config"; then
  fail 'mode 0640 Prometheus configuration fixture is unexpectedly readable'
fi
sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" "" "$repo_root" >/dev/null
assert_live_canary_context "$repo_root" "$host_root/deploy"
[ "$(sudo stat -c '%U:%G:%a' "$host_root/deploy")" = "root:$owner_group:750" ] ||
  fail 'canonical deploy directory is writable by the runtime owner'
[ "$(sudo stat -c '%U:%G:%a' "$host_root/deploy/compose.prod.yml")" = "root:$owner_group:640" ] ||
  fail 'canonical root control file ownership or mode is invalid'
[ "$(sudo stat -c '%U:%G:%a' "$host_root/deploy/.runtime")" = "$owner_user:$owner_group:750" ] ||
  fail 'operator runtime directory lost its bounded write contract'
[ "$(sudo stat -c '%U:%G:%a' "$host_root/deploy/.deploy-runtime")" = root:root:700 ] ||
  fail 'root deployment runtime directory is not isolated'
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'release-context installation changed protected metadata or contents'
[ "$(stat -c '%a' "$prometheus_config")" = 644 ] ||
  fail 'installed Prometheus configuration is not mode 0644'
sudo chmod 0755 "$host_root/deploy" "$host_root/deploy/prometheus"
prometheus_runtime_read_probe "$prometheus_config" ||
  fail 'mode 0644 Prometheus configuration is unreadable by the runtime identity'
sudo chmod 0750 "$host_root/deploy" "$host_root/deploy/prometheus"
[ "$prometheus_payload_sha" = "$(sudo sha256sum "$prometheus_payload")" ] ||
  fail 'release-context installation changed Prometheus data contents'

compose_target=$host_root/deploy/compose.prod.yml
compose_backup=$tmp_dir/compose.prod.yml.backup
sudo mv "$compose_target" "$compose_backup"
sudo ln -s "$postgres_dir/global/pg_control" "$compose_target"
if sudo env \
  PHOENIX_DEPLOY_ROOT="$host_root" \
  PHOENIX_ENV_FILE="$valid_env" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$context_installer" "" "$repo_root" >/dev/null 2>&1
then
  fail 'release-context installation followed a protected-data symlink'
fi
sudo rm -f -- "$compose_target"
sudo mv "$compose_backup" "$compose_target"
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
[ "$(sudo stat -c '%u:%g:%a' "$prometheus_payload")" = 65534:65534:640 ] ||
  fail 'idempotent bootstrap changed the Prometheus runtime contract'
[ "$prometheus_payload_sha" = "$(sudo sha256sum "$prometheus_payload")" ] ||
  fail 'idempotent bootstrap changed Prometheus data contents'

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
assert_live_canary_context \
  "$host_root/releases/$rollback_sha" "$host_root/deploy"
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
assert_live_canary_context \
  "$host_root/releases/$release_sha" "$host_root/deploy"
snapshot_metadata "$after"
cmp "$before" "$after" >/dev/null ||
  fail 'maintenance-success install changed protected metadata or contents'

immutable_tree=$host_root/releases/$release_sha
immutable_before=$tmp_dir/immutable.before
immutable_after=$tmp_dir/immutable.after
snapshot_tree "$immutable_tree" "$immutable_before"
sudo env PYTHONDONTWRITEBYTECODE=1 \
  /usr/bin/python3 -I -B "$immutable_tree/scripts/release_assets.py" verify-tree \
    --root "$immutable_tree" \
    --manifest "$asset_dir/release-assets-manifest.json" \
    --expected-sha "$release_sha" >/dev/null ||
  fail 'first explicit immutable-tree verification failed'
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
    "$asset_dir/release-assets-checksums.txt" >/dev/null ||
  fail 'idempotent immutable release installation failed'
sudo env PYTHONDONTWRITEBYTECODE=1 \
  /usr/bin/python3 -I -B "$immutable_tree/scripts/release_assets.py" verify-tree \
    --root "$immutable_tree" \
    --manifest "$asset_dir/release-assets-manifest.json" \
    --expected-sha "$release_sha" >/dev/null ||
  fail 'second explicit immutable-tree verification failed'
snapshot_tree "$immutable_tree" "$immutable_after"
cmp "$immutable_before" "$immutable_after" >/dev/null ||
  fail 'repeated install or verification changed immutable tree metadata or contents'
if sudo find "$immutable_tree" -xdev \
  \( -type d -name __pycache__ -o -type f \( -name '*.pyc' -o -name '*.pyo' \) \) \
  -print -quit | grep . >/dev/null
then
  fail 'repeated install or verification generated Python bytecode'
fi

[ ! -e /opt/phoenix ] ||
  fail 'fresh-host fixture requires /opt/phoenix to be absent'
[ ! -e /etc/phoenix/phoenix.env ] ||
  fail 'fresh-host fixture requires /etc/phoenix/phoenix.env to be absent'
if [ ! -d /etc/phoenix ]; then
  sudo install -d -m 0755 -o root -g root /etc/phoenix
  v5_etc_created=true
fi
sudo install -d -m 0750 -o root -g root /opt/phoenix
fresh_fixture_created=true
sudo install -m 0600 -o root -g root "$valid_env" /etc/phoenix/phoenix.env
fresh_env_created=true
fresh_first_output=$(sudo env \
  PHOENIX_DEPLOY_ROOT=/opt/phoenix \
  PHOENIX_ENV_FILE=/etc/phoenix/phoenix.env \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$bootstrap") ||
  fail 'fresh pre-created /opt/phoenix provisioning failed'
printf '%s\n' "$fresh_first_output" | grep -F 'PRODUCTION_PROVISION_OK:' >/dev/null ||
  fail 'fresh provisioning did not emit its stage completion diagnostic'
printf '%s\n' "$fresh_first_output" | grep -F 'BOOTSTRAP_OK:' >/dev/null ||
  fail 'fresh bootstrap did not emit its stage completion diagnostic'
[ "$(sudo stat -c '%U:%G:%a' /opt/phoenix)" = "$owner_user:$owner_group:750" ] ||
  fail 'fresh pre-created deployment root ownership or mode was not repaired'
fresh_before=$tmp_dir/fresh.before
fresh_after=$tmp_dir/fresh.after
snapshot_tree /opt/phoenix "$fresh_before"
sudo env \
  PHOENIX_DEPLOY_ROOT=/opt/phoenix \
  PHOENIX_ENV_FILE=/etc/phoenix/phoenix.env \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$bootstrap" >/dev/null ||
  fail 'second fresh-host provisioning invocation failed'
snapshot_tree /opt/phoenix "$fresh_after"
cmp "$fresh_before" "$fresh_after" >/dev/null ||
  fail 'second fresh-host provisioning changed metadata or contents'
sudo rm -rf -- /opt/phoenix
fresh_fixture_created=false
sudo rm -f -- /etc/phoenix/phoenix.env
fresh_env_created=false

[ ! -e /opt/phoenix-v5 ] ||
  fail 'exact v5 override fixture requires /opt/phoenix-v5 to be absent'
[ ! -e /opt/phoenix ] ||
  fail 'exact v5 override fixture requires the default deployment root to be absent'
[ ! -e /etc/phoenix/phoenix-v5.env ] ||
  fail 'exact v5 override fixture requires the v5 environment file to be absent'
[ ! -e /etc/phoenix/phoenix.env ] ||
  fail 'exact v5 override fixture requires the default environment file to be absent'
if [ ! -d /etc/phoenix ]; then
  sudo install -d -m 0755 -o root -g root /etc/phoenix
  v5_etc_created=true
fi
v5_fixture_created=true
sudo install -d -m 0750 -o root -g root /opt/phoenix-v5
sudo install -m 0600 -o root -g root "$valid_env" /etc/phoenix/phoenix-v5.env
sudo env \
  PHOENIX_DEPLOY_ROOT=/opt/phoenix-v5 \
  PHOENIX_RELEASE_ROOT=/opt/phoenix-v5/releases \
  PHOENIX_ENV_FILE=/etc/phoenix/phoenix-v5.env \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  PHOENIX_CONTEXT_INSTALLER="$context_installer" \
  /bin/sh "$release_installer" \
    "$release_sha" \
    "$asset_dir/phoenix-release-assets-$release_sha.tar.gz" \
    "$asset_dir/release-assets-manifest.json" \
    "$asset_dir/release-assets-checksums.txt" >/dev/null
[ -d "/opt/phoenix-v5/releases/$release_sha" ] ||
  fail 'explicit v5 release root was not used'
assert_live_canary_context \
  "/opt/phoenix-v5/releases/$release_sha" /opt/phoenix-v5/deploy
[ ! -e /opt/phoenix ] ||
  fail 'explicit v5 overrides silently created the default deployment root'
[ ! -e /etc/phoenix/phoenix.env ] ||
  fail 'explicit v5 override silently created the default environment file'

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

outside_prometheus=$tmp_dir/outside-prometheus
mkdir -p "$outside_prometheus"
printf 'outside-marker\n' >"$outside_prometheus/marker"
outside_prometheus_state=$(stat -c '%u:%g:%a' "$outside_prometheus/marker")
outside_prometheus_sha=$(sha256sum "$outside_prometheus/marker")

unsafe_prometheus_root=$tmp_dir/unsafe-prometheus-host
mkdir -p "$unsafe_prometheus_root/data"
ln -s "$outside_prometheus" "$unsafe_prometheus_root/data/prometheus"
if sudo env \
  PHOENIX_DEPLOY_ROOT="$unsafe_prometheus_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null 2>&1
then
  fail 'Prometheus data symlink was accepted'
fi
if [ "$outside_prometheus_state" != "$(stat -c '%u:%g:%a' "$outside_prometheus/marker")" ] ||
  [ "$outside_prometheus_sha" != "$(sha256sum "$outside_prometheus/marker")" ]
then
  fail 'Prometheus data symlink changed an outside path'
fi

nested_prometheus_root=$tmp_dir/nested-prometheus-host
mkdir -p "$nested_prometheus_root/data/prometheus"
ln -s "$outside_prometheus/marker" \
  "$nested_prometheus_root/data/prometheus/linked-marker"
nested_prometheus_state=$(
  stat -c '%u:%g:%a' "$nested_prometheus_root/data/prometheus"
)
if sudo env \
  PHOENIX_DEPLOY_ROOT="$nested_prometheus_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null 2>&1
then
  fail 'nested Prometheus data symlink was accepted'
fi
[ "$nested_prometheus_state" = "$(
  stat -c '%u:%g:%a' "$nested_prometheus_root/data/prometheus"
)" ] ||
  fail 'nested Prometheus symlink failure partially changed the data directory'
if [ "$outside_prometheus_state" != "$(stat -c '%u:%g:%a' "$outside_prometheus/marker")" ] ||
  [ "$outside_prometheus_sha" != "$(sha256sum "$outside_prometheus/marker")" ]
then
  fail 'nested Prometheus data symlink changed an outside path'
fi

linked_prometheus_root=$tmp_dir/linked-prometheus-host
mkdir -p "$linked_prometheus_root/data/prometheus"
ln "$outside_prometheus/marker" \
  "$linked_prometheus_root/data/prometheus/hard-linked-marker"
if sudo env \
  PHOENIX_DEPLOY_ROOT="$linked_prometheus_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null 2>&1
then
  fail 'hard-linked Prometheus data file was accepted'
fi
if [ "$outside_prometheus_state" != "$(stat -c '%u:%g:%a' "$outside_prometheus/marker")" ] ||
  [ "$outside_prometheus_sha" != "$(sha256sum "$outside_prometheus/marker")" ]
then
  fail 'Prometheus hard-link failure changed an outside path'
fi

file_prometheus_root=$tmp_dir/file-prometheus-host
mkdir -p "$file_prometheus_root/data"
printf 'not-a-directory\n' >"$file_prometheus_root/data/prometheus"
if sudo env \
  PHOENIX_DEPLOY_ROOT="$file_prometheus_root" \
  PHOENIX_OWNER_USER="$owner_user" \
  PHOENIX_OWNER_GROUP="$owner_group" \
  /bin/sh "$provisioner" >/dev/null 2>&1
then
  fail 'non-directory Prometheus data path was accepted'
fi

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
