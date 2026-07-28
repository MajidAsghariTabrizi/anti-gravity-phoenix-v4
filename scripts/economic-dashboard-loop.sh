#!/usr/bin/env sh
set -eu
umask 027

output=${PHOENIX_ECONOMIC_DASHBOARD_OUTPUT:-/evidence/latest-dashboard.json}
interval=${PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS:-45}
sql_file=${PHOENIX_ECONOMIC_DASHBOARD_SQL:-/opt/phoenix/economic-dashboard-snapshot.sql}

case "$interval" in
  ''|*[!0-9]*) echo "ECONOMIC_DASHBOARD_FAILED: invalid interval" >&2; exit 1 ;;
esac
[ "$interval" -ge 30 ] && [ "$interval" -le 60 ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: interval must be 30-60 seconds" >&2; exit 1; }
[ -f "$sql_file" ] && [ ! -L "$sql_file" ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: SQL contract is unavailable" >&2; exit 1; }

output_dir=${output%/*}
[ -d "$output_dir" ] && [ ! -L "$output_dir" ] ||
  { echo "ECONOMIC_DASHBOARD_FAILED: output directory is unsafe" >&2; exit 1; }

while :; do
  candidate=$(mktemp "$output_dir/.economic-dashboard.XXXXXX") ||
    { echo "ECONOMIC_DASHBOARD_FAILED: staging failed" >&2; exit 1; }
  if psql -X -q -A -t "$POSTGRES_DSN" -f "$sql_file" >"$candidate" &&
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
