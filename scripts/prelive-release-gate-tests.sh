#!/usr/bin/env sh
# shellcheck disable=SC2016
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
deploy_workflow=$repo_root/.github/workflows/deploy-shadow.yml
build_workflow=$repo_root/.github/workflows/build-images.yml
release_validator=$script_dir/release_provenance.py
deploy_script=$script_dir/deploy-release.sh
rollback_script=$script_dir/rollback-release.sh
installer=$script_dir/install-release-assets.sh
bootstrap=$script_dir/bootstrap-production.sh
context_installer=$script_dir/install-production-release-context.sh
provisioner=$script_dir/provision-production-host.sh
gateway_helper=$script_dir/phoenix_shadow_deploy.py
component_registry=$repo_root/release-components.json
component_helper=$script_dir/release_components.py

fail() {
  echo "prelive-release-gate-tests: $1" >&2
  exit 1
}

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi
command -v python3 >/dev/null 2>&1 || fail 'Python is unavailable'

grep -F 'workflow_dispatch:' "$deploy_workflow" >/dev/null || fail 'deployment is not manual dispatch'
if grep -F 'workflow_run:' "$deploy_workflow" >/dev/null; then
  fail 'deployment still auto-runs after an image build'
fi
for input in release_sha build_run_id rollback_sha rollback_build_run_id acknowledgement; do
  grep -F "      $input:" "$deploy_workflow" >/dev/null || fail "deployment input is missing: $input"
done
grep -F '[ "$ACKNOWLEDGEMENT" = DEPLOY_PRELIVE_SHADOW ]' "$deploy_workflow" >/dev/null ||
  fail 'deployment acknowledgement guard is missing'
grep -F 'environment: production-shadow' "$deploy_workflow" >/dev/null ||
  fail 'deployment environment gate is missing'
grep -F 'phoenix-release-assets-${{ inputs.release_sha }}' "$deploy_workflow" >/dev/null ||
  fail 'release assets are not downloaded by exact SHA'
grep -F 'phoenix-release-manifest-${{ inputs.rollback_sha }}' "$deploy_workflow" >/dev/null ||
  fail 'rollback manifest is not downloaded by exact SHA'
grep -F 'validate-deploy-pair' "$deploy_workflow" >/dev/null ||
  fail 'protected image inheritance is not validated before SSH'
grep -F 'validate-source-ci-api' "$deploy_workflow" >/dev/null ||
  fail 'candidate exact-main CI evidence is not revalidated before SSH'
grep -F 'protected image changed for {name}; maintenance is required' "$release_validator" >/dev/null ||
  fail 'legacy protected image changes are not rejected'
grep -F 'validate_host(identity)' "$gateway_helper" >/dev/null ||
  fail 'gateway does not verify active rollback host identity before installation'
grep -F 'validate_immutable_tree(deploy_root / "releases"' "$gateway_helper" >/dev/null ||
  fail 'gateway does not verify the active immutable rollback tree'
grep -F 'ssh "${ssh_opts[@]}" "$remote" sudo -n "$gateway" "$@"' "$deploy_workflow" >/dev/null ||
  fail 'deployment does not use the constrained root gateway'
grep -F 'gateway_remote start "${gateway_args[@]}"' "$deploy_workflow" >/dev/null ||
  fail 'deployment does not start the bounded gateway operation'
if grep -E 'sudo /bin/sh|scripts/.*\.(sh|py).*\$remote_stage' "$deploy_workflow" >/dev/null; then
  fail 'deployment stages or invokes mutable privileged code'
fi
if grep -E 'SIGNER_PRIVATE_KEY|WALLET_ADDRESS|EXECUTOR_ADDRESS|eth_send(Raw)?Transaction' "$deploy_workflow" >/dev/null; then
  fail 'deployment workflow contains forbidden LIVE configuration or submission methods'
fi

[ -f "$component_registry" ] || fail 'canonical release component registry is missing'
python3 "$component_helper" validate >/dev/null ||
  fail 'canonical release component registry is invalid'
grep -F 'fromJSON(needs.preflight.outputs.build_matrix)' "$build_workflow" >/dev/null ||
  fail 'image publication matrix is not loaded from the canonical registry'
grep -F 'scripts/release_components.py build-matrix' "$build_workflow" >/dev/null ||
  fail 'canonical release component matrix generation is missing'
grep -F 'name: release-assets' "$build_workflow" >/dev/null ||
  fail 'release-assets publication job is missing'
grep -F 'workflow_dispatch:' "$build_workflow" >/dev/null ||
  fail 'image publication is not manual dispatch'
if grep -E '^  (push|pull_request):' "$build_workflow" >/dev/null; then
  fail 'image publication still has an automatic trigger'
fi
for input in release_sha release_intent confirm_publish ci_run_id ci_run_attempt protected_base_sha protected_base_build_run_id; do
  grep -F "      $input:" "$build_workflow" >/dev/null ||
    fail "image publication input is missing: $input"
done
grep -F 'validate-source-ci' "$build_workflow" >/dev/null ||
  fail 'exact-main source CI validation is missing'
grep -F 'source-ci-evidence-${{ inputs.release_sha }}' "$build_workflow" >/dev/null ||
  fail 'validated source CI evidence is not retained'
grep -F '"$(git rev-parse origin/main)" != "$RELEASE_SHA"' "$build_workflow" >/dev/null ||
  fail 'release SHA is not required to equal the exact main tip'
grep -F 'needs: [preflight, build, assets]' "$build_workflow" >/dev/null ||
  fail 'release manifest is not gated on immutable assets'
grep -F 'inherit-protected' "$build_workflow" >/dev/null ||
  fail 'protected image inheritance materialization is missing'
grep -F 'matrix.protected == false' "$build_workflow" >/dev/null ||
  fail 'protected images are not excluded from inherited builds'

for release_script in "$deploy_script" "$rollback_script"; do
  grep -F "protected_services='nitro-feed-relay feed-ingestor nats postgres recorder'" "$release_script" >/dev/null ||
    fail "protected service set is incomplete: $release_script"
  grep -F "optional_services='prometheus rpc-gateway shadow-dispatcher phoenix-engine dashboard'" "$release_script" >/dev/null ||
    fail "optional service set is incomplete: $release_script"
  grep -F 'compose up -d --no-deps "$service"' "$release_script" >/dev/null ||
    fail "optional services are not started individually: $release_script"
  grep -F 'wait_service_healthy "$service"' "$release_script" >/dev/null ||
    fail "optional services are not health-gated in order: $release_script"
  grep -F 'cmp "$protected_before" "$protected_after"' "$release_script" >/dev/null ||
    fail "protected identity comparison is missing: $release_script"
  if grep -E '^[[:space:]]*compose up -d[[:space:]]*$' "$release_script" >/dev/null; then
    fail "broad Compose startup remains: $release_script"
  fi
done
grep -F 'compose run --rm --no-deps migration-runner' "$deploy_script" >/dev/null ||
  fail 'migration runner can still start dependencies'
grep -F 'installed release assets do not match release SHA' "$deploy_script" >/dev/null ||
  fail 'deploy script does not require exact release assets'
grep -F 'immutable rollback release assets failed integrity validation' "$rollback_script" >/dev/null ||
  fail 'rollback does not validate its immutable release-assets tree'
grep -F 'rollback release assets could not be restored' "$rollback_script" >/dev/null ||
  fail 'rollback does not restore its exact release assets'
if grep -F 'bootstrap-production.sh' "$rollback_script" >/dev/null; then
  fail 'rollback still invokes general host bootstrap'
fi
grep -F '/bin/sh "$context_installer" "$release_sha" "$release_assets_root"' \
  "$rollback_script" >/dev/null ||
  fail 'rollback does not use the scoped release-context installer'

grep -F 'not member.isfile()' "$installer" >/dev/null ||
  fail 'release installer does not reject non-file archive members'
grep -F 'release_assets.py" verify' "$installer" >/dev/null ||
  fail 'release installer does not run the canonical verifier'
grep -F 'export PYTHONDONTWRITEBYTECODE=1' "$installer" >/dev/null ||
  fail 'release installer does not disable Python bytecode writes for child helpers'
if [ "$(grep -c '/usr/bin/python3 -I -B' "$installer")" -lt 4 ]; then
  fail 'release installer Python checks are not isolated and no-bytecode'
fi
if [ "$(grep -c 'release_assets.py" verify-tree' "$installer")" -lt 3 ]; then
  fail 'release installer does not verify candidate and final immutable trees'
fi
grep -F '/bin/sh "$context_installer" "$release_sha" "$final_root"' "$installer" \
  >/dev/null ||
  fail 'release installer does not bind scoped context installation to the exact tree'
if grep -F 'bootstrap-production.sh' "$installer" >/dev/null; then
  fail 'release installer still invokes general host bootstrap'
fi
grep -F 'provision-production-host.sh' "$bootstrap" >/dev/null ||
  fail 'bootstrap does not separate first-host provisioning'
grep -F 'install-production-release-context.sh' "$bootstrap" >/dev/null ||
  fail 'bootstrap does not separate release-context installation'
grep -F 'validate_existing_postgres' "$provisioner" >/dev/null ||
  fail 'first-host provisioning lacks fail-closed PostgreSQL ownership validation'

validation_line=$(grep -n 'validate-production-env.sh" "$env_file"' "$context_installer" | tail -n 1 | cut -d: -f1)
marker_line=$(grep -n 'mv "$marker" "$deploy_dir/release-assets.sha"' "$context_installer" | cut -d: -f1)
[ -n "$validation_line" ] && [ -n "$marker_line" ] && [ "$marker_line" -gt "$validation_line" ] ||
  fail 'release-assets marker is not promoted after production validation'

echo 'prelive-release-gate-tests: ok'
