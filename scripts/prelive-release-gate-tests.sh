#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
deploy_workflow=$repo_root/.github/workflows/deploy-shadow.yml
build_workflow=$repo_root/.github/workflows/build-images.yml
deploy_script=$script_dir/deploy-release.sh
rollback_script=$script_dir/rollback-release.sh
installer=$script_dir/install-release-assets.sh
bootstrap=$script_dir/bootstrap-production.sh

fail() {
  echo "prelive-release-gate-tests: $1" >&2
  exit 1
}

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
grep -F 'protected image digest changed for {protected}' "$deploy_workflow" >/dev/null ||
  fail 'protected image changes do not fail before SSH'
if grep -E 'SIGNER_PRIVATE_KEY|WALLET_ADDRESS|EXECUTOR_ADDRESS|eth_send(Raw)?Transaction' "$deploy_workflow" >/dev/null; then
  fail 'deployment workflow contains forbidden LIVE configuration or submission methods'
fi

grep -F 'image: fork-sandbox' "$build_workflow" >/dev/null ||
  fail 'fork-sandbox immutable image publication is missing'
grep -F 'name: release-assets' "$build_workflow" >/dev/null ||
  fail 'release-assets publication job is missing'
grep -F 'needs: [build, assets]' "$build_workflow" >/dev/null ||
  fail 'release manifest is not gated on immutable assets'

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

grep -F 'not member.isfile()' "$installer" >/dev/null ||
  fail 'release installer does not reject non-file archive members'
grep -F 'release_assets.py" verify' "$installer" >/dev/null ||
  fail 'release installer does not run the canonical verifier'
grep -F 'bootstrap-production.sh" "$release_sha"' "$installer" >/dev/null ||
  fail 'release installer does not bind bootstrap to the exact SHA'

validation_line=$(grep -n 'validate-production-env.sh" /etc/phoenix/phoenix.env' "$bootstrap" | tail -n 1 | cut -d: -f1)
marker_line=$(grep -n 'mv "$marker" /opt/phoenix/deploy/release-assets.sha' "$bootstrap" | cut -d: -f1)
[ -n "$validation_line" ] && [ -n "$marker_line" ] && [ "$marker_line" -gt "$validation_line" ] ||
  fail 'release-assets marker is not promoted after production validation'

echo 'prelive-release-gate-tests: ok'
