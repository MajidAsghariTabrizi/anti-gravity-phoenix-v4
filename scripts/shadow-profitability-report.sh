#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
compose_file=${PHOENIX_COMPOSE_FILE:-$deploy_root/deploy/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$deploy_root/deploy/current-release.env}
sql_file=$script_dir/sql/shadow-profitability-report.sql
analyzer=$script_dir/shadow_profitability_report.py
report_format=text
report_limit=100

fail() {
  echo "shadow profitability report failed: $1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: shadow-profitability-report.sh [--format text|json] [--limit 1..1000]
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --format)
      [ "$#" -ge 2 ] || fail "--format requires a value"
      report_format=$2
      shift 2
      ;;
    --limit)
      [ "$#" -ge 2 ] || fail "--limit requires a value"
      report_limit=$2
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
case "$report_limit" in
  ''|*[!0-9]*) fail "--limit must be an integer from 1 through 1000" ;;
esac
[ "$report_limit" -ge 1 ] && [ "$report_limit" -le 1000 ] ||
  fail "--limit must be an integer from 1 through 1000"

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
[ -f "$sql_file" ] || fail "report SQL is unavailable"
[ -f "$analyzer" ] || fail "report analyzer is unavailable"

report_rows=$(mktemp "${TMPDIR:-/tmp}/phoenix-profitability.XXXXXX") ||
  fail "could not allocate bounded report input"
trap 'rm -f "$report_rows"' EXIT HUP INT TERM

(
  unset ENGINE_ROUTE_REGISTRY_JSON
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
    docker compose --env-file "$env_file" --env-file "$release_env" \
      -f "$compose_file" exec -T -e PHOENIX_REPORT_LIMIT="$report_limit" postgres \
      sh -c 'psql -X -qAt -v ON_ERROR_STOP=1 -v report_limit="$PHOENIX_REPORT_LIMIT" -U "$POSTGRES_USER" -d "$POSTGRES_DB"'
) <"$sql_file" >"$report_rows" || fail "read-only PostgreSQL query failed"

"$python_command" "$analyzer" --format "$report_format" --limit "$report_limit" \
  <"$report_rows"
