#!/usr/bin/env sh
set -eu
umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
compose_file=${PHOENIX_COMPOSE_FILE:-$deploy_root/deploy/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$deploy_root/deploy/current-release.env}
sql_file=$script_dir/sql/prelive-money-path-report.sql
analyzer=$script_dir/prelive_money_path_report.py
report_format=text
window_hours=24
reason_limit=17

fail() {
  echo "PRELIVE_MONEY_PATH_REPORT_BLOCKED: $1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: prelive-money-path-report.sh [--format text|json] [--window-hours 1..168]
EOF
}

bounded_integer() {
  value=$1
  minimum=$2
  maximum=$3
  case "$value" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$value" -ge "$minimum" ] && [ "$value" -le "$maximum" ]
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --format)
      [ "$#" -ge 2 ] || fail "--format requires a value"
      report_format=$2
      shift 2
      ;;
    --window-hours)
      [ "$#" -ge 2 ] || fail "--window-hours requires a value"
      window_hours=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) fail "unknown argument: $1" ;;
  esac
done

case "$report_format" in
  text|json) ;;
  *) fail "--format must be text or json" ;;
esac
bounded_integer "$window_hours" 1 168 || fail "--window-hours must be from 1 through 168"

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
[ -f "$release_env" ] || fail "digest-pinned release environment is unavailable"
[ -f "$sql_file" ] || fail "money-path SQL is unavailable"
[ -f "$analyzer" ] || fail "money-path analyzer is unavailable"
[ -x "$script_dir/validate-production-env.sh" ] || fail "production environment validator is unavailable"
[ -x "$script_dir/render-production-compose.sh" ] || fail "production Compose renderer is unavailable"

state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-money-path.XXXXXX") ||
  fail "could not allocate private report state"
database_source=$state_dir/database-source.json
rendered_compose=$state_dir/compose.rendered.json
render_metadata=$state_dir/render.metadata.json
cleanup_state() {
  rm -rf "$state_dir"
}
trap cleanup_state EXIT
trap 'exit 1' HUP INT TERM

"$script_dir/validate-production-env.sh" "$env_file" >/dev/null ||
  fail "production SHADOW environment validation failed"
"$script_dir/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --output "$rendered_compose" \
  --metadata-output "$render_metadata" >/dev/null ||
  fail "digest-pinned production context validation failed"

(
  unset ENGINE_ROUTE_REGISTRY_JSON
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose --env-file "$env_file" --env-file "$release_env" \
      -f "$compose_file" exec -T \
      -e PHOENIX_REPORT_WINDOW_HOURS="$window_hours" \
      -e PHOENIX_REPORT_REASON_LIMIT="$reason_limit" \
      postgres sh -c \
      'psql -X -qAt -v ON_ERROR_STOP=1 -v window_hours="$PHOENIX_REPORT_WINDOW_HOURS" -v reason_limit="$PHOENIX_REPORT_REASON_LIMIT" -U "$POSTGRES_USER" -d "$POSTGRES_DB"'
) <"$sql_file" >"$database_source" || fail "bounded read-only PostgreSQL report failed"

"$python_command" "$analyzer" \
  --source "$database_source" \
  --format "$report_format" \
  --window-hours "$window_hours"
