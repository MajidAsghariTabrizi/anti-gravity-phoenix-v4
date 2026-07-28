#!/usr/bin/env sh
set -eu
umask 077

release_sha=${1:-}
readiness_file=${2:-}
authorization_file=${3:-}
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=$deploy_dir/current-release.env
compose_file=$deploy_dir/compose.prod.yml
overlay_file=$deploy_dir/compose.live-autonomous.yml

fail() {
  echo "ECONOMIC_CANARY_ACTIVATION_FAILED: $1" >&2
  exit 1
}

case "$release_sha" in
  *[!0-9a-f]*|"") fail "release SHA is invalid" ;;
esac
[ "${#release_sha}" -eq 40 ] || fail "release SHA is invalid"
[ "$(tr -d '\r\n' <"$deploy_dir/current-release")" = "$release_sha" ] ||
  fail "release is not the active exact release"

for contract_file in "$readiness_file" "$authorization_file"; do
  [ -f "$contract_file" ] && [ ! -L "$contract_file" ] ||
    fail "authorization contract file is unsafe"
  [ "$(stat -c '%u:%g:%a:%h' "$contract_file")" = 0:0:600:1 ] ||
    fail "authorization contract metadata is unsafe"
  [ "$(stat -c '%s' "$contract_file")" -le 262144 ] ||
    fail "authorization contract file is oversized"
done

read_ids=$(
  python3 -I -B - "$readiness_file" "$authorization_file" <<'PY'
import json
import sys
import uuid
from pathlib import Path

readiness = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
authorization = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))
print(uuid.UUID(readiness["readiness_id"]))
print(uuid.UUID(authorization["authorization_id"]))
PY
) || fail "authorization contract IDs are invalid"
readiness_id=$(printf '%s\n' "$read_ids" | sed -n '1p')
authorization_id=$(printf '%s\n' "$read_ids" | sed -n '2p')

compose() {
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose \
      --env-file "$env_file" \
      --env-file "$release_env" \
      -f "$compose_file" \
      -f "$overlay_file" \
      --profile live-autonomous "$@"
}

owner_unpause_attempted=0
compensate() {
  code=$?
  trap - EXIT
  compose stop -t 30 live-executor >/dev/null 2>&1 || true
  if [ "$owner_unpause_attempted" -eq 1 ]; then
    compose run --rm --no-deps \
      -e PHOENIX_RELEASE_SHA="$release_sha" \
      -e PHOENIX_EXECUTOR_OWNER_PAUSE_ACK=PAUSE_EXECUTOR_AFTER_FAILED_DEPLOY_42161 \
      --entrypoint /usr/local/bin/autonomous-live-control \
      live-executor owner-pause >/dev/null 2>&1 || true
  fi
  compose run --rm --no-deps \
    -e PHOENIX_AUTONOMOUS_DISARM_ACK=DISARM_AUTONOMOUS_LIVE_42161 \
    -e PHOENIX_AUTONOMOUS_DISARM_REASON=canary_activation_failure \
    autonomous-control disarm >/dev/null 2>&1 || true
  exit "$code"
}
trap compensate EXIT

[ -z "$(compose ps -q live-executor | awk 'NF { print; exit }')" ] ||
  fail "live-executor must be stopped before authorization"

compose run --rm --no-deps \
  -v "$readiness_file:/run/phoenix/canary-readiness.json:ro" \
  -e PHOENIX_CANARY_READINESS_FILE=/run/phoenix/canary-readiness.json \
  -e PHOENIX_CANARY_READINESS_ACK=CREATE_HASH_BOUND_CANARY_READINESS_42161 \
  autonomous-control create-readiness

compose run --rm --no-deps \
  -v "$authorization_file:/run/phoenix/automation-authorization.json:ro" \
  -e PHOENIX_AUTOMATION_AUTHORIZATION_FILE=/run/phoenix/automation-authorization.json \
  -e PHOENIX_AUTOMATION_AUTHORIZATION_ACK=INSTALL_BOUNDED_AUTOMATION_AUTHORIZATION_42161 \
  autonomous-control install-authorization

compose run --rm --no-deps autonomous-control preflight
compose run --rm --no-deps \
  -e PHOENIX_CANARY_READINESS_ID="$readiness_id" \
  -e PHOENIX_AUTOMATION_AUTHORIZATION_ID="$authorization_id" \
  -e PHOENIX_AUTONOMOUS_ACTIVATION_ACK=ACTIVATE_READY_MIN_CANARY_42161 \
  autonomous-control activate-ready-canary

owner_unpause_attempted=1
compose run --rm --no-deps \
  -e PHOENIX_RELEASE_SHA="$release_sha" \
  -e PHOENIX_EXECUTOR_OWNER_UNPAUSE_ACK=UNPAUSE_CONFIGURED_EXECUTOR_42161 \
  --entrypoint /usr/local/bin/autonomous-live-control \
  live-executor owner-unpause
owner_unpause_attempted=0

compose up -d --no-deps live-executor
compose run --rm --no-deps autonomous-control status

trap - EXIT
echo "ECONOMIC_CANARY_ACTIVATION_OK: release=$release_sha level=MIN input_wei=100000000000000"
