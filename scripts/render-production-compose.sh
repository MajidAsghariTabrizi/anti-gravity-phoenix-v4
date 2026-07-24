#!/usr/bin/env sh
set -eu
umask 077

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
compose_file=
overlay_file=
env_file=
release_env=
release_manifest=
rendered_output=
metadata_output=

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
    --overlay-file) [ "$#" -ge 2 ] || usage; overlay_file=$2; shift 2 ;;
    --env-file) [ "$#" -ge 2 ] || usage; env_file=$2; shift 2 ;;
    --release-env) [ "$#" -ge 2 ] || usage; release_env=$2; shift 2 ;;
    --release-manifest) [ "$#" -ge 2 ] || usage; release_manifest=$2; shift 2 ;;
    --output) [ "$#" -ge 2 ] || usage; rendered_output=$2; shift 2 ;;
    --metadata-output) [ "$#" -ge 2 ] || usage; metadata_output=$2; shift 2 ;;
    *) usage ;;
  esac
done

[ -n "$compose_file" ] || usage
[ -n "$env_file" ] || usage
[ -n "$rendered_output" ] || usage
[ -n "$metadata_output" ] || usage
[ -f "$compose_file" ] || fail PRODUCTION_COMPOSE_CONTEXT_MISSING
if [ -n "$overlay_file" ]; then
  [ -f "$overlay_file" ] || fail PRODUCTION_COMPOSE_CONTEXT_MISSING
fi
[ -f "$env_file" ] || fail PRODUCTION_ENV_MISSING
if [ -z "$release_env" ] && [ -z "$release_manifest" ]; then
  fail RELEASE_ENV_MISSING
fi

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi
command -v python3 >/dev/null 2>&1 || fail PRODUCTION_COMPOSE_CONTEXT_MISSING

state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-production-render.XXXXXX") ||
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
cleanup_state() {
  rm -rf "$state_dir"
}
trap cleanup_state EXIT
trap 'exit 1' HUP INT TERM

manifest_args=
if [ -n "$release_manifest" ]; then
  [ -f "$release_manifest" ] || fail RELEASE_MANIFEST_MISSING
  manifest_args="--manifest=$release_manifest"
  if [ -z "$release_env" ]; then
    release_env=$state_dir/release.env
    python3 "$script_dir/production_context.py" manifest-env \
      --manifest "$release_manifest" \
      --output "$release_env" || exit 1
  fi
fi
if [ -n "$release_env" ]; then
  [ -f "$release_env" ] || fail RELEASE_ENV_MISSING
fi

set -- python3 "$script_dir/production_context.py" validate-output-paths \
  --output "$rendered_output" \
  --metadata-output "$metadata_output" \
  --input "$compose_file" \
  --input "$env_file" \
  --input "$release_env"
if [ -n "$overlay_file" ]; then
  set -- "$@" --input "$overlay_file"
fi
if [ -n "$release_manifest" ]; then
  set -- "$@" --input "$release_manifest"
fi
"$@" || exit 1

rendered_tmp=$state_dir/compose.rendered.json
metadata_tmp=$state_dir/render.metadata.json

if [ -n "${PHOENIX_COMPOSE_BIN:-}" ]; then
  compose_command=$PHOENIX_COMPOSE_BIN
  compose_prefix=
else
  command -v docker >/dev/null 2>&1 || fail PRODUCTION_COMPOSE_CONTEXT_MISSING
  compose_command=docker
  compose_prefix=compose
fi

set -- -f "$compose_file"
if [ -n "$overlay_file" ]; then
  set -- "$@" -f "$overlay_file" --profile live-autonomous
fi

if ! env -i \
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
    "$@" \
    config --format json >"$rendered_tmp" 2>/dev/null; then
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
fi

set -- python3 "$script_dir/production_context.py" validate-render \
  --compose-config "$rendered_tmp" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --metadata-output "$metadata_tmp"
if [ -n "$manifest_args" ]; then
  set -- "$@" "$manifest_args"
fi
"$@" || exit 1

publish_output() {
  publish_source=$1
  publish_target=$2
  publish_mode=$3
  publish_dir=$(dirname -- "$publish_target")
  mkdir -p "$publish_dir" || return 1
  publish_tmp=$(mktemp "$publish_dir/.phoenix-production-output.XXXXXX") || return 1
  if ! cp "$publish_source" "$publish_tmp" ||
    ! chmod "$publish_mode" "$publish_tmp" ||
    ! mv "$publish_tmp" "$publish_target"
  then
    rm -f "$publish_tmp"
    return 1
  fi
}

publish_output "$rendered_tmp" "$rendered_output" 0600 ||
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
publish_output "$metadata_tmp" "$metadata_output" 0640 ||
  fail PRODUCTION_COMPOSE_CONTEXT_MISSING
cat "$metadata_output"
