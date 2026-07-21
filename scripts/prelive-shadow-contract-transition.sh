#!/usr/bin/env sh
set -eu
umask 077

mode=${1:-}
stage_dir=${2:-}
confirmation=${3:-}

release_sha=ac4868ae86227dc61ea003cf0e4e96032be9c96c
rollback_sha=654dad176fe705d90628b418750a122b8ae30283
required_confirmation=APPLY_EXACT_PHOENIX_SHADOW_CONTRACT_TRANSITION

deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
release_root=$deploy_root/releases
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
runtime_root=$deploy_dir/.runtime
evidence_root=$deploy_root/evidence/shadow-contract-transition

protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_stop_order='phoenix-engine shadow-dispatcher rpc-gateway dashboard prometheus'
runtime_services='nitro-feed-relay nats postgres recorder feed-ingestor rpc-gateway phoenix-engine shadow-dispatcher dashboard prometheus'
service_wait_seconds=180
drain_wait_seconds=180
progress_wait_seconds=120

state_dir=
evidence_dir=
mutation_started=0
finalized=0
initial_optional_running=

fail() {
  echo "PHOENIX_SHADOW_CONTRACT_TRANSITION_FAILED: $1" >&2
  exit 1
}

case "$mode" in
  plan)
    [ "$#" -eq 2 ] || fail 'plan mode requires exactly a stage directory'
    ;;
  apply)
    [ "$#" -eq 3 ] || fail 'apply mode requires a stage directory and confirmation'
    [ "$confirmation" = "$required_confirmation" ] ||
      fail 'exact apply confirmation is required'
    ;;
  *) fail 'mode must be plan or apply' ;;
esac

[ "$(id -u)" -eq 0 ] || fail 'root privileges are required'
[ "$(uname -s)" = Linux ] || fail 'Linux is required'
case "$stage_dir:$deploy_root:$env_file" in
  /*:/*:/*) ;;
  *) fail 'stage, deployment, and environment paths must be absolute' ;;
esac
if [ ! -d "$stage_dir" ] || [ -L "$stage_dir" ]; then
  fail 'stage directory is unsafe'
fi

release_dir=$stage_dir/release
rollback_dir=$stage_dir/rollback
release_manifest=$release_dir/release-manifest.json
release_archive=$release_dir/phoenix-release-assets-$release_sha.tar.gz
release_assets_manifest=$release_dir/release-assets-manifest.json
release_checksums=$release_dir/release-assets-checksums.txt
release_provenance=$release_dir/release-provenance.json
release_run_evidence=$release_dir/build-run-evidence.json
rollback_manifest=$rollback_dir/release-manifest.json
rollback_archive=$rollback_dir/phoenix-release-assets-$rollback_sha.tar.gz
rollback_assets_manifest=$rollback_dir/release-assets-manifest.json
rollback_checksums=$rollback_dir/release-assets-checksums.txt
rollback_provenance=$rollback_dir/release-provenance.json
rollback_run_evidence=$rollback_dir/build-run-evidence.json
candidate_route_registry=$stage_dir/candidate-route-registry.json
plan_file=$stage_dir/shadow-contract-transition-plan.json
helper=$stage_dir/prelive_shadow_contract_transition.py
maintenance_helper=$stage_dir/prelive_protected_maintenance.py

for required_file in \
  "$release_manifest" \
  "$release_archive" \
  "$release_assets_manifest" \
  "$release_checksums" \
  "$release_provenance" \
  "$release_run_evidence" \
  "$rollback_manifest" \
  "$rollback_archive" \
  "$rollback_assets_manifest" \
  "$rollback_checksums" \
  "$rollback_provenance" \
  "$rollback_run_evidence" \
  "$candidate_route_registry" \
  "$plan_file" \
  "$helper" \
  "$maintenance_helper" \
  "$stage_dir/production_context.py" \
  "$stage_dir/release_assets.py" \
  "$stage_dir/shadow_route_discovery.py" \
  "$env_file"
do
  if [ ! -f "$required_file" ] || [ -L "$required_file" ] || [ ! -s "$required_file" ]; then
    fail 'required staged evidence or helper is missing or unsafe'
  fi
done

for required_command in \
  awk cmp cp date df docker find grep install mktemp python3 sha256sum stat tar
do
  command -v "$required_command" >/dev/null 2>&1 ||
    fail "required command is unavailable: $required_command"
done
docker compose version >/dev/null 2>&1 || fail 'Docker Compose is unavailable'
[ -z "$(find "$deploy_dir" -type l -print -quit)" ] ||
  fail 'deployment context contains an unsafe symbolic link'

state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-shadow-contract-transition.XXXXXX") ||
  fail 'private transition state could not be created'

cleanup_state() {
  [ -z "$state_dir" ] || rm -rf -- "$state_dir"
}

compose_with() (
  compose_operator_env=$1
  compose_release_env=$2
  shift 2
  unset COMPOSE_FILE COMPOSE_PROFILES ENGINE_ROUTE_REGISTRY_JSON
  PHOENIX_ENV_FILE="$compose_operator_env" PHOENIX_RELEASE_ENV="$compose_release_env" \
    docker compose \
      --project-directory "$deploy_dir" \
      --env-file "$compose_operator_env" \
      --env-file "$compose_release_env" \
      -f "$deploy_dir/compose.prod.yml" "$@"
)

container_id() (
  container_operator_env=$1
  container_release_env=$2
  container_service=$3
  container_ids=$(compose_with \
    "$container_operator_env" "$container_release_env" ps -a -q "$container_service") ||
    exit 1
  container_count=$(printf '%s\n' "$container_ids" |
    awk 'NF { count += 1 } END { print count + 0 }')
  [ "$container_count" -eq 1 ] || exit 1
  printf '%s\n' "$container_ids" | awk 'NF { print; exit }'
)

service_healthy() (
  healthy_operator_env=$1
  healthy_release_env=$2
  healthy_service=$3
  healthy_id=$(container_id \
    "$healthy_operator_env" "$healthy_release_env" "$healthy_service") || exit 1
  healthy_state=$(docker inspect --format \
    '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.State.OOMKilled}}' \
    "$healthy_id" 2>/dev/null) || exit 1
  [ "$healthy_state" = 'running|healthy|false' ]
)

wait_service_healthy() (
  wait_operator_env=$1
  wait_release_env=$2
  wait_service=$3
  wait_deadline=$(( $(date +%s) + service_wait_seconds ))
  while [ "$(date +%s)" -lt "$wait_deadline" ]; do
    service_healthy "$wait_operator_env" "$wait_release_env" "$wait_service" && exit 0
    sleep 3
  done
  exit 1
)

assert_protected_healthy() (
  assert_operator_env=$1
  assert_release_env=$2
  for assert_service in $protected_services; do
    service_healthy "$assert_operator_env" "$assert_release_env" "$assert_service" ||
      exit 1
  done
)

assert_control_services_stopped() (
  stopped_operator_env=$1
  stopped_release_env=$2
  for stopped_service in $optional_stop_order migration-runner; do
    stopped_ids=$(compose_with \
      "$stopped_operator_env" "$stopped_release_env" ps -q "$stopped_service") ||
      exit 1
    [ -z "$stopped_ids" ] || exit 1
  done
  live_ids=$(docker ps -q \
    --filter 'label=com.docker.compose.service=live-executor') || exit 1
  [ -z "$live_ids" ]
)

assert_forbidden_services_stopped() (
  forbidden_operator_env=$1
  forbidden_release_env=$2
  migration_ids=$(compose_with \
    "$forbidden_operator_env" "$forbidden_release_env" ps -q migration-runner) ||
    exit 1
  [ -z "$migration_ids" ] || exit 1
  live_ids=$(docker ps -q \
    --filter 'label=com.docker.compose.service=live-executor') || exit 1
  [ -z "$live_ids" ]
)

capture_service_inspect() (
  inspect_operator_env=$1
  inspect_release_env=$2
  inspect_service=$3
  inspect_output=$4
  inspect_id=$(container_id \
    "$inspect_operator_env" "$inspect_release_env" "$inspect_service") || exit 1
  docker inspect --format \
    '{"container_id":{{json .Id}},"configured_image":{{json .Config.Image}},"local_image_id":{{json .Image}},"created_at":{{json .Created}},"started_at":{{json .State.StartedAt}},"restart_count":{{.RestartCount}},"oom_killed":{{.State.OOMKilled}},"status":{{json .State.Status}},"health":{{if .State.Health}}{{json .State.Health.Status}}{{else}}"none"{{end}},"mounts":{{json .Mounts}},"networks":{{json .NetworkSettings.Networks}}}' \
    "$inspect_id" >"$inspect_output"
  [ -s "$inspect_output" ]
)

capture_running_inventory() (
  inventory_operator_env=$1
  inventory_release_env=$2
  inventory_output=$3
  : >"$inventory_output"
  for inventory_service in $runtime_services; do
    inventory_ids=$(compose_with \
      "$inventory_operator_env" "$inventory_release_env" \
      ps -a -q "$inventory_service") || exit 1
    inventory_count=$(printf '%s\n' "$inventory_ids" |
      awk 'NF { count += 1 } END { print count + 0 }')
    case "$inventory_count" in
      0) printf '%s\tabsent\t-\t-\n' "$inventory_service" >>"$inventory_output" ;;
      1)
        inventory_id=$(printf '%s\n' "$inventory_ids" | awk 'NF { print; exit }')
        inventory_value=$(docker inspect --format \
          '{{.State.Status}}{{printf "\t"}}{{.Config.Image}}{{printf "\t"}}{{.Image}}' \
          "$inventory_id") || exit 1
        printf '%s\t%s\n' "$inventory_service" "$inventory_value" >>"$inventory_output"
        ;;
      *) exit 1 ;;
    esac
  done
  [ -s "$inventory_output" ]
)

capture_jetstream() (
  jetstream_operator_env=$1
  jetstream_release_env=$2
  jetstream_output=$3
  compose_with "$jetstream_operator_env" "$jetstream_release_env" \
    exec -T nats wget -q -O - \
    'http://127.0.0.1:8222/jsz?streams=true&consumers=true&config=true' \
    >"$jetstream_output"
  [ -s "$jetstream_output" ]
)

capture_database() (
  database_operator_env=$1
  database_release_env=$2
  database_output=$3
  compose_with "$database_operator_env" "$database_release_env" exec -T postgres \
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
  metrics_operator_env=$1
  metrics_release_env=$2
  metrics_service=$3
  metrics_port=$4
  metrics_output=$5
  metrics_id=$(container_id \
    "$metrics_operator_env" "$metrics_release_env" "$metrics_service") || exit 1
  metrics_status=$(docker inspect --format '{{.State.Status}}' "$metrics_id") || exit 1
  if [ "$metrics_status" != running ]; then
    : >"$metrics_output"
    exit 0
  fi
  compose_with "$metrics_operator_env" "$metrics_release_env" \
    exec -T "$metrics_service" wget -q -O - \
    "http://127.0.0.1:$metrics_port/metrics" >"$metrics_output"
  [ -s "$metrics_output" ]
)

capture_protected_storage() (
  storage_operator_env=$1
  storage_release_env=$2
  storage_output=$3
  postgres_id=$(container_id \
    "$storage_operator_env" "$storage_release_env" postgres) || exit 1
  postgres_mount=$(docker inspect --format \
    '{{range .Mounts}}{{if eq .Destination "/var/lib/postgresql/data"}}{{.Type}}|{{.Source}}|{{.RW}}{{"\n"}}{{end}}{{end}}' \
    "$postgres_id") || exit 1
  [ "$(printf '%s\n' "$postgres_mount" |
    awk 'NF { count += 1 } END { print count + 0 }')" -eq 1 ] || exit 1
  old_ifs=$IFS
  IFS='|' read -r postgres_mount_type postgres_source postgres_rw <<EOF
$postgres_mount
EOF
  IFS=$old_ifs
  [ "$postgres_mount_type" = bind ] || exit 1
  [ "$postgres_source" = "$deploy_root/data/postgres" ] || exit 1
  [ "$postgres_rw" = true ] || exit 1
  [ -d "$postgres_source" ] && [ ! -L "$postgres_source" ] || exit 1

  nats_id=$(container_id "$storage_operator_env" "$storage_release_env" nats) ||
    exit 1
  nats_mount=$(docker inspect --format \
    '{{range .Mounts}}{{if eq .Destination "/data/jetstream"}}{{.Type}}|{{.Name}}|{{.RW}}{{"\n"}}{{end}}{{end}}' \
    "$nats_id") || exit 1
  [ "$(printf '%s\n' "$nats_mount" |
    awk 'NF { count += 1 } END { print count + 0 }')" -eq 1 ] || exit 1
  IFS='|' read -r nats_mount_type nats_volume nats_rw <<EOF
$nats_mount
EOF
  IFS=$old_ifs
  [ "$nats_mount_type" = volume ] || exit 1
  [ -n "$nats_volume" ] && [ "$nats_rw" = true ] || exit 1
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
      . PG_VERSION base global pg_wal global/pg_control global/pg_filenode.map postmaster.pid
    do
      storage_target=$postgres_source
      [ "$storage_path" = . ] || storage_target=$postgres_source/$storage_path
      [ -e "$storage_target" ] && [ ! -L "$storage_target" ] || exit 1
      printf 'postgres-path|%s|%s\n' \
        "$storage_path" "$(stat -c '%u|%g|%f' "$storage_target")"
    done
    printf 'nats-volume|%s\n' "$nats_volume_metadata"
    printf 'nats-mountpoint|%s\n' "$(stat -c '%u|%g|%f' "$nats_mountpoint")"
  } >"$storage_output"
  [ -s "$storage_output" ]
)

capture_snapshot() (
  snapshot_phase=$1
  snapshot_sha=$2
  snapshot_operator_env=$3
  snapshot_release_env=$4
  snapshot_output=$5
  snapshot_dir=$(mktemp -d "$state_dir/snapshot-$snapshot_phase.XXXXXX") || exit 1
  trap 'rm -rf -- "$snapshot_dir"' EXIT HUP INT TERM

  set -- python3 "$maintenance_helper" snapshot \
    --phase "$snapshot_phase" \
    --release-sha "$snapshot_sha"
  for snapshot_service in $protected_services; do
    snapshot_service_file=$snapshot_dir/$snapshot_service.json
    capture_service_inspect \
      "$snapshot_operator_env" "$snapshot_release_env" \
      "$snapshot_service" "$snapshot_service_file" || exit 1
    set -- "$@" --service "$snapshot_service=$snapshot_service_file"
  done
  capture_jetstream \
    "$snapshot_operator_env" "$snapshot_release_env" \
    "$snapshot_dir/jetstream.json" || exit 1
  capture_database \
    "$snapshot_operator_env" "$snapshot_release_env" \
    "$snapshot_dir/database.json" || exit 1
  capture_metrics \
    "$snapshot_operator_env" "$snapshot_release_env" \
    feed-ingestor 9100 "$snapshot_dir/feed.metrics" || exit 1
  capture_metrics \
    "$snapshot_operator_env" "$snapshot_release_env" \
    recorder 9400 "$snapshot_dir/recorder.metrics" || exit 1
  capture_protected_storage \
    "$snapshot_operator_env" "$snapshot_release_env" \
    "$snapshot_dir/storage.metadata" || exit 1
  snapshot_disk_free=$(df -Pk "$deploy_root" |
    awk 'NR == 2 { printf "%.0f\n", $4 * 1024 }')
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

capture_runtime_state() (
  runtime_phase=$1
  runtime_sha=$2
  runtime_operator_env=$3
  runtime_release_env=$4
  runtime_output=$5
  runtime_dir=$(mktemp -d "$state_dir/runtime-$runtime_phase.XXXXXX") || exit 1
  trap 'rm -rf -- "$runtime_dir"' EXIT HUP INT TERM
  set -- python3 "$helper" runtime-state \
    --plan "$plan_file" \
    --phase "$runtime_phase" \
    --release-sha "$runtime_sha"
  for runtime_service in $runtime_services; do
    runtime_service_file=$runtime_dir/$runtime_service.json
    capture_service_inspect \
      "$runtime_operator_env" "$runtime_release_env" \
      "$runtime_service" "$runtime_service_file" || exit 1
    set -- "$@" --service "$runtime_service=$runtime_service_file"
  done
  "$@" --output "$runtime_output" >/dev/null
)

wait_recorder_drain() (
  drain_operator_env=$1
  drain_release_env=$2
  drain_deadline=$(( $(date +%s) + drain_wait_seconds ))
  drain_jetstream=$state_dir/drain-jetstream.json
  while [ "$(date +%s)" -lt "$drain_deadline" ]; do
    capture_jetstream \
      "$drain_operator_env" "$drain_release_env" "$drain_jetstream" || exit 1
    drain_state=$(python3 "$maintenance_helper" consumer-state \
      --jetstream "$drain_jetstream") || exit 1
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

read_validation_error_code() (
  validation_error=$1
  awk -F '"' '
    NR == 1 && $2 == "code" && $4 ~ /^[a-z][a-z0-9_:.-]*$/ {
      print $4
      found = 1
      exit
    }
    END { if (!found) exit 1 }
  ' "$validation_error"
)

preserve_progress_timeout() (
  timeout_role=$1
  timeout_candidate=$2
  timeout_code=$3
  [ -z "$evidence_dir" ] || {
    [ ! -s "$timeout_candidate" ] || install -m 0640 -o root -g phoenix \
      "$timeout_candidate" "$evidence_dir/$timeout_role-progress-timeout.json"
    printf \
      '{"failed_predicate":"%s","role":"%s","status":"timeout"}\n' \
      "$timeout_code" "$timeout_role" >"$state_dir/progress-timeout.json"
    install -m 0640 -o root -g phoenix \
      "$state_dir/progress-timeout.json" \
      "$evidence_dir/$timeout_role-progress-timeout-diagnostic.json"
  }
  echo \
    "PHOENIX_SHADOW_CONTRACT_TRANSITION_PROGRESS_TIMEOUT: role=$timeout_role failed_predicate=$timeout_code" \
    >&2
)

wait_for_progress() (
  progress_role=$1
  progress_phase=$2
  progress_sha=$3
  progress_operator_env=$4
  progress_release_env=$5
  progress_baseline=$6
  progress_output=$7
  progress_deadline=$(( $(date +%s) + progress_wait_seconds ))
  progress_candidate=$state_dir/progress-$progress_role.json
  progress_error=$state_dir/progress-$progress_role.error.json
  progress_code=snapshot_capture_failed
  while [ "$(date +%s)" -lt "$progress_deadline" ]; do
    if capture_snapshot \
      "$progress_phase" "$progress_sha" \
      "$progress_operator_env" "$progress_release_env" "$progress_candidate"
    then
      if python3 "$helper" validate-data-transition \
        --plan "$plan_file" \
        --baseline "$state_dir/pre.json" \
        --progress-baseline "$progress_baseline" \
        --current "$progress_candidate" \
        --role "$progress_role" >/dev/null 2>"$progress_error"
      then
        cp "$progress_candidate" "$progress_output"
        exit 0
      fi
      progress_code=$(read_validation_error_code "$progress_error") ||
        progress_code=transition_validation_error_unparseable
    fi
    sleep 3
  done
  preserve_progress_timeout "$progress_role" "$progress_candidate" "$progress_code"
  exit 1
)

start_service() {
  start_operator_env=$1
  start_release_env=$2
  start_service_name=$3
  case "$start_service_name" in
    recorder|feed-ingestor|rpc-gateway|phoenix-engine|shadow-dispatcher|dashboard|prometheus) ;;
    *) return 1 ;;
  esac
  compose_with "$start_operator_env" "$start_release_env" \
    up -d --no-deps "$start_service_name" >/dev/null || return 1
  wait_service_healthy \
    "$start_operator_env" "$start_release_env" "$start_service_name"
}

restore_initial_optional_services() {
  restore_operator_env=$1
  restore_release_env=$2
  for restore_service in rpc-gateway phoenix-engine shadow-dispatcher dashboard prometheus; do
    case " $initial_optional_running " in
      *" $restore_service "*)
        start_service \
          "$restore_operator_env" "$restore_release_env" "$restore_service" ||
          return 1
        ;;
    esac
  done
  assert_protected_healthy "$restore_operator_env" "$restore_release_env"
}

stop_transition_services() {
  stop_operator_env=$1
  stop_release_env=$2
  for stop_service in $optional_stop_order; do
    compose_with "$stop_operator_env" "$stop_release_env" \
      stop "$stop_service" >/dev/null || return 1
  done
  compose_with "$stop_operator_env" "$stop_release_env" \
    stop feed-ingestor >/dev/null || return 1
  wait_recorder_drain "$stop_operator_env" "$stop_release_env" || return 1
  compose_with "$stop_operator_env" "$stop_release_env" \
    stop recorder >/dev/null
}

install_active_file() (
  active_source=$1
  active_target=$2
  active_owner=$3
  active_group=$4
  active_mode=$5
  active_dir=$(dirname -- "$active_target")
  active_tmp=$(mktemp "$active_dir/.shadow-contract-transition.XXXXXX") || exit 1
  trap 'rm -f -- "$active_tmp"' EXIT HUP INT TERM
  cp "$active_source" "$active_tmp" &&
    chown "$active_owner:$active_group" "$active_tmp" &&
    chmod "$active_mode" "$active_tmp" &&
    mv "$active_tmp" "$active_target"
)

write_active_value() (
  active_value=$1
  active_target=$2
  active_tmp=$(mktemp "$runtime_root/.shadow-contract-value.XXXXXX") || exit 1
  trap 'rm -f -- "$active_tmp"' EXIT HUP INT TERM
  printf '%s\n' "$active_value" >"$active_tmp" &&
    chown phoenix:phoenix "$active_tmp" &&
    chmod 0640 "$active_tmp" &&
    mv "$active_tmp" "$active_target"
)

restore_optional_path() {
  restore_relative=$1
  restore_target=$deploy_dir/$restore_relative
  restore_marker=$state_dir/backup/optional-paths/$restore_relative.present
  restore_source=$state_dir/backup/optional-paths/$restore_relative
  if [ -f "$restore_marker" ]; then
    install -D -m 0640 -o phoenix -g phoenix "$restore_source" "$restore_target"
  else
    rm -f -- "$restore_target"
  fi
}

restore_context_backup() {
  tar -xzf "$state_dir/backup/deploy-context.tar.gz" -C "$deploy_dir" || return 1
  restore_optional_path compose.live-canary.yml || return 1
  restore_optional_path live-executor/schema/001_live_canary.sql || return 1
  restore_optional_path live-executor/schema/002_approval_evidence.sql || return 1
  restore_optional_path manifests/$release_sha.json || return 1
  restore_optional_path manifests/$release_sha.env || return 1
  restore_optional_path manifests/$release_sha.render.json || return 1
  restore_optional_path manifests/$release_sha.state.json || return 1
}

restore_environment_backup() {
  install_active_file "$state_dir/backup/phoenix.env" "$env_file" root root 0600
}

backup_transition_state() {
  backup_root=$state_dir/backup
  install -d -m 0700 "$backup_root" "$backup_root/optional-paths" || return 1
  cp "$env_file" "$backup_root/phoenix.env" || return 1
  chmod 0600 "$backup_root/phoenix.env" || return 1
  tar -czf "$backup_root/deploy-context.tar.gz" \
    --exclude='./.runtime' \
    --exclude="./manifests/$release_sha.json" \
    --exclude="./manifests/$release_sha.env" \
    --exclude="./manifests/$release_sha.render.json" \
    --exclude="./manifests/$release_sha.state.json" \
    -C "$deploy_dir" . || return 1
  for backup_relative in \
    compose.live-canary.yml \
    live-executor/schema/001_live_canary.sql \
    live-executor/schema/002_approval_evidence.sql \
    manifests/$release_sha.json \
    manifests/$release_sha.env \
    manifests/$release_sha.render.json \
    manifests/$release_sha.state.json
  do
    backup_source=$deploy_dir/$backup_relative
    backup_target=$backup_root/optional-paths/$backup_relative
    if [ -e "$backup_source" ]; then
      [ -f "$backup_source" ] && [ ! -L "$backup_source" ] || return 1
      install -D -m 0600 "$backup_source" "$backup_target" || return 1
      : >"$backup_target.present"
    fi
  done
}

rollback_transition() {
  echo "PHOENIX_SHADOW_CONTRACT_TRANSITION_ROLLBACK_STARTED: $rollback_sha" >&2
  for rollback_stop in $optional_stop_order; do
    compose_with "$candidate_env" "$release_env" stop "$rollback_stop" \
      >/dev/null 2>&1 || true
  done
  compose_with "$candidate_env" "$release_env" stop feed-ingestor \
    >/dev/null 2>&1 || true
  wait_recorder_drain "$candidate_env" "$release_env" >/dev/null 2>&1 || true
  compose_with "$candidate_env" "$release_env" stop recorder \
    >/dev/null 2>&1 || true

  restore_environment_backup || return 1
  PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_ENV_FILE="$env_file" \
    /bin/sh "$rollback_tree/scripts/install-production-release-context.sh" \
      "$rollback_sha" "$rollback_tree" >/dev/null || return 1
  restore_context_backup || return 1
  python3 "$helper" validate-env \
    --plan "$plan_file" --env-file "$env_file" --role rollback >/dev/null ||
    return 1

  start_service "$env_file" "$rollback_env" recorder || return 1
  start_service "$env_file" "$rollback_env" feed-ingestor || return 1
  capture_snapshot \
    rollback-start "$rollback_sha" "$env_file" "$rollback_env" \
    "$state_dir/rollback-start.json" || return 1
  wait_for_progress \
    rollback rollback-final "$rollback_sha" "$env_file" "$rollback_env" \
    "$state_dir/rollback-start.json" "$state_dir/rollback-progress.json" ||
    return 1
  for rollback_start in rpc-gateway phoenix-engine shadow-dispatcher dashboard prometheus; do
    start_service "$env_file" "$rollback_env" "$rollback_start" || return 1
  done
  capture_runtime_state \
    rollback "$rollback_sha" "$env_file" "$rollback_env" \
    "$state_dir/rollback-runtime.json" || return 1
  python3 "$helper" validate-runtime \
    --plan "$plan_file" \
    --baseline "$state_dir/pre.json" \
    --runtime "$state_dir/rollback-runtime.json" \
    --role rollback >/dev/null || return 1
  assert_protected_healthy "$env_file" "$rollback_env" || return 1
  [ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$rollback_sha" ] ||
    return 1
  python3 "$helper" validate-state \
    --plan "$plan_file" \
    --state "$deploy_dir/current-release.json" \
    --role rollback >/dev/null || return 1
  if [ -n "$evidence_dir" ]; then
    install -m 0640 -o root -g phoenix \
      "$state_dir/rollback-progress.json" "$evidence_dir/rollback-progress.json"
    install -m 0640 -o root -g phoenix \
      "$state_dir/rollback-runtime.json" "$evidence_dir/rollback-runtime.json"
  fi
  echo "PHOENIX_SHADOW_CONTRACT_TRANSITION_ROLLBACK_OK: $rollback_sha" >&2
}

unexpected_exit() {
  unexpected_code=$?
  trap - EXIT HUP INT TERM
  if [ "$finalized" -ne 1 ] && [ "$mutation_started" -eq 1 ]; then
    if ! rollback_transition; then
      echo \
        'PHOENIX_SHADOW_CONTRACT_TRANSITION_ROLLBACK_FAILED: operator action required' \
        >&2
    fi
  fi
  cleanup_state
  [ "$unexpected_code" -ne 0 ] || unexpected_code=1
  exit "$unexpected_code"
}

trap unexpected_exit EXIT
trap 'exit 1' HUP INT TERM

python3 "$helper" plan \
  --release-manifest "$release_manifest" \
  --release-archive "$release_archive" \
  --release-assets-manifest "$release_assets_manifest" \
  --release-checksums "$release_checksums" \
  --release-provenance "$release_provenance" \
  --release-run-evidence "$release_run_evidence" \
  --rollback-manifest "$rollback_manifest" \
  --rollback-archive "$rollback_archive" \
  --rollback-assets-manifest "$rollback_assets_manifest" \
  --rollback-checksums "$rollback_checksums" \
  --rollback-provenance "$rollback_provenance" \
  --rollback-run-evidence "$rollback_run_evidence" \
  --candidate-route-registry "$candidate_route_registry" \
  --output "$state_dir/remote-plan.json" >/dev/null ||
  fail 'immutable transition evidence validation failed'
cmp "$state_dir/remote-plan.json" "$plan_file" >/dev/null ||
  fail 'staged plan differs from independently reconstructed evidence'

install -d -m 0700 "$state_dir/release" "$state_dir/rollback"
tar -xzf "$release_archive" -C "$state_dir/release" ||
  fail 'candidate release archive extraction failed'
tar -xzf "$rollback_archive" -C "$state_dir/rollback" ||
  fail 'rollback release archive extraction failed'
release_tree=$state_dir/release/phoenix-release-$release_sha
rollback_fixture_tree=$state_dir/rollback/phoenix-release-$rollback_sha
if [ ! -d "$release_tree" ] || [ -L "$release_tree" ]; then
  fail 'candidate release tree is missing'
fi
if [ ! -d "$rollback_fixture_tree" ] || [ -L "$rollback_fixture_tree" ]; then
  fail 'rollback release tree is missing'
fi
python3 "$release_tree/scripts/release_assets.py" verify-tree \
  --root "$release_tree" \
  --manifest "$release_assets_manifest" \
  --expected-sha "$release_sha" >/dev/null ||
  fail 'candidate release tree verification failed'
python3 "$rollback_fixture_tree/scripts/release_assets.py" verify-tree \
  --root "$rollback_fixture_tree" \
  --manifest "$rollback_assets_manifest" \
  --expected-sha "$rollback_sha" >/dev/null ||
  fail 'rollback release tree verification failed'

current_release=$deploy_dir/current-release
previous_release=$deploy_dir/previous-release
current_release_env=$deploy_dir/current-release.env
current_release_state=$deploy_dir/current-release.json
current_release_context=$deploy_dir/current-release-context.json
asset_marker=$deploy_dir/release-assets.sha
rollback_tree=$release_root/$rollback_sha
rollback_host_manifest=$deploy_dir/manifests/$rollback_sha.json

for current_file in \
  "$current_release" \
  "$current_release_env" \
  "$current_release_state" \
  "$current_release_context" \
  "$asset_marker" \
  "$rollback_host_manifest" \
  "$rollback_tree/release-assets-manifest.json" \
  "$rollback_tree/compose.prod.yml" \
  "$deploy_dir/compose.prod.yml"
do
  if [ ! -f "$current_file" ] || [ -L "$current_file" ] || [ ! -s "$current_file" ]; then
    fail 'current canonical release context is incomplete or unsafe'
  fi
done
[ "$(tr -d '\r\n' <"$current_release")" = "$rollback_sha" ] ||
  fail 'current release is not the reviewed rollback release'
[ "$(tr -d '\r\n' <"$asset_marker")" = "$rollback_sha" ] ||
  fail 'current release-assets marker is not the reviewed rollback release'
cmp "$rollback_manifest" "$rollback_host_manifest" >/dev/null ||
  fail 'host rollback manifest differs from reviewed evidence'
cmp "$rollback_assets_manifest" "$rollback_tree/release-assets-manifest.json" >/dev/null ||
  fail 'installed rollback assets differ from reviewed evidence'

release_env=$state_dir/release.env
rollback_env=$state_dir/rollback.env
candidate_env=$state_dir/candidate.env
python3 "$stage_dir/production_context.py" manifest-env \
  --manifest "$release_manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env" || fail 'candidate image environment is invalid'
python3 "$stage_dir/production_context.py" manifest-env \
  --manifest "$rollback_manifest" \
  --expected-sha "$rollback_sha" \
  --output "$rollback_env" || fail 'rollback image environment is invalid'
cmp "$rollback_env" "$current_release_env" >/dev/null ||
  fail 'current image environment differs from the rollback manifest'

python3 "$helper" validate-env \
  --plan "$plan_file" \
  --env-file "$env_file" \
  --role rollback \
  --output "$state_dir/environment-summary.json" >/dev/null ||
  fail 'operator environment is not the exact rollback SHADOW contract'
python3 "$helper" install-route-env \
  --plan "$plan_file" \
  --source "$env_file" \
  --output "$candidate_env" \
  --summary-output "$state_dir/candidate-environment-summary.json" >/dev/null ||
  fail 'candidate route environment could not be materialized'
"$rollback_fixture_tree/scripts/validate-production-env.sh" "$env_file" \
  >"$state_dir/rollback-env-validation.log" ||
  fail 'rollback production environment validation failed'
"$release_tree/scripts/validate-production-env.sh" "$candidate_env" \
  >"$state_dir/candidate-env-validation.log" ||
  fail 'candidate production environment validation failed'

set -a
# shellcheck disable=SC1090
. "$env_file"
set +a
[ "$POSTGRES_DB" = phoenix_v5_654dad17 ] ||
  fail 'PostgreSQL database identity is not phoenix_v5_654dad17'

"$release_tree/scripts/render-production-compose.sh" \
  --compose-file "$release_tree/compose.prod.yml" \
  --env-file "$candidate_env" \
  --release-env "$release_env" \
  --release-manifest "$release_manifest" \
  --output "$state_dir/release.compose.json" \
  --metadata-output "$state_dir/release.render.json" >/dev/null ||
  fail 'candidate production render failed'
"$rollback_fixture_tree/scripts/render-production-compose.sh" \
  --compose-file "$rollback_fixture_tree/compose.prod.yml" \
  --env-file "$env_file" \
  --release-env "$rollback_env" \
  --release-manifest "$rollback_manifest" \
  --output "$state_dir/rollback.compose.json" \
  --metadata-output "$state_dir/rollback.render.json" >/dev/null ||
  fail 'rollback production render failed'
python3 "$helper" validate-render-pair \
  --plan "$plan_file" \
  --release-metadata "$state_dir/release.render.json" \
  --rollback-metadata "$state_dir/rollback.render.json" \
  --release-compose "$state_dir/release.compose.json" \
  --rollback-compose "$state_dir/rollback.compose.json" >/dev/null ||
  fail 'candidate and rollback semantic Compose contracts are not reviewed'

"$deploy_dir/render-production-compose.sh" \
  --compose-file "$deploy_dir/compose.prod.yml" \
  --env-file "$env_file" \
  --release-env "$rollback_env" \
  --release-manifest "$rollback_manifest" \
  --output "$state_dir/current.rollback.compose.json" \
  --metadata-output "$state_dir/current.rollback.render.json" >/dev/null ||
  fail 'current rollback production render failed'

python3 "$rollback_fixture_tree/scripts/production_context.py" write-state \
  --manifest "$rollback_manifest" \
  --release-env "$rollback_env" \
  --render-metadata "$state_dir/current.rollback.render.json" \
  --compose-config "$state_dir/current.rollback.compose.json" \
  --output "$state_dir/rollback.expected.state.json" ||
  fail 'rollback release state reconstruction failed'
cmp "$state_dir/rollback.expected.state.json" "$current_release_state" >/dev/null ||
  fail 'current release state differs from the exact rollback context'
python3 "$helper" validate-state \
  --plan "$plan_file" \
  --state "$current_release_state" \
  --role rollback >/dev/null || fail 'current rollback state is invalid'

assert_protected_healthy "$env_file" "$rollback_env" ||
  fail 'protected data plane is unhealthy before transition'
assert_forbidden_services_stopped "$env_file" "$rollback_env" ||
  fail 'migration-runner or live-executor is active'
compose_with "$env_file" "$rollback_env" exec -T postgres \
  pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null ||
  fail 'PostgreSQL readiness failed'
compose_with "$env_file" "$rollback_env" exec -T nats wget -q -O - \
  'http://127.0.0.1:8222/healthz?js-enabled-only=true' >/dev/null ||
  fail 'JetStream readiness failed'

preflight_dir=$state_dir/preflight
install -d -m 0700 "$preflight_dir"
for preflight_service in $protected_services; do
  capture_service_inspect \
    "$env_file" "$rollback_env" "$preflight_service" \
    "$preflight_dir/$preflight_service.json" ||
    fail 'pre-transition service identity capture failed'
done
capture_jetstream "$env_file" "$rollback_env" "$preflight_dir/jetstream.json" ||
  fail 'pre-transition JetStream capture failed'
capture_database "$env_file" "$rollback_env" "$preflight_dir/database.json" ||
  fail 'pre-transition database capture failed'
python3 "$helper" validate-database \
  --plan "$plan_file" --database "$preflight_dir/database.json" >/dev/null ||
  fail 'pre-transition database contract is invalid'
capture_metrics \
  "$env_file" "$rollback_env" feed-ingestor 9100 "$preflight_dir/feed.metrics" ||
  fail 'pre-transition Feed metrics capture failed'
capture_metrics \
  "$env_file" "$rollback_env" recorder 9400 "$preflight_dir/recorder.metrics" ||
  fail 'pre-transition Recorder metrics capture failed'
capture_protected_storage \
  "$env_file" "$rollback_env" "$preflight_dir/storage.metadata" ||
  fail 'pre-transition protected storage capture failed'
capture_running_inventory \
  "$env_file" "$rollback_env" "$preflight_dir/running-images.tsv" ||
  fail 'pre-transition running image inventory failed'
df -Pk "$deploy_root" >"$preflight_dir/disk.txt" ||
  fail 'pre-transition disk evidence capture failed'
cp "$state_dir/environment-summary.json" "$preflight_dir/environment-summary.json"
cp "$state_dir/remote-plan.json" "$preflight_dir/plan.json"
cp "$current_release_state" "$preflight_dir/current-release.json"
cp "$current_release_context" "$preflight_dir/current-release-context.json"
sha256sum \
  "$current_release" \
  "$current_release_env" \
  "$current_release_state" \
  "$current_release_context" \
  "$asset_marker" \
  "$rollback_host_manifest" >"$preflight_dir/current-context-sha256.txt"

if [ "$mode" = plan ]; then
  finalized=1
  trap - EXIT HUP INT TERM
  cleanup_state
  echo \
    "PHOENIX_SHADOW_CONTRACT_TRANSITION_DRY_RUN_OK: release=$release_sha rollback=$rollback_sha"
  exit 0
fi

install -d -m 0750 -o phoenix -g phoenix "$runtime_root" "$evidence_root"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(printf '%.8s' "$release_sha")"
evidence_dir=$evidence_root/$run_id
install -d -m 0750 -o root -g phoenix "$evidence_dir"

for evidence_file in \
  environment-summary.json \
  plan.json \
  current-release.json \
  current-release-context.json \
  current-context-sha256.txt \
  jetstream.json \
  database.json \
  feed.metrics \
  recorder.metrics \
  running-images.tsv \
  storage.metadata \
  disk.txt \
  nitro-feed-relay.json \
  feed-ingestor.json \
  nats.json \
  postgres.json \
  recorder.json
do
  install -m 0640 -o root -g phoenix \
    "$preflight_dir/$evidence_file" "$evidence_dir/pre-$evidence_file" ||
    fail 'pre-transition evidence publication failed'
done
install -m 0640 -o root -g phoenix \
  "$state_dir/release.render.json" "$evidence_dir/candidate-render.json"
install -m 0640 -o root -g phoenix \
  "$state_dir/rollback.render.json" "$evidence_dir/rollback-render.json"
install -m 0640 -o root -g phoenix \
  "$release_manifest" "$evidence_dir/candidate-release-manifest.json"
install -m 0640 -o root -g phoenix \
  "$rollback_manifest" "$evidence_dir/rollback-release-manifest.json"
install -m 0640 -o root -g phoenix \
  "$release_provenance" "$evidence_dir/candidate-release-provenance.json"
install -m 0640 -o root -g phoenix \
  "$rollback_provenance" "$evidence_dir/rollback-release-provenance.json"

backup_transition_state || fail 'atomic transition backup failed'
cp "$release_manifest" "$state_dir/backup/candidate-release-manifest.json"
cp "$rollback_manifest" "$state_dir/backup/rollback-release-manifest.json"

python3 "$helper" image-refs --plan "$plan_file" >"$state_dir/image-refs.tsv" ||
  fail 'image references could not be read from the reviewed plan'
while IFS='	' read -r image_role image_name image_reference image_sha; do
  case "$image_name" in
    dashboard|feed-ingestor|phoenix-engine|recorder|rpc-gateway) ;;
    *) continue ;;
  esac
  docker pull "$image_reference" >/dev/null ||
    fail "reviewed image is not pullable: $image_role/$image_name"
  image_revision=$(docker image inspect --format \
    '{{index .Config.Labels "org.opencontainers.image.revision"}}' \
    "$image_reference") || fail 'reviewed image OCI labels are unavailable'
  [ "$image_revision" = "$image_sha" ] ||
    fail "reviewed image OCI revision mismatch: $image_role/$image_name"
done <"$state_dir/image-refs.tsv"

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

for optional_service in $optional_stop_order; do
  optional_running_ids=$(compose_with \
    "$env_file" "$rollback_env" ps -q "$optional_service") ||
    fail 'initial optional-service state could not be captured'
  if [ -n "$optional_running_ids" ]; then
    initial_optional_running="$initial_optional_running $optional_service"
  fi
done
for optional_service in $optional_stop_order; do
  if ! compose_with "$env_file" "$rollback_env" stop "$optional_service" >/dev/null; then
    restore_initial_optional_services "$env_file" "$rollback_env" >/dev/null 2>&1 ||
      fail 'partial optional-service stop could not be restored'
    fail "optional SHADOW service could not be stopped: $optional_service"
  fi
done
if ! assert_control_services_stopped "$env_file" "$rollback_env"; then
  restore_initial_optional_services "$env_file" "$rollback_env" >/dev/null 2>&1 ||
    fail 'partial optional-service state could not be restored'
  fail 'optional SHADOW, migration, or live-canary service remained active'
fi

if ! capture_snapshot pre "$rollback_sha" "$env_file" "$rollback_env" \
  "$state_dir/pre.json"
then
  restore_initial_optional_services "$env_file" "$rollback_env" >/dev/null 2>&1 ||
    fail 'optional services could not be restored after baseline capture failure'
  fail 'normalized pre-transition capture failed'
fi
if ! python3 "$helper" validate-baseline \
  --plan "$plan_file" --snapshot "$state_dir/pre.json" >/dev/null
then
  restore_initial_optional_services "$env_file" "$rollback_env" >/dev/null 2>&1 ||
    fail 'optional services could not be restored after baseline validation failure'
  fail 'normalized pre-transition contract is invalid'
fi
install -m 0640 -o root -g phoenix \
  "$state_dir/pre.json" "$evidence_dir/pre-transition.json"

mutation_started=1
compose_with "$env_file" "$rollback_env" stop feed-ingestor >/dev/null ||
  fail 'Feed Ingestor could not be quiesced'
wait_recorder_drain "$env_file" "$rollback_env" ||
  fail 'Recorder durable consumer did not drain'
compose_with "$env_file" "$rollback_env" stop recorder >/dev/null ||
  fail 'Recorder could not be stopped after drain'

install_active_file "$candidate_env" "$env_file" root root 0600 ||
  fail 'candidate route environment installation failed'
python3 "$helper" validate-env \
  --plan "$plan_file" --env-file "$env_file" --role release >/dev/null ||
  fail 'installed candidate environment is invalid'

PHOENIX_RELEASE_ROOT="$release_root" \
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_CONTEXT_INSTALLER="$release_tree/scripts/install-production-release-context.sh" \
  /bin/sh "$release_tree/scripts/install-release-assets.sh" \
    "$release_sha" "$release_archive" "$release_assets_manifest" "$release_checksums" \
    >/dev/null || fail 'candidate immutable release context installation failed'

install -d -m 0750 -o phoenix -g phoenix "$deploy_dir/manifests"
install -m 0640 -o phoenix -g phoenix \
  "$release_manifest" "$deploy_dir/manifests/$release_sha.json"
install -m 0640 -o phoenix -g phoenix \
  "$rollback_manifest" "$deploy_dir/manifests/$rollback_sha.json"
install_active_file \
  "$release_env" "$deploy_dir/manifests/$release_sha.env" phoenix phoenix 0640 ||
  fail 'candidate image environment installation failed'

start_service "$env_file" "$release_env" recorder ||
  fail 'candidate Recorder did not become healthy'
start_service "$env_file" "$release_env" feed-ingestor ||
  fail 'candidate Feed Ingestor did not become healthy'
capture_snapshot post-start "$release_sha" "$env_file" "$release_env" \
  "$state_dir/candidate-progress-baseline.json" ||
  fail 'candidate progress baseline capture failed'
wait_for_progress \
  release final "$release_sha" "$env_file" "$release_env" \
  "$state_dir/candidate-progress-baseline.json" "$state_dir/candidate-progress.json" ||
  fail 'candidate Feed and Recorder progress was not proven'

for candidate_service in rpc-gateway phoenix-engine shadow-dispatcher dashboard prometheus; do
  start_service "$env_file" "$release_env" "$candidate_service" ||
    fail "candidate service did not become healthy: $candidate_service"
done
assert_forbidden_services_stopped "$env_file" "$release_env" ||
  fail 'migration-runner or live-executor became active'

"$deploy_dir/render-production-compose.sh" \
  --compose-file "$deploy_dir/compose.prod.yml" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$deploy_dir/manifests/$release_sha.json" \
  --output "$state_dir/candidate.active.compose.json" \
  --metadata-output "$state_dir/candidate.active.render.json" >/dev/null ||
  fail 'installed candidate render failed'
cmp "$state_dir/candidate.active.render.json" "$state_dir/release.render.json" >/dev/null ||
  fail 'installed candidate render differs from reviewed pre-transition render'
python3 "$deploy_dir/production_context.py" write-state \
  --manifest "$deploy_dir/manifests/$release_sha.json" \
  --release-env "$release_env" \
  --render-metadata "$state_dir/candidate.active.render.json" \
  --compose-config "$state_dir/candidate.active.compose.json" \
  --output "$state_dir/candidate.active.state.json" ||
  fail 'candidate release state could not be constructed'
python3 "$helper" validate-state \
  --plan "$plan_file" \
  --state "$state_dir/candidate.active.state.json" \
  --role release >/dev/null || fail 'candidate release state is invalid'

install_active_file \
  "$state_dir/candidate.active.render.json" \
  "$deploy_dir/manifests/$release_sha.render.json" phoenix phoenix 0640 ||
  fail 'candidate render metadata promotion failed'
install_active_file \
  "$state_dir/candidate.active.state.json" \
  "$deploy_dir/manifests/$release_sha.state.json" phoenix phoenix 0640 ||
  fail 'candidate state promotion failed'
install_active_file "$release_env" "$current_release_env" phoenix phoenix 0640 ||
  fail 'current image environment promotion failed'
install_active_file \
  "$state_dir/candidate.active.state.json" "$current_release_state" \
  phoenix phoenix 0640 || fail 'current release state promotion failed'
write_active_value "$rollback_sha" "$previous_release" ||
  fail 'previous release pointer promotion failed'
write_active_value "$release_sha" "$current_release" ||
  fail 'current release pointer promotion failed'

"$deploy_dir/validate-production-release-context.sh" \
  --compose-file "$deploy_dir/compose.prod.yml" \
  --env-file "$env_file" \
  --release-env "$current_release_env" \
  --release-manifest "$deploy_dir/manifests/$release_sha.json" \
  --current-release "$current_release" \
  --release-state "$current_release_state" \
  --inspect-running \
  --rendered-output "$state_dir/final.compose.json" \
  --metadata-output "$state_dir/final.render.json" \
  --output "$state_dir/final.context.json" >/dev/null ||
  fail 'candidate active release context validation failed'
install_active_file \
  "$state_dir/final.context.json" "$current_release_context" \
  phoenix phoenix 0640 || fail 'candidate release context promotion failed'

capture_runtime_state candidate "$release_sha" "$env_file" "$release_env" \
  "$state_dir/candidate-runtime.json" || fail 'candidate runtime capture failed'
python3 "$helper" validate-runtime \
  --plan "$plan_file" \
  --baseline "$state_dir/pre.json" \
  --runtime "$state_dir/candidate-runtime.json" \
  --role release >/dev/null || fail 'candidate runtime contract is invalid'
python3 "$helper" validate-env \
  --plan "$plan_file" \
  --env-file "$env_file" \
  --role release \
  --output "$state_dir/final-environment-summary.json" >/dev/null ||
  fail 'final operator environment is invalid'

[ "$(tr -d '\r\n' <"$current_release")" = "$release_sha" ] ||
  fail 'current release pointer verification failed'
[ "$(tr -d '\r\n' <"$previous_release")" = "$rollback_sha" ] ||
  fail 'previous release pointer verification failed'
[ "$(tr -d '\r\n' <"$asset_marker")" = "$release_sha" ] ||
  fail 'candidate release-assets marker verification failed'
assert_protected_healthy "$env_file" "$release_env" ||
  fail 'protected services are unhealthy after transition'
assert_forbidden_services_stopped "$env_file" "$release_env" ||
  fail 'migration-runner or live-executor is active after transition'

compose_with "$env_file" "$release_env" logs --no-color --tail 1000 \
  nats recorder feed-ingestor >"$state_dir/protected.log" 2>&1 || true
if grep -Eiq \
  'slow consumer|core_nats_message_drop|Core NATS delivery loss|recorder_nats_slow_consumer' \
  "$state_dir/protected.log"
then
  fail 'a protected-service loss indicator was observed'
fi

for final_artifact in \
  candidate-progress-baseline.json \
  candidate-progress.json \
  candidate-runtime.json \
  candidate.active.render.json \
  candidate.active.state.json \
  final.context.json \
  final-environment-summary.json
do
  install -m 0640 -o root -g phoenix \
    "$state_dir/$final_artifact" "$evidence_dir/$final_artifact"
done
sha256sum "$evidence_dir"/* >"$evidence_dir/SHA256SUMS"
chown root:phoenix "$evidence_dir/SHA256SUMS"
chmod 0640 "$evidence_dir/SHA256SUMS"

finalized=1
trap - EXIT HUP INT TERM
cleanup_state
echo \
  "PHOENIX_SHADOW_CONTRACT_TRANSITION_OK: release=$release_sha rollback=$rollback_sha evidence=$evidence_dir"
