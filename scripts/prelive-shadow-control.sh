#!/usr/bin/env sh
set -eu
umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
compose_file=${PHOENIX_COMPOSE_FILE:-$deploy_dir/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$deploy_dir/current-release.env}
current_release_file=${PHOENIX_CURRENT_RELEASE_FILE:-$deploy_dir/current-release}
release_state=${PHOENIX_RELEASE_STATE:-$deploy_dir/current-release.json}
evidence_root=${PHOENIX_EVIDENCE_ROOT:-$deploy_root/evidence/control-plane}
dashboard_evidence_dir=${PHOENIX_DASHBOARD_EVIDENCE_DIR:-$deploy_root/evidence/dashboard}
docker_bin=${PHOENIX_DOCKER_BIN:-docker}
python_bin=${PHOENIX_PYTHON_BIN:-python3}
helper=$script_dir/prelive_shadow_control.py
dashboard_collector=$script_dir/prelive_dashboard_live.py
dashboard_compiler=$script_dir/prelive_dashboard_snapshot.py
dashboard_sql=$script_dir/sql/prelive-dashboard-source.sql
sample_interval=${PHOENIX_SHADOW_SAMPLE_INTERVAL_SECONDS:-30}
readiness_timeout=${PHOENIX_SHADOW_READINESS_TIMEOUT_SECONDS:-300}
positive_timeout=${PHOENIX_SHADOW_POSITIVE_ROUTE_TIMEOUT_SECONDS:-900}

protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'
optional_services='prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'
full_services="$protected_services $optional_services"
stop_services='dashboard phoenix-engine shadow-dispatcher rpc-gateway prometheus'

state_dir=
optional_started=0
finalized=0
stop_requested=0
runtime_sampling_baseline=

blocked() {
  echo "PRELIVE_SHADOW_CONTROL_BLOCKED: $1" >&2
  exit 1
}

fail() {
  echo "PRELIVE_SHADOW_CONTROL_FAIL: $1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage:
  prelive-shadow-control.sh plan MODE
  prelive-shadow-control.sh preflight MODE
  prelive-shadow-control.sh run MODE

MODE is one of: 15m, 1h, 6h, 24h, continuous
EOF
}

bounded_integer() {
  value=$1
  minimum=$2
  maximum=$3
  case "$value" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$value" -ge "$minimum" ] && [ "$value" -le "$maximum" ]
}

utc_now() {
  date -u +%Y-%m-%dT%H:%M:%SZ
}

compose() {
  (
    unset COMPOSE_FILE COMPOSE_PROFILES ENGINE_ROUTE_REGISTRY_JSON
    PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
      "$docker_bin" compose --env-file "$env_file" --env-file "$release_env" \
        -f "$compose_file" "$@"
  )
}

python_value() {
  file=$1
  expression=$2
  "$python_bin" -c 'import json,sys; value=json.load(open(sys.argv[1], encoding="utf-8")); print(eval(sys.argv[2], {"__builtins__": {}}, {"value": value}))' \
    "$file" "$expression"
}

sql_count() {
  query=$1
  result=$(compose exec -T postgres psql -X -qAt -v ON_ERROR_STOP=1 \
    -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "$query") || return 1
  result=$(printf '%s' "$result" | tr -d '[:space:]')
  case "$result" in
    ''|*[!0-9]*) return 1 ;;
  esac
  printf '%s\n' "$result"
}

database_clock_utc() {
  database_clock_utc_value=$(compose exec -T postgres psql -X -qAt \
    -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
    -c "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')") \
    || return 1
  printf '%s' "$database_clock_utc_value" |
    grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' ||
    return 1
  printf '%s\n' "$database_clock_utc_value"
}

container_id() {
  compose ps -a -q "$1" 2>/dev/null | awk 'NF { print; exit }'
}

container_health() {
  id=$(container_id "$1")
  [ -n "$id" ] || return 1
  state=$("$docker_bin" inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id" 2>/dev/null) || return 1
  [ "$state" = 'running|healthy' ]
}

wait_service_healthy() {
  service=$1
  deadline=$(( $(date +%s) + readiness_timeout ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    container_health "$service" && return 0
    sleep 3
  done
  return 1
}

configured_digest() {
  configured_digest_service=$1
  awk -F '\t' -v service="$configured_digest_service" '$1 == service { print $2; exit }' "$state_dir/image-digests.tsv"
}

configured_reference() {
  configured_reference_service=$1
  awk -F '\t' -v service="$configured_reference_service" '$1 == service { print $3; exit }' "$state_dir/image-digests.tsv"
}

capture_service_states() {
  capture_states_output=$1
  capture_states_observed_at=$(utc_now)
  capture_states_raw=$state_dir/service-states.raw
  : >"$capture_states_raw"
  for capture_states_service in $full_services; do
    capture_states_digest=$(configured_digest "$capture_states_service")
    capture_states_reference=$(configured_reference "$capture_states_service")
    [ -n "$capture_states_digest" ] && [ -n "$capture_states_reference" ] || return 1
    capture_states_id=$(container_id "$capture_states_service")
    if [ -z "$capture_states_id" ]; then
      printf '%s\tmissing\tmissing\t%s\t0\t0\n' \
        "$capture_states_service" "$capture_states_digest" >>"$capture_states_raw"
      continue
    fi
    capture_states_selected=$("$docker_bin" inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.Config.Image}}|{{.Image}}|{{.RestartCount}}|{{.State.ExitCode}}' "$capture_states_id" 2>/dev/null) || return 1
    capture_states_status=${capture_states_selected%%|*}
    capture_states_selected=${capture_states_selected#*|}
    capture_states_health=${capture_states_selected%%|*}
    capture_states_selected=${capture_states_selected#*|}
    capture_states_configured_image=${capture_states_selected%%|*}
    capture_states_selected=${capture_states_selected#*|}
    capture_states_local_image_id=${capture_states_selected%%|*}
    capture_states_selected=${capture_states_selected#*|}
    capture_states_restart_count=${capture_states_selected%%|*}
    capture_states_exit_code=${capture_states_selected##*|}
    [ "$capture_states_configured_image" = "$capture_states_reference" ] || return 1
    printf '%s' "$capture_states_local_image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || return 1
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$capture_states_service" "$capture_states_status" "$capture_states_health" \
      "$capture_states_digest" "$capture_states_restart_count" "$capture_states_exit_code" \
      >>"$capture_states_raw"
  done
  "$python_bin" "$helper" normalize-service-states \
    --input "$capture_states_raw" \
    --observed-at "$capture_states_observed_at" \
    --output "$capture_states_output" >/dev/null
}

capture_dashboard_services() {
  capture_dashboard_output=$1
  capture_dashboard_observed_at=$(utc_now)
  capture_dashboard_raw=$state_dir/dashboard-services.raw
  : >"$capture_dashboard_raw"
  for capture_dashboard_service in $full_services; do
    capture_dashboard_digest=$(configured_digest "$capture_dashboard_service")
    capture_dashboard_reference=$(configured_reference "$capture_dashboard_service")
    [ -n "$capture_dashboard_digest" ] && [ -n "$capture_dashboard_reference" ] || return 1
    capture_dashboard_id=$(container_id "$capture_dashboard_service")
    if [ -z "$capture_dashboard_id" ]; then
      printf '%s\tmissing\tmissing\t%s\t0\tnull\tnull\tfalse\n' \
        "$capture_dashboard_service" "$capture_dashboard_digest" >>"$capture_dashboard_raw"
      continue
    fi
    capture_dashboard_selected=$("$docker_bin" inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}|{{.Config.Image}}|{{.Image}}|{{.RestartCount}}|{{.State.ExitCode}}|{{.State.StartedAt}}|{{.State.OOMKilled}}' "$capture_dashboard_id" 2>/dev/null) || return 1
    capture_dashboard_old_ifs=$IFS
    IFS='|'
    # Intentional field splitting against the temporary pipe delimiter.
    # shellcheck disable=SC2086
    set -- $capture_dashboard_selected
    IFS=$capture_dashboard_old_ifs
    [ "$#" -eq 8 ] || return 1
    capture_dashboard_status=$1
    capture_dashboard_health=$2
    capture_dashboard_configured_image=$3
    capture_dashboard_local_image_id=$4
    capture_dashboard_restart_count=$5
    capture_dashboard_exit_code=$6
    capture_dashboard_started_at=$7
    capture_dashboard_oom_killed=$8
    [ "$capture_dashboard_configured_image" = "$capture_dashboard_reference" ] || return 1
    printf '%s' "$capture_dashboard_local_image_id" | grep -Eq '^sha256:[0-9a-f]{64}$' || return 1
    case "$capture_dashboard_oom_killed" in true|false) ;; *) return 1 ;; esac
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$capture_dashboard_service" "$capture_dashboard_status" "$capture_dashboard_health" \
      "$capture_dashboard_digest" "$capture_dashboard_restart_count" \
      "$capture_dashboard_exit_code" "$capture_dashboard_started_at" \
      "$capture_dashboard_oom_killed" >>"$capture_dashboard_raw"
  done
  "$python_bin" "$dashboard_collector" normalize-services \
    --input "$capture_dashboard_raw" \
    --observed-at "$capture_dashboard_observed_at" \
    --output "$capture_dashboard_output" >/dev/null
}

capture_jetstream() {
  capture_jetstream_output=$1
  compose exec -T nats wget -q -O - \
    'http://127.0.0.1:8222/jsz?streams=true&consumers=true&config=true' \
    >"$capture_jetstream_output"
  [ -s "$capture_jetstream_output" ]
}

capture_protected_identity() {
  capture_identity_output=$1
  capture_identity_services_file=$state_dir/protected.raw
  capture_identity_jetstream_file=$state_dir/jetstream.identity.json
  : >"$capture_identity_services_file"
  for capture_identity_service in $protected_services; do
    capture_identity_id=$(container_id "$capture_identity_service")
    [ -n "$capture_identity_id" ] || return 1
    capture_identity_selected=$("$docker_bin" inspect --format '{{.Id}}|{{.Image}}|{{.Created}}|{{.State.StartedAt}}|{{.RestartCount}}|{{json .Mounts}}' "$capture_identity_id" 2>/dev/null) || return 1
    printf '%s|%s\n' "$capture_identity_service" "$capture_identity_selected" \
      >>"$capture_identity_services_file"
  done
  capture_jetstream "$capture_identity_jetstream_file" || return 1
  "$python_bin" "$helper" protected-identity \
    --services "$capture_identity_services_file" \
    --jetstream "$capture_identity_jetstream_file" \
    --output "$capture_identity_output" >/dev/null
}

protected_runtime_healthy() {
  for service in $protected_services; do
    container_health "$service" || return 1
  done
  compose exec -T postgres pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null 2>&1
}

runtime_healthy() {
  protected_runtime_healthy || return 1
  for service in $optional_services; do
    container_health "$service" || return 1
  done
}

start_optional_runtime() {
  for service in $optional_services; do
    compose up -d --no-deps "$service" >/dev/null || return 1
    wait_service_healthy "$service" || return 1
  done
  optional_started=1
}

stop_optional_runtime() {
  # The reviewed service list is intentionally expanded into Compose arguments.
  # shellcheck disable=SC2086
  compose stop $stop_services >/dev/null
  optional_started=0
}

record_preflight() {
  check=$1
  printf '%s\tpass\t%s\n' "$check" "$(utc_now)" >>"$state_dir/preflight.tsv"
}

execution_activity_count() {
  sql_count 'SELECT ((SELECT count(*) FROM execution_attempts) + (SELECT count(*) FROM executions) + (SELECT count(*) FROM realized_pnl) + (SELECT count(*) FROM shadow_decisions WHERE execution_eligible) + (SELECT count(*) FROM shadow_profitability_facts WHERE execution_request_created))::text'
}

execution_request_count() {
  sql_count 'SELECT count(*)::text FROM shadow_profitability_facts WHERE execution_request_created'
}

assert_runtime_safety() {
  [ "$(execution_activity_count)" -eq 0 ] || return 1
  # Expansion must occur inside the target containers.
  # shellcheck disable=SC2016
  container_check='[ "${PHOENIX_MODE:-}" = "SHADOW" ] && [ "${LIVE_EXECUTION:-}" = "false" ] && [ -z "${SIGNER_PRIVATE_KEY:-}" ] && [ -z "${WALLET_ADDRESS:-}" ] && [ -z "${EXECUTOR_ADDRESS:-}" ]'
  compose exec -T phoenix-engine sh -c "$container_check" >/dev/null 2>&1 || return 1
  compose exec -T shadow-dispatcher sh -c "$container_check" >/dev/null 2>&1
}

run_preflight() {
  : >"$state_dir/preflight.tsv"

  "$script_dir/validate-production-env.sh" "$env_file" >"$state_dir/env-validation.log" || fail 'production environment validation failed'
  "$script_dir/render-production-compose.sh" \
    --compose-file "$compose_file" \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --release-manifest "$release_manifest" \
    --output "$state_dir/compose.rendered.json" \
    --metadata-output "$state_dir/render.metadata.json" >/dev/null || fail 'canonical production render failed'
  record_preflight canonical_render

  "$python_bin" "$helper" render-image-digests --metadata "$state_dir/render.metadata.json" >"$state_dir/image-digests.tsv" || fail 'immutable image evidence failed'
  record_preflight immutable_images
  [ -s "$release_manifest" ] && [ -s "$release_state" ] || fail 'release manifest or checksum state is missing'
  record_preflight release_manifest
  route_hash=$(python_value "$state_dir/render.metadata.json" 'value["route_registry_hash"]') || fail 'route registry metadata is invalid'
  printf '%s' "$route_hash" | grep -Eq '^sha256:[0-9a-f]{64}$' || fail 'route registry hash is invalid'
  record_preflight route_registry
  [ "${CHAIN_ID:-}" = 42161 ] || fail 'chain ID is not 42161'
  record_preflight chain_id
  [ "${PHOENIX_MODE:-}" = SHADOW ] || fail 'PHOENIX_MODE must be SHADOW'
  record_preflight shadow_mode
  [ "${LIVE_EXECUTION:-}" = false ] || fail 'LIVE_EXECUTION must be false'
  record_preflight live_execution_disabled
  [ -z "${SIGNER_PRIVATE_KEY:-}" ] && [ -z "${WALLET_ADDRESS:-}" ] && [ -z "${EXECUTOR_ADDRESS:-}" ] || fail 'execution configuration must be blank'
  record_preflight execution_configuration_blank
  case "${PUBLIC_TRANSACTION_SUBMISSION:-}${PRIVATE_RELAY_SUBMISSION:-}${TRANSACTION_BROADCAST_URL:-}" in
    '') ;;
    *) fail 'broadcast configuration must be blank' ;;
  esac
  record_preflight broadcast_disabled
  bounded_integer "${RPC_STATE_REQUESTS_PER_MINUTE:-}" 12 1000000 || fail 'RPC state budget must be at least 12 per minute'
  record_preflight rpc_state_budget
  bounded_integer "${RPC_UPSTREAM_CALLS_PER_SECOND:-}" 1 1000000 || fail 'RPC upstream rate must be positive'
  bounded_integer "${RPC_UPSTREAM_CALL_BURST:-}" 1 1000000 || fail 'RPC upstream burst must be positive'
  record_preflight rpc_upstream_budget
  compose exec -T postgres pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null 2>&1 || fail 'PostgreSQL is unavailable'
  database_clock_baseline=$(database_clock_utc) || fail 'database clock query failed'
  record_preflight postgres_connectivity
  compose exec -T nats wget -q -O - 'http://127.0.0.1:8222/healthz?js-enabled-only=true' >/dev/null || fail 'NATS is unavailable'
  record_preflight nats_connectivity
  capture_protected_identity "$state_dir/identity.before.json" || fail 'protected service or JetStream identity is unavailable'
  record_preflight jetstream_resources
  compose run --rm --no-deps migration-runner >"$state_dir/migrations.log" || fail 'additive migrations failed'
  record_preflight migrations
  "$python_bin" "$script_dir/verify_dashboard_compose.py" "$state_dir/compose.rendered.json" >"$state_dir/dashboard-compose.log" || fail 'Dashboard isolation validation failed'
  record_preflight dashboard_read_only
  compose run --rm --no-deps --entrypoint /bin/promtool prometheus check config /etc/prometheus/prometheus.yml >"$state_dir/prometheus-validation.log" || fail 'Prometheus configuration validation failed'
  record_preflight prometheus_config
  [ "$(execution_activity_count)" -eq 0 ] || fail 'execution or submission evidence is non-zero'
  execution_request_count_before=$(execution_request_count) || fail 'execution request baseline query failed'
  [ "$execution_request_count_before" -eq 0 ] || fail 'execution request baseline is non-zero'
  record_preflight execution_requests_zero

  protected_runtime_healthy || fail 'protected data plane is not healthy'
  capture_service_states "$state_dir/states.before.json" || fail 'preflight service-state capture failed'
}

verify_active_release() {
  "$script_dir/validate-production-release-context.sh" \
    --compose-file "$compose_file" \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --release-manifest "$release_manifest" \
    --current-release "$current_release_file" \
    --release-state "$release_state" \
    --inspect-running \
    --rendered-output "$state_dir/active.compose.json" \
    --metadata-output "$state_dir/active.metadata.json" \
    --output "$state_dir/active.context.json" >/dev/null
}

run_positive_route_evidence() {
  positive_route_raw_log=$state_dir/positive-route.log
  positive_route_child_exit=0
  PHOENIX_COMPOSE_FILE="$compose_file" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_RELEASE_ENV="$release_env" \
  PHOENIX_RELEASE_MANIFEST="$release_manifest" \
    "$script_dir/shadow-positive-route-evidence.sh" \
      --timeout-seconds "$positive_timeout" >"$positive_route_raw_log" 2>&1 ||
    positive_route_child_exit=$?

  positive_route_result=1
  if [ "$positive_route_child_exit" -ne 0 ]; then
    positive_route_terminal_reason=child_exit
  elif grep -Fx 'POSITIVE_ROUTE_EVIDENCE_FOUND' "$positive_route_raw_log" >/dev/null; then
    positive_route_terminal_reason=evidence_found
    positive_route_result=0
  elif grep -Fx 'POSITIVE_ROUTE_EVIDENCE_NOT_FOUND' "$positive_route_raw_log" >/dev/null; then
    positive_route_terminal_reason=evidence_not_found
  else
    positive_route_terminal_reason=terminal_marker_missing
  fi

  positive_route_attempt_dir=$evidence_root/positive-route-attempts
  mkdir -p "$positive_route_attempt_dir" || return 1
  chmod 0750 "$positive_route_attempt_dir" || return 1
  positive_route_attempt_path=$(mktemp \
    "$positive_route_attempt_dir/positive-route-$(date -u +%Y%m%dT%H%M%SZ)-XXXXXX.log") ||
    return 1
  positive_route_attempt_name=${positive_route_attempt_path##*/}
  positive_route_attempt_id=${positive_route_attempt_name%.log}
  if ! "$python_bin" "$helper" retain-attempt-log \
    --input "$positive_route_raw_log" \
    --output "$positive_route_attempt_path" \
    --attempt-id "$positive_route_attempt_id" \
    --terminal-reason "$positive_route_terminal_reason" \
    --source-exit-code "$positive_route_child_exit" >/dev/null; then
    rm -f -- "$positive_route_attempt_path"
    return 1
  fi
  echo "PRELIVE_SHADOW_POSITIVE_ROUTE_ATTEMPT: id=$positive_route_attempt_id reason=$positive_route_terminal_reason path=$positive_route_attempt_path"
  [ "$positive_route_result" -eq 0 ]
}

collect_sample() {
  runtime_healthy || return 1
  assert_runtime_safety || return 1
  capture_protected_identity "$state_dir/identity.current.json" || return 1
  before=$(python_value "$state_dir/identity.before.json" 'value["fingerprint_sha256"]') || return 1
  current=$(python_value "$state_dir/identity.current.json" 'value["fingerprint_sha256"]') || return 1
  [ "$before" = "$current" ] || return 1
  capture_jetstream "$state_dir/jetstream.sample.json" || return 1
  PHOENIX_COMPOSE_FILE="$compose_file" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_RELEASE_ENV="$release_env" \
    "$script_dir/prelive-money-path-report.sh" \
      --format json --window-hours "$window_hours" >"$state_dir/money-path.json" || return 1
  "$python_bin" "$helper" create-sample \
    --money-path "$state_dir/money-path.json" \
    --jetstream "$state_dir/jetstream.sample.json" \
    --output "$state_dir/sample.json" >/dev/null || return 1
  "$python_bin" "$helper" append-sample \
    --sample "$state_dir/sample.json" --samples "$state_dir/samples.ndjson" >/dev/null
  collect_dashboard_snapshot
}

collect_dashboard_snapshot() {
  [ -n "$runtime_sampling_baseline" ] || return 1
  capture_dashboard_services "$state_dir/dashboard-services.json" || return 1
  route_hash_value=${route_hash#sha256:}
  # Expansion must occur inside the PostgreSQL container.
  # shellcheck disable=SC2016
  dashboard_psql_command='psql -X -qAt -v ON_ERROR_STOP=1 -v window_hours="$PHOENIX_DASHBOARD_WINDOW_HOURS" -v route_hash="$PHOENIX_DASHBOARD_ROUTE_HASH" -v evidence_start="$PHOENIX_DASHBOARD_EVIDENCE_START" -U "$POSTGRES_USER" -d "$POSTGRES_DB"'
  compose exec -T \
    -e PHOENIX_DASHBOARD_WINDOW_HOURS="$window_hours" \
    -e PHOENIX_DASHBOARD_ROUTE_HASH="$route_hash_value" \
    -e PHOENIX_DASHBOARD_EVIDENCE_START="$runtime_sampling_baseline" \
    postgres sh -c "$dashboard_psql_command" \
      <"$dashboard_sql" >"$state_dir/dashboard-source.json" || return 1
  postgres_data_dir=${PHOENIX_POSTGRES_DATA_DIR:-$deploy_root/data/postgres}
  [ -d "$postgres_data_dir" ] || return 1
  disk_headroom_bytes=$(df -Pk "$postgres_data_dir" | awk 'NR == 2 { printf "%.0f\n", $4 * 1024 }')
  case "$disk_headroom_bytes" in ''|*[!0-9]*) return 1 ;; esac
  dashboard_candidate=$dashboard_evidence_dir/candidate-dashboard.json
  dashboard_latest=$dashboard_evidence_dir/latest-dashboard.json
  "$python_bin" "$dashboard_collector" build \
    --money-path "$state_dir/money-path.json" \
    --source "$state_dir/dashboard-source.json" \
    --jetstream "$state_dir/jetstream.sample.json" \
    --services "$state_dir/dashboard-services.json" \
    --release-metadata "$state_dir/render.metadata.json" \
    --rendered-compose "$state_dir/compose.rendered.json" \
    --identity-before "$state_dir/identity.before.json" \
    --identity-current "$state_dir/identity.current.json" \
    --preflight "$state_dir/preflight.tsv" \
    --release-manifest "$release_manifest" \
    --release-checksum "$release_state" \
    --history "$state_dir/dashboard-history.ndjson" \
    --output-dir "$dashboard_evidence_dir" \
    --candidate "$dashboard_candidate" \
    --artifact-manifest "$state_dir/artifacts.json" \
    --disk-headroom-bytes "$disk_headroom_bytes" \
    --rpc-calls-per-second "$RPC_UPSTREAM_CALLS_PER_SECOND" >/dev/null || return 1
  "$python_bin" "$dashboard_compiler" \
    --input "$dashboard_candidate" --output "$dashboard_latest" >/dev/null || return 1
  rm -f -- "$dashboard_candidate"
  "$python_bin" "$dashboard_collector" prune \
    --directory "$dashboard_evidence_dir" --snapshot "$dashboard_latest" --retain 3 >/dev/null
}

control_signal() {
  stop_requested=1
}

unexpected_exit() {
  code=$?
  trap - EXIT HUP INT TERM
  if [ "$finalized" -ne 1 ]; then
    if [ "$optional_started" -eq 1 ]; then
      stop_optional_runtime >/dev/null 2>&1 || true
    fi
    echo 'PRELIVE_SHADOW_CONTROL_FAIL: unexpected control-plane termination' >&2
  fi
  [ "$code" -ne 0 ] || code=1
  [ -z "$state_dir" ] || rm -rf -- "$state_dir"
  exit "$code"
}

preflight_main() {
  run_preflight
  capture_protected_identity "$state_dir/identity.after.json" || fail 'post-preflight protected identity capture failed'
  before=$(python_value "$state_dir/identity.before.json" 'value["fingerprint_sha256"]')
  after=$(python_value "$state_dir/identity.after.json" 'value["fingerprint_sha256"]')
  [ "$before" = "$after" ] || fail 'protected identity changed during preflight'
  finalized=1
  trap - EXIT HUP INT TERM
  rm -rf -- "$state_dir"
  echo "PRELIVE_SHADOW_PREFLIGHT_OK: mode=$mode"
}

run_main() {
  run_preflight
  start_optional_runtime || fail 'optional SHADOW services failed to start'
  capture_service_states "$state_dir/states.during.json" || fail 'runtime service-state capture failed'
  run_positive_route_evidence || fail 'positive-route evidence was not observed'
  compose up -d --no-deps rpc-gateway >/dev/null || fail 'RPC Gateway restart after positive evidence failed'
  wait_service_healthy rpc-gateway || fail 'RPC Gateway did not become healthy after positive evidence'
  compose up -d --no-deps phoenix-engine >/dev/null || fail 'Engine restart after positive evidence failed'
  wait_service_healthy phoenix-engine || fail 'Engine did not become healthy after positive evidence'
  runtime_sampling_baseline=$(database_clock_utc) || fail 'runtime sampling clock query failed'
  echo "PRELIVE_SHADOW_SAMPLING_BASELINE=$runtime_sampling_baseline"
  verify_active_release || fail 'active release context validation failed'
  capture_service_states "$state_dir/states.during.json" || fail 'runtime service-state recapture failed'

  started_at=$(utc_now)
  started_epoch=$(date +%s)
  collect_sample || fail 'initial bounded evidence sample failed'
  if [ "$mode" = continuous ]; then
    while [ "$stop_requested" -eq 0 ]; do
      sleep "$sample_interval" || true
      [ "$stop_requested" -ne 0 ] || collect_sample || fail 'continuous bounded evidence sample failed'
    done
    run_status=interrupted
  else
    deadline=$((started_epoch + duration_seconds))
    while [ "$(date +%s)" -lt "$deadline" ]; do
      [ "$stop_requested" -eq 0 ] || fail 'finite SHADOW run was interrupted before its full duration'
      remaining=$((deadline - $(date +%s)))
      sleep_for=$sample_interval
      [ "$remaining" -ge "$sleep_for" ] || sleep_for=$remaining
      sleep "$sleep_for" || true
      [ "$(date +%s)" -lt "$deadline" ] || break
      collect_sample || fail 'bounded evidence sample failed'
    done
    [ "$(date +%s)" -ge "$deadline" ] || fail 'finite SHADOW duration did not fully elapse'
    collect_sample || fail 'final bounded evidence sample failed'
    run_status=completed
  fi
  ended_at=$(utc_now)

  stop_optional_runtime || fail 'optional SHADOW services failed to stop cleanly'
  capture_service_states "$state_dir/states.after.json" || fail 'post-run service-state capture failed'
  capture_protected_identity "$state_dir/identity.after.json" || fail 'post-run protected identity capture failed'
  before=$(python_value "$state_dir/identity.before.json" 'value["fingerprint_sha256"]')
  after=$(python_value "$state_dir/identity.after.json" 'value["fingerprint_sha256"]')
  [ "$before" = "$after" ] || fail 'protected identity changed during SHADOW run'
  [ "$(execution_activity_count)" -eq 0 ] || fail 'execution or submission evidence changed during SHADOW run'
  execution_request_count_after=$(execution_request_count) || fail 'execution request final query failed'
  [ "$execution_request_count_after" -eq 0 ] || fail 'execution request final count is non-zero'

  # The Dashboard collector populates this manifest before final evidence assembly.
  [ -s "$state_dir/artifacts.json" ] || printf '[]\n' >"$state_dir/artifacts.json"
  candidate=$evidence_root/candidate-control-evidence.json
  latest=$evidence_root/latest-control-evidence.json
  "$python_bin" "$helper" assemble-evidence \
    --mode "$mode" --status "$run_status" \
    --started-at "$started_at" --ended-at "$ended_at" \
    --database-clock-baseline "$database_clock_baseline" \
    --execution-request-count-before "$execution_request_count_before" \
    --execution-request-count-after "$execution_request_count_after" \
    --release-metadata "$state_dir/render.metadata.json" \
    --release-manifest "$release_manifest" \
    --release-checksum "$release_state" \
    --preflight "$state_dir/preflight.tsv" \
    --identity-before "$state_dir/identity.before.json" \
    --identity-after "$state_dir/identity.after.json" \
    --states-before "$state_dir/states.before.json" \
    --states-during "$state_dir/states.during.json" \
    --states-after "$state_dir/states.after.json" \
    --samples "$state_dir/samples.ndjson" \
    --artifacts "$state_dir/artifacts.json" \
    --output "$candidate" >/dev/null || fail 'control-plane evidence assembly failed'
  "$python_bin" "$helper" promote-evidence --input "$candidate" --output "$latest" >/dev/null || fail 'control-plane evidence promotion failed'
  rm -f -- "$candidate"
  dashboard_candidate=$dashboard_evidence_dir/candidate-dashboard.json
  dashboard_latest=$dashboard_evidence_dir/latest-dashboard.json
  "$python_bin" "$dashboard_collector" attach-evidence \
    --evidence "$latest" --snapshot "$dashboard_latest" --candidate "$dashboard_candidate" >/dev/null || fail 'Dashboard evidence-bundle attachment failed'
  "$python_bin" "$dashboard_compiler" \
    --input "$dashboard_candidate" --output "$dashboard_latest" >/dev/null || fail 'final Dashboard evidence promotion failed'
  rm -f -- "$dashboard_candidate"
  "$python_bin" "$dashboard_collector" prune \
    --directory "$dashboard_evidence_dir" --snapshot "$dashboard_latest" --retain 3 >/dev/null || fail 'Dashboard evidence retention failed'

  finalized=1
  trap - EXIT HUP INT TERM
  rm -rf -- "$state_dir"
  echo "PRELIVE_SHADOW_CONTROL_OK: mode=$mode status=$run_status"
}

control_main() {
  action=${1:-}
  mode=${2:-}
  [ "$#" -eq 2 ] || { usage >&2; exit 2; }
  case "$action" in
    plan|preflight|run) ;;
    *) usage >&2; exit 2 ;;
  esac
  case "$mode" in
    15m) duration_seconds=900; window_hours=1 ;;
    1h) duration_seconds=3600; window_hours=1 ;;
    6h) duration_seconds=21600; window_hours=6 ;;
    24h) duration_seconds=86400; window_hours=24 ;;
    continuous) duration_seconds=; window_hours=24 ;;
    *) blocked 'mode must be 15m, 1h, 6h, 24h, or continuous' ;;
  esac
  command -v "$python_bin" >/dev/null 2>&1 || blocked "$python_bin is unavailable"
  [ -f "$helper" ] || blocked 'control-plane helper is unavailable'
  [ -f "$dashboard_collector" ] || blocked 'Dashboard live collector is unavailable'
  [ -f "$dashboard_compiler" ] || blocked 'Dashboard snapshot compiler is unavailable'
  [ -f "$dashboard_sql" ] || blocked 'Dashboard source SQL is unavailable'
  if [ "$action" = plan ]; then
    exec "$python_bin" "$helper" plan --mode "$mode"
  fi

  command -v "$docker_bin" >/dev/null 2>&1 || blocked 'docker is unavailable'
  bounded_integer "$sample_interval" 30 300 || blocked 'sample interval must be from 30 through 300 seconds'
  bounded_integer "$readiness_timeout" 30 900 || blocked 'readiness timeout must be from 30 through 900 seconds'
  bounded_integer "$positive_timeout" 30 86400 || blocked 'positive-route timeout must be from 30 through 86400 seconds'
  [ -f "$compose_file" ] || blocked 'production Compose file is missing'
  [ -f "$env_file" ] || blocked 'production environment file is missing'
  [ -f "$release_env" ] || blocked 'release environment file is missing'
  [ -s "$current_release_file" ] || blocked 'current release pointer is missing'
  release_sha=$(tr -d '\r\n' <"$current_release_file")
  case "$release_sha" in
    *[!0-9a-f]*|'') blocked 'current release pointer is invalid' ;;
  esac
  [ "${#release_sha}" -eq 40 ] || blocked 'current release pointer is invalid'
  release_manifest=$deploy_dir/manifests/$release_sha.json
  [ -f "$release_manifest" ] || blocked 'release manifest is missing'
  [ -f "$release_state" ] || blocked 'release checksum state is missing'

  set -a
  # shellcheck disable=SC1090
  . "$env_file"
  # shellcheck disable=SC1090
  . "$release_env"
  set +a

  mkdir -p "$evidence_root" "$dashboard_evidence_dir"
  chmod 0750 "$evidence_root"
  chmod 0755 "$dashboard_evidence_dir"
  state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-shadow-control.XXXXXX") || blocked 'private runtime state could not be created'
  trap unexpected_exit EXIT
  trap control_signal HUP INT TERM

  case "$action" in
    preflight) preflight_main ;;
    run) run_main ;;
  esac
}

if [ "${PHOENIX_SHADOW_CONTROL_LIBRARY_ONLY:-0}" = 1 ]; then
  # The exit fallback is used only when the script is executed instead of sourced.
  # shellcheck disable=SC2317
  return 0 2>/dev/null || exit 0
fi

control_main "$@"
