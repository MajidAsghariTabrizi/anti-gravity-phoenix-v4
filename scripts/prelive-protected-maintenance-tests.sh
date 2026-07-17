#!/usr/bin/env sh
# shellcheck disable=SC2016
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
workflow=$repo_root/.github/workflows/deploy-prelive-protected-maintenance.yml
normal_workflow=$repo_root/.github/workflows/deploy-shadow.yml
runtime=$script_dir/prelive-protected-maintenance.sh
helper=$script_dir/prelive_protected_maintenance.py
context_installer=$script_dir/install-production-release-context.sh
launcher=$script_dir/prelive-protected-maintenance-launch.sh
unit_runner=$script_dir/prelive-protected-maintenance-unit.sh

fail() {
  echo "prelive-protected-maintenance-tests: $1" >&2
  exit 1
}

for required in \
  "$workflow" \
  "$normal_workflow" \
  "$runtime" \
  "$helper" \
  "$context_installer" \
  "$launcher" \
  "$unit_runner"
do
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
for path_binding in \
  'release_root="$PWD/release-tree/phoenix-release-${RELEASE_SHA}"' \
  'rollback_root="$PWD/rollback-tree/phoenix-release-${ROLLBACK_SHA}"' \
  'release_manifest="$PWD/release/release-manifest.json"' \
  'rollback_manifest="$PWD/rollback/release-manifest.json"' \
  'maintenance_plan="$PWD/protected-maintenance-plan.json"' \
  'validation_env="$PWD/maintenance-validation.env"' \
  'release_validation_env="$PWD/release-validation.env"' \
  'rollback_validation_env="$PWD/rollback-validation.env"' \
  'release_render="$PWD/release-render.json"' \
  'rollback_render="$PWD/rollback-render.json"' \
  'release_render_metadata="$PWD/release-render-metadata.json"' \
  'rollback_render_metadata="$PWD/rollback-render-metadata.json"'
do
  grep -F "$path_binding" "$workflow" >/dev/null ||
    fail "canonical workflow path is missing: $path_binding"
done
grep -F 'cat >"$validation_env"' "$workflow" >/dev/null ||
  fail 'validation environment is not written through its canonical path'
validator_env_count=$(
  grep -F -c 'scripts/validate-production-env.sh" "$validation_env"' "$workflow"
)
[ "$validator_env_count" -eq 2 ] ||
  fail 'release and rollback validators do not share the canonical environment'
for canonical_use in \
  '--manifest "$release_manifest"' \
  '--manifest "$rollback_manifest"' \
  '--output "$release_validation_env"' \
  '--output "$rollback_validation_env"' \
  '--env-file "$validation_env"' \
  '--release-env "$release_validation_env"' \
  '--release-env "$rollback_validation_env"' \
  '--output "$release_render"' \
  '--output "$rollback_render"' \
  '--metadata-output "$release_render_metadata"' \
  '--metadata-output "$rollback_render_metadata"' \
  '--plan "$maintenance_plan"' \
  '--release-metadata "$release_render_metadata"' \
  '--rollback-metadata "$rollback_render_metadata"'
do
  grep -F -- "$canonical_use" "$workflow" >/dev/null ||
    fail "canonical render-contract use is missing: $canonical_use"
done
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

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-maintenance-path-test.XXXXXX")
trap 'rm -rf "$tmp_dir"' 0 HUP INT TERM

bare_workspace=$tmp_dir/bare-workspace
immutable_scripts=$tmp_dir/immutable-release/scripts
mkdir -p "$bare_workspace" "$immutable_scripts"
bare_env=$bare_workspace/maintenance-validation.env
cat >"$bare_env" <<'ENV'
PHOENIX_MODE=SHADOW
LIVE_EXECUTION=false
SIGNER_PRIVATE_KEY=
WALLET_ADDRESS=
EXECUTOR_ADDRESS=
PUBLIC_TRANSACTION_SUBMISSION=
PRIVATE_RELAY_SUBMISSION=
TRANSACTION_BROADCAST_URL=
ENV
cat >"$immutable_scripts/validate-production-env.sh" <<'SH'
#!/usr/bin/env sh
set -eu
env_file=$1
. "$env_file"
[ "$PHOENIX_MODE" = SHADOW ]
[ "$LIVE_EXECUTION" = false ]
[ -z "$SIGNER_PRIVATE_KEY" ]
[ -z "$WALLET_ADDRESS" ]
[ -z "$EXECUTOR_ADDRESS" ]
[ -z "$PUBLIC_TRANSACTION_SUBMISSION" ]
[ -z "$PRIVATE_RELAY_SUBMISSION" ]
[ -z "$TRANSACTION_BROADCAST_URL" ]
SH
chmod +x "$immutable_scripts/validate-production-env.sh"

bare_failure_log=$tmp_dir/bare-failure.log
if (
  cd "$bare_workspace"
  PATH=/usr/bin:/bin sh "$immutable_scripts/validate-production-env.sh" \
    maintenance-validation.env
) >"$bare_failure_log" 2>&1
then
  fail 'bare validation environment unexpectedly bypasses POSIX PATH lookup'
fi
grep -E 'not found|No such file|cannot open' "$bare_failure_log" >/dev/null ||
  fail 'bare validation environment did not reproduce the POSIX source failure'
(
  cd "$tmp_dir"
  PATH=/usr/bin:/bin sh "$immutable_scripts/validate-production-env.sh" "$bare_env"
) || fail 'canonical absolute validation environment cannot be sourced'

command -v bash >/dev/null 2>&1 ||
  fail 'bash is required for the workflow render-path fixture'
step_script=$tmp_dir/render-contract-step.sh
awk '
  /^      - name: Validate exact release and rollback render contracts$/ {
    in_step = 1
    next
  }
  in_step && /^        run: \|$/ {
    in_run = 1
    next
  }
  in_run && /^      - name:/ {
    exit
  }
  in_run {
    sub(/^          /, "")
    print
  }
' "$workflow" >"$step_script"
[ -s "$step_script" ] || fail 'render-contract workflow step could not be extracted'

fixture=$tmp_dir/workflow-fixture
fixture_bin=$fixture/bin
fixture_scripts=$fixture/scripts
release_assets=$fixture/release-assets
rollback_assets=$fixture/rollback-assets
mkdir -p \
  "$fixture_bin" \
  "$fixture_scripts" \
  "$release_assets" \
  "$rollback_assets" \
  "$fixture/release" \
  "$fixture/rollback"

release_sha=ddbc3e6820f565b41d0d0a2323f67a4187b3dd45
rollback_sha=e84aa5eb69a749da1a01e308422d76a34f0409e8
printf '{}\n' >"$fixture/release/release-manifest.json"
printf '{}\n' >"$fixture/rollback/release-manifest.json"
printf '{}\n' >"$fixture/protected-maintenance-plan.json"
: >"$fixture_scripts/prelive_protected_maintenance.py"

validator_template=$tmp_dir/fixture-validate-production-env.sh
cat >"$validator_template" <<'SH'
#!/usr/bin/env sh
set -eu
env_file=$1
case "$env_file" in
  /*) ;;
  *) exit 81 ;;
esac
[ "$env_file" = "$EXPECTED_VALIDATION_ENV" ] || exit 82
. "$env_file"
[ "$PHOENIX_ENV" = production ] || exit 83
[ "$PHOENIX_MODE" = SHADOW ] || exit 84
[ "$LIVE_EXECUTION" = false ] || exit 85
[ -z "$SIGNER_PRIVATE_KEY" ] || exit 86
[ -z "$WALLET_ADDRESS" ] || exit 87
[ -z "$EXECUTOR_ADDRESS" ] || exit 88
[ -z "$PUBLIC_TRANSACTION_SUBMISSION" ] || exit 89
[ -z "$PRIVATE_RELAY_SUBMISSION" ] || exit 90
[ -z "$TRANSACTION_BROADCAST_URL" ] || exit 91
[ -n "$ENGINE_ROUTE_REGISTRY_JSON" ] || exit 92
case "$0" in
  */release-tree/*) role=release ;;
  */rollback-tree/*) role=rollback ;;
  *) exit 93 ;;
esac
printf '%s\t%s\n' "$role" "$env_file" >>"$VALIDATOR_TRACE"
SH

render_template=$tmp_dir/fixture-render-production-compose.sh
cat >"$render_template" <<'SH'
#!/usr/bin/env sh
set -eu
env_file=
release_env=
release_manifest=
compose_file=
output=
metadata=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --env-file) env_file=$2; shift 2 ;;
    --release-env) release_env=$2; shift 2 ;;
    --release-manifest) release_manifest=$2; shift 2 ;;
    --compose-file) compose_file=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    --metadata-output) metadata=$2; shift 2 ;;
    *) shift ;;
  esac
done
for generated_path in \
  "$env_file" "$release_env" "$release_manifest" "$compose_file" "$output" "$metadata"
do
  case "$generated_path" in
    /*) ;;
    *) exit 101 ;;
  esac
done
[ "$env_file" = "$EXPECTED_VALIDATION_ENV" ] || exit 102
case "$0" in
  */release-tree/*)
    role=release
    [ "$release_env" = "$EXPECTED_RELEASE_VALIDATION_ENV" ] || exit 103
    ;;
  */rollback-tree/*)
    role=rollback
    [ "$release_env" = "$EXPECTED_ROLLBACK_VALIDATION_ENV" ] || exit 104
    ;;
  *) exit 105 ;;
esac
printf '%s\t%s\t%s\t%s\n' \
  "$role" "$env_file" "$release_env" "$metadata" >>"$RENDER_TRACE"
printf '{}\n' >"$output"
printf '{}\n' >"$metadata"
SH

fake_python=$fixture_bin/python3
cat >"$fake_python" <<'SH'
#!/usr/bin/env sh
set -eu
is_absolute() {
  case "$1" in
    /*) return 0 ;;
    *) return 1 ;;
  esac
}
case "${1-}" in
  -c)
    printf '{}\n'
    ;;
  */production_context.py)
    shift
    [ "${1-}" = manifest-env ] || exit 111
    shift
    manifest=
    output=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --manifest) manifest=$2; shift 2 ;;
        --output) output=$2; shift 2 ;;
        *) shift ;;
      esac
    done
    is_absolute "$manifest" || exit 112
    is_absolute "$output" || exit 113
    printf 'FIXTURE_RELEASE_ENV=true\n' >"$output"
    printf '%s\n' "$output" >>"$CONTEXT_TRACE"
    ;;
  */prelive_protected_maintenance.py)
    [ "${2-}" = validate-render-pair ] || exit 114
    shift 2
    plan=
    release_metadata=
    rollback_metadata=
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --plan) plan=$2; shift 2 ;;
        --release-metadata) release_metadata=$2; shift 2 ;;
        --rollback-metadata) rollback_metadata=$2; shift 2 ;;
        *) shift ;;
      esac
    done
    is_absolute "$plan" || exit 115
    is_absolute "$release_metadata" || exit 116
    is_absolute "$rollback_metadata" || exit 117
    printf '%s\t%s\t%s\n' \
      "$plan" "$release_metadata" "$rollback_metadata" >>"$VALIDATE_TRACE"
    [ "${FAIL_RENDER_PAIR:-0}" -eq 0 ] || exit 118
    ;;
  *)
    exit 119
    ;;
esac
SH
chmod +x "$fake_python"

make_fixture_release() {
  role=$1
  sha=$2
  archive_dir=$3
  source_dir=$tmp_dir/$role-source
  release_root=$source_dir/phoenix-release-$sha
  mkdir -p "$release_root/scripts"
  cp "$validator_template" "$release_root/scripts/validate-production-env.sh"
  cp "$render_template" "$release_root/scripts/render-production-compose.sh"
  : >"$release_root/scripts/production_context.py"
  : >"$release_root/compose.prod.yml"
  chmod +x \
    "$release_root/scripts/validate-production-env.sh" \
    "$release_root/scripts/render-production-compose.sh"
  tar -czf "$archive_dir/phoenix-release-assets-$sha.tar.gz" \
    -C "$source_dir" "phoenix-release-$sha"
}
make_fixture_release release "$release_sha" "$release_assets"
make_fixture_release rollback "$rollback_sha" "$rollback_assets"

fixture=$(CDPATH='' cd -- "$fixture" && pwd)
expected_validation_env=$fixture/maintenance-validation.env
expected_release_validation_env=$fixture/release-validation.env
expected_rollback_validation_env=$fixture/rollback-validation.env
validator_trace=$tmp_dir/validator.trace
context_trace=$tmp_dir/context.trace
render_trace=$tmp_dir/render.trace
validate_trace=$tmp_dir/validate.trace
: >"$validator_trace"
: >"$context_trace"
: >"$render_trace"
: >"$validate_trace"
export EXPECTED_VALIDATION_ENV="$expected_validation_env"
export EXPECTED_RELEASE_VALIDATION_ENV="$expected_release_validation_env"
export EXPECTED_ROLLBACK_VALIDATION_ENV="$expected_rollback_validation_env"
export VALIDATOR_TRACE="$validator_trace"
export CONTEXT_TRACE="$context_trace"
export RENDER_TRACE="$render_trace"
export VALIDATE_TRACE="$validate_trace"

(
  cd "$fixture"
  PATH="$fixture_bin:$PATH" \
    RELEASE_SHA=$release_sha \
    ROLLBACK_SHA=$rollback_sha \
    bash "$step_script"
) >"$tmp_dir/render-success.log" 2>&1 ||
  fail 'exact workflow render-contract fixture did not complete'

[ "$(wc -l <"$validator_trace" | tr -d ' ')" -eq 2 ] ||
  fail 'release and rollback validators were not both invoked'
awk -F '\t' -v expected="$expected_validation_env" \
  '$1 == "release" && $2 == expected { found = 1 } END { exit !found }' \
  "$validator_trace" ||
  fail 'release validator did not receive the canonical environment'
awk -F '\t' -v expected="$expected_validation_env" \
  '$1 == "rollback" && $2 == expected { found = 1 } END { exit !found }' \
  "$validator_trace" ||
  fail 'rollback validator did not receive the canonical environment'
grep -Fx "$expected_release_validation_env" "$context_trace" >/dev/null ||
  fail 'release manifest-env output is not canonical'
grep -Fx "$expected_rollback_validation_env" "$context_trace" >/dev/null ||
  fail 'rollback manifest-env output is not canonical'
[ "$(wc -l <"$render_trace" | tr -d ' ')" -eq 2 ] ||
  fail 'release and rollback renders were not both invoked'
[ -s "$validate_trace" ] ||
  fail 'workflow fixture did not reach validate-render-pair'

failure_validate_trace=$tmp_dir/failure-validate.trace
ssh_gate_marker=$tmp_dir/ssh-gate-executed
: >"$failure_validate_trace"
if (
  cd "$fixture"
  PATH="$fixture_bin:$PATH" \
    RELEASE_SHA=$release_sha \
    ROLLBACK_SHA=$rollback_sha \
    VALIDATE_TRACE=$failure_validate_trace \
    FAIL_RENDER_PAIR=1 \
    bash "$step_script"
) >"$tmp_dir/render-failure.log" 2>&1
then
  : >"$ssh_gate_marker"
fi
[ -s "$failure_validate_trace" ] ||
  fail 'render-failure fixture did not reach validate-render-pair'
[ ! -e "$ssh_gate_marker" ] ||
  fail 'SSH or remote maintenance could advance after render validation failed'

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
if grep -F 'bootstrap-production.sh' "$runtime" >/dev/null; then
  fail 'maintenance rollback still calls general host bootstrap'
fi
grep -F '/bin/sh "$context_installer" "$rollback_sha"' "$runtime" >/dev/null ||
  fail 'rollback does not use the scoped release-context installer'
grep -F 'capture_protected_storage' "$runtime" >/dev/null ||
  fail 'protected storage ownership and volume metadata are not captured'
grep -F -- '--storage-metadata "$snapshot_dir/storage.metadata"' "$runtime" \
  >/dev/null ||
  fail 'protected storage metadata is absent from snapshots'
grep -F 'protected_storage_identity_sha256' "$helper" >/dev/null ||
  fail 'protected storage identity is absent from the evidence contract'
grep -F 'protected_storage_metadata_changed' "$helper" >/dev/null ||
  fail 'protected storage drift does not fail closed'
promoted_validation_line=$(
  grep -n -- '--stage promoted' "$runtime" | tail -n 1 | cut -d: -f1
)
release_pointer_line=$(
  grep -n 'write_active_value "$release_sha" "$current_release"' "$runtime" |
    tail -n 1 |
    cut -d: -f1
)
[ -n "$promoted_validation_line" ] && [ -n "$release_pointer_line" ] &&
  [ "$promoted_validation_line" -lt "$release_pointer_line" ] ||
  fail 'candidate release can be marked current before continuity validation'
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

for candidate in \
  "$workflow" \
  "$runtime" \
  "$context_installer" \
  "$launcher" \
  "$unit_runner"
do
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
