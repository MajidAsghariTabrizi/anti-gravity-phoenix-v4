#!/usr/bin/env sh
set -eu
umask 077

release_sha=${1:-}
rollback_sha=${2:-}
release_manifest=${3:-}
rollback_manifest=${4:-}
release_archive=${5:-}
release_assets_manifest=${6:-}
release_checksums=${7:-}
plan_file=${8:-}

deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
release_root=$deploy_root/releases
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
compose_file=$deploy_dir/compose.prod.yml
runtime_root=$deploy_dir/.runtime
evidence_root=$deploy_root/evidence/protected-maintenance
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
helper=$script_dir/prelive_protected_maintenance.py
installer=$script_dir/install-release-assets.sh
context_installer=$script_dir/install-production-release-context.sh

protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_services='migration-runner prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'
service_wait_seconds=180
drain_wait_seconds=180
progress_wait_seconds=120

state_dir=
evidence_dir=
mutation_started=0
finalized=0

fail() {
  echo "PROTECTED_MAINTENANCE_FAILED: $1" >&2
  exit 1
}

case "$release_sha:$rollback_sha" in
  *[!0-9a-f:]*|:*|*:|*::*)
    fail 'release and rollback SHAs must be 40 lowercase hexadecimal characters'
    ;;
esac
[ "${#release_sha}" -eq 40 ] && [ "${#rollback_sha}" -eq 40 ] ||
  fail 'release and rollback SHAs must be 40 lowercase hexadecimal characters'
[ "$release_sha" != "$rollback_sha" ] || fail 'release and rollback SHAs must differ'
[ "$#" -eq 8 ] || fail 'exactly eight maintenance arguments are required'
[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
[ "$(uname -s)" = Linux ] || fail 'Linux is required'

for maintenance_file in \
  "$release_manifest" \
  "$rollback_manifest" \
  "$release_archive" \
  "$release_assets_manifest" \
  "$release_checksums" \
  "$plan_file" \
  "$helper" \
  "$installer" \
  "$context_installer" \
  "$compose_file" \
  "$env_file"
do
  [ -s "$maintenance_file" ] || fail "required maintenance file is missing"
done
for maintenance_command in python3 docker cmp df awk grep install mktemp sha256sum stat; do
  command -v "$maintenance_command" >/dev/null 2>&1 ||
    fail "required command is unavailable: $maintenance_command"
done
docker compose version >/dev/null 2>&1 || fail 'Docker Compose is unavailable'

install -d -m 0750 -o phoenix -g phoenix "$runtime_root" "$evidence_root"
state_dir=$(mktemp -d "$runtime_root/protected-maintenance-$release_sha.XXXXXX") ||
  fail 'private maintenance state could not be created'
release_prefix=$(printf '%.8s' "$release_sha")
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$release_prefix"
evidence_dir=$evidence_root/$run_id
install -d -m 0750 -o root -g phoenix "$evidence_dir"

cleanup_state() {
  [ -z "$state_dir" ] || rm -rf -- "$state_dir"
}

compose_with() (
  compose_release_env=$1
  shift
  unset COMPOSE_FILE COMPOSE_PROFILES ENGINE_ROUTE_REGISTRY_JSON
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$compose_release_env" \
    docker compose \
      --project-directory "$deploy_dir" \
      --env-file "$env_file" \
      --env-file "$compose_release_env" \
      -f "$compose_file" "$@"
)

container_id() (
  container_release_env=$1
  container_service=$2
  container_ids=$(compose_with "$container_release_env" ps -a -q "$container_service") ||
    exit 1
  container_count=$(printf '%s\n' "$container_ids" | awk 'NF { count += 1 } END { print count + 0 }')
  [ "$container_count" -eq 1 ] || exit 1
  printf '%s\n' "$container_ids" | awk 'NF { print; exit }'
)

service_healthy() (
  healthy_release_env=$1
  healthy_service=$2
  healthy_id=$(container_id "$healthy_release_env" "$healthy_service") || exit 1
  healthy_state=$(docker inspect --format \
    '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.State.OOMKilled}}' \
    "$healthy_id" 2>/dev/null) || exit 1
  [ "$healthy_state" = 'running|healthy|false' ]
)

wait_service_healthy() (
  wait_release_env=$1
  wait_service=$2
  wait_deadline=$(( $(date +%s) + service_wait_seconds ))
  while [ "$(date +%s)" -lt "$wait_deadline" ]; do
    service_healthy "$wait_release_env" "$wait_service" && exit 0
    sleep 3
  done
  exit 1
)

assert_optional_stopped() (
  optional_release_env=$1
  for optional_service in $optional_services; do
    optional_running=$(compose_with "$optional_release_env" ps -q "$optional_service") ||
      exit 1
    [ -z "$optional_running" ] || exit 1
  done
)

assert_protected_healthy() (
  protected_release_env=$1
  for protected_service in $protected_services; do
    service_healthy "$protected_release_env" "$protected_service" || exit 1
  done
)

capture_service_inspect() (
  inspect_release_env=$1
  inspect_service=$2
  inspect_output=$3
  inspect_id=$(container_id "$inspect_release_env" "$inspect_service") || exit 1
  docker inspect --format \
    '{"container_id":{{json .Id}},"configured_image":{{json .Config.Image}},"local_image_id":{{json .Image}},"created_at":{{json .Created}},"started_at":{{json .State.StartedAt}},"restart_count":{{.RestartCount}},"oom_killed":{{.State.OOMKilled}},"status":{{json .State.Status}},"health":{{if .State.Health}}{{json .State.Health.Status}}{{else}}"none"{{end}},"mounts":{{json .Mounts}},"networks":{{json .NetworkSettings.Networks}}}' \
    "$inspect_id" >"$inspect_output"
  [ -s "$inspect_output" ]
)

capture_jetstream() (
  jetstream_release_env=$1
  jetstream_output=$2
  compose_with "$jetstream_release_env" exec -T nats wget -q -O - \
    'http://127.0.0.1:8222/jsz?streams=true&consumers=true&config=true' \
    >"$jetstream_output"
  [ -s "$jetstream_output" ]
)

capture_database() (
  database_release_env=$1
  database_output=$2
  compose_with "$database_release_env" exec -T postgres \
    psql -X -qAt -v ON_ERROR_STOP=1 \
      -U "$POSTGRES_USER" -d "$POSTGRES_DB" >"$database_output" <<'SQL'
SELECT json_build_object(
  'migrations',
  COALESCE(
    (
      SELECT json_agg(
        json_build_object('version', version, 'checksum', checksum)
        ORDER BY version
      )
      FROM schema_migrations
    ),
    '[]'::json
  ),
  'counts',
  json_build_object(
    'execution_attempts', (SELECT count(*) FROM execution_attempts),
    'executions', (SELECT count(*) FROM executions),
    'realized_pnl', (SELECT count(*) FROM realized_pnl),
    'execution_eligible', (
      SELECT count(*) FROM shadow_decisions WHERE execution_eligible
    ),
    'execution_requests', (
      SELECT count(*) FROM shadow_profitability_facts
      WHERE execution_request_created
    ),
    'fork_execution_eligible', (
      SELECT count(*) FROM fork_simulation_results WHERE execution_eligible
    ),
    'fork_execution_requests', (
      SELECT count(*) FROM fork_simulation_results
      WHERE execution_request_created
    ),
    'origin_transactions', (SELECT count(*) FROM origin_transactions),
    'feed_events', (SELECT count(*) FROM feed_events),
    'duplicate_origins', (
      SELECT count(*) FROM (
        SELECT tx_hash FROM origin_transactions
        GROUP BY tx_hash HAVING count(*) > 1
      ) AS duplicate_origins
    ),
    'duplicate_feed_events', (
      SELECT count(*) FROM (
        SELECT sequence_number, tx_hash FROM feed_events
        GROUP BY sequence_number, tx_hash HAVING count(*) > 1
      ) AS duplicate_feed_events
    )
  ),
  'max_feed_sequence',
  COALESCE((SELECT max(sequence_number)::text FROM feed_events), '0')
)::text;
SQL
  [ -s "$database_output" ]
)

capture_metrics() (
  metrics_release_env=$1
  metrics_service=$2
  metrics_port=$3
  metrics_output=$4
  metrics_id=$(container_id "$metrics_release_env" "$metrics_service") || exit 1
  metrics_status=$(docker inspect --format '{{.State.Status}}' "$metrics_id") || exit 1
  if [ "$metrics_status" != running ]; then
    : >"$metrics_output"
    exit 0
  fi
  compose_with "$metrics_release_env" exec -T "$metrics_service" wget -q -O - \
    "http://127.0.0.1:$metrics_port/metrics" >"$metrics_output"
  [ -s "$metrics_output" ]
)

capture_protected_storage() (
  storage_release_env=$1
  storage_output=$2

  postgres_id=$(container_id "$storage_release_env" postgres) || exit 1
  postgres_mount=$(docker inspect --format \
    '{{range .Mounts}}{{if eq .Destination "/var/lib/postgresql/data"}}{{.Type}}|{{.Source}}|{{.RW}}{{"\n"}}{{end}}{{end}}' \
    "$postgres_id") || exit 1
  [ "$(printf '%s\n' "$postgres_mount" | awk 'NF { count += 1 } END { print count + 0 }')" -eq 1 ] ||
    exit 1
  old_ifs=$IFS
  IFS='|' read -r postgres_mount_type postgres_source postgres_rw <<EOF
$postgres_mount
EOF
  IFS=$old_ifs
  [ "$postgres_mount_type" = bind ] || exit 1
  [ "$postgres_source" = "$deploy_root/data/postgres" ] || exit 1
  [ "$postgres_rw" = true ] || exit 1
  [ -d "$postgres_source" ] && [ ! -L "$postgres_source" ] || exit 1

  nats_id=$(container_id "$storage_release_env" nats) || exit 1
  nats_mount=$(docker inspect --format \
    '{{range .Mounts}}{{if eq .Destination "/data/jetstream"}}{{.Type}}|{{.Name}}|{{.RW}}{{"\n"}}{{end}}{{end}}' \
    "$nats_id") || exit 1
  [ "$(printf '%s\n' "$nats_mount" | awk 'NF { count += 1 } END { print count + 0 }')" -eq 1 ] ||
    exit 1
  IFS='|' read -r nats_mount_type nats_volume nats_rw <<EOF
$nats_mount
EOF
  IFS=$old_ifs
  [ "$nats_mount_type" = volume ] || exit 1
  [ -n "$nats_volume" ] || exit 1
  [ "$nats_rw" = true ] || exit 1
  nats_volume_metadata=$(docker volume inspect --format \
    '{{.Name}}|{{.Driver}}|{{.Scope}}|{{.Mountpoint}}|{{json .Labels}}|{{json .Options}}' \
    "$nats_volume") || exit 1
  nats_mountpoint=$(docker volume inspect --format '{{.Mountpoint}}' "$nats_volume") ||
    exit 1
  [ -d "$nats_mountpoint" ] && [ ! -L "$nats_mountpoint" ] || exit 1

  {
    printf 'postgres-mount|%s|%s|%s\n' \
      "$postgres_mount_type" "$postgres_source" "$postgres_rw"
    for storage_path in \
      . \
      PG_VERSION \
      base \
      global \
      pg_wal \
      global/pg_control \
      global/pg_filenode.map \
      postmaster.pid
    do
      storage_target=$postgres_source
      [ "$storage_path" = . ] || storage_target=$postgres_source/$storage_path
      [ -e "$storage_target" ] && [ ! -L "$storage_target" ] || exit 1
      printf 'postgres-path|%s|%s\n' \
        "$storage_path" "$(stat -c '%u|%g|%f' "$storage_target")"
    done
    printf 'nats-volume|%s\n' "$nats_volume_metadata"
    printf 'nats-mountpoint|%s\n' \
      "$(stat -c '%u|%g|%f' "$nats_mountpoint")"
  } >"$storage_output"
  [ -s "$storage_output" ]
)

capture_snapshot() (
  snapshot_phase=$1
  snapshot_sha=$2
  snapshot_release_env=$3
  snapshot_output=$4
  snapshot_dir=$(mktemp -d "$state_dir/snapshot-$snapshot_phase.XXXXXX") ||
    exit 1
  trap 'rm -rf -- "$snapshot_dir"' EXIT HUP INT TERM

  set -- python3 "$helper" snapshot \
    --phase "$snapshot_phase" \
    --release-sha "$snapshot_sha"
  for snapshot_service in $protected_services; do
    snapshot_service_file=$snapshot_dir/$snapshot_service.json
    capture_service_inspect \
      "$snapshot_release_env" "$snapshot_service" "$snapshot_service_file" ||
      exit 1
    set -- "$@" --service "$snapshot_service=$snapshot_service_file"
  done

  capture_jetstream "$snapshot_release_env" "$snapshot_dir/jetstream.json" ||
    exit 1
  capture_database "$snapshot_release_env" "$snapshot_dir/database.json" ||
    exit 1
  capture_metrics \
    "$snapshot_release_env" feed-ingestor 9100 "$snapshot_dir/feed.metrics" ||
    exit 1
  capture_metrics \
    "$snapshot_release_env" recorder 9400 "$snapshot_dir/recorder.metrics" ||
    exit 1
  capture_protected_storage \
    "$snapshot_release_env" "$snapshot_dir/storage.metadata" || exit 1
  snapshot_disk_free=$(df -Pk "$deploy_root" | awk 'NR == 2 { printf "%.0f\n", $4 * 1024 }')
  case "$snapshot_disk_free" in
    ''|*[!0-9]*) exit 1 ;;
  esac

  "$@" \
    --jetstream "$snapshot_dir/jetstream.json" \
    --database "$snapshot_dir/database.json" \
    --feed-metrics "$snapshot_dir/feed.metrics" \
    --recorder-metrics "$snapshot_dir/recorder.metrics" \
    --safety "$state_dir/safety.json" \
    --storage-metadata "$snapshot_dir/storage.metadata" \
    --disk-free-bytes "$snapshot_disk_free" \
    --output "$snapshot_output" >/dev/null
)

wait_recorder_drain() (
  drain_release_env=$1
  drain_deadline=$(( $(date +%s) + drain_wait_seconds ))
  drain_jetstream=$state_dir/drain-jetstream.json
  while [ "$(date +%s)" -lt "$drain_deadline" ]; do
    capture_jetstream "$drain_release_env" "$drain_jetstream" || exit 1
    drain_state=$(python3 "$helper" consumer-state --jetstream "$drain_jetstream") ||
      exit 1
    drain_pending=$(printf '%s' "$drain_state" | awk '{ print $1 }')
    drain_ack_pending=$(printf '%s' "$drain_state" | awk '{ print $2 }')
    case "$drain_pending:$drain_ack_pending" in
      *[!0-9:]*|:*|*:|*::*) exit 1 ;;
    esac
    if [ "$drain_pending" -eq 0 ] && [ "$drain_ack_pending" -eq 0 ]; then
      exit 0
    fi
    sleep 3
  done
  exit 1
)

wait_for_progress() (
  progress_stage=$1
  progress_sha=$2
  progress_release_env=$3
  progress_baseline=$4
  progress_output=$5
  progress_phase=$progress_stage
  [ "$progress_stage" != rollback ] || progress_phase=rollback-final
  progress_deadline=$(( $(date +%s) + progress_wait_seconds ))
  progress_candidate=$state_dir/progress-$progress_stage.json
  while [ "$(date +%s)" -lt "$progress_deadline" ]; do
    if capture_snapshot \
      "$progress_phase" "$progress_sha" "$progress_release_env" "$progress_candidate" &&
      python3 "$helper" validate-transition \
        --plan "$plan_file" \
        --baseline "$state_dir/pre.json" \
        --current "$progress_candidate" \
        --stage "$progress_stage" \
        --progress-baseline "$progress_baseline" >/dev/null 2>&1
    then
      cp "$progress_candidate" "$progress_output"
      exit 0
    fi
    sleep 3
  done
  exit 1
)

install_active_file() (
  active_source=$1
  active_target=$2
  active_mode=$3
  active_tmp=$(mktemp "$runtime_root/.protected-maintenance-active.XXXXXX") ||
    exit 1
  trap 'rm -f -- "$active_tmp"' EXIT HUP INT TERM
  cp "$active_source" "$active_tmp" &&
    chown phoenix:phoenix "$active_tmp" &&
    chmod "$active_mode" "$active_tmp" &&
    mv "$active_tmp" "$active_target"
)

write_active_value() (
  active_value=$1
  active_target=$2
  active_tmp=$(mktemp "$runtime_root/.protected-maintenance-value.XXXXXX") ||
    exit 1
  trap 'rm -f -- "$active_tmp"' EXIT HUP INT TERM
  printf '%s\n' "$active_value" >"$active_tmp" &&
    chown phoenix:phoenix "$active_tmp" &&
    chmod 0640 "$active_tmp" &&
    mv "$active_tmp" "$active_target"
)

restore_release_context() {
  PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_ENV_FILE="$env_file" \
    /bin/sh "$context_installer" "$rollback_sha" "$release_root/$rollback_sha" ||
    return 1
  install_active_file \
    "$state_dir/backup/current-release.env" "$deploy_dir/current-release.env" 0640 ||
    return 1
  install_active_file \
    "$state_dir/backup/current-release.json" "$deploy_dir/current-release.json" 0640 ||
    return 1
  install_active_file \
    "$state_dir/backup/current-release-context.json" \
    "$deploy_dir/current-release-context.json" 0640 || return 1
  install_active_file \
    "$state_dir/backup/release-assets.sha" "$deploy_dir/release-assets.sha" 0640 ||
    return 1
  if [ -s "$state_dir/backup/previous-release" ]; then
    install_active_file \
      "$state_dir/backup/previous-release" "$deploy_dir/previous-release" 0640 ||
      return 1
  else
    rm -f -- "$deploy_dir/previous-release"
  fi
  install_active_file \
    "$state_dir/backup/current-release" "$deploy_dir/current-release" 0640
}

rollback_protected() {
  echo "PROTECTED_MAINTENANCE_ROLLBACK_STARTED: $rollback_sha" >&2
  compose_with "$release_env" stop feed-ingestor >/dev/null 2>&1 || true
  if ! wait_recorder_drain "$release_env"; then
    echo "PROTECTED_MAINTENANCE_ROLLBACK_NOTE: candidate Recorder did not drain before rollback" >&2
  fi
  if ! compose_with "$rollback_env" up -d --no-deps recorder >/dev/null; then
    restore_release_context >/dev/null 2>&1 || true
    return 1
  fi
  if ! wait_service_healthy "$rollback_env" recorder; then
    restore_release_context >/dev/null 2>&1 || true
    return 1
  fi
  if ! compose_with "$rollback_env" up -d --no-deps feed-ingestor >/dev/null; then
    restore_release_context >/dev/null 2>&1 || true
    return 1
  fi
  if ! wait_service_healthy "$rollback_env" feed-ingestor; then
    restore_release_context >/dev/null 2>&1 || true
    return 1
  fi
  if ! assert_optional_stopped "$rollback_env"; then
    restore_release_context >/dev/null 2>&1 || true
    return 1
  fi

  capture_snapshot \
    rollback-start "$rollback_sha" "$rollback_env" "$state_dir/rollback-start.json" ||
    {
      restore_release_context >/dev/null 2>&1 || true
      return 1
    }
  restore_release_context || return 1
  wait_for_progress \
    rollback "$rollback_sha" "$rollback_env" \
    "$state_dir/rollback-start.json" "$state_dir/rollback-final.json" ||
    return 1
  assert_optional_stopped "$rollback_env" || return 1
  assert_protected_healthy "$rollback_env" || return 1
  capture_snapshot \
    rollback-final "$rollback_sha" "$rollback_env" \
    "$state_dir/rollback-promoted.json" || return 1
  python3 "$helper" validate-transition \
    --plan "$plan_file" \
    --baseline "$state_dir/pre.json" \
    --current "$state_dir/rollback-promoted.json" \
    --stage rollback \
    --progress-baseline "$state_dir/rollback-start.json" >/dev/null || return 1
  install -m 0640 -o root -g phoenix \
    "$state_dir/rollback-promoted.json" "$evidence_dir/rollback-final.json"
  echo "PROTECTED_MAINTENANCE_ROLLBACK_OK: $rollback_sha" >&2
}

unexpected_exit() {
  unexpected_code=$?
  trap - EXIT HUP INT TERM
  if [ "$finalized" -ne 1 ] && [ "$mutation_started" -eq 1 ]; then
    if ! rollback_protected; then
      echo "PROTECTED_MAINTENANCE_ROLLBACK_FAILED: operator action required" >&2
    fi
  fi
  cleanup_state
  [ "$unexpected_code" -ne 0 ] || unexpected_code=1
  exit "$unexpected_code"
}

trap unexpected_exit EXIT
trap 'exit 1' HUP INT TERM

python3 "$helper" image-refs --plan "$plan_file" >"$state_dir/validated-plan.tsv" ||
  fail 'maintenance plan is invalid'

current_release=$deploy_dir/current-release
current_env=$deploy_dir/current-release.env
current_state=$deploy_dir/current-release.json
current_context=$deploy_dir/current-release-context.json
asset_marker=$deploy_dir/release-assets.sha
previous_release=$deploy_dir/previous-release
rollback_tree=$release_root/$rollback_sha
rollback_host_manifest=$deploy_dir/manifests/$rollback_sha.json

for current_file in \
  "$current_release" \
  "$current_env" \
  "$current_state" \
  "$current_context" \
  "$asset_marker" \
  "$rollback_host_manifest" \
  "$rollback_tree/release-assets-manifest.json" \
  "$rollback_tree/compose.prod.yml"
do
  [ -s "$current_file" ] || fail 'current canonical release state is incomplete'
done
[ "$(tr -d '\r\n' <"$current_release")" = "$rollback_sha" ] ||
  fail 'current release is not the reviewed rollback release'
[ "$(tr -d '\r\n' <"$asset_marker")" = "$rollback_sha" ] ||
  fail 'current release-assets marker is not the reviewed rollback release'
cmp "$rollback_manifest" "$rollback_host_manifest" >/dev/null ||
  fail 'host rollback manifest differs from reviewed rollback evidence'

python3 "$deploy_dir/release_assets.py" verify-tree \
  --root "$rollback_tree" \
  --manifest "$rollback_tree/release-assets-manifest.json" \
  --expected-sha "$rollback_sha" >/dev/null ||
  fail 'immutable rollback release tree failed verification'
(
  cd "$(dirname -- "$release_archive")"
  sha256sum -c "$(basename -- "$release_checksums")" >/dev/null
) || fail 'candidate release asset checksums failed verification'
python3 "$helper" plan \
  --release-manifest "$release_manifest" \
  --rollback-manifest "$rollback_manifest" \
  --release-assets-manifest "$release_assets_manifest" \
  --rollback-assets-manifest "$rollback_tree/release-assets-manifest.json" \
  --release-sha "$release_sha" \
  --rollback-sha "$rollback_sha" \
  --output "$state_dir/remote-plan.json" >/dev/null ||
  fail 'remote maintenance plan validation failed'
cmp "$state_dir/remote-plan.json" "$plan_file" >/dev/null ||
  fail 'remote maintenance plan differs from pre-SSH evidence'

release_env=$state_dir/release.env
rollback_env=$state_dir/rollback.env
python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$release_manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env" || fail 'candidate manifest validation failed'
python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$rollback_manifest" \
  --expected-sha "$rollback_sha" \
  --output "$rollback_env" || fail 'rollback manifest validation failed'
cmp "$rollback_env" "$current_env" >/dev/null ||
  fail 'current release environment differs from rollback manifest'

"$deploy_dir/validate-production-env.sh" "$env_file" >"$state_dir/env-validation.log" ||
  fail 'production environment validation failed'
set -a
# shellcheck disable=SC1090
. "$env_file"
set +a
[ "${PHOENIX_MODE:-}" = SHADOW ] || fail 'PHOENIX_MODE must remain SHADOW'
[ "${LIVE_EXECUTION:-}" = false ] || fail 'LIVE_EXECUTION must remain false'
[ -z "${SIGNER_PRIVATE_KEY:-}" ] || fail 'signer configuration must remain blank'
[ -z "${WALLET_ADDRESS:-}" ] || fail 'wallet configuration must remain blank'
[ -z "${EXECUTOR_ADDRESS:-}" ] || fail 'executor configuration must remain blank'
case "${PUBLIC_TRANSACTION_SUBMISSION:-}${PRIVATE_RELAY_SUBMISSION:-}${TRANSACTION_BROADCAST_URL:-}" in
  '') ;;
  *) fail 'submission and broadcast configuration must remain blank' ;;
esac
cat >"$state_dir/safety.json" <<'JSON'
{
  "mode": "SHADOW",
  "live_execution": false,
  "signer_configured": false,
  "wallet_configured": false,
  "executor_configured": false,
  "public_submission_configured": false,
  "private_submission_configured": false,
  "broadcast_configured": false,
  "execution_eligible": false,
  "execution_request_created": false,
  "optional_services_stopped": true
}
JSON

"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$release_manifest" \
  --output "$state_dir/release.compose.json" \
  --metadata-output "$state_dir/release.render.json" >/dev/null ||
  fail 'candidate production render failed'
"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$rollback_env" \
  --release-manifest "$rollback_manifest" \
  --output "$state_dir/rollback.compose.json" \
  --metadata-output "$state_dir/rollback.render.json" >/dev/null ||
  fail 'rollback production render failed'
python3 "$helper" validate-render-pair \
  --plan "$plan_file" \
  --release-metadata "$state_dir/release.render.json" \
  --rollback-metadata "$state_dir/rollback.render.json" >/dev/null ||
  fail 'release and rollback render contracts differ'

python3 "$deploy_dir/production_context.py" write-state \
  --manifest "$rollback_manifest" \
  --release-env "$rollback_env" \
  --render-metadata "$state_dir/rollback.render.json" \
  --compose-config "$state_dir/rollback.compose.json" \
  --output "$state_dir/rollback.expected.state.json" ||
  fail 'rollback release state could not be reconstructed'
cmp "$state_dir/rollback.expected.state.json" "$current_state" >/dev/null ||
  fail 'current release state differs from exact rollback evidence'

assert_optional_stopped "$rollback_env" ||
  fail 'optional services must already be stopped before maintenance'
assert_protected_healthy "$rollback_env" ||
  fail 'protected data plane is not healthy before maintenance'
compose_with "$rollback_env" exec -T postgres \
  pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null ||
  fail 'PostgreSQL readiness failed'
compose_with "$rollback_env" exec -T nats wget -q -O - \
  'http://127.0.0.1:8222/healthz?js-enabled-only=true' >/dev/null ||
  fail 'JetStream readiness failed'

python3 "$helper" image-refs --plan "$plan_file" >"$state_dir/image-refs.tsv" ||
  fail 'image plan could not be read'
while IFS='	' read -r image_role image_name image_reference image_sha; do
  case "$image_name" in
    feed-ingestor|recorder) ;;
    *) continue ;;
  esac
  docker pull "$image_reference" >/dev/null ||
    fail "protected image is not pullable: $image_role/$image_name"
  image_revision=$(docker image inspect --format \
    '{{index .Config.Labels "org.opencontainers.image.revision"}}' \
    "$image_reference") || fail 'protected image OCI labels are unavailable'
  [ "$image_revision" = "$image_sha" ] ||
    fail "protected image OCI revision mismatch: $image_role/$image_name"
done <"$state_dir/image-refs.tsv"

mkdir -p "$state_dir/backup"
for backup_name in \
  current-release \
  current-release.env \
  current-release.json \
  current-release-context.json \
  release-assets.sha
do
  cp "$deploy_dir/$backup_name" "$state_dir/backup/$backup_name" ||
    fail 'current release backup failed'
done
if [ -s "$previous_release" ]; then
  cp "$previous_release" "$state_dir/backup/previous-release" ||
    fail 'previous release backup failed'
fi

capture_snapshot pre "$rollback_sha" "$rollback_env" "$state_dir/pre.json" ||
  fail 'pre-maintenance evidence capture failed'
python3 "$helper" validate-baseline \
  --plan "$plan_file" --snapshot "$state_dir/pre.json" >/dev/null ||
  fail 'pre-maintenance protected baseline is invalid'
install -m 0640 -o root -g phoenix "$state_dir/pre.json" \
  "$evidence_dir/pre-maintenance.json"
install -m 0640 -o root -g phoenix "$plan_file" "$evidence_dir/plan.json"
sha256sum "$evidence_dir/pre-maintenance.json" "$evidence_dir/plan.json" \
  >"$evidence_dir/SHA256SUMS"
chown root:phoenix "$evidence_dir/SHA256SUMS"
chmod 0640 "$evidence_dir/SHA256SUMS"
echo "PROTECTED_MAINTENANCE_EVIDENCE: $evidence_dir"

mutation_started=1
compose_with "$rollback_env" stop feed-ingestor >/dev/null ||
  fail 'feed-ingestor could not be quiesced'
wait_recorder_drain "$rollback_env" ||
  fail 'Recorder durable consumer did not drain before replacement'

compose_with "$release_env" up -d --no-deps recorder >/dev/null ||
  fail 'candidate Recorder update failed'
wait_service_healthy "$release_env" recorder ||
  fail 'candidate Recorder did not become healthy'
capture_snapshot \
  recorder "$release_sha" "$release_env" "$state_dir/recorder-stage.json" ||
  fail 'Recorder-stage evidence capture failed'
python3 "$helper" validate-transition \
  --plan "$plan_file" \
  --baseline "$state_dir/pre.json" \
  --current "$state_dir/recorder-stage.json" \
  --stage recorder >/dev/null ||
  fail 'Recorder-stage continuity validation failed'

compose_with "$release_env" up -d --no-deps feed-ingestor >/dev/null ||
  fail 'candidate feed-ingestor update failed'
wait_service_healthy "$release_env" feed-ingestor ||
  fail 'candidate feed-ingestor did not become healthy'
assert_optional_stopped "$release_env" ||
  fail 'an optional service started during protected maintenance'
capture_snapshot \
  post-start "$release_sha" "$release_env" "$state_dir/post-start.json" ||
  fail 'post-start evidence capture failed'
wait_for_progress \
  final "$release_sha" "$release_env" \
  "$state_dir/post-start.json" "$state_dir/final.json" ||
  fail 'Feed and Recorder progress was not proven after maintenance'

compose_with "$release_env" logs --no-color --tail 1000 \
  nats recorder feed-ingestor >"$state_dir/protected.log" 2>&1 || true
if grep -Eiq \
  'slow consumer|core_nats_message_drop|Core NATS delivery loss|recorder_nats_slow_consumer' \
  "$state_dir/protected.log"
then
  fail 'a bounded protected-service loss indicator was observed'
fi
install -m 0640 -o root -g phoenix \
  "$state_dir/recorder-stage.json" "$evidence_dir/recorder-stage.json"
install -m 0640 -o root -g phoenix \
  "$state_dir/final.json" "$evidence_dir/post-maintenance.json"

install -d -m 0750 -o phoenix -g phoenix "$deploy_dir/manifests"
install -m 0640 -o phoenix -g phoenix \
  "$release_manifest" "$deploy_dir/manifests/$release_sha.json"
install -m 0640 -o phoenix -g phoenix \
  "$rollback_manifest" "$deploy_dir/manifests/$rollback_sha.json"
PHOENIX_RELEASE_ROOT="$release_root" \
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_CONTEXT_INSTALLER="$context_installer" \
  /bin/sh "$installer" \
  "$release_sha" "$release_archive" "$release_assets_manifest" "$release_checksums" ||
  fail 'candidate immutable release assets could not be installed'

python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$deploy_dir/manifests/$release_sha.json" \
  --expected-sha "$release_sha" \
  --output "$deploy_dir/manifests/$release_sha.env" ||
  fail 'promoted candidate release environment is invalid'
"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$deploy_dir/manifests/$release_sha.env" \
  --release-manifest "$deploy_dir/manifests/$release_sha.json" \
  --output "$state_dir/promoted.compose.json" \
  --metadata-output "$state_dir/promoted.render.json" >/dev/null ||
  fail 'promoted candidate render failed'
python3 "$deploy_dir/production_context.py" write-state \
  --manifest "$deploy_dir/manifests/$release_sha.json" \
  --release-env "$deploy_dir/manifests/$release_sha.env" \
  --render-metadata "$state_dir/promoted.render.json" \
  --compose-config "$state_dir/promoted.compose.json" \
  --output "$state_dir/promoted.state.json" ||
  fail 'promoted candidate release state failed'

assert_optional_stopped "$deploy_dir/manifests/$release_sha.env" ||
  fail 'optional services changed during release-context promotion'
capture_snapshot \
  promoted "$release_sha" "$deploy_dir/manifests/$release_sha.env" \
  "$state_dir/promoted.json" || fail 'promoted maintenance evidence capture failed'
python3 "$helper" validate-transition \
  --plan "$plan_file" \
  --baseline "$state_dir/pre.json" \
  --current "$state_dir/promoted.json" \
  --stage promoted \
  --progress-baseline "$state_dir/post-start.json" >/dev/null ||
  fail 'promoted protected release validation failed'
python3 "$helper" context \
  --plan "$plan_file" \
  --snapshot "$state_dir/promoted.json" \
  --render-metadata "$state_dir/promoted.render.json" \
  --output "$state_dir/promoted.context.json" >/dev/null ||
  fail 'protected maintenance release context failed'

install_active_file \
  "$state_dir/promoted.render.json" "$deploy_dir/manifests/$release_sha.render.json" 0640 ||
  fail 'release render metadata promotion failed'
install_active_file \
  "$state_dir/promoted.state.json" "$deploy_dir/manifests/$release_sha.state.json" 0640 ||
  fail 'release state promotion failed'
install_active_file \
  "$deploy_dir/manifests/$release_sha.env" "$current_env" 0640 ||
  fail 'current release environment promotion failed'
install_active_file \
  "$state_dir/promoted.state.json" "$current_state" 0640 ||
  fail 'current release state promotion failed'
install_active_file \
  "$state_dir/promoted.context.json" "$current_context" 0640 ||
  fail 'current protected maintenance context promotion failed'
write_active_value "$rollback_sha" "$previous_release" ||
  fail 'previous release pointer promotion failed'
write_active_value "$release_sha" "$current_release" ||
  fail 'current release pointer promotion failed'

[ "$(tr -d '\r\n' <"$current_release")" = "$release_sha" ] ||
  fail 'current release pointer verification failed'
[ "$(tr -d '\r\n' <"$asset_marker")" = "$release_sha" ] ||
  fail 'release-assets marker verification failed'
cmp "$current_env" "$deploy_dir/manifests/$release_sha.env" >/dev/null ||
  fail 'current release environment verification failed'
assert_optional_stopped "$current_env" ||
  fail 'optional services did not remain stopped'
assert_protected_healthy "$current_env" ||
  fail 'protected services are not healthy after release promotion'

install -m 0640 -o root -g phoenix \
  "$state_dir/promoted.json" "$evidence_dir/promoted.json"
install -m 0640 -o root -g phoenix \
  "$state_dir/promoted.context.json" "$evidence_dir/release-context.json"
sha256sum \
  "$evidence_dir/pre-maintenance.json" \
  "$evidence_dir/recorder-stage.json" \
  "$evidence_dir/post-maintenance.json" \
  "$evidence_dir/promoted.json" \
  "$evidence_dir/release-context.json" \
  "$evidence_dir/plan.json" >"$evidence_dir/SHA256SUMS"
chown root:phoenix "$evidence_dir/SHA256SUMS"
chmod 0640 "$evidence_dir/SHA256SUMS"

finalized=1
trap - EXIT HUP INT TERM
cleanup_state
echo "PROTECTED_MAINTENANCE_OK: release=$release_sha rollback=$rollback_sha evidence=$evidence_dir"
