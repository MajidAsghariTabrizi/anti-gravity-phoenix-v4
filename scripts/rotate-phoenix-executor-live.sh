#!/bin/sh
# shellcheck disable=SC2046,SC2086
set -eu

OLD_EXECUTOR=0x634f62d7cd28d1c4dcf503d901b88d666c2626ad
OLD_HASH=99a485d5a711180b4455028620bf4d5374558f85ef185ba00a51481c7c239c58
DEPLOY_ROOT=/opt/phoenix/deploy
ENV_FILE=/etc/phoenix/phoenix.env
RELEASE_ENV=$DEPLOY_ROOT/current-release.env
PLAN=$DEPLOY_ROOT/config/phoenix-executor-rotation-plan.json
BYTECODE=$DEPLOY_ROOT/contracts/PhoenixExecutor.creation.bin
CONTEXT=$DEPLOY_ROOT/phoenix_executor_rotation_context.py
SELF=$DEPLOY_ROOT/rotate-phoenix-executor-live.sh
STATE_ROOT=/var/lib/phoenix-release/phoenix-executor-rotation
PRODUCTION_COMPOSE=$DEPLOY_ROOT/production_compose.py

fail() { echo "PHOENIX_EXECUTOR_ROTATION_FAILED:$1" >&2; exit 1; }
[ -f "$PLAN" ] && [ -f "$BYTECODE" ] && [ -x "$CONTEXT" ] && [ -f "$PRODUCTION_COMPOSE" ] || fail assets_unavailable

plan_source_sha=$(/usr/bin/python3 -I -B - "$PLAN" <<'PY'
import json,re,sys
v=json.load(open(sys.argv[1],encoding="utf-8")); s=v.get("source_sha","")
if not re.fullmatch(r"[0-9a-f]{40}",s): raise SystemExit(1)
print(s)
PY
)
active_release_sha=$(/usr/bin/python3 -I -B - "$RELEASE_ENV" <<'PY'
import re,sys
values=[]
for raw in open(sys.argv[1],encoding="utf-8"):
    if raw.startswith("PHOENIX_RELEASE_SHA="): values.append(raw.rstrip("\n").split("=",1)[1])
if len(values)!=1 or not re.fullmatch(r"[0-9a-f]{40}",values[0]): raise SystemExit(1)
print(values[0])
PY
)
[ "$active_release_sha" = "$plan_source_sha" ] || fail release_identity_mismatch
STATE=$STATE_ROOT/$plan_source_sha.json

compose() {
  /usr/bin/python3 -I -B "$PRODUCTION_COMPOSE" \
    --mode LIVE --env-file "$ENV_FILE" --release-env "$RELEASE_ENV" \
    --compose-file "$DEPLOY_ROOT/compose.prod.yml" \
    --overlay-file "$DEPLOY_ROOT/compose.live-autonomous.yml" \
    --project-directory "$DEPLOY_ROOT" "$@"
}

selected_env() {
  /usr/bin/python3 -I -B - "$ENV_FILE" "$1" <<'PY'
import sys
name=sys.argv[2]; found=[]
for raw in open(sys.argv[1],encoding="utf-8"):
    line=raw.rstrip("\n")
    if line.startswith(name+"="): found.append(line.split("=",1)[1])
if len(found)!=1 or not found[0]: raise SystemExit(1)
print(found[0])
PY
}

authority_snapshot() {
  compose exec -T postgres sh -eu -c 'psql -X -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -At' <<'SQL'
SELECT jsonb_build_object(
  'economic_phase',e.phase,'size',e.current_size_level,'input',e.current_input_wei::text,
  'global_armed',g.armed,'global_kill',g.kill_switch,'execution_mode',g.execution_mode,
  'generic_armed',c.armed,'generic_kill',c.kill_switch,
  'open_generic_routes',(SELECT count(*) FROM live_canary.autonomous_route_controls WHERE enabled OR NOT kill_switch),
  'aave',(SELECT jsonb_build_object('armed',armed,'kill',kill_switch,'maximum',maximum_input_amount::text) FROM live_canary.revenue_lane_controls WHERE lane='aave_liquidation'),
  'atlas',(SELECT jsonb_build_object('armed',armed,'kill',kill_switch,'maximum',maximum_input_amount::text) FROM live_canary.revenue_lane_controls WHERE lane='atlas_solver'),
  'provider_ready',p.exact_execution_ready,'provider_mode','single_primary',
  'primary',p.sample_3_primary_provider,'confirmation',p.sample_3_confirmation_provider,'quorum',1,
  'provider_recovery_status',p.recovery_status,'provider_sample_count',p.sample_count,
  'active_attempts',(SELECT count(*) FROM live_canary.execution_attempts WHERE status IN ('claimed','nonce_allocated','submission_unknown','pending','timed_out')),
  'unresolved_submissions',(SELECT count(*) FROM live_canary.execution_requests WHERE status IN ('claimed','nonce_allocated','submission_unknown','pending','timed_out')),
  'active_atlas',(SELECT count(*) FROM live_canary.atlas_solver_requests WHERE status IN ('claimed','signed','submitted','submission_unknown')),
  'lock_free',(SELECT active_lane IS NULL AND active_identity IS NULL FROM live_canary.global_revenue_submission_lock WHERE singleton)
)::text
FROM live_canary.economic_control e
CROSS JOIN live_canary.autonomous_global_control g
CROSS JOIN live_canary.control c
CROSS JOIN live_canary.revenue_provider_authority p
WHERE e.singleton AND g.singleton AND c.singleton AND p.singleton;
SQL
}

verify_authority_snapshot() {
  /usr/bin/python3 -I -B - "$1" <<'PY'
import json,sys
v=json.loads(sys.argv[1])
ok=(v.get('economic_phase')=='DISARMED_EVIDENCE' and v.get('size')=='MAX_REVIEWED' and v.get('input')=='10000000000000000'
 and v.get('global_armed') is False and v.get('global_kill') is True and v.get('execution_mode')=='disarmed'
 and v.get('generic_armed') is False and v.get('generic_kill') is True and v.get('open_generic_routes')==0
 and v.get('aave')=={'armed':True,'kill':False,'maximum':'10000000000000000'}
 and v.get('atlas')=={'armed':True,'kill':False,'maximum':'10000000000000000'}
 and v.get('provider_ready') is True and v.get('provider_mode')=='single_primary'
 and v.get('primary')=='production-nownodes-arbitrum' and v.get('confirmation') is None and v.get('quorum')==1
 and v.get('provider_recovery_status') in ('ready','recovered') and v.get('provider_sample_count')==3
 and v.get('active_attempts')==0 and v.get('unresolved_submissions')==0
 and v.get('active_atlas')==0 and v.get('lock_free') is True)
if not ok: raise SystemExit(1)
PY
}

identity_services() {
  compose config --format json | /usr/bin/python3 -I -B -c '
import json,sys
p=json.load(sys.stdin); names=[]
expected={"atlas-observer","economic-supervisor","live-executor","phoenix-engine"}
for name,service in p.get("services",{}).items():
    env=service.get("environment") or {}
    command=service.get("command") or []
    if isinstance(command,str): command=[command]
    command_identity=any(isinstance(value,str) and (value.startswith("--executor-address=") or value.startswith("--executor-code-hash=")) for value in command)
    if command_identity and name not in expected: raise SystemExit(1)
    if name in expected and ("LIVE_EXECUTOR_EXECUTOR_ADDRESS" in env or "EXECUTOR_ADDRESS" in env or command_identity):
        names.append(name)
if set(names)!=expected: raise SystemExit(1)
print(" ".join(sorted(names)))'
}

running_identity_services() {
  result=
  for service in $(identity_services); do
    id=$(compose ps -q "$service")
    [ -z "$id" ] || result="$result $service"
  done
  [ "$result" = " atlas-observer economic-supervisor live-executor phoenix-engine" ] || fail identity_consumers_incomplete
  echo "$result"
}

verify_consumer_identity() {
  expected_address=$1 expected_hash=$2
  for service in $(running_identity_services); do
    id=$(compose ps -q "$service"); [ -n "$id" ] || fail identity_consumer_stopped
    expected_image=$(compose config --format json | /usr/bin/python3 -I -B -c 'import json,sys;print(json.load(sys.stdin)["services"][sys.argv[1]]["image"])' "$service")
    actual_image=$(/usr/bin/docker inspect -f '{{.Config.Image}}' "$id")
    [ "$actual_image" = "$expected_image" ] || fail identity_consumer_image_mismatch
    values=$(/usr/bin/docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$id")
    address=$(printf '%s\n' "$values" | /usr/bin/awk -F= '$1=="LIVE_EXECUTOR_EXECUTOR_ADDRESS"||$1=="EXECUTOR_ADDRESS"{print $2}')
    hash=$(printf '%s\n' "$values" | /usr/bin/awk -F= '$1=="LIVE_EXECUTOR_EXECUTOR_CODE_HASH"{print $2}')
    command=$(/usr/bin/docker inspect -f '{{json .Config.Cmd}}' "$id")
    command_identity=$(printf '%s' "$command" | /usr/bin/python3 -I -B -c '
import json,sys
values=json.load(sys.stdin) or []
addresses=[v.split("=",1)[1] for v in values if isinstance(v,str) and v.startswith("--executor-address=")]
hashes=[v.split("=",1)[1] for v in values if isinstance(v,str) and v.startswith("--executor-code-hash=")]
if len(addresses)>1 or len(hashes)>1: raise SystemExit(1)
print((addresses or [""])[0]+":"+(hashes or [""])[0])') || fail identity_consumer_command_invalid
    command_address=${command_identity%%:*}; command_hash=${command_identity#*:}
    [ -n "$address" ] || address=$command_address
    [ -n "$hash" ] || hash=$command_hash
    [ -n "$address" ] && [ "$address" = "$expected_address" ] || fail mixed_executor_address
    [ -n "$hash" ] && [ "$hash" = "$expected_hash" ] || fail mixed_executor_hash
    observed_mode=$(printf '%s\n' "$values" | /usr/bin/awk -F= '$1=="PHOENIX_MODE"{print $2}')
    observed_live=$(printf '%s\n' "$values" | /usr/bin/awk -F= '$1=="LIVE_EXECUTION"{print $2}')
    observed_autonomous=$(printf '%s\n' "$values" | /usr/bin/awk -F= '$1=="AUTONOMOUS_EXECUTION"{print $2}')
    [ -z "$observed_mode" ] || [ "$observed_mode" = LIVE ] || fail identity_consumer_mode_mismatch
    [ -z "$observed_live" ] || [ "$observed_live" = true ] || fail identity_consumer_live_mismatch
    [ -z "$observed_autonomous" ] || [ "$observed_autonomous" = true ] || fail identity_consumer_autonomous_mismatch
    running=$(/usr/bin/docker inspect -f '{{.State.Running}}:{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}:{{.RestartCount}}:{{.State.OOMKilled}}' "$id")
    case "$running" in true:healthy:0:false|true:none:0:false) ;; *) fail identity_consumer_unhealthy ;; esac
  done
}

verify_support_services() {
  for service in rpc-gateway atlas-observer; do
    id=$(compose ps -q "$service"); [ -n "$id" ] || fail support_service_absent
    state=$(/usr/bin/docker inspect -f '{{.State.Running}}:{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}:{{.RestartCount}}:{{.State.OOMKilled}}' "$id")
    [ "$state" = true:healthy:0:false ] || fail support_service_unhealthy
  done
  gateway_id=$(compose ps -q rpc-gateway)
  gateway_env=$(/usr/bin/docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$gateway_id")
  [ "$(printf '%s\n' "$gateway_env" | /usr/bin/awk -F= '$1=="RPC_AUTHORITY_MODE"{print $2}')" = single_primary ] || fail rpc_authority_mode
  [ "$(printf '%s\n' "$gateway_env" | /usr/bin/awk -F= '$1=="RPC_AUTH_PROVIDER_ID"{print $2}')" = production-nownodes-arbitrum ] || fail rpc_primary_identity
  compose exec -T atlas-observer wget -q -O - http://127.0.0.1:9700/readyz | /usr/bin/python3 -I -B -c '
import json,sys
v=json.load(sys.stdin)
ok=(v.get("exact_execution_readiness") is True and v.get("hunting_health") is True and v.get("atlas_connected") is True
 and v.get("degraded_reason","")=="" and v.get("rpc_authority_mode")=="single_primary"
 and v.get("primary_provider")=="production-nownodes-arbitrum" and v.get("confirmation_provider") is None
 and v.get("quorum")==1 and v.get("provider_recovery_state")=="ready" and v.get("provider_recovery_sample_count")==3
 and v.get("provider_current_class_failure_streak")==0)
if not ok: raise SystemExit(1)' || fail provider_readiness
}

internal=${PHOENIX_EXECUTOR_ROTATION_INTERNAL:-false}
if [ "$internal" = true ]; then
  mode=${1:-}; supplied_plan=${2:-}; supplied_state=${3:-}
  [ "$supplied_plan" = "$PLAN" ] && [ "$supplied_state" = "$STATE" ] || fail internal_identity_mismatch
  /usr/bin/python3 -I -B "$CONTEXT" validate --plan "$PLAN" --provenance "$STATE" >/dev/null
  case "$mode" in
    spl-gate)
      work=$(/usr/bin/mktemp -d "$STATE_ROOT/.spl.XXXXXX")
      trap 'rm -f "$work/exact-request.json" "$work/exact-response.json" "$work/sim-request.json" "$work/sim-response.json"; rmdir "$work" 2>/dev/null || true' EXIT HUP INT TERM
      /usr/bin/python3 -I -B "$CONTEXT" exact-request --plan "$PLAN" --output "$work/exact-request.json" >/dev/null
      compose exec -T rpc-gateway wget -q -O - --header='Content-Type: application/json' --post-file=/dev/stdin http://127.0.0.1:9650/v1/aave/exact <"$work/exact-request.json" >"$work/exact-response.json" || fail spl_exact_rpc
      /usr/bin/python3 -I -B "$CONTEXT" simulation-request --plan "$PLAN" --provenance "$STATE" --exact-response "$work/exact-response.json" --output "$work/sim-request.json" >/dev/null
      compose exec -T rpc-gateway wget -q -O - --header='Content-Type: application/json' --post-file=/dev/stdin http://127.0.0.1:9650/v1/aave/simulate-batch <"$work/sim-request.json" >"$work/sim-response.json" || fail spl_simulation_rpc
      /usr/bin/python3 -I -B "$CONTEXT" verify-simulation --plan "$PLAN" --provenance "$STATE" --request "$work/sim-request.json" --response "$work/sim-response.json" >/dev/null || fail spl_proof
      ;;
    drain)
      compose run --rm --no-deps --user 0:0 \
        -v "$DEPLOY_ROOT/config:/rotation:ro" -v "$STATE_ROOT:/rotation-state" \
        --entrypoint /usr/local/bin/phoenix-executor-rotation live-executor \
        drain-store /rotation/phoenix-executor-rotation-plan.json "/rotation-state/$plan_source_sha.json" >/dev/null || fail drain
      ;;
    cutover)
      before=$(authority_snapshot); verify_authority_snapshot "$before" || fail pre_cutover_authority
      consumers=$(running_identity_services)
      /usr/bin/python3 -I -B "$CONTEXT" record-consumers --plan "$PLAN" --provenance "$STATE" --consumers "$consumers" >/dev/null
      /usr/bin/python3 -I -B "$CONTEXT" mark-cutover-started --plan "$PLAN" --provenance "$STATE" >/dev/null
      compose stop -t 30 $consumers
      "$SELF" drain "$PLAN" "$STATE" || fail post_stop_drain
      /usr/bin/python3 -I -B "$CONTEXT" materialize-new --plan "$PLAN" --provenance "$STATE" --env-file "$ENV_FILE" --output "$ENV_FILE" >/dev/null
      compose up --detach --no-deps --force-recreate --wait --wait-timeout 120 $consumers
      new_address=$(/usr/bin/python3 -I -B -c 'import json,sys;print(json.load(open(sys.argv[1]))["new_executor"])' "$STATE")
      new_hash=$(/usr/bin/python3 -I -B -c 'import json,sys;print(json.load(open(sys.argv[1]))["new_runtime_sha256"])' "$STATE")
      verify_consumer_identity "$new_address" "$new_hash"
      verify_support_services
      after=$(authority_snapshot); [ "$after" = "$before" ] || fail control_snapshot_changed
      /usr/bin/python3 -I -B "$CONTEXT" mark-cutover --plan "$PLAN" --provenance "$STATE" >/dev/null
      ;;
    reconcile)
      snapshot=$(authority_snapshot); verify_authority_snapshot "$snapshot" || fail reconcile_authority
      new_address=$(/usr/bin/python3 -I -B -c 'import json,sys;print(json.load(open(sys.argv[1]))["new_executor"])' "$STATE")
      new_hash=$(/usr/bin/python3 -I -B -c 'import json,sys;print(json.load(open(sys.argv[1]))["new_runtime_sha256"])' "$STATE")
      verify_consumer_identity "$new_address" "$new_hash"
      verify_support_services
      "$SELF" spl-gate "$PLAN" "$STATE" || fail post_cutover_spl
      /usr/bin/python3 -I -B "$CONTEXT" mark-reconciled --plan "$PLAN" --provenance "$STATE" >/dev/null
      ;;
    rollback)
      /usr/bin/python3 -I -B "$CONTEXT" claim-rollback --plan "$PLAN" --provenance "$STATE" >/dev/null || fail rollback_already_used
      before=$(authority_snapshot); verify_authority_snapshot "$before" || fail rollback_authority
      consumers=$(/usr/bin/python3 -I -B -c 'import json,sys;print(" ".join(json.load(open(sys.argv[1]))["identity_consumers"]))' "$STATE")
      [ -n "$consumers" ] || fail rollback_consumers_missing
      compose stop -t 30 $consumers
      /usr/bin/python3 -I -B "$CONTEXT" materialize-old --plan "$PLAN" --provenance "$STATE" --env-file "$ENV_FILE" --output "$ENV_FILE" >/dev/null
      compose up --detach --no-deps --force-recreate --wait --wait-timeout 120 $consumers
      verify_consumer_identity "$OLD_EXECUTOR" "$OLD_HASH"
      verify_support_services
      after=$(authority_snapshot); [ "$after" = "$before" ] || fail rollback_control_snapshot_changed
      /usr/bin/python3 -I -B "$CONTEXT" mark-rollback-complete --plan "$PLAN" --provenance "$STATE" >/dev/null
      ;;
    *) fail internal_mode_invalid ;;
  esac
  exit 0
fi

mode=${1:-}; [ $# -eq 1 ] || fail arguments_invalid
case "$mode" in validate|prepare|execute|rollback) ;; *) fail mode_invalid ;; esac

exec 9>/run/lock/phoenix-release.lock
/usr/bin/flock -w 30 9 || fail release_lock_busy
exec 8>/run/lock/phoenix-economic-activation.lock
/usr/bin/flock -w 30 8 || fail activation_lock_busy

mkdir -p "$STATE_ROOT"; chmod 0700 "$STATE_ROOT"
snapshot=$(authority_snapshot); verify_authority_snapshot "$snapshot" || fail authority_preflight
verify_support_services
[ "$(selected_env PHOENIX_MODE)" = LIVE ] && [ "$(selected_env LIVE_EXECUTION)" = true ] && [ "$(selected_env AUTONOMOUS_EXECUTION)" = true ] || fail mode_preflight
[ "$(selected_env LIVE_EXECUTOR_MAX_INPUT_AMOUNT)" = 10000000000000000 ] || fail maximum_preflight
[ "$(selected_env LIVE_EXECUTOR_MIN_EXPECTED_PROFIT)" = 1000000000000 ] || fail profit_preflight
[ "$(selected_env LIVE_EXECUTOR_MAX_DAILY_LOSS_WEI)" = 600000000000000 ] || fail loss_preflight
if [ "$mode" = prepare ] || [ "$mode" = execute ]; then
  [ "$(selected_env LIVE_EXECUTOR_EXECUTOR_ADDRESS)" = "$OLD_EXECUTOR" ] || fail old_identity_not_active
  [ "$(selected_env LIVE_EXECUTOR_EXECUTOR_CODE_HASH)" = "$OLD_HASH" ] || fail old_identity_hash_not_active
  verify_consumer_identity "$OLD_EXECUTOR" "$OLD_HASH"
fi

image=$(/usr/bin/python3 -I -B - "$RELEASE_ENV" <<'PY'
import sys
v=[]
for raw in open(sys.argv[1],encoding="utf-8"):
    if raw.startswith("LIVE_EXECUTOR_IMAGE="): v.append(raw.rstrip("\n").split("=",1)[1])
if len(v)!=1 or not v[0]: raise SystemExit(1)
print(v[0])
PY
)
case "$image" in *@sha256:[0-9a-f][0-9a-f]*) ;; *) fail image_not_immutable ;; esac
binary_dir=$(/usr/bin/mktemp -d "$STATE_ROOT/.binary.XXXXXX")
container=phoenix-executor-rotation-copy-$$
cleanup() { /usr/bin/docker rm -f "$container" >/dev/null 2>&1 || true; rm -f "$binary_dir/phoenix-executor-rotation"; rmdir "$binary_dir" 2>/dev/null || true; }
trap cleanup EXIT HUP INT TERM
/usr/bin/docker create --name "$container" "$image" >/dev/null
/usr/bin/docker cp "$container:/usr/local/bin/phoenix-executor-rotation" "$binary_dir/phoenix-executor-rotation"
/usr/bin/docker rm "$container" >/dev/null
chmod 0700 "$binary_dir/phoenix-executor-rotation"

if [ "$mode" = validate ]; then
  "$binary_dir/phoenix-executor-rotation" validate "$PLAN" "$BYTECODE"
  exit 0
fi

export LIVE_EXECUTOR_RPC_URL=https://arbitrum.nownodes.io/
export LIVE_EXECUTOR_RPC_ALLOWLIST=https://arbitrum.nownodes.io/
export LIVE_EXECUTOR_RPC_HEADER_NAME=api-key
export LIVE_EXECUTOR_RPC_HEADER_FILE=/etc/phoenix/secrets/phoenix-rpc-provider-slot-1-api-key
export PHOENIX_EXECUTOR_ROTATION_SIGNER_FILE
PHOENIX_EXECUTOR_ROTATION_SIGNER_FILE=$(selected_env LIVE_EXECUTOR_SIGNER_FILE)
export PHOENIX_EXECUTOR_ROTATION_INTERNAL=true
"$binary_dir/phoenix-executor-rotation" "$mode" "$PLAN" "$BYTECODE" "$STATE" "$SELF"
