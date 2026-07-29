#!/usr/bin/env sh
set -eu
umask 077

release_sha=${1:-}
candidate_root=${2:-}
release_manifest=${3:-}
deploy_root=${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}
deploy_dir=$deploy_root/deploy
env_file=${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}
active_release_env=$deploy_dir/current-release.env
compose_file=$candidate_root/compose.prod.yml
overlay_file=$candidate_root/compose.live-autonomous.yml
compose_runner=$candidate_root/scripts/production_compose.py

fail() {
  printf 'PRODUCTION_RELEASE_REHEARSAL_FAILED: %s\n' "$1" >&2
  exit 1
}

case "$release_sha" in
  *[!0-9a-f]*|"") fail release_sha_invalid ;;
esac
[ "${#release_sha}" -eq 40 ] || fail release_sha_invalid
short_sha=$(printf '%.12s' "$release_sha")
[ "$(id -u)" -eq 0 ] || fail root_required
[ -d "$candidate_root" ] && [ ! -L "$candidate_root" ] ||
  fail candidate_root_invalid
for required in \
  "$release_manifest" \
  "$env_file" \
  "$active_release_env" \
  "$compose_file" \
  "$overlay_file" \
  "$compose_runner" \
  "$candidate_root/scripts/production_context.py" \
  "$candidate_root/scripts/render-production-compose.sh" \
  "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql"
do
  [ -f "$required" ] && [ ! -L "$required" ] ||
    fail candidate_input_invalid
done

state_dir=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-release-rehearsal.$release_sha.XXXXXX") ||
  fail temporary_directory_failed
cleanup() {
  rm -rf -- "$state_dir"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

release_env=$state_dir/candidate-release.env
rendered=$state_dir/candidate-compose.json
metadata=$state_dir/candidate-render.json
monitor_output=$state_dir/monitor
mkdir -m 0700 "$monitor_output"
chown 1000:1000 "$monitor_output"
monitor_container=
database_container=
database_network=
cleanup_monitor() {
  [ -z "$monitor_container" ] ||
    /usr/bin/docker rm -f "$monitor_container" >/dev/null 2>&1 || true
}
cleanup_database() {
  [ -z "$database_container" ] ||
    /usr/bin/docker rm -f "$database_container" >/dev/null 2>&1 || true
  [ -z "$database_network" ] ||
    /usr/bin/docker network rm "$database_network" >/dev/null 2>&1 || true
}
cleanup_all() {
  cleanup_monitor
  cleanup_database
  cleanup
}
trap cleanup_all EXIT

python3 "$candidate_root/scripts/production_context.py" manifest-env \
  --manifest "$release_manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env" ||
  fail candidate_manifest_invalid

"$candidate_root/scripts/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --overlay-file "$overlay_file" \
  --env-file "$env_file" \
  --release-env "$release_env" \
  --release-manifest "$release_manifest" \
  --output "$rendered" \
  --metadata-output "$metadata" >/dev/null ||
  fail candidate_compose_render_failed

postgres_image=$(
  python3 -I -B - "$rendered" <<'PY'
import json
import re
import sys
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
image = value["services"]["postgres"]["image"]
if not isinstance(image, str) or re.fullmatch(
    r"[a-z0-9._/-]+@sha256:[0-9a-f]{64}", image
) is None:
    raise SystemExit(1)
print(image)
PY
) || fail candidate_postgres_image_invalid

# Apply every candidate migration to an isolated, tmpfs-backed PostgreSQL
# instance using the exact Compose-pinned image. It has no published port,
# external network, persistent volume, or Production credential.
database_network="phoenix-release-rehearsal-$short_sha-$$"
database_container="phoenix-release-rehearsal-db-$short_sha-$$"
/usr/bin/docker network create --internal "$database_network" >/dev/null ||
  fail rehearsal_database_network_failed
/usr/bin/docker run -d \
  --name "$database_container" \
  --network "$database_network" \
  --tmpfs /var/lib/postgresql/data:rw,nosuid,nodev \
  --tmpfs /run/postgresql:rw,nosuid,nodev \
  -e POSTGRES_DB=phoenix_rehearsal \
  -e POSTGRES_USER=phoenix_rehearsal \
  -e POSTGRES_PASSWORD=phoenix_rehearsal_only \
  "$postgres_image" >/dev/null ||
  fail rehearsal_database_start_failed
database_deadline=$(( $(date +%s) + 60 ))
database_ready=0
while [ "$(date +%s)" -lt "$database_deadline" ]; do
  if /usr/bin/docker exec "$database_container" \
    pg_isready -U phoenix_rehearsal -d phoenix_rehearsal >/dev/null 2>&1
  then
    database_ready=1
    break
  fi
  sleep 1
done
[ "$database_ready" -eq 1 ] || fail rehearsal_database_unhealthy
{
  for migration in "$candidate_root"/migrations/*.sql; do
    cat "$migration"
  done
  for migration in "$candidate_root"/live-executor/schema/*.sql; do
    cat "$migration"
  done
} | /usr/bin/docker exec -i "$database_container" \
  psql -X -q -v ON_ERROR_STOP=1 \
    -U phoenix_rehearsal -d phoenix_rehearsal >/dev/null ||
  fail candidate_migrations_failed
schema_contract=$(
  /usr/bin/docker exec "$database_container" \
    psql -X -qAt -v ON_ERROR_STOP=1 \
      -U phoenix_rehearsal -d phoenix_rehearsal \
      -c "SELECT count(*) FROM live_canary.schema_contract"
) || fail candidate_schema_contract_failed
[ "$schema_contract" -gt 0 ] || fail candidate_schema_contract_missing
cleanup_database
database_container=
database_network=

compose() {
  python3 "$compose_runner" \
    --mode LIVE \
    --env-file "$env_file" \
    --release-env "$release_env" \
    --compose-file "$compose_file" \
    --overlay-file "$overlay_file" \
    --project-directory "$deploy_dir" \
    -- "$@"
}

compose config --quiet || fail candidate_compose_config_failed
compose run --rm --no-deps autonomous-control status >/dev/null ||
  fail candidate_control_status_failed

# Prove the candidate dashboard query against the live schema in a read-only
# transaction. psql receives the reviewed SQL over stdin; no host path is
# mounted into a long-running Production container.
{
  printf '%s\n' 'BEGIN TRANSACTION READ ONLY;'
  cat "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql"
  printf '%s\n' 'ROLLBACK;'
} | compose exec -T postgres /bin/sh -c \
  'exec psql -X -q -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
  >"$state_dir/sql.stdout" 2>"$state_dir/sql.stderr" ||
  fail candidate_sql_or_schema_failed

# Run the complete candidate monitor entrypoint with an isolated writable
# output directory. The Production database mount remains read-only to the
# monitor and the existing output inode cannot be replaced.
monitor_container=$(
  compose run -d --no-deps \
  --name "phoenix-release-rehearsal-monitor-$short_sha-$$" \
  -e PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS=30 \
  -v "$candidate_root/scripts/economic-dashboard-loop.sh:/opt/phoenix/economic-dashboard-loop.sh:ro" \
  -v "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql:/opt/phoenix/economic-dashboard-snapshot.sql:ro" \
  -v "$monitor_output:/evidence" \
  economic-monitor
) ||
  fail candidate_monitor_failed
case "$monitor_container" in
  *[!0-9a-f]*|"") fail candidate_monitor_id_invalid ;;
esac
/usr/bin/docker inspect --format '{{json .Mounts}}' "$monitor_container" \
  >"$state_dir/monitor-mounts.json" ||
  fail candidate_monitor_mount_inspect_failed
python3 -I -B - \
  "$state_dir/monitor-mounts.json" \
  "$candidate_root/scripts/economic-dashboard-loop.sh" \
  "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql" \
  "$monitor_output" <<'PY' ||
import json
import sys
from pathlib import Path

mounts = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
observed = {
    item["Destination"]: Path(item["Source"]).resolve()
    for item in mounts
    if isinstance(item, dict)
}
expected = {
    "/opt/phoenix/economic-dashboard-loop.sh": Path(sys.argv[2]).resolve(),
    "/opt/phoenix/economic-dashboard-snapshot.sql": Path(sys.argv[3]).resolve(),
    "/evidence": Path(sys.argv[4]).resolve(),
}
if observed != expected:
    raise SystemExit(1)
PY
  fail candidate_monitor_mount_mismatch
host_sql_inode=$(stat -c '%i' "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql")
container_sql_inode=$(
  /usr/bin/docker exec "$monitor_container" \
    stat -c '%i' /opt/phoenix/economic-dashboard-snapshot.sql
) || fail candidate_monitor_sql_inode_unavailable
[ "$container_sql_inode" = "$host_sql_inode" ] ||
  fail candidate_monitor_sql_inode_mismatch
deadline=$(( $(date +%s) + 75 ))
monitor_health=
while [ "$(date +%s)" -lt "$deadline" ]; do
  monitor_health=$(
    /usr/bin/docker inspect \
      --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
      "$monitor_container" 2>/dev/null || true
  )
  [ "$monitor_health" = healthy ] && break
  sleep 2
done
[ "$monitor_health" = healthy ] || fail candidate_monitor_unhealthy

latest=$monitor_output/latest-dashboard.json
[ -f "$latest" ] && [ ! -L "$latest" ] ||
  fail candidate_monitor_output_missing
[ "$(stat -c '%u:%g' "$latest")" = 1000:1000 ] ||
  fail candidate_monitor_output_owner_invalid
python3 -I -B - "$latest" <<'PY' ||
import json
import sys
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if not isinstance(value, dict):
    raise SystemExit(1)
PY
  fail candidate_monitor_output_invalid
cleanup_monitor
monitor_container=

PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$release_env" \
PHOENIX_COMPOSE_FILE="$compose_file" \
PHOENIX_COMPOSE_OVERLAY_FILE="$overlay_file" \
PHOENIX_COMPOSE_PROJECT_DIRECTORY="$deploy_dir" \
PHOENIX_COMPOSE_RUNNER="$compose_runner" \
PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE \
  "$candidate_root/scripts/production-healthcheck.sh" >/dev/null ||
  fail candidate_health_contract_failed

printf '%s\n' \
  "{\"schema\":\"phoenix.release-rehearsal.v1\",\"release_sha\":\"$release_sha\",\"status\":\"passed\"}"
