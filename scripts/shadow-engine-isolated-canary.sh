#!/usr/bin/env sh

isolated_canary_required_services='nitro-feed-relay nats postgres feed-ingestor recorder'
isolated_canary_protected_services='nitro-feed-relay nats postgres feed-ingestor recorder shadow-dispatcher prometheus dashboard'

isolated_canary_container_id() {
  compose ps -a -q "$1" 2>/dev/null | awk 'NF { print; exit }'
}

isolated_canary_container_is_healthy() {
  isolated_service=$1
  isolated_container_id=$(isolated_canary_container_id "$isolated_service")
  [ -n "$isolated_container_id" ] || return 1
  isolated_state=$(docker inspect --format '{{.State.Running}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}missing{{end}}' "$isolated_container_id" 2>/dev/null) || return 1
  [ "$isolated_state" = 'true|healthy' ]
}

isolated_canary_image_is_local_and_pinned() {
  isolated_image=$1
  printf '%s' "$isolated_image" | grep -Eq '^.+@sha256:[0-9a-fA-F]{64}$' || return 1
  docker image inspect "$isolated_image" >/dev/null 2>&1
}

isolated_canary_route_registry_preflight() {
  isolated_rendered_config=$isolated_canary_state_dir/compose.rendered.json
  compose config --format json >"$isolated_rendered_config" 2>/dev/null || return 1
  python3 "$repo_dir/scripts/verify-compose-route-registry.py" \
    --compose-config "$isolated_rendered_config" \
    --expected-env-file "$env_file" \
    --expected-env-file "$release_env" >/dev/null 2>&1
}

isolated_canary_snapshot_value() {
  isolated_service=$1
  isolated_container_id=$(isolated_canary_container_id "$isolated_service")
  if [ -z "$isolated_container_id" ]; then
    printf 'absent\n'
    return 0
  fi
  if ! isolated_snapshot=$(docker inspect --format '{{.Id}}|{{.Config.Image}}|{{.Image}}|{{.Created}}|{{.State.StartedAt}}|{{.RestartCount}}|{{.State.Running}}' "$isolated_container_id" 2>/dev/null); then
    printf 'invalid\n'
    return 0
  fi
  printf '%s\n' "$isolated_snapshot"
}

isolated_canary_record_snapshot() {
  for isolated_service in $isolated_canary_protected_services; do
    isolated_snapshot=$(isolated_canary_snapshot_value "$isolated_service")
    [ "$isolated_snapshot" != 'invalid' ] || return 1
    printf '%s\n' "$isolated_snapshot" >"$isolated_canary_state_dir/snapshot.$isolated_service"
  done
  isolated_canary_snapshot_recorded=1
}

isolated_canary_verify_snapshot() {
  isolated_canary_changed_service=
  for isolated_service in $isolated_canary_protected_services; do
    isolated_expected=$(cat "$isolated_canary_state_dir/snapshot.$isolated_service")
    isolated_actual=$(isolated_canary_snapshot_value "$isolated_service")
    if [ "$isolated_actual" != "$isolated_expected" ]; then
      isolated_canary_changed_service=$isolated_service
      return 1
    fi
  done
}

isolated_canary_stop_watcher() {
  if [ -n "${isolated_canary_watcher_pid:-}" ]; then
    if kill -0 "$isolated_canary_watcher_pid" >/dev/null 2>&1; then
      kill "$isolated_canary_watcher_pid" >/dev/null 2>&1 || true
    fi
    wait "$isolated_canary_watcher_pid" >/dev/null 2>&1 || true
    isolated_canary_watcher_pid=
  fi
}

isolated_canary_cleanup_optional_runtime() {
  compose stop phoenix-engine rpc-gateway >/dev/null 2>&1
}

isolated_canary_remove_state() {
  if [ -n "${isolated_canary_state_dir:-}" ]; then
    rm -rf -- "$isolated_canary_state_dir"
    isolated_canary_state_dir=
  fi
}

isolated_canary_fail() {
  isolated_failure_reason=$1
  isolated_canary_stop_watcher

  if [ "${isolated_canary_snapshot_recorded:-0}" -eq 1 ] && ! isolated_canary_verify_snapshot; then
    isolated_failure_reason="protected service changed: $isolated_canary_changed_service"
  fi
  if ! isolated_canary_cleanup_optional_runtime; then
    isolated_failure_reason='optional runtime cleanup failed'
  fi
  if [ "${isolated_canary_snapshot_recorded:-0}" -eq 1 ] && ! isolated_canary_verify_snapshot; then
    isolated_failure_reason="protected service changed: $isolated_canary_changed_service"
  fi

  isolated_canary_remove_state
  isolated_canary_finalized=1
  trap - EXIT HUP INT TERM
  echo "SHADOW_ENGINE_CANARY_FAIL: $isolated_failure_reason" >&2
  exit 1
}

isolated_canary_exit_guard() {
  isolated_exit_status=$?
  trap - EXIT HUP INT TERM
  if [ "${isolated_canary_finalized:-0}" -ne 1 ]; then
    isolated_failure_reason='unexpected isolated canary failure'
    isolated_canary_stop_watcher
    if [ "${isolated_canary_snapshot_recorded:-0}" -eq 1 ] && ! isolated_canary_verify_snapshot; then
      isolated_failure_reason="protected service changed: $isolated_canary_changed_service"
    fi
    isolated_canary_cleanup_optional_runtime >/dev/null 2>&1 || true
    if [ "${isolated_canary_snapshot_recorded:-0}" -eq 1 ] && ! isolated_canary_verify_snapshot; then
      isolated_failure_reason="protected service changed: $isolated_canary_changed_service"
    fi
    isolated_canary_remove_state
    echo "SHADOW_ENGINE_CANARY_FAIL: $isolated_failure_reason" >&2
  fi
  [ "$isolated_exit_status" -ne 0 ] || isolated_exit_status=1
  exit "$isolated_exit_status"
}

isolated_canary_dependency_preflight() {
  isolated_canary_route_registry_preflight || isolated_canary_fail 'route registry rendering invalid'

  for isolated_service in $isolated_canary_required_services; do
    isolated_canary_container_is_healthy "$isolated_service" || isolated_canary_fail "dependency not ready: $isolated_service"
  done

  service_ready feed-ingestor 9100 || isolated_canary_fail 'dependency not ready: feed-ingestor'
  service_ready recorder 9400 || isolated_canary_fail 'dependency not ready: recorder'
  postgres_ready || isolated_canary_fail 'dependency not ready: postgres'
  [ "$(engine_js_value stream_exists)" = '1' ] || isolated_canary_fail 'dependency not ready: nats'
  [ "$(engine_js_value consumer_exists)" = '1' ] || isolated_canary_fail 'dependency not ready: nats'

  isolated_canary_image_is_local_and_pinned "${RPC_GATEWAY_IMAGE:-}" || isolated_canary_fail 'optional image unavailable: rpc-gateway'
  isolated_canary_image_is_local_and_pinned "${PHOENIX_ENGINE_IMAGE:-}" || isolated_canary_fail 'optional image unavailable: phoenix-engine'
}

isolated_canary_engine_failure_reason() {
  isolated_engine_container=$(isolated_canary_container_id phoenix-engine)
  if [ -z "$isolated_engine_container" ]; then
    printf 'engine-exited\n'
    return 0
  fi
  isolated_engine_state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}missing{{end}}|{{.RestartCount}}' "$isolated_engine_container" 2>/dev/null) || return 1

  isolated_engine_status=${isolated_engine_state%%|*}
  isolated_engine_rest=${isolated_engine_state#*|}
  isolated_engine_health=${isolated_engine_rest%%|*}
  isolated_engine_restarts=${isolated_engine_rest##*|}
  case "$isolated_engine_restarts" in
    ''|*[!0-9]*) isolated_engine_restarts=0 ;;
  esac
  isolated_engine_failure=
  if [ "$isolated_engine_restarts" -gt 0 ] || [ "$isolated_engine_status" = restarting ]; then
    isolated_engine_failure=restart-loop
  fi
  if [ -z "$isolated_engine_failure" ]; then
    case "$isolated_engine_status" in
      exited|dead) isolated_engine_failure=engine-exited ;;
    esac
  fi
  if [ -z "$isolated_engine_failure" ] && [ "$isolated_engine_health" = unhealthy ]; then
    isolated_engine_failure=engine-unhealthy
  fi
  [ -n "$isolated_engine_failure" ] || return 1

  if compose logs --no-color --tail 20 phoenix-engine 2>/dev/null | grep -Fq 'invalid Engine route registry'; then
    printf 'invalid-route-registry\n'
  else
    printf '%s\n' "$isolated_engine_failure"
  fi
}

isolated_canary_watch_target() {
  : >"$isolated_canary_state_dir/watcher-ready"
  isolated_target_deadline=$(( $(date +%s) + evidence_timeout ))
  while [ "$(date +%s)" -lt "$isolated_target_deadline" ]; do
    if [ ! -f "$isolated_canary_state_dir/engine-started" ]; then
      sleep "$canary_poll_interval"
      continue
    fi
    if isolated_engine_failure=$(isolated_canary_engine_failure_reason); then
      printf '%s\n' "$isolated_engine_failure" >"$isolated_canary_state_dir/watcher-result"
      return 1
    fi
    isolated_observed=$(service_metric_count phoenix-engine 9200 phoenix_engine_inputs_processed_total)
    if canary_target_reached 0 "$isolated_observed" "$canary_input_limit"; then
      printf '%s\n' "$isolated_observed" >"$isolated_canary_state_dir/metric-at-stop"
      if compose stop phoenix-engine >/dev/null 2>&1; then
        printf 'reached\n' >"$isolated_canary_state_dir/watcher-result"
        return 0
      fi
      printf 'stop-failed\n' >"$isolated_canary_state_dir/watcher-result"
      return 1
    fi
    sleep "$canary_poll_interval"
  done
  printf 'timeout\n' >"$isolated_canary_state_dir/watcher-result"
  return 1
}

isolated_canary_wait_for_watcher_ready() {
  isolated_ready_deadline=$(( $(date +%s) + 5 ))
  while [ "$(date +%s)" -lt "$isolated_ready_deadline" ]; do
    [ -f "$isolated_canary_state_dir/watcher-ready" ] && return 0
    kill -0 "$isolated_canary_watcher_pid" >/dev/null 2>&1 || return 1
    sleep 0.05
  done
  return 1
}

isolated_canary_start_and_watch() {
  isolated_canary_watch_target &
  isolated_canary_watcher_pid=$!
  isolated_canary_wait_for_watcher_ready || isolated_canary_fail 'input watcher did not arm'

  compose up -d --no-deps --force-recreate rpc-gateway phoenix-engine >/dev/null 2>&1 || isolated_canary_fail 'Engine and RPC Gateway failed to start'
  : >"$isolated_canary_state_dir/engine-started"

  if wait "$isolated_canary_watcher_pid"; then
    isolated_watcher_exit=0
  else
    isolated_watcher_exit=$?
  fi
  isolated_canary_watcher_pid=
  isolated_watcher_result=$(cat "$isolated_canary_state_dir/watcher-result" 2>/dev/null || printf 'failed')
  if [ "$isolated_watcher_exit" -ne 0 ] || [ "$isolated_watcher_result" != 'reached' ]; then
    case "$isolated_watcher_result" in
      invalid-route-registry) isolated_canary_fail 'Engine rejected the route registry' ;;
      restart-loop) isolated_canary_fail 'Engine entered a restart loop' ;;
      engine-exited) isolated_canary_fail 'Engine exited before the canary threshold' ;;
      engine-unhealthy) isolated_canary_fail 'Engine became unhealthy before the canary threshold' ;;
      *) isolated_canary_fail 'Engine did not reach the requested canary input threshold' ;;
    esac
  fi
  isolated_canary_metric_at_stop=$(cat "$isolated_canary_state_dir/metric-at-stop")
}

run_isolated_canary() {
  isolated_canary_state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-shadow-canary.XXXXXX") || blocked 'isolated canary state could not be created'
  isolated_canary_watcher_pid=
  isolated_canary_snapshot_recorded=0
  isolated_canary_finalized=0
  trap isolated_canary_exit_guard EXIT HUP INT TERM

  isolated_canary_dependency_preflight
  isolated_canary_record_snapshot || isolated_canary_fail 'protected service snapshot failed'

  smoke_started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  classifications_before=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
  execution_attempts_before=$(sql_count 'SELECT count(*) FROM execution_attempts')
  executions_before=$(sql_count 'SELECT count(*) FROM executions')
  realized_before=$(sql_count 'SELECT count(*) FROM realized_pnl')

  isolated_canary_start_and_watch
  wait_for_canary_ack_settle || isolated_canary_fail 'Engine ACK-pending did not settle after the canary stop'

  isolated_classifications_after=$(sql_count 'SELECT count(*) FROM shadow_engine_classifications')
  isolated_persisted=$(canary_processed_delta "$classifications_before" "$isolated_classifications_after") || isolated_canary_fail 'classification count moved backwards during canary'
  isolated_accounted=$isolated_canary_metric_at_stop
  if [ "$isolated_persisted" -gt "$isolated_accounted" ]; then
    isolated_accounted=$isolated_persisted
  fi
  canary_target_reached 0 "$isolated_accounted" "$canary_input_limit" || isolated_canary_fail 'persisted canary inputs did not reach the requested threshold'
  canary_within_overshoot_bound 0 "$isolated_accounted" "$canary_input_limit" "$canary_max_overshoot" || isolated_canary_fail 'canary input overshoot exceeded one Engine pull batch'
  [ "$(engine_js_value stream_exists)" = '1' ] || isolated_canary_fail 'Engine stream disappeared during canary stop'
  [ "$(engine_js_value consumer_exists)" = '1' ] || isolated_canary_fail 'Engine durable consumer disappeared during canary stop'

  execution_attempts_after=$(sql_count 'SELECT count(*) FROM execution_attempts')
  executions_after=$(sql_count 'SELECT count(*) FROM executions')
  realized_after=$(sql_count 'SELECT count(*) FROM realized_pnl')
  execution_eligible=$(sql_count "SELECT count(*) FROM shadow_decisions WHERE created_at >= '$smoke_started'::timestamptz AND execution_eligible")
  [ "$execution_attempts_after" -eq "$execution_attempts_before" ] || isolated_canary_fail 'execution attempts changed during bounded SHADOW canary'
  [ "$executions_after" -eq "$executions_before" ] || isolated_canary_fail 'executions changed during bounded SHADOW canary'
  [ "$realized_after" -eq "$realized_before" ] || isolated_canary_fail 'realized PnL rows changed during bounded SHADOW canary'
  [ "$execution_eligible" -eq 0 ] || isolated_canary_fail 'a bounded SHADOW canary decision became execution eligible'

  isolated_canary_verify_snapshot || isolated_canary_fail "protected service changed: $isolated_canary_changed_service"
  isolated_pending=$(engine_js_value pending)
  isolated_canary_cleanup_optional_runtime || isolated_canary_fail 'optional runtime cleanup failed'
  isolated_canary_verify_snapshot || isolated_canary_fail "protected service changed: $isolated_canary_changed_service"

  isolated_canary_remove_state
  isolated_canary_finalized=1
  trap - EXIT HUP INT TERM
  echo 'SHADOW_ENGINE_CANARY_PASS: isolated Engine and RPC Gateway canary completed'
  echo "processed_inputs=$isolated_accounted metric_at_stop=$isolated_canary_metric_at_stop persisted_classifications=$isolated_persisted requested_limit=$canary_input_limit max_overshoot=$canary_max_overshoot pending_replayable=$isolated_pending ack_pending=0"
  exit 0
}
