#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
helper=$script_dir/required-service-absence.sh
tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-required-absence.XXXXXX")
trap 'rm -rf -- "$tmp_root"' EXIT HUP INT TERM

fail() {
  echo "REQUIRED_SERVICE_ABSENCE_TEST_FAILED: $1" >&2
  exit 1
}

if ! command -v python3 >/dev/null 2>&1; then
  python_fallback=$(command -v python 2>/dev/null || true)
  [ -n "$python_fallback" ] || fail 'python3 is unavailable'
  cat >"$tmp_root/python3" <<SH
#!/usr/bin/env sh
exec "$python_fallback" "\$@"
SH
  chmod 0755 "$tmp_root/python3"
  PATH=$tmp_root:$PATH
  export PATH
fi

test_compose() {
  [ "$#" -eq 3 ] && [ "$1" = config ] &&
    [ "$2" = --format ] && [ "$3" = json ] || return 1
  printf '%s\n' '{"name":"app"}'
}

fake_docker=$tmp_root/docker
cat >"$fake_docker" <<'SH'
#!/usr/bin/env sh
set -eu
[ "$#" -eq 8 ] || exit 91
[ "$1" = ps ] && [ "$2" = --all ] && [ "$3" = --quiet ] &&
  [ "$4" = --no-trunc ] && [ "$5" = --filter ] &&
  [ "$6" = label=com.docker.compose.project=app ] &&
  [ "$7" = --filter ] &&
  [ "$8" = label=com.docker.compose.service=live-executor ] || exit 92
count=0
[ ! -f "$PHOENIX_TEST_COUNT_FILE" ] || count=$(cat "$PHOENIX_TEST_COUNT_FILE")
count=$((count + 1))
printf '%s\n' "$count" >"$PHOENIX_TEST_COUNT_FILE"
case "$PHOENIX_TEST_SEQUENCE" in
  absent) exit 0 ;;
  removing)
    [ "$count" -ge 3 ] && exit 0
    if [ "$count" -eq 1 ]; then
      printf '%064d\n' 0
    else
      printf '%064d\n' 1
    fi
    ;;
  running) printf '%064d\n' 2 ;;
  stopped) printf '%064d\n' 3 ;;
  malformed) printf '%s\n' not-a-container-id ;;
  error) exit 93 ;;
  *) exit 94 ;;
esac
SH
chmod 0755 "$fake_docker"

# shellcheck source=required-service-absence.sh
. "$helper"
export PHOENIX_DOCKER_BIN=$fake_docker
export PHOENIX_REQUIRED_ABSENCE_POLL_SECONDS=1

run_case() {
  case_name=$1
  wait_seconds=$2
  expect=$3
  count_file=$tmp_root/count.$case_name
  rm -f -- "$count_file"
  export PHOENIX_TEST_SEQUENCE=$case_name
  export PHOENIX_TEST_COUNT_FILE=$count_file
  export PHOENIX_REQUIRED_ABSENCE_WAIT_SECONDS=$wait_seconds
  if phoenix_wait_required_service_absent test_compose live-executor; then
    result=success
  else
    result=failure
  fi
  [ "$result" = "$expect" ] || fail "$case_name returned $result"
}

run_case absent 1 success
[ "$(cat "$tmp_root/count.absent")" -eq 1 ] ||
  fail 'already-absent lookup was not immediate'
run_case removing 5 success
[ "$(cat "$tmp_root/count.removing")" -eq 3 ] ||
  fail 'removing container was not polled through fresh identities'
run_case running 1 failure
run_case stopped 1 failure
run_case malformed 1 failure
run_case error 1 failure

export PHOENIX_TEST_SEQUENCE=absent
export PHOENIX_TEST_COUNT_FILE=$tmp_root/count.invalid
phoenix_wait_required_service_absent test_compose '../live-executor' &&
  fail 'invalid service identity was accepted'

printf '%s\n' REQUIRED_SERVICE_ABSENCE_TEST_OK
