#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
script=$repo_root/scripts/rotate-phoenix-executor-live.sh

sh -n "$script"
grep -F 'OLD_EXECUTOR=0x634f62d7cd28d1c4dcf503d901b88d666c2626ad' "$script" >/dev/null
grep -F 'recover-existing)' "$script" >/dev/null
grep -F 'validate|prepare|execute|rollback)' "$script" >/dev/null
grep -F 'LEGACY_ROTATION_SOURCE_SHA=79c364f8aa56b6b6e27cd74cd2167e75a0b13610' "$script" >/dev/null
grep -F 'verify_recovery_authority_snapshot' "$script" >/dev/null
grep -F 'verify_rotation_lineage' "$script" >/dev/null
grep -F 'RELEASE_ASSETS=$DEPLOY_ROOT/release_assets.py' "$script" >/dev/null
grep -F 'PHOENIX_EXECUTOR_ROTATION_RECOVERY_OK' "$script" >/dev/null
grep -F '/run/lock/phoenix-release.lock' "$script" >/dev/null
grep -F '/run/lock/phoenix-economic-activation.lock' "$script" >/dev/null
grep -F "p.sample_3_primary_provider" "$script" >/dev/null
grep -F "p.sample_3_confirmation_provider" "$script" >/dev/null
grep -F "p.sample_count" "$script" >/dev/null
grep -F "p.recovery_status" "$script" >/dev/null

for forbidden in \
  'production_mode.py shadow' \
  'autonomous-control disarm' \
  'arm-revenue' \
  'p.rpc_authority_mode' \
  'p.primary_provider' \
  'p.confirmation_provider' \
  'p.provider_quorum'
do
  if grep -F "$forbidden" "$script" >/dev/null; then
    echo "ROTATION_HOST_CONTRACT_FAILED:$forbidden" >&2
    exit 1
  fi
done

release_line=$(grep -n '/run/lock/phoenix-release.lock' "$script" | head -n 1 | cut -d: -f1)
activation_line=$(grep -n '/run/lock/phoenix-economic-activation.lock' "$script" | head -n 1 | cut -d: -f1)
[ "$release_line" -lt "$activation_line" ]

printf '%s\n' PHOENIX_EXECUTOR_ROTATION_HOST_CONTRACT_OK
