#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
compose_file=${PHOENIX_COMPOSE_FILE:-$deploy_root/deploy/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$deploy_root/deploy/current-release.env}
sql_file=$script_dir/sql/shadow-atlas-validation.sql
analyzer=$script_dir/atlas_shadow_validation_report.py

window_end=$(date -u +%FT%TZ)
window_start=$(date -u -d "@$(( $(date -u +%s) - 86400 ))" +%FT%TZ 2>/dev/null || true)
[ -n "$window_start" ] ||
  window_start=$(date -u -d '-86400 seconds' +%FT%TZ 2>/dev/null || true)
output_dir=
report_format=text

fail() {
  echo "shadow atlas validation failed: $1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: atlas-shadow-validation.sh
       [--window-start ISO8601Z] [--window-end ISO8601Z]
       [--output-dir DIR] [--format text|json]
Defaults: the trailing 24 hours up to the current UTC time.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --window-start)
      [ "$#" -ge 2 ] || fail "--window-start requires a value"
      window_start=$2
      shift 2
      ;;
    --window-end)
      [ "$#" -ge 2 ] || fail "--window-end requires a value"
      window_end=$2
      shift 2
      ;;
    --output-dir)
      [ "$#" -ge 2 ] || fail "--output-dir requires a value"
      output_dir=$2
      shift 2
      ;;
    --format)
      [ "$#" -ge 2 ] || fail "--format requires a value"
      report_format=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

case "$report_format" in
  text|json) ;;
  *) fail "--format must be text or json" ;;
esac

case "$window_start" in
  *[!0-9TZ:-]*|'') fail "window start is not ISO-8601 UTC" ;;
esac
case "$window_end" in
  *[!0-9TZ:-]*|'') fail "window end is not ISO-8601 UTC" ;;
esac
[ "$(printf '%s' "$window_start" | wc -c)" -eq 20 ] ||
  fail "window start must be 20 characters (YYYY-MM-DDTHH:MM:SSZ)"
[ "$(printf '%s' "$window_end" | wc -c)" -eq 20 ] ||
  fail "window end must be 20 characters (YYYY-MM-DDTHH:MM:SSZ)"

start_epoch=$(date -u -d "$window_start" +%s 2>/dev/null) ||
  fail "window start is not a valid UTC timestamp"
end_epoch=$(date -u -d "$window_end" +%s 2>/dev/null) ||
  fail "window end is not a valid UTC timestamp"
[ "$start_epoch" -lt "$end_epoch" ] || fail "window start must precede window end"
[ "$((end_epoch - start_epoch))" -le 2678400 ] ||
  fail "window must not exceed 31 days"
[ "$end_epoch" -le "$(($(date -u +%s) + 3600))" ] ||
  fail "window end must not be in the future"

command -v docker >/dev/null 2>&1 || fail "docker is unavailable"
if command -v python3 >/dev/null 2>&1; then
  python_command=python3
elif command -v python >/dev/null 2>&1; then
  python_command=python
else
  fail "python is unavailable"
fi
[ -f "$compose_file" ] || fail "production Compose file is unavailable"
[ -f "$env_file" ] || fail "production environment file is unavailable"
[ -f "$release_env" ] || fail "release environment file is unavailable"
[ -f "$sql_file" ] || fail "validation SQL is unavailable"
[ -f "$analyzer" ] || fail "validation analyzer is unavailable"

validation_rows=$(mktemp "${TMPDIR:-/tmp}/phoenix-atlas-validation.XXXXXX") ||
  fail "could not allocate bounded validation input"
trap 'rm -f "$validation_rows"' EXIT HUP INT TERM

(
  unset ENGINE_ROUTE_REGISTRY_JSON
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose --env-file "$env_file" --env-file "$release_env" \
      -f "$compose_file" exec -T postgres \
      sh -c 'psql -X -qAt -v ON_ERROR_STOP=1 \
        -v window_start="$1" -v window_end="$2" \
        -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
      sh "$window_start" "$window_end"
) <"$sql_file" >"$validation_rows" ||
  fail "read-only PostgreSQL validation query failed"

set -- "$python_command" "$analyzer" \
  --window-start "$window_start" --window-end "$window_end" \
  --format "$report_format"
if [ -n "$output_dir" ]; then
  set -- "$@" --output-dir "$output_dir"
fi
"$@" <"$validation_rows"
