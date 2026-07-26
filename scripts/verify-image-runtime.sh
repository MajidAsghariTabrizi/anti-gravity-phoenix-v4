#!/usr/bin/env sh
set -eu

fail() {
  echo "IMAGE_RUNTIME_CONTRACT_FAILED: $1" >&2
  exit 1
}

[ "$#" -eq 3 ] || fail argument_count_invalid

image_name=$1
image_reference=$2
expected_revision=$3

[ "$image_name" = live-executor ] || fail unsupported_image
[ -n "$image_reference" ] || fail image_reference_missing

command -v docker >/dev/null 2>&1 || fail docker_missing

docker image inspect "$image_reference" >/dev/null 2>&1 ||
  fail image_not_available

if [ -n "$expected_revision" ]; then
  revision=$(
    docker image inspect \
      --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' \
      "$image_reference"
  ) || fail image_revision_unavailable

  [ "$revision" = "$expected_revision" ] ||
    fail image_revision_mismatch
fi

configured_user=$(
  docker image inspect --format '{{.Config.User}}' "$image_reference"
) || fail image_user_unavailable

[ "$configured_user" = "65532:65532" ] ||
  fail image_user_invalid

configured_entrypoint=$(
  docker image inspect --format '{{json .Config.Entrypoint}}' "$image_reference"
) || fail image_entrypoint_unavailable

[ "$configured_entrypoint" = '["/usr/local/bin/service"]' ] ||
  fail image_entrypoint_invalid

docker run --rm --entrypoint /bin/sh "$image_reference" -c '
  set -eu
  test -x /usr/local/bin/service
  test -x /usr/local/bin/approve-execution-request
  test -x /usr/local/bin/autonomous-live-control
' >/dev/null || fail required_binary_missing

probe_stderr=$(mktemp)
trap 'rm -f "$probe_stderr"' 0 1 2 15

set +e
probe_output=$(
  docker run --rm \
    --entrypoint /usr/local/bin/autonomous-live-control \
    "$image_reference" \
    __image_runtime_probe__ 2>"$probe_stderr"
)
probe_status=$?
set -e

[ "$probe_status" -eq 0 ] ||
  fail autonomous_control_probe_failed

[ ! -s "$probe_stderr" ] ||
  fail autonomous_control_probe_stderr

[ "$probe_output" = AUTONOMOUS_CONTROL_RUNTIME_OK ] ||
  fail autonomous_control_probe_stdout

case "$probe_output" in
  *AUTONOMOUS_CONTROL_FAILED:*) fail autonomous_control_reported_failure ;;
esac

rm -f "$probe_stderr"
trap - 0 1 2 15

echo "IMAGE_RUNTIME_CONTRACT_OK: image=$image_name reference=$image_reference"
