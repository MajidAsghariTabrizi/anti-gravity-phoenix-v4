#!/usr/bin/env sh
# shellcheck disable=SC2016
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
workflow=$repo_root/.github/workflows/deploy-prelive-protected-maintenance.yml
normal_workflow=$repo_root/.github/workflows/deploy-shadow.yml
runtime=$script_dir/prelive-protected-maintenance.sh
helper=$script_dir/prelive_protected_maintenance.py

fail() {
  echo "prelive-protected-maintenance-tests: $1" >&2
  exit 1
}

for required in "$workflow" "$normal_workflow" "$runtime" "$helper"; do
  [ -s "$required" ] || fail "required file is missing: $required"
done

grep -F 'workflow_dispatch:' "$workflow" >/dev/null ||
  fail 'maintenance workflow is not manual dispatch'
if grep -F 'workflow_run:' "$workflow" >/dev/null; then
  fail 'maintenance workflow can run automatically'
fi
for input in release_sha build_run_id rollback_sha rollback_build_run_id acknowledgement; do
  grep -F "      $input:" "$workflow" >/dev/null ||
    fail "maintenance input is missing: $input"
done
grep -F '[ "$ACKNOWLEDGEMENT" = DEPLOY_PRELIVE_PROTECTED_MAINTENANCE ]' \
  "$workflow" >/dev/null || fail 'exact acknowledgement guard is missing'
grep -F 'environment: production-shadow' "$workflow" >/dev/null ||
  fail 'protected GitHub Environment is missing'
grep -F 'REVIEWED_RELEASE_SHA: ddbc3e6820f565b41d0d0a2323f67a4187b3dd45' \
  "$workflow" >/dev/null || fail 'reviewed v3 source is not pinned'
grep -F 'REVIEWED_BUILD_RUN_ID: "29519008274"' "$workflow" >/dev/null ||
  fail 'reviewed v3 build run is not pinned'
grep -F 'REVIEWED_ROLLBACK_SHA: e84aa5eb69a749da1a01e308422d76a34f0409e8' \
  "$workflow" >/dev/null || fail 'reviewed v2 rollback source is not pinned'
grep -F 'REVIEWED_ROLLBACK_BUILD_RUN_ID: "29487710804"' "$workflow" >/dev/null ||
  fail 'reviewed v2 rollback build is not pinned'

grep -F 'phoenix-release-assets-${{ inputs.rollback_sha }}' "$workflow" >/dev/null ||
  fail 'complete rollback assets are not downloaded'
verify_count=$(grep -c 'python3 scripts/release_assets.py verify' "$workflow")
[ "$verify_count" -eq 2 ] || fail 'both release asset bundles are not verified'
grep -F 'validate-render-pair' "$workflow" >/dev/null ||
  fail 'release and rollback render contracts are not compared'
grep -F 'image-refs.tsv' "$workflow" >/dev/null ||
  fail 'digest-pinned image inspection is missing'
grep -F 'org.opencontainers.image.revision' "$workflow" >/dev/null ||
  fail 'OCI revision verification is missing'

oci_line=$(grep -n 'Verify every image digest and OCI revision before SSH' "$workflow" | cut -d: -f1)
ssh_install_line=$(grep -n 'Install SSH material' "$workflow" | cut -d: -f1)
ssh_run_line=$(grep -n 'Run bounded protected maintenance' "$workflow" | cut -d: -f1)
[ -n "$oci_line" ] && [ -n "$ssh_install_line" ] && [ -n "$ssh_run_line" ] ||
  fail 'workflow gate ordering markers are missing'
[ "$oci_line" -lt "$ssh_install_line" ] && [ "$ssh_install_line" -lt "$ssh_run_line" ] ||
  fail 'an SSH step occurs before immutable preflight completes'

grep -F 'protected image digest changed for {protected}' "$normal_workflow" >/dev/null ||
  fail 'normal deployment no longer rejects changed protected images'
grep -F 'a separately authorized maintenance gate is required' "$normal_workflow" >/dev/null ||
  fail 'normal deployment protected-image refusal changed'

stop_line=$(grep -n 'stop feed-ingestor' "$runtime" | tail -n 1 | cut -d: -f1)
recorder_line=$(grep -n 'up -d --no-deps recorder' "$runtime" | tail -n 1 | cut -d: -f1)
feed_line=$(grep -n 'up -d --no-deps feed-ingestor' "$runtime" | tail -n 1 | cut -d: -f1)
[ -n "$stop_line" ] && [ -n "$recorder_line" ] && [ -n "$feed_line" ] ||
  fail 'bounded maintenance sequence is incomplete'
[ "$stop_line" -lt "$recorder_line" ] && [ "$recorder_line" -lt "$feed_line" ] ||
  fail 'maintenance order is not Feed quiesce, Recorder, then Feed'
grep -F 'wait_recorder_drain "$rollback_env"' "$runtime" >/dev/null ||
  fail 'durable Recorder drain is not required'
grep -F 'assert_optional_stopped "$release_env"' "$runtime" >/dev/null ||
  fail 'optional services are not held stopped during maintenance'
grep -F 'assert_optional_stopped "$current_env"' "$runtime" >/dev/null ||
  fail 'optional services are not verified stopped after promotion'

if grep -E 'up -d --no-deps (nitro-feed-relay|nats|postgres)' "$runtime" >/dev/null; then
  fail 'a fixed protected service can be recreated'
fi
if grep -E 'stop (nitro-feed-relay|nats|postgres)' "$runtime" >/dev/null; then
  fail 'a fixed protected service can be stopped'
fi
if grep -E '^[[:space:]]*compose_with[^#]*up -d[[:space:]]*(>|$)' "$runtime" >/dev/null; then
  fail 'broad Compose startup exists in maintenance'
fi

grep -F 'trap unexpected_exit EXIT' "$runtime" >/dev/null ||
  fail 'automatic rollback trap is missing'
grep -F 'rollback_protected' "$runtime" >/dev/null ||
  fail 'protected rollback implementation is missing'
grep -F 'PROTECTED_MAINTENANCE_ROLLBACK_OK' "$runtime" >/dev/null ||
  fail 'rollback completion gate is missing'
grep -F 'sha256sum -c "$(basename -- "$release_checksums")"' "$runtime" >/dev/null ||
  fail 'remote checksum verification is missing before mutation'
grep -F 'cmp "$state_dir/remote-plan.json" "$plan_file"' "$runtime" >/dev/null ||
  fail 'remote allowlist plan is not reconciled with pre-SSH evidence'
grep -F 'compose_with "$rollback_env" up -d --no-deps recorder' "$runtime" >/dev/null ||
  fail 'rollback does not restore exact v2 Recorder'
grep -F 'compose_with "$rollback_env" up -d --no-deps feed-ingestor' "$runtime" >/dev/null ||
  fail 'rollback does not restore exact v2 Feed Ingestor'

grep -F '"execution_attempts"' "$helper" >/dev/null ||
  fail 'execution-attempt validation is missing'
grep -F '"realized_pnl"' "$helper" >/dev/null ||
  fail 'realized-PnL validation is missing'
grep -F '"execution_eligible"' "$helper" >/dev/null ||
  fail 'execution eligibility validation is missing'
grep -F '"execution_request_created"' "$helper" >/dev/null ||
  fail 'execution-request validation is missing'
grep -F 'optional_services_stopped' "$helper" >/dev/null ||
  fail 'optional-service state is absent from evidence'

for candidate in "$workflow" "$runtime"; do
  if grep -Eiq \
    'docker[[:space:]]+compose[[:space:]]+down|docker[[:space:]]+system[[:space:]]+prune|docker[[:space:]]+volume[[:space:]]+(prune|rm)|nats[^[:space:]]*[[:space:]]+(delete|purge|reset)|jetstream[^[:space:]]*[[:space:]]+(delete|purge|reset)|DROP[[:space:]]+DATABASE|TRUNCATE[[:space:]]|migration[[:space:]_-]*rollback' \
    "$candidate"
  then
    fail "forbidden destructive command exists: $candidate"
  fi
  if grep -F 'continue-on-error' "$candidate" >/dev/null; then
    fail "continue-on-error exists: $candidate"
  fi
  if grep -E 'LIVE_EXECUTION=(true|1)|SIGNER_PRIVATE_KEY=.+|WALLET_ADDRESS=0x|EXECUTOR_ADDRESS=0x|eth_send(Raw)?Transaction' \
    "$candidate" >/dev/null
  then
    fail "LIVE or transaction-submission configuration exists: $candidate"
  fi
done

echo 'prelive-protected-maintenance-tests: ok'
