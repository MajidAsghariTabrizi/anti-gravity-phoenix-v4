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
  "$candidate_root/scripts/production_mode.py" \
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
candidate_evidence_env=$state_dir/candidate-evidence.env
rendered=$state_dir/candidate-compose.json
metadata=$state_dir/candidate-render.json
monitor_output=$state_dir/monitor
mkdir -m 0700 "$monitor_output"
chown 1000:1000 "$monitor_output"
monitor_container=
control_container=
database_container=
database_network=
cleanup_monitor() {
  [ -z "$monitor_container" ] ||
    /usr/bin/docker rm -f -v "$monitor_container" >/dev/null 2>&1 || true
}
cleanup_database() {
  [ -z "$database_container" ] ||
    /usr/bin/docker rm -f "$database_container" >/dev/null 2>&1 || true
  [ -z "$database_network" ] ||
    /usr/bin/docker network rm "$database_network" >/dev/null 2>&1 || true
}
cleanup_control() {
  [ -z "$control_container" ] ||
    /usr/bin/docker rm -f "$control_container" >/dev/null 2>&1 || true
}
cleanup_all() {
  cleanup_monitor
  cleanup_control
  cleanup_database
  cleanup
}
trap cleanup_all EXIT

python3 "$candidate_root/scripts/production_context.py" manifest-env \
  --manifest "$release_manifest" \
  --expected-sha "$release_sha" \
  --route-registry "$candidate_root/fixtures/routes/weth_usdc_uniswap_v3.json" \
  --output "$release_env" ||
  fail candidate_manifest_invalid

cp "$env_file" "$candidate_evidence_env" ||
  fail candidate_evidence_environment_copy_failed
chmod 0600 "$candidate_evidence_env"
[ "$(stat -c '%u:%g:%a:%h' "$candidate_evidence_env")" = 0:0:600:1 ] ||
  fail candidate_evidence_environment_metadata_invalid
python3 "$candidate_root/scripts/production_mode.py" shadow \
  --env-file "$candidate_evidence_env" ||
  fail candidate_evidence_environment_invalid

"$candidate_root/scripts/render-production-compose.sh" \
  --compose-file "$compose_file" \
  --overlay-file "$overlay_file" \
  --expected-mode DISARMED_EVIDENCE \
  --env-file "$candidate_evidence_env" \
  --release-env "$release_env" \
  --release-manifest "$release_manifest" \
  --output "$rendered" \
  --metadata-output "$metadata" >/dev/null ||
  fail candidate_compose_render_failed
rm -f "$candidate_evidence_env"

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

monitor_image=$(
  python3 -I -B - "$rendered" <<'PY'
import sys
from pathlib import Path
import json
import re

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
service = value["services"]["economic-monitor"]
image = service.get("image")
if not isinstance(image, str) or re.fullmatch(
    r"[a-z0-9._/-]+@sha256:[0-9a-f]{64}", image
) is None:
    raise SystemExit(1)
expected = {
    "entrypoint": ["/bin/sh", "/opt/phoenix/economic-dashboard-loop.sh"],
    "user": "1000:1000",
    "read_only": True,
    "cap_drop": ["ALL"],
    "security_opt": ["no-new-privileges:true"],
}
for key, expected_value in expected.items():
    if service.get(key) != expected_value:
        raise SystemExit(1)
healthcheck = service.get("healthcheck")
if not isinstance(healthcheck, dict) or healthcheck.get("test") != [
    "CMD-SHELL",
    "test -s /evidence/latest-dashboard.json",
]:
    raise SystemExit(1)
environment = service.get("environment")
if not isinstance(environment, dict) or any(
    environment.get(key) != expected_value
    for key, expected_value in {
        "PHOENIX_ECONOMIC_DASHBOARD_SQL": (
            "/opt/phoenix/economic-dashboard-snapshot.sql"
        ),
        "PHOENIX_ECONOMIC_DASHBOARD_OUTPUT": "/evidence/latest-dashboard.json",
    }.items()
):
    raise SystemExit(1)
print(image)
PY
) || fail candidate_monitor_contract_invalid
[ "$monitor_image" = "$postgres_image" ] || fail candidate_monitor_image_invalid

control_image=$(
  python3 -I -B - "$rendered" <<'PY'
import json
import re
import sys
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
service = value["services"]["autonomous-control"]
image = service.get("image")
if not isinstance(image, str) or re.fullmatch(
    r"[a-z0-9._/-]+@sha256:[0-9a-f]{64}", image
) is None:
    raise SystemExit(1)
expected = {
    "entrypoint": ["/usr/local/bin/autonomous-live-control"],
    "user": "65532:65532",
    "read_only": True,
    "cap_drop": ["ALL"],
    "security_opt": ["no-new-privileges:true"],
}
if any(service.get(key) != expected_value for key, expected_value in expected.items()):
    raise SystemExit(1)
print(image)
PY
) || fail candidate_control_contract_invalid

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

compose() {
  /usr/bin/timeout --signal=TERM --kill-after=2s 45s \
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
control_container="phoenix-release-rehearsal-control-$short_sha-$$"
/usr/bin/timeout --signal=TERM --kill-after=2s 30s \
  /usr/bin/docker run --rm \
    --name "$control_container" \
    --network "$database_network" \
    --init \
    --user 65532:65532 \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m \
    -e "POSTGRES_DSN=postgres://phoenix_rehearsal:phoenix_rehearsal_only@$database_container:5432/phoenix_rehearsal" \
    --entrypoint /usr/local/bin/autonomous-live-control \
    "$control_image" status >"$state_dir/control-status.json" ||
  fail candidate_control_status_failed
control_container=
python3 -I -B - "$state_dir/control-status.json" <<'PY' ||
import json
import sys
from pathlib import Path

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
global_control = value.get("global")
if (
    value.get("schema") != "phoenix.autonomous-live-status.v2"
    or not isinstance(global_control, dict)
    or global_control.get("armed") is not False
    or global_control.get("kill_switch") is not True
    or global_control.get("execution_mode") != "disabled"
):
    raise SystemExit(1)
PY
  fail candidate_control_status_invalid

# Prove the candidate dashboard query against the fully migrated isolated
# schema in a read-only transaction. The probe must not depend on or inspect
# the live Production database.
cat "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql" | \
  /usr/bin/timeout --signal=TERM --kill-after=2s 45s \
  /usr/bin/docker exec -i \
    -e 'PGOPTIONS=-c statement_timeout=30000 -c lock_timeout=5000' \
    "$database_container" \
    psql -X -q -v ON_ERROR_STOP=1 \
      -U phoenix_rehearsal -d phoenix_rehearsal \
  >"$state_dir/sql.stdout" 2>"$state_dir/sql.stderr" ||
  fail candidate_sql_or_schema_failed

# Run the complete candidate monitor entrypoint with an isolated writable
# output directory and the already-migrated tmpfs PostgreSQL fixture. The
# rehearsal must never depend on the live Production DSN: that dependency can
# hang before the loop emits evidence and does not prove the candidate schema.
monitor_container=$(
  /usr/bin/docker run -d \
    --name "phoenix-release-rehearsal-monitor-$short_sha-$$" \
    --network "$database_network" \
    --user 1000:1000 \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m \
    --health-cmd 'test -s /evidence/latest-dashboard.json' \
    --health-interval 45s \
    --health-timeout 3s \
    --health-retries 3 \
    -e "POSTGRES_DSN=postgres://phoenix_rehearsal:phoenix_rehearsal_only@$database_container:5432/phoenix_rehearsal" \
    -e PGCONNECT_TIMEOUT=5 \
    -e 'PGOPTIONS=-c statement_timeout=60000 -c lock_timeout=5000' \
    -e PHOENIX_ECONOMIC_DASHBOARD_INTERVAL_SECONDS=30 \
    -e PHOENIX_ECONOMIC_DASHBOARD_SQL=/opt/phoenix/economic-dashboard-snapshot.sql \
    -e PHOENIX_ECONOMIC_DASHBOARD_OUTPUT=/evidence/latest-dashboard.json \
    -v "$candidate_root/scripts/economic-dashboard-loop.sh:/opt/phoenix/economic-dashboard-loop.sh:ro" \
    -v "$candidate_root/scripts/sql/economic-dashboard-snapshot.sql:/opt/phoenix/economic-dashboard-snapshot.sql:ro" \
    -v "$monitor_output:/evidence" \
    --entrypoint /bin/sh \
    "$monitor_image" \
    /opt/phoenix/economic-dashboard-loop.sh
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
import re
import sys
from pathlib import Path

mounts = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
observed = {
    item["Destination"]: item
    for item in mounts
    if isinstance(item, dict)
}
expected = {
    "/opt/phoenix/economic-dashboard-loop.sh": (
        Path(sys.argv[2]).resolve(),
        False,
    ),
    "/opt/phoenix/economic-dashboard-snapshot.sql": (
        Path(sys.argv[3]).resolve(),
        False,
    ),
    "/evidence": (Path(sys.argv[4]).resolve(), True),
}
image_volume = "/var/lib/postgresql/data"
if (
    len(observed) != len(mounts)
    or set(observed) != set(expected) | {image_volume}
):
    raise SystemExit(1)
for destination, (source, writable) in expected.items():
    item = observed[destination]
    if (
        item.get("Type") != "bind"
        or Path(item.get("Source", "")).resolve() != source
        or item.get("RW") is not writable
    ):
        raise SystemExit(1)
volume = observed[image_volume]
if (
    volume.get("Type") != "volume"
    or volume.get("RW") is not True
    or re.fullmatch(
        r"/var/lib/docker/volumes/[0-9a-f]{64}/_data",
        str(volume.get("Source", "")),
    )
    is None
):
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
log_monitor_diagnostics() {
  /usr/bin/docker inspect --format \
    'state={{.State.Status}} exit={{.State.ExitCode}} oom={{.State.OOMKilled}} health={{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
    "$monitor_container" >&2 || true
  /usr/bin/docker inspect --format \
    '{{if .State.Health}}{{json .State.Health.Log}}{{else}}[]{{end}}' \
    "$monitor_container" >&2 || true
  /usr/bin/docker logs --tail 20 "$monitor_container" >&2 || true
}
monitor_started_at=$(date +%s)
deadline=$(( monitor_started_at + 180 ))
monitor_health=
while [ "$(date +%s)" -lt "$deadline" ]; do
  monitor_health=$(
    /usr/bin/docker inspect \
      --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
      "$monitor_container" 2>/dev/null || true
  )
  [ "$monitor_health" = healthy ] && break
  monitor_state=$(
    /usr/bin/docker inspect \
      --format '{{.State.Status}}:{{.State.ExitCode}}' \
      "$monitor_container" 2>/dev/null || true
  )
  case "$monitor_state" in
    exited:*|dead:*)
      log_monitor_diagnostics
      fail candidate_monitor_exited
      ;;
  esac
  sleep 2
done
if [ "$monitor_health" != healthy ]; then
  log_monitor_diagnostics
  fail candidate_monitor_unhealthy
fi
monitor_healthy_seconds=$(( $(date +%s) - monitor_started_at ))

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
cleanup_database
database_container=
database_network=

# Exercise the candidate health implementation against the immutable active
# release topology. The active fail-closed runtime can be SHADOW after rollback
# or LIVE with its executor stopped before candidate mutation begins.
active_health_mode=$(awk -F= '
  $1 == "PHOENIX_MODE" { print $2; found += 1 }
  END { if (found != 1) exit 1 }
' "$env_file") || fail active_health_mode_invalid
case "$active_health_mode" in
  SHADOW) ;;
  LIVE) active_health_mode=DISARMED_EVIDENCE ;;
  *) fail active_health_mode_invalid ;;
esac
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$active_release_env" \
PHOENIX_COMPOSE_FILE="$compose_file" \
PHOENIX_COMPOSE_OVERLAY_FILE="$overlay_file" \
PHOENIX_COMPOSE_PROJECT_DIRECTORY="$deploy_dir" \
PHOENIX_COMPOSE_RUNNER="$compose_runner" \
PHOENIX_HEALTH_EXPECTED_MODE="$active_health_mode" \
PHOENIX_HEALTH_RETRIES=1 \
PHOENIX_HEALTH_SLEEP_SECONDS=0 \
PHOENIX_HEALTH_COMMAND_TIMEOUT_SECONDS=15 \
  "$candidate_root/scripts/production-healthcheck.sh" ||
  fail candidate_health_contract_failed

printf '%s\n' \
  "{\"schema\":\"phoenix.release-rehearsal.v1\",\"release_sha\":\"$release_sha\",\"status\":\"passed\",\"monitor_healthy_seconds\":$monitor_healthy_seconds}"
