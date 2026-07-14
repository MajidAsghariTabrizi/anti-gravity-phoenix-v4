#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$script_dir/shadow-engine-canary-control.sh"

assert_true() {
  "$@" || {
    echo "expected success: $*" >&2
    exit 1
  }
}

assert_false() {
  if "$@"; then
    echo "expected failure: $*" >&2
    exit 1
  fi
}

assert_true canary_input_limit_is_valid 0
assert_true canary_input_limit_is_valid 500
assert_true canary_input_limit_is_valid 1000000
assert_false canary_input_limit_is_valid ''
assert_false canary_input_limit_is_valid -1
assert_false canary_input_limit_is_valid 1.5
assert_false canary_input_limit_is_valid 1000001

assert_false canary_is_enabled 0
assert_true canary_is_enabled 500
assert_false canary_target_reached 1000 1499 500
assert_true canary_target_reached 1000 1500 500
assert_true canary_target_reached 1000 1564 500
assert_true canary_within_overshoot_bound 1000 1564 500 64
assert_false canary_within_overshoot_bound 1000 1565 500 64
assert_false canary_processed_delta 1001 1000 >/dev/null

assert_true canary_positive_timeout_is_valid 1
assert_true canary_positive_timeout_is_valid 3600
assert_false canary_positive_timeout_is_valid 0
assert_false canary_positive_timeout_is_valid 3601

smoke_script="$script_dir/shadow-engine-live-smoke.sh"
grep -F 'stop_engine_for_canary' "$smoke_script" >/dev/null
grep -F 'wait_for_canary_ack_settle' "$smoke_script" >/dev/null
grep -F 'engine_js_value stream_exists' "$smoke_script" >/dev/null
grep -F 'engine_js_value consumer_exists' "$smoke_script" >/dev/null
if grep -E 'consumer[[:space:]]+delete|stream[[:space:]]+delete' "$smoke_script" >/dev/null; then
  echo "canary script must not delete JetStream state" >&2
  exit 1
fi

echo "shadow-engine-canary-control-tests: ok"
