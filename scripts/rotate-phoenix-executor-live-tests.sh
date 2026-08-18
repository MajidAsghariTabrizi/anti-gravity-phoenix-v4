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

grep -F 'http://127.0.0.1:9300/v1/aave/exact' "$script" >/dev/null
grep -F 'http://127.0.0.1:9300/v1/aave/simulate-batch' "$script" >/dev/null

if grep -F 'http://127.0.0.1:9650/v1/aave/' "$script" >/dev/null; then
  echo "ROTATION_HOST_CONTRACT_FAILED:stale_rpc_gateway_9650" >&2
  exit 1
fi

grep -F 'rpc_gateway_post_json()' "$script" >/dev/null
grep -F 'mktemp /tmp/phoenix-spl-body.XXXXXX' "$script" >/dev/null
grep -F 'cat >"$body"' "$script" >/dev/null
grep -F -- '--post-file="$body"' "$script" >/dev/null
grep -F 'rpc_gateway_post_json http://127.0.0.1:9300/v1/aave/exact' "$script" >/dev/null
grep -F 'rpc_gateway_post_json http://127.0.0.1:9300/v1/aave/simulate-batch' "$script" >/dev/null

if grep -F -- '--post-file=/dev/stdin' "$script" >/dev/null; then
  echo "ROTATION_HOST_CONTRACT_FAILED:nonseekable_stdin_post_file" >&2
  exit 1
fi

# Behavioral regression: feed non-seekable stdin into the real helper,
# stage it as a regular temp file, and prove cleanup after wget returns.
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

cat >"$test_root/wget" <<'SH'
#!/bin/sh
set -eu
post_file=
for arg in "$@"; do
  case "$arg" in
    --post-file=*) post_file=${arg#--post-file=} ;;
  esac
done
[ -n "$post_file" ]
[ -f "$post_file" ]
[ "$(cat "$post_file")" = '{"probe":"seekable"}' ]
printf '%s' "$post_file" >"$PHOENIX_TEST_POST_PATH"
printf '%s\n' '{"ok":true}'
SH

chmod 0700 "$test_root/wget"
export PHOENIX_TEST_POST_PATH="$test_root/post-path"

helper=$(sed -n '/^rpc_gateway_post_json() {/,/^}/p' "$script")
[ -n "$helper" ]

(
  PATH="$test_root:$PATH"
  export PATH PHOENIX_TEST_POST_PATH

  compose() {
    [ "$1" = exec ]
    [ "$2" = -T ]
    [ "$3" = rpc-gateway ]
    shift 3
    "$@"
  }

  eval "$helper"

  result=$(printf '%s' '{"probe":"seekable"}' | \
    rpc_gateway_post_json http://127.0.0.1:9300/v1/aave/exact)

  [ "$result" = '{"ok":true}' ]
)

post_path=$(cat "$test_root/post-path")
[ -n "$post_path" ]
[ ! -e "$post_path" ]


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
