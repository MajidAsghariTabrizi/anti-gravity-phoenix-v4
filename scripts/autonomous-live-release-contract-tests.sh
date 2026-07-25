#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)

gateway_installer=$repo_root/scripts/install-autonomous-live-deploy-gateway.sh
context_installer=$repo_root/scripts/install-production-release-context.sh
build_workflow=$repo_root/.github/workflows/build-images.yml
ci_workflow=$repo_root/.github/workflows/ci.yml
dockerfile=$repo_root/deploy/rust.Dockerfile
runtime_verifier=$repo_root/scripts/verify-image-runtime.sh

fail() {
  echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_FAILED: $1" >&2
  exit 1
}

for required in \
  "$gateway_installer" \
  "$context_installer" \
  "$build_workflow" \
  "$ci_workflow" \
  "$dockerfile" \
  "$runtime_verifier"
do
  [ -f "$required" ] && [ ! -L "$required" ] ||
    fail "required_file_missing:$required"
done

for specification in \
  'release_assets.py:0600' \
  'release_components.py:0600' \
  'release_provenance.py:0600' \
  'install-release-assets.sh:0700' \
  'install-production-release-context.sh:0700' \
  'production-healthcheck.sh:0700' \
  'production_mode.py:0700' \
  'prelive-protected-maintenance.sh:0700' \
  'prelive_protected_maintenance.py:0600' \
  'prelive-protected-maintenance-launch.sh:0700' \
  'prelive-protected-maintenance-unit.sh:0700' \
  'rollback-release.sh:0700'
do
  grep -F "'$specification'" "$gateway_installer" >/dev/null ||
    fail "gateway_runtime_missing:$specification"
done

for safety_script in \
  prelive-protected-maintenance.sh \
  prelive_protected_maintenance.py \
  prelive-protected-maintenance-launch.sh \
  prelive-protected-maintenance-unit.sh
do
  grep -F "$safety_script" "$context_installer" >/dev/null ||
    fail "context_installer_runtime_missing:$safety_script"
done

PYTHONDONTWRITEBYTECODE=1 \
  python3 -I -B "$repo_root/scripts/release_provenance.py" --help >/dev/null ||
  fail release_provenance_isolated_import_failed

grep -F 'libgcc-s1' "$dockerfile" >/dev/null ||
  fail rust_runtime_dependency_missing

grep -F 'autonomous-live-control __image_runtime_probe__' "$dockerfile" >/dev/null ||
  fail dockerfile_runtime_probe_missing

grep -F 'Verify immutable image runtime contract' "$build_workflow" >/dev/null ||
  fail immutable_build_runtime_gate_missing

grep -F 'scripts/verify-image-runtime.sh' "$build_workflow" >/dev/null ||
  fail immutable_build_runtime_verifier_missing

grep -F 'scripts/verify-image-runtime.sh' "$ci_workflow" >/dev/null ||
  fail source_ci_runtime_verifier_missing

echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_OK"
