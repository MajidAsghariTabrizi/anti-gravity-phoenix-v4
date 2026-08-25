#!/usr/bin/env sh
set -eu
umask 027

output=${PHOENIX_ECONOMIC_DASHBOARD_OUTPUT:-/evidence/latest-dashboard.json}
interval=${PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS:-45}
query_timeout=${PHOENIX_ECONOMIC_DASHBOARD_QUERY_TIMEOUT_SECONDS:-30}
sql_file=${PHOENIX_ECONOMIC_DASHBOARD_SQL:-/opt/phoenix/economic-dashboard-snapshot.sql}

case "$interval" in
  ''|*[!0-9]*) echo "ECONOMIC_DASHBOARD_FAILED: invalid interval" >&2; exit 1 ;;
esac
[ "$interval" -ge 30 ] && [ "$interval" -le 60 ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: interval must be 30-60 seconds" >&2; exit 1; }
case "$query_timeout" in
  ''|*[!0-9]*) echo "ECONOMIC_DASHBOARD_FAILED: invalid query timeout" >&2; exit 1 ;;
esac
[ "$query_timeout" -ge 5 ] && [ "$query_timeout" -le 120 ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: query timeout must be 5-120 seconds" >&2; exit 1; }
[ -f "$sql_file" ] && [ ! -L "$sql_file" ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: SQL contract is unavailable" >&2; exit 1; }

output_dir=${output%/*}
[ -d "$output_dir" ] && [ ! -L "$output_dir" ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: output directory is unsafe" >&2; exit 1; }

while :; do
  candidate=$(mktemp "$output_dir/.economic-dashboard.XXXXXX") ||
    { echo "ECONOMIC_DASHBOARD_FAILED: staging failed" >&2; exit 1; }
  # work_mem 256MB: the snapshot's largest sorts spill ~180MB at 64MB;
  # measured on production-scale data: 62.6s @64MB vs 59.0s @256MB
  # (config-only change; statement_timeout unchanged).
  if PGOPTIONS="-c statement_timeout=${query_timeout}s -c lock_timeout=5s -c work_mem=256MB" \
    psql -X -q -A -t "$POSTGRES_DSN" -f "$sql_file" >"$candidate" &&
    [ -s "$candidate" ]
  then
    chmod 0640 "$candidate"
    mv "$candidate" "$output"
  else
    rm -f "$candidate"
    echo "ECONOMIC_DASHBOARD_REFRESH_FAILED" >&2
  fi
  sleep "$interval"
done
