#!/usr/bin/env sh
set -eu
umask 077

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
compose_file=${PHOENIX_COMPOSE_FILE:-$deploy_root/deploy/compose.prod.yml}
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
release_env=${PHOENIX_RELEASE_ENV:-$deploy_root/deploy/current-release.env}
sql_file=$script_dir/sql/shadow-route-discovery-enrichment.sql
analyzer=$script_dir/shadow_route_discovery.py
pool_proofs=$script_dir/routes/arbitrum_uniswap_v3_pool_proofs.json
report_format=text
scan_limit=10000
evidence_limit=10000
top_limit=10

fail() {
  echo "SHADOW_ROUTE_DISCOVERY_BLOCKED: $1" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: shadow-route-discovery.sh [--format text|json] [--limit 1..100000] [--evidence-limit 1..100000] [--top 1..10]
EOF
}

bounded_integer() {
  discovery_value=$1
  discovery_minimum=$2
  discovery_maximum=$3
  case "$discovery_value" in
    ''|*[!0-9]*) return 1 ;;
  esac
  [ "$discovery_value" -ge "$discovery_minimum" ] &&
    [ "$discovery_value" -le "$discovery_maximum" ]
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
      scan_limit=$2
      shift 2
      ;;
    --evidence-limit)
      [ "$#" -ge 2 ] || fail "--evidence-limit requires a value"
      evidence_limit=$2
      shift 2
      ;;
    --top)
      [ "$#" -ge 2 ] || fail "--top requires a value"
      top_limit=$2
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
bounded_integer "$scan_limit" 1 100000 || fail "--limit must be from 1 through 100000"
bounded_integer "$evidence_limit" 1 100000 || fail "--evidence-limit must be from 1 through 100000"
bounded_integer "$top_limit" 1 10 || fail "--top must be from 1 through 10"

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
[ -f "$sql_file" ] || fail "route discovery SQL is unavailable"
[ -f "$analyzer" ] || fail "route discovery analyzer is unavailable"
[ -f "$pool_proofs" ] || fail "reviewed pool proof registry is unavailable"
[ -x "$script_dir/validate-production-env.sh" ] || fail "production environment validator is unavailable"
[ -x "$script_dir/render-production-compose.sh" ] || fail "production Compose renderer is unavailable"

state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-route-discovery.XXXXXX") ||
  fail "could not allocate private report state"
decoded_rows=$state_dir/decoded.ndjson
enrichment_rows=$state_dir/enrichment.ndjson
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

compose() {
  (
    unset ENGINE_ROUTE_REGISTRY_JSON
    PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$release_env" \
      docker compose --env-file "$env_file" --env-file "$release_env" \
        -f "$compose_file" "$@"
  )
}

compose exec -T phoenix-engine /usr/local/bin/shadow-positive-route-evidence \
  scan-postgres \
  --dsn-env POSTGRES_DSN \
  --route-registry-env ENGINE_ROUTE_REGISTRY_JSON \
  --limit "$scan_limit" >"$decoded_rows" ||
  fail "bounded production decoder scan failed"

compose exec -T -e PHOENIX_ROUTE_EVIDENCE_LIMIT="$evidence_limit" postgres \
  sh -c 'psql -X -qAt -v ON_ERROR_STOP=1 -v evidence_limit="$PHOENIX_ROUTE_EVIDENCE_LIMIT" -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
  <"$sql_file" >"$enrichment_rows" ||
  fail "bounded read-only PostgreSQL enrichment failed"

"$python_command" "$analyzer" \
  --decoded "$decoded_rows" \
  --enrichment "$enrichment_rows" \
  --pool-proofs "$pool_proofs" \
  --format "$report_format" \
  --limit "$scan_limit" \
  --top "$top_limit"
