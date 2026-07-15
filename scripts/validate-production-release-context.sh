#!/usr/bin/env sh
set -eu
umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
compose_file=
env_file=
release_env=
release_manifest=
current_release=
release_state=
running_images=
inspect_running=0
rendered_output=
metadata_output=
result_output=

fail() {
  printf '{"code":"%s","status":"error"}\n' "$1" >&2
  exit 1
}

usage() {
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --compose-file) [ "$#" -ge 2 ] || usage; compose_file=$2; shift 2 ;;
    --env-file) [ "$#" -ge 2 ] || usage; env_file=$2; shift 2 ;;
    --release-env) [ "$#" -ge 2 ] || usage; release_env=$2; shift 2 ;;
    --release-manifest) [ "$#" -ge 2 ] || usage; release_manifest=$2; shift 2 ;;
    --current-release) [ "$#" -ge 2 ] || usage; current_release=$2; shift 2 ;;
    --release-state) [ "$#" -ge 2 ] || usage; release_state=$2; shift 2 ;;
    --running-images-file) [ "$#" -ge 2 ] || usage; running_images=$2; shift 2 ;;
    --inspect-running) inspect_running=1; shift ;;
    --rendered-output) [ "$#" -ge 2 ] || usage; rendered_output=$2; shift 2 ;;
    --metadata-output) [ "$#" -ge 2 ] || usage; metadata_output=$2; shift 2 ;;
    --output) [ "$#" -ge 2 ] || usage; result_output=$2; shift 2 ;;
    *) usage ;;
  esac
done

for value in \
  "$compose_file" \
  "$env_file" \
  "$release_env" \
  "$release_manifest" \
  "$current_release" \
  "$release_state" \
  "$rendered_output" \
  "$metadata_output" \
  "$result_output"
do
  [ -n "$value" ] || usage
done
if [ "$inspect_running" -eq 1 ] && [ -n "$running_images" ]; then
  usage
fi
if [ "$inspect_running" -eq 0 ] && [ -z "$running_images" ]; then
  fail RUNNING_IMAGE_MISMATCH
fi

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi
command -v python3 >/dev/null 2>&1 || fail PRODUCTION_COMPOSE_CONTEXT_MISSING

state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-release-context.XXXXXX") ||
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
cleanup_state() {
  rm -rf "$state_dir"
}
trap cleanup_state EXIT
trap 'exit 1' HUP INT TERM

"$script_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$release_manifest" \
  --output "$rendered_output" \
  --metadata-output "$metadata_output" >/dev/null || exit 1

if [ "$inspect_running" -eq 1 ]; then
  running_tsv=$state_dir/running-images.tsv
  running_images=$state_dir/running-images.json
  : >"$running_tsv"
  if [ -n "${PHOENIX_COMPOSE_BIN:-}" ]; then
    compose_command=$PHOENIX_COMPOSE_BIN
    compose_prefix=
  else
    command -v docker >/dev/null 2>&1 || fail RUNNING_IMAGE_MISMATCH
    compose_command=docker
    compose_prefix=compose
  fi
  inspect_command=${PHOENIX_DOCKER_BIN:-docker}
  if [ -z "${PHOENIX_DOCKER_BIN:-}" ]; then
    command -v docker >/dev/null 2>&1 || fail RUNNING_IMAGE_MISMATCH
  fi
  for service in \
    nitro-feed-relay nats postgres rpc-gateway feed-ingestor \
    phoenix-engine shadow-dispatcher recorder dashboard prometheus
  do
    container_id=$(env -i \
      PATH="${PATH:-}" \
      HOME="${HOME:-}" \
      DOCKER_CONFIG="${DOCKER_CONFIG:-}" \
      DOCKER_CONTEXT="${DOCKER_CONTEXT:-}" \
      DOCKER_HOST="${DOCKER_HOST:-}" \
      XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-}" \
      PHOENIX_ENV_FILE="$env_file" \
      "$compose_command" ${compose_prefix:+"$compose_prefix"} \
        --env-file "$env_file" \
        --env-file "$release_env" \
        -f "$compose_file" ps -q "$service" 2>/dev/null) ||
      fail RUNNING_IMAGE_MISMATCH
    [ -n "$container_id" ] || fail RUNNING_IMAGE_MISMATCH
    image_pair=$(
      "$inspect_command" inspect \
        --format '{{.Config.Image}}{{printf "\t"}}{{.Image}}' \
        "$container_id" 2>/dev/null
    ) || fail RUNNING_IMAGE_MISMATCH
    configured_image=${image_pair%%	*}
    image_id=${image_pair#*	}
    [ "$configured_image" != "$image_pair" ] || fail RUNNING_IMAGE_MISMATCH
    printf '%s\t%s\t%s\n' "$service" "$configured_image" "$image_id" >>"$running_tsv"
  done
  python3 "$script_dir/production_context.py" running-from-tsv \
    --input "$running_tsv" \
    --output "$running_images" || exit 1
else
  [ -f "$running_images" ] || fail RUNNING_IMAGE_MISMATCH
fi

result_tmp=$state_dir/release-context.json
python3 "$script_dir/production_context.py" validate-active \
  --manifest "$release_manifest" \
  --release-env "$release_env" \
  --render-metadata "$metadata_output" \
  --compose-config "$rendered_output" \
  --current-release "$current_release" \
  --release-state "$release_state" \
  --running-images "$running_images" \
  --output "$result_tmp" || exit 1

result_dir=$(dirname -- "$result_output")
mkdir -p "$result_dir" || fail PRODUCTION_COMPOSE_CONTEXT_MISSING
result_publish=$(mktemp "$result_dir/.phoenix-release-context.XXXXXX") ||
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
if ! cp "$result_tmp" "$result_publish" ||
  ! chmod 0640 "$result_publish" ||
  ! mv "$result_publish" "$result_output"
then
  rm -f "$result_publish"
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
fi
cat "$result_output"
