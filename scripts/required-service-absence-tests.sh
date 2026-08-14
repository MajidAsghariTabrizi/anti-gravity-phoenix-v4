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
case "$1" in
  ps)
    [ "$#" -eq 8 ] || exit 91
    [ "$2" = --all ] && [ "$3" = --quiet ] &&
      [ "$4" = --no-trunc ] && [ "$5" = --filter ] &&
      [ "$6" = label=com.docker.compose.project=app ] &&
      [ "$7" = --filter ] &&
      [ "$8" = label=com.docker.compose.service=live-executor ] || exit 92
    count=0
    [ ! -f "$PHOENIX_TEST_COUNT_FILE" ] ||
      count=$(cat "$PHOENIX_TEST_COUNT_FILE")
    count=$((count + 1))
    printf '%s\n' "$count" >"$PHOENIX_TEST_COUNT_FILE"
    case "$PHOENIX_TEST_SEQUENCE" in
      absent) exit 0 ;;
      oneoff)
        [ -f "$PHOENIX_TEST_STATE_FILE" ] || printf '%064d\n' 4
        ;;
      changing)
        phase=first
        [ ! -f "$PHOENIX_TEST_STATE_FILE" ] ||
          phase=$(cat "$PHOENIX_TEST_STATE_FILE")
        case "$phase" in
          first) printf '%064d\n' 5 ;;
          second) printf '%064d\n' 6 ;;
          done) ;;
          *) exit 93 ;;
        esac
        ;;
      removing)
        [ "$count" -ge 3 ] || printf '%064d\n' 7
        ;;
      race_disappeared)
        [ -f "$PHOENIX_TEST_STATE_FILE" ] || printf '%064d\n' 8
        ;;
      running) printf '%064d\n' 2 ;;
      stopped) printf '%064d\n' 3 ;;
      malformed) printf '%s\n' not-a-container-id ;;
      error) exit 94 ;;
      *) exit 95 ;;
    esac
    ;;
  container)
    [ "$#" -eq 4 ] && [ "$2" = rm ] && [ "$3" = --force ] || exit 96
    remove_count=0
    [ ! -f "$PHOENIX_TEST_REMOVE_COUNT_FILE" ] ||
      remove_count=$(cat "$PHOENIX_TEST_REMOVE_COUNT_FILE")
    printf '%s\n' "$((remove_count + 1))" >"$PHOENIX_TEST_REMOVE_COUNT_FILE"
    case "$PHOENIX_TEST_SEQUENCE:$4" in
      oneoff:0000000000000000000000000000000000000000000000000000000000000004)
        : >"$PHOENIX_TEST_STATE_FILE"
        ;;
      changing:0000000000000000000000000000000000000000000000000000000000000005)
        printf '%s\n' second >"$PHOENIX_TEST_STATE_FILE"
        exit 97
        ;;
      changing:0000000000000000000000000000000000000000000000000000000000000006)
        printf '%s\n' done >"$PHOENIX_TEST_STATE_FILE"
        ;;
      race_disappeared:0000000000000000000000000000000000000000000000000000000000000008)
        : >"$PHOENIX_TEST_STATE_FILE"
        exit 98
        ;;
      removing:*|running:*|stopped:*) exit 99 ;;
      *) exit 100 ;;
    esac
    ;;
  *) exit 101 ;;
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
  state_file=$tmp_root/state.$case_name
  remove_count_file=$tmp_root/remove-count.$case_name
  rm -f -- "$count_file"
  rm -f -- "$state_file" "$remove_count_file"
  export PHOENIX_TEST_SEQUENCE=$case_name
  export PHOENIX_TEST_COUNT_FILE=$count_file
  export PHOENIX_TEST_STATE_FILE=$state_file
  export PHOENIX_TEST_REMOVE_COUNT_FILE=$remove_count_file
  export PHOENIX_REQUIRED_ABSENCE_WAIT_SECONDS=$wait_seconds
  if phoenix_reconcile_required_service_absent test_compose live-executor; then
    result=success
  else
    result=failure
  fi
  [ "$result" = "$expect" ] || fail "$case_name returned $result"
}

run_case absent 1 success
[ "$(cat "$tmp_root/count.absent")" -eq 1 ] ||
  fail 'already-absent lookup was not immediate'
[ ! -e "$tmp_root/remove-count.absent" ] ||
  fail 'already-absent service triggered removal'
run_case oneoff 3 success
[ "$(cat "$tmp_root/remove-count.oneoff")" -eq 1 ] ||
  fail 'created one-off container was not removed exactly once'
run_case changing 5 success
[ "$(cat "$tmp_root/remove-count.changing")" -eq 2 ] ||
  fail 'changing container identities were not freshly reconciled'
run_case removing 5 success
[ "$(cat "$tmp_root/count.removing")" -eq 3 ] ||
  fail 'removing container was not polled through fresh identities'
run_case race_disappeared 3 success
run_case running 1 failure
run_case stopped 1 failure
run_case malformed 1 failure
run_case error 1 failure

export PHOENIX_TEST_SEQUENCE=absent
export PHOENIX_TEST_COUNT_FILE=$tmp_root/count.invalid
export PHOENIX_TEST_STATE_FILE=$tmp_root/state.invalid
export PHOENIX_TEST_REMOVE_COUNT_FILE=$tmp_root/remove-count.invalid
phoenix_reconcile_required_service_absent test_compose '../live-executor' &&
  fail 'invalid service identity was accepted'

printf '%s\n' REQUIRED_SERVICE_ABSENCE_TEST_OK
