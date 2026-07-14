#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
workflow=$script_dir/shadow-positive-route-evidence.sh
test_log=$(mktemp)
test_state=$(mktemp -d)
trap 'rm -f "$test_log"; rm -rf "$test_state"' EXIT HUP INT TERM

fail() {
  echo "shadow-positive-route-evidence-tests: $1" >&2
  exit 1
}

SHADOW_POSITIVE_ROUTE_EVIDENCE_LIBRARY_ONLY=1
export SHADOW_POSITIVE_ROUTE_EVIDENCE_LIBRARY_ONLY
# shellcheck disable=SC1090
. "$workflow"

compose() {
  printf '%s\n' "$*" >>"$test_log"
}

positive_evidence_start_runtime || fail 'optional runtime start failed'
positive_evidence_cleanup_runtime || fail 'optional runtime cleanup failed'
grep -Fx 'stop phoenix-engine rpc-gateway' "$test_log" >/dev/null ||
  fail 'cleanup did not stop exactly Engine and RPC Gateway'
grep -Fx 'up -d --no-deps rpc-gateway phoenix-engine' "$test_log" >/dev/null ||
  fail 'start did not use the exact isolated Compose command'
if grep -E '^(up|stop).*(nitro-feed-relay|nats|postgres|migration-runner|feed-ingestor|recorder|shadow-dispatcher|prometheus|dashboard)' "$test_log" >/dev/null; then
  fail 'workflow touched a protected service'
fi

if grep -E 'compose[[:space:]]+(down|pull|build|rm)|--remove-orphans|compose[[:space:]]+up.*migration-runner' "$workflow" >/dev/null; then
  fail 'workflow contains a forbidden Compose mutation'
fi
if grep -Ei '(consumer|stream).*(delete|reset|purge|create|update)|(delete|reset|purge|create|update).*(consumer|stream)' "$workflow" >/dev/null; then
  fail 'workflow contains a durable consumer or stream administrative mutation'
fi
if grep -E '^[[:space:]]*(INSERT|UPDATE|DELETE|TRUNCATE|ALTER|DROP|CREATE)[[:space:]]' "$workflow" >/dev/null; then
  fail 'workflow SQL is not read only'
fi

grep -F 'timeout_seconds=${SHADOW_POSITIVE_ROUTE_TIMEOUT_SECONDS:-900}' "$workflow" >/dev/null ||
  fail 'default timeout is not 15 minutes'
grep -F '[ "${PHOENIX_MODE:-}" = SHADOW ]' "$workflow" >/dev/null ||
  fail 'SHADOW guard is missing'
grep -F '[ "${LIVE_EXECUTION:-}" = false ]' "$workflow" >/dev/null ||
  fail 'LIVE guard is missing'
grep -F 'isolated_canary_route_registry_preflight' "$workflow" >/dev/null ||
  fail 'route-registry preservation preflight is missing'
grep -F 'POSITIVE_ROUTE_EVIDENCE_FOUND' "$workflow" >/dev/null ||
  fail 'positive terminal result is missing'
grep -F 'POSITIVE_ROUTE_EVIDENCE_NOT_FOUND' "$workflow" >/dev/null ||
  fail 'no-evidence terminal result is missing'
grep -F -- '--source-sequence "$positive_source_sequence"' "$workflow" >/dev/null ||
  fail 'production replay is not scoped to the exact source sequence'
grep -F 'execution eligibility changed during SHADOW evidence run' "$workflow" >/dev/null ||
  fail 'SHADOW execution-eligibility guard is missing'
for field in \
  source_transaction_hash matched_route_id candidate_count \
  primary_provider_result independent_provider_result pinned_block_number \
  pinned_block_hash primary_state_hash verification_status \
  verification_agreement classification_id processing_attempt_id \
  source_sequence persisted_timestamp rejection_reason; do
  grep -F "'$field'" "$workflow" >/dev/null || fail "runtime report omits $field"
done

compose() {
  if [ "${1:-}" = ps ]; then
    positive_last=
    for positive_argument in "$@"; do
      positive_last=$positive_argument
    done
    printf 'container-%s\n' "$positive_last"
    return 0
  fi
  printf '%s\n' "$*" >>"$test_log"
}

snapshot_generation=0
docker() {
  [ "${1:-}" = inspect ] || return 1
  positive_last=
  for positive_argument in "$@"; do
    positive_last=$positive_argument
  done
  positive_service=${positive_last#container-}
  if [ "$snapshot_generation" -eq 0 ]; then
    printf 'id-%s|image-%s|digest-%s|created-%s|started-%s|0|true\n' \
      "$positive_service" "$positive_service" "$positive_service" "$positive_service" "$positive_service"
  else
    printf 'id-%s|changed-%s|digest-%s|created-%s|started-%s|1|true\n' \
      "$positive_service" "$positive_service" "$positive_service" "$positive_service" "$positive_service"
  fi
}

isolated_canary_state_dir=$test_state
isolated_canary_snapshot_recorded=0
isolated_canary_record_snapshot || fail 'protected snapshot could not be recorded with fake Docker'
isolated_canary_verify_snapshot || fail 'unchanged protected services failed snapshot verification'
snapshot_generation=1
if isolated_canary_verify_snapshot; then
  fail 'protected service identity/restart changes were not detected'
fi
[ -n "$isolated_canary_changed_service" ] || fail 'changed protected service was not identified'

grep -F 'sh ./scripts/shadow-positive-route-evidence-tests.sh' "$repo_dir/.github/workflows/ci.yml" >/dev/null ||
  fail 'positive-evidence orchestration tests are not wired into CI'
grep -F 'cargo test --manifest-path phoenix-engine/Cargo.toml --test positive_route_evidence' \
  "$repo_dir/.github/workflows/ci.yml" >/dev/null ||
  fail 'positive-route Rust fixture test is not wired into CI'

echo 'shadow-positive-route-evidence-tests: ok'
