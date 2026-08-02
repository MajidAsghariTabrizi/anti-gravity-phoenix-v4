#!/usr/bin/env sh
set -eu

deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
release_root="${PHOENIX_RELEASE_ROOT:-$deploy_root/releases}"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
overlay_file="$deploy_dir/compose.live-autonomous.yml"
current_file="$deploy_dir/current-release"
current_env="$deploy_dir/current-release.env"
live_release_env="${PHOENIX_CURRENT_LIVE_RELEASE_ENV:-$current_env}"
current_state="$deploy_dir/current-release.json"
current_context="$deploy_dir/current-release-context.json"
release_assets_file="$deploy_dir/release-assets.sha"
candidate_release_assets_file="$deploy_dir/candidate-release-assets.sha"
previous_file="$deploy_dir/previous-release"
runtime_dir="${PHOENIX_DEPLOY_RUNTIME_DIR:-$deploy_dir/.deploy-runtime}"
service_wait_seconds=${PHOENIX_DEPLOY_SERVICE_WAIT_SECONDS:-300}
recorder_drain_seconds=${PHOENIX_RECORDER_DRAIN_SECONDS:-180}
reconciliation_seconds=${PHOENIX_ROLLBACK_RECONCILIATION_SECONDS:-180}

fail() {
  echo "ROLLBACK_FAILED: $1"
  exit 1
}

[ -s "$previous_file" ] || fail "previous release is missing"
release_sha=$(tr -d '\r\n' <"$previous_file")
case "$release_sha" in
  *[!0-9a-f]*|"") fail "previous release SHA is invalid" ;;
esac
[ "${#release_sha}" -eq 40 ] || fail "previous release SHA is invalid"

release_assets_root="$release_root/$release_sha"
manifest="$deploy_dir/manifests/$release_sha.json"
release_env="$deploy_dir/manifests/$release_sha.env"
release_metadata="$deploy_dir/manifests/$release_sha.render.json"
release_state="$deploy_dir/manifests/$release_sha.state.json"
context_installer="${PHOENIX_CONTEXT_INSTALLER:-$deploy_dir/install-production-release-context.sh}"
[ -f "$manifest" ] || fail "release manifest is missing"
[ -f "$compose_file" ] || fail "production compose file is missing"
[ -f "$env_file" ] || fail "production environment file is missing"
[ -d "$release_assets_root" ] || fail "immutable rollback release assets are missing"
[ -f "$release_assets_root/release-assets-manifest.json" ] ||
  fail "rollback release-assets manifest is missing"
[ -f "$context_installer" ] && [ ! -L "$context_installer" ] ||
  fail "release-context installer is missing or unsafe"
case "$service_wait_seconds" in
  ''|*[!0-9]*) fail "service wait seconds must be an integer" ;;
esac
[ "$service_wait_seconds" -ge 30 ] && [ "$service_wait_seconds" -le 900 ] ||
  fail "service wait seconds must be from 30 through 900"
case "$recorder_drain_seconds" in
  ''|*[!0-9]*) fail "Recorder drain seconds must be an integer" ;;
esac
[ "$recorder_drain_seconds" -ge 30 ] && [ "$recorder_drain_seconds" -le 900 ] ||
  fail "Recorder drain seconds must be from 30 through 900"
case "$reconciliation_seconds" in
  ''|*[!0-9]*) fail "reconciliation seconds must be an integer" ;;
esac
[ "$reconciliation_seconds" -ge 30 ] && [ "$reconciliation_seconds" -le 900 ] ||
  fail "reconciliation seconds must be from 30 through 900"

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v cmp >/dev/null 2>&1 || fail "cmp is unavailable"
mkdir -p "$runtime_dir"
chmod 0700 "$runtime_dir"

rollback_from=
if [ -s "$current_file" ]; then
  rollback_from=$(tr -d '\r\n' <"$current_file")
fi
case "$rollback_from" in
  *[!0-9a-f]*|"") fail "active release SHA is invalid" ;;
esac
[ "${#rollback_from}" -eq 40 ] || fail "active release SHA is invalid"
source_release_env="$deploy_dir/manifests/$rollback_from.env"
source_manifest="$deploy_dir/manifests/$rollback_from.json"
[ -s "$source_release_env" ] || fail "active release environment is missing"
[ -f "$source_manifest" ] || fail "active release manifest is missing"
protected_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode SHADOW --field protected_services) ||
  fail "rollback target topology is invalid"
fixed_protected_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode SHADOW --field fixed_protected_services) ||
  fail "rollback target topology is invalid"
start_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode SHADOW --field start_services) ||
  fail "rollback target topology is invalid"
pull_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode SHADOW --field pull_services) ||
  fail "rollback target topology is invalid"
remove_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --source-manifest "$source_manifest" \
  --mode SHADOW --field remove_services) ||
  fail "rollback transition topology is invalid"
absent_services=$(python3 "$deploy_dir/release_components.py" topology \
  --manifest "$manifest" --mode SHADOW --field intentional_absence) ||
  fail "rollback target topology is invalid"

compose_with_release_env() {
  selected_release_env=$1
  shift
  python3 "$deploy_dir/production_compose.py" \
    --mode SHADOW \
    --env-file "$env_file" \
    --release-env "$selected_release_env" \
    --compose-file "$compose_file" \
    -- "$@"
}

remove_source_only_services() {
  for service in $remove_services; do
    compose_with_release_env "$source_release_env" stop -t 30 "$service" \
      >/dev/null 2>&1 || true
    compose_with_release_env "$source_release_env" rm -f "$service" \
      >/dev/null || return 1
  done
}

release_env_value() {
  value_file=$1
  value_name=$2
  awk -F= -v expected="$value_name" '
    $1 == expected {
      print substr($0, length($1) + 2)
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$value_file"
}

wait_service_healthy_with_env() {
  healthy_release_env=$1
  healthy_service=$2
  healthy_deadline=$(( $(date +%s) + service_wait_seconds ))
  while [ "$(date +%s)" -lt "$healthy_deadline" ]; do
    healthy_id=$(
      compose_with_release_env "$healthy_release_env" ps -a -q \
        "$healthy_service" | awk 'NF { print; exit }'
    )
    if [ -n "$healthy_id" ]; then
      healthy_state=$(docker inspect --format \
        '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
        "$healthy_id" 2>/dev/null || true)
      [ "$healthy_state" = 'running|healthy' ] && return 0
    fi
    sleep 3
  done
  return 1
}

wait_recorder_drain() {
  drain_release_env=$1
  drain_helper="$release_root/$rollback_from/scripts/prelive_protected_maintenance.py"
  [ -f "$drain_helper" ] && [ ! -L "$drain_helper" ] || return 1
  drain_snapshot=$(mktemp "$runtime_dir/recorder-drain.XXXXXX") || return 1
  drain_deadline=$(( $(date +%s) + recorder_drain_seconds ))
  while [ "$(date +%s)" -lt "$drain_deadline" ]; do
    if compose_with_release_env "$drain_release_env" exec -T nats \
      wget -q -O - \
        'http://127.0.0.1:8222/jsz?streams=true&consumers=true&config=true' \
        >"$drain_snapshot"
    then
      drain_state=$(
        python3 -B "$drain_helper" consumer-state \
          --jetstream "$drain_snapshot"
      ) || drain_state=
      drain_pending=$(printf '%s\n' "$drain_state" | awk '{ print $1 }')
      drain_ack_pending=$(printf '%s\n' "$drain_state" | awk '{ print $2 }')
      case "$drain_pending:$drain_ack_pending" in
        *[!0-9:]*|:*|*:|*::*) ;;
        *)
          if [ "$drain_pending" -eq 0 ] && [ "$drain_ack_pending" -eq 0 ]; then
            rm -f "$drain_snapshot"
            return 0
          fi
          ;;
      esac
    fi
    sleep 3
  done
  rm -f "$drain_snapshot"
  return 1
}

transition_mutable_protected() {
  source_env=$1
  target_env=$2
  source_feed=$(release_env_value "$source_env" FEED_INGESTOR_IMAGE) ||
    return 1
  target_feed=$(release_env_value "$target_env" FEED_INGESTOR_IMAGE) ||
    return 1
  source_recorder=$(release_env_value "$source_env" RECORDER_IMAGE) ||
    return 1
  target_recorder=$(release_env_value "$target_env" RECORDER_IMAGE) ||
    return 1
  if [ "$source_feed" = "$target_feed" ] &&
    [ "$source_recorder" = "$target_recorder" ]
  then
    compose_with_release_env "$target_env" up -d --no-deps recorder \
      >/dev/null || return 1
    wait_service_healthy_with_env "$target_env" recorder || return 1
    compose_with_release_env "$target_env" up -d --no-deps feed-ingestor \
      >/dev/null || return 1
    wait_service_healthy_with_env "$target_env" feed-ingestor || return 1
    return 0
  fi

  transition_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  compose_with_release_env "$source_env" stop -t 30 feed-ingestor >/dev/null ||
    return 1
  wait_recorder_drain "$source_env" || return 1
  if [ "$source_recorder" != "$target_recorder" ]; then
    compose_with_release_env "$target_env" up -d --no-deps \
      --force-recreate recorder >/dev/null || return 1
    wait_service_healthy_with_env "$target_env" recorder || return 1
  fi
  if [ "$source_feed" = "$target_feed" ]; then
    compose_with_release_env "$target_env" up -d --no-deps feed-ingestor \
      >/dev/null || return 1
  else
    compose_with_release_env "$target_env" up -d --no-deps \
      --force-recreate feed-ingestor >/dev/null || return 1
  fi
  wait_service_healthy_with_env "$target_env" feed-ingestor || return 1
  transition_log=$(mktemp "$runtime_dir/protected-transition.XXXXXX") ||
    return 1
  if ! compose_with_release_env "$target_env" logs --no-color \
    --since "$transition_started_at" nats recorder feed-ingestor \
    >"$transition_log" 2>&1
  then
    rm -f "$transition_log"
    return 1
  fi
  if grep -Eiq \
    'slow consumer|core_nats_message_drop|Core NATS delivery loss|recorder_nats_slow_consumer' \
    "$transition_log"
  then
    rm -f "$transition_log"
    return 1
  fi
  rm -f "$transition_log"
  return 0
}

capture_fixed_ids() {
  fixed_release_env=$1
  fixed_output=$2
  : >"$fixed_output"
  for fixed_service in $fixed_protected_services; do
    fixed_id=$(
      compose_with_release_env "$fixed_release_env" ps -a -q "$fixed_service" |
        awk 'NF { print; exit }'
    )
    [ -n "$fixed_id" ] || return 1
    fixed_state=$(docker inspect --format \
      '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
      "$fixed_id") || return 1
    [ "$fixed_state" = 'running|healthy' ] || return 1
    printf '%s\t%s\n' "$fixed_service" "$fixed_id" >>"$fixed_output"
  done
}

state_dir=
mutable_transition_started=0
restore_mutable_on_failure() {
  rollback_exit_code=$?
  trap - EXIT
  if [ "$rollback_exit_code" -ne 0 ] &&
    [ "$mutable_transition_started" -eq 1 ]
  then
    if ! transition_mutable_protected "$release_env" "$source_release_env"; then
      echo "ROLLBACK_RESTORE_FAILED: mutable protected services require operator attention"
    fi
  fi
  [ -z "$state_dir" ] || rm -rf "$state_dir"
  exit "$rollback_exit_code"
}
trap restore_mutable_on_failure EXIT

current_live_compose() {
  python3 "$deploy_dir/production_compose.py" \
    --mode LIVE \
    --env-file "$env_file" \
    --release-env "$live_release_env" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    -- "$@"
}

if [ -f "$overlay_file" ] && [ -s "$live_release_env" ]; then
  live_executor_id=$(current_live_compose ps -a -q live-executor | awk 'NF { print; exit }')
  current_live_compose config --services |
    grep -F -x autonomous-control >/dev/null ||
    fail "signerless autonomous control service is unavailable"
  current_live_compose run --rm --no-deps \
    -e PHOENIX_AUTONOMOUS_DISARM_ACK=DISARM_AUTONOMOUS_LIVE_42161 \
    -e PHOENIX_AUTONOMOUS_DISARM_REASON=operator_rollback \
    autonomous-control disarm ||
    fail "autonomous controls could not enter DISARMED_FAILURE"
  if [ -n "$live_executor_id" ]; then
    reconciliation_deadline=$(( $(date +%s) + reconciliation_seconds ))
    reconciled=0
    while [ "$(date +%s)" -lt "$reconciliation_deadline" ]; do
      if current_live_compose run --rm --no-deps \
        autonomous-control reconciliation-status >/dev/null 2>&1
      then
        reconciled=1
        break
      fi
      sleep 3
    done
    if [ "$reconciled" -eq 0 ]; then
      echo "ROLLBACK_NOTICE: receipt reconciliation timeout elapsed"
    fi
    current_live_compose stop -t 30 live-executor ||
      fail "autonomous LIVE executor could not be stopped"
  fi
  current_live_compose stop -t 30 economic-monitor >/dev/null 2>&1 || true
  current_live_compose stop -t 30 economic-supervisor >/dev/null 2>&1 || true
  current_live_compose rm -f economic-monitor >/dev/null 2>&1 || true
  current_live_compose rm -f economic-supervisor >/dev/null 2>&1 || true
fi
python3 "$deploy_dir/production_mode.py" shadow --env-file "$env_file" ||
  fail "SHADOW production mode could not be restored"
remove_source_only_services ||
  fail "source-only services could not be removed before rollback promotion"

python3 "$deploy_dir/production_context.py" manifest-env \
  --manifest "$manifest" \
  --expected-sha "$release_sha" \
  --route-registry "$release_assets_root/fixtures/routes/weth_usdc_uniswap_v3.json" \
  --output "$release_env" || fail "release manifest validation failed"
chmod 0640 "$release_env"
fixed_identity_before=$(mktemp "$runtime_dir/fixed-before.XXXXXX") ||
  fail "fixed protected identity evidence could not be created"
fixed_identity_after=$(mktemp "$runtime_dir/fixed-after.XXXXXX") ||
  fail "fixed protected identity evidence could not be created"
capture_fixed_ids "$source_release_env" "$fixed_identity_before" ||
  fail "fixed protected services are not ready before rollback"
mutable_transition_started=1
transition_mutable_protected "$source_release_env" "$release_env" ||
  fail "mutable protected services could not roll back without loss"
capture_fixed_ids "$release_env" "$fixed_identity_after" ||
  fail "fixed protected services are not ready after rollback transition"
cmp "$fixed_identity_before" "$fixed_identity_after" >/dev/null ||
  fail "fixed protected service identity changed during rollback transition"
rm -f "$fixed_identity_before" "$fixed_identity_after"

python3 "$deploy_dir/release_assets.py" verify-tree \
  --root "$release_assets_root" \
  --manifest "$release_assets_root/release-assets-manifest.json" \
  --expected-sha "$release_sha" >/dev/null ||
  fail "immutable rollback release assets failed integrity validation"
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
  /bin/sh "$context_installer" "$release_sha" "$release_assets_root" ||
  fail "rollback release assets could not be restored"
[ -s "$candidate_release_assets_file" ] || fail "rollback release-assets marker is missing"
installed_assets_sha=$(tr -d '\r\n' <"$candidate_release_assets_file")
[ "$installed_assets_sha" = "$release_sha" ] || fail "rollback release-assets marker is invalid"
"$deploy_dir/validate-production-env.sh" "$env_file"

state_dir=$(mktemp -d "$runtime_dir/rollback-$release_sha.XXXXXX") ||
  fail "temporary rollback state could not be created"
trap 'exit 1' HUP INT TERM
rendered_candidate="$state_dir/compose.rendered.json"
metadata_candidate="$state_dir/render.metadata.json"
state_candidate="$state_dir/release-state.json"
pointer_candidate="$state_dir/current-release"
assets_pointer_candidate="$state_dir/release-assets.sha"
context_candidate="$state_dir/release-context.json"
context_rendered="$state_dir/context.compose.json"
context_metadata="$state_dir/context.metadata.json"
protected_before="$state_dir/protected.before.tsv"
protected_after="$state_dir/protected.after.tsv"

"$deploy_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$rendered_candidate" \
  --metadata-output "$metadata_candidate" >/dev/null ||
  fail "canonical production rendering failed"

compose() {
  python3 "$deploy_dir/production_compose.py" \
    --mode SHADOW \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --compose-file "$compose_file" \
    -- "$@"
}

capture_protected_ids() {
  output=$1
  : >"$output"
  for service in $protected_services; do
    id=$(compose ps -a -q "$service" | awk 'NF { print; exit }')
    [ -n "$id" ] || return 1
    state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id") || return 1
    [ "$state" = 'running|healthy' ] || return 1
    printf '%s\t%s\n' "$service" "$id" >>"$output"
  done
}

wait_service_healthy() {
  service=$1
  deadline=$(( $(date +%s) + service_wait_seconds ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    id=$(compose ps -a -q "$service" | awk 'NF { print; exit }')
    if [ -n "$id" ]; then
      state=$(docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$id" 2>/dev/null || true)
      [ "$state" = 'running|healthy' ] && return 0
    fi
    sleep 3
  done
  return 1
}

install_active_file() {
  source_file=$1
  target_file=$2
  target_mode=$3
  active_tmp=$(mktemp "$runtime_dir/active.XXXXXX") || return 1
  if ! cp "$source_file" "$active_tmp" ||
    ! chmod "$target_mode" "$active_tmp" ||
    ! mv "$active_tmp" "$target_file"
  then
    rm -f "$active_tmp"
    return 1
  fi
}

verify_active_release_coherence() {
  expected_sha=$1
  expected_previous_sha=$2
  [ "$(tr -d '\r\n' <"$current_file")" = "$expected_sha" ] || return 1
  [ "$(tr -d '\r\n' <"$release_assets_file")" = "$expected_sha" ] || return 1
  [ "$(tr -d '\r\n' <"$previous_file")" = "$expected_previous_sha" ] ||
    return 1
  grep -F -x "PHOENIX_RELEASE_SHA=$expected_sha" "$current_env" >/dev/null ||
    return 1
  python3 -I -B - "$expected_sha" "$current_state" "$current_context" <<'PY'
import json
import sys
from pathlib import Path

expected = sys.argv[1]
for name in sys.argv[2:]:
    try:
        value = json.loads(Path(name).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise SystemExit(1)
    if not isinstance(value, dict) or value.get("release_sha") != expected:
        raise SystemExit(1)
PY
}

capture_protected_ids "$protected_before" || fail "protected services are not ready before rollback"
# Word splitting is intentional for manifest-derived validated service names.
# shellcheck disable=SC2086
compose pull $pull_services
for service in $start_services; do
  compose up -d --no-deps "$service"
  wait_service_healthy "$service" || fail "optional service did not become healthy during rollback: $service"
done
for service in $absent_services; do
  [ -z "$(compose ps -a -q "$service" | awk 'NF { print; exit }')" ] ||
    fail "a service required to be absent remained after rollback: $service"
done
capture_protected_ids "$protected_after" || fail "protected services are not ready after rollback"
cmp "$protected_before" "$protected_after" >/dev/null || fail "protected service identity changed during rollback"
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$release_env" \
PHOENIX_HEALTH_EXPECTED_MODE=SHADOW \
  "$deploy_dir/production-healthcheck.sh"

printf '%s\n' "$release_sha" >"$pointer_candidate"
printf '%s\n' "$release_sha" >"$assets_pointer_candidate"
python3 "$deploy_dir/production_context.py" write-state \
  --manifest "$manifest" \
  --release-env "$release_env" \
  --render-metadata "$metadata_candidate" \
  --compose-config "$rendered_candidate" \
  --output "$state_candidate"

"$deploy_dir/validate-production-release-context.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --current-release "$pointer_candidate" \
  --release-state "$state_candidate" \
  --inspect-running \
  --rendered-output "$context_rendered" \
  --metadata-output "$context_metadata" \
  --output "$context_candidate" >/dev/null

install_active_file "$metadata_candidate" "$release_metadata" 0640
install_active_file "$state_candidate" "$release_state" 0640
install_active_file "$release_env" "$current_env" 0640
install_active_file "$state_candidate" "$current_state" 0640
install_active_file "$context_candidate" "$current_context" 0640
install_active_file "$assets_pointer_candidate" "$release_assets_file" 0640
install_active_file "$pointer_candidate" "$current_file" 0640
expected_previous_sha=$release_sha
if [ -n "$rollback_from" ] && [ "$rollback_from" != "$release_sha" ]; then
  printf '%s\n' "$rollback_from" >"$previous_file"
  expected_previous_sha=$rollback_from
fi
verify_active_release_coherence "$release_sha" "$expected_previous_sha" ||
  fail "rollback release pointers are incoherent after promotion"
rm -f "$candidate_release_assets_file"

mutable_transition_started=0
trap - EXIT HUP INT TERM
rm -rf "$state_dir"
echo "ROLLBACK_OK: $release_sha"
