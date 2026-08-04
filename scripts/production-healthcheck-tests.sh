#!/usr/bin/env sh
# Literal health contracts must retain their dollar signs.
# shellcheck disable=SC2016
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
healthcheck=$script_dir/production-healthcheck.sh
compose_file=$repo_root/compose.prod.yml

fail() {
  echo "production-healthcheck-tests: $1" >&2
  exit 1
}

listener_contract="grep -Eq ':25AA[[:space:]].*[[:space:]]0A[[:space:]]' /proc/net/tcp /proc/net/tcp6"
grep -F "$listener_contract" "$healthcheck" >/dev/null ||
  fail 'Nitro relay healthcheck does not use the reviewed port 9642 listener contract'
grep -F "$listener_contract" "$compose_file" >/dev/null ||
  fail 'script and Compose Nitro relay health contracts differ'
if grep -F '8547' "$healthcheck" >/dev/null ||
  grep -F 'livenessprobe' "$healthcheck" >/dev/null
then
  fail 'invalid Nitro relay HTTP health endpoint remains'
fi

tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-production-healthcheck.XXXXXX")
cleanup() {
  rm -rf -- "$tmp_root"
}
trap cleanup EXIT HUP INT TERM

deploy_root=$tmp_root/phoenix
deploy_dir=$deploy_root/deploy
fake_bin=$tmp_root/bin
docker_log=$tmp_root/docker.log
output=$tmp_root/output.log
env_file=$tmp_root/phoenix.env
release_env=$deploy_dir/current-release.env
release_sha=1111111111111111111111111111111111111111
mkdir -p "$deploy_dir/manifests" "$fake_bin"
: >"$env_file"
printf 'PHOENIX_RELEASE_SHA=%s\n' "$release_sha" >"$release_env"
: >"$deploy_dir/compose.prod.yml"
: >"$docker_log"
cp "$repo_root/release-components.json" "$deploy_dir/release-components.json"
cp "$script_dir/release_components.py" "$deploy_dir/release_components.py"
cat >"$deploy_dir/manifests/$release_sha.json" <<EOF
{"images":{"atlas-observer":{},"dashboard":{},"feed-ingestor":{},"fork-sandbox":{},"live-executor":{},"phoenix-engine":{},"recorder":{},"rpc-gateway":{}}}
EOF

cat >"$fake_bin/docker" <<'SH'
#!/usr/bin/env sh
set -eu
: "${PHOENIX_HEALTHCHECK_DOCKER_LOG:?}"
{
  printf 'docker'
  for argument in "$@"; do
    printf '<%s>' "$argument"
  done
  printf '\n'
} >>"$PHOENIX_HEALTHCHECK_DOCKER_LOG"
case " $* " in
  *' nitro-feed-relay '*)
    [ -z "${PHOENIX_HEALTHCHECK_DOCKER_SLEEP_SECONDS:-}" ] ||
      sleep "$PHOENIX_HEALTHCHECK_DOCKER_SLEEP_SECONDS"
    ;;
esac
SH
chmod 0755 "$fake_bin/docker"

PATH="$fake_bin:$PATH" \
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$release_env" \
PHOENIX_HEALTH_RETRIES=2 \
PHOENIX_HEALTH_SLEEP_SECONDS=0 \
PHOENIX_HEALTHCHECK_DOCKER_LOG="$docker_log" \
  /bin/sh "$healthcheck" >"$output"

grep -Fx 'HEALTH_OK: nitro-feed-relay' "$output" >/dev/null ||
  fail 'Nitro relay listener check did not succeed'
grep -Fx 'PRODUCTION_HEALTH_OK' "$output" >/dev/null ||
  fail 'production healthcheck did not complete'
[ "$(grep -c '<nitro-feed-relay>' "$docker_log")" -eq 1 ] ||
  fail 'Nitro relay healthcheck invocation count differs'
grep -F '<atlas-health><false>' "$docker_log" >/dev/null ||
  fail 'Production health did not default to the exact Atlas/Aave binary'
relay_call=$(grep '<nitro-feed-relay>' "$docker_log")
printf '%s\n' "$relay_call" | grep -F "$listener_contract" >/dev/null ||
  fail 'Nitro relay listener contract did not reach Compose execution'
case "$relay_call" in
  *'<wget>'*|*8547*|*livenessprobe*)
    fail 'Nitro relay healthcheck still invokes the invalid HTTP probe'
    ;;
esac

for unchanged_contract in \
  'pg_isready -U "$POSTGRES_USER" -d "$POSTGRES_DB"' \
  'http://127.0.0.1:8222/healthz' \
  'http://127.0.0.1:9300/readyz' \
  'http://127.0.0.1:9100/readyz' \
  'http://127.0.0.1:9200/readyz' \
  'http://127.0.0.1:9400/readyz' \
  'http://127.0.0.1:9090/-/ready' \
  'http://127.0.0.1:8501/_stcore/health' \
  '[ "$PHOENIX_MODE" = SHADOW ] && [ "$LIVE_EXECUTION" = false ]'
do
  grep -F "$unchanged_contract" "$docker_log" >/dev/null ||
    fail "existing health contract changed: $unchanged_contract"
done

cat >"$env_file" <<'EOF'
PHOENIX_MODE=LIVE
LIVE_EXECUTION=true
AUTONOMOUS_EXECUTION=true
EOF
cat >"$release_env" <<'EOF'
PHOENIX_RELEASE_SHA=1111111111111111111111111111111111111111
EOF
: >"$docker_log"
PHOENIX_MODE=SHADOW \
LIVE_EXECUTION=false \
AUTONOMOUS_EXECUTION=false \
PATH="$fake_bin:$PATH" \
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$release_env" \
PHOENIX_HEALTH_EXPECTED_MODE=LIVE \
PHOENIX_HEALTH_RETRIES=2 \
PHOENIX_HEALTH_SLEEP_SECONDS=0 \
PHOENIX_HEALTHCHECK_DOCKER_LOG="$docker_log" \
  /bin/sh "$healthcheck" >"$output"

for live_contract in \
  'HEALTH_OK: live-executor' \
  'HEALTH_OK: autonomous-live-mode' \
  'HEALTH_OK: autonomous-controls' \
  'HEALTH_OK: event-metrics'
do
  grep -Fx "$live_contract" "$output" >/dev/null ||
    fail "explicit LIVE expectation did not select health contract: $live_contract"
done
if grep -F 'HEALTH_OK: shadow-mode' "$output" >/dev/null; then
  fail 'inherited SHADOW mode overrode the explicit LIVE expectation'
fi
grep -F '/compose.live-autonomous.yml>' "$docker_log" >/dev/null ||
  fail 'LIVE healthcheck did not use the autonomous Compose overlay'
grep -F '<--profile><live-autonomous>' "$docker_log" >/dev/null ||
  fail 'LIVE healthcheck did not select the autonomous Compose profile'
grep -F '<live-executor></usr/local/bin/autonomous-live-control><status>' \
  "$docker_log" >/dev/null ||
  fail 'LIVE healthcheck did not inspect autonomous controls'

candidate_manifest=$tmp_root/candidate-manifest.json
cp "$deploy_dir/manifests/$release_sha.json" "$candidate_manifest"
rm -f "$deploy_dir/manifests/$release_sha.json"
cat >"$deploy_dir/release_components.py" <<'PY'
raise SystemExit("active release helper must not validate a candidate")
PY
: >"$docker_log"
PATH="$fake_bin:$PATH" \
PHOENIX_DEPLOY_ROOT="$deploy_root" \
PHOENIX_ENV_FILE="$env_file" \
PHOENIX_RELEASE_ENV="$release_env" \
PHOENIX_RELEASE_MANIFEST="$candidate_manifest" \
PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE \
PHOENIX_HEALTH_RETRIES=1 \
PHOENIX_HEALTH_SLEEP_SECONDS=0 \
PHOENIX_HEALTH_COMMAND_TIMEOUT_SECONDS=15 \
PHOENIX_HEALTHCHECK_DOCKER_LOG="$docker_log" \
  /bin/sh "$healthcheck" >"$output"
grep -Fx 'PRODUCTION_HEALTH_OK' "$output" >/dev/null ||
  fail 'explicit candidate release manifest did not pass'

if PATH="$fake_bin:$PATH" \
  PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_RELEASE_ENV="$release_env" \
  PHOENIX_RELEASE_MANIFEST="$candidate_manifest" \
  PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE \
  PHOENIX_HEALTH_RETRIES=1 \
  PHOENIX_HEALTH_SLEEP_SECONDS=0 \
  PHOENIX_HEALTH_COMMAND_TIMEOUT_SECONDS=1 \
  PHOENIX_HEALTHCHECK_DOCKER_SLEEP_SECONDS=3 \
  PHOENIX_HEALTHCHECK_DOCKER_LOG="$docker_log" \
    /bin/sh "$healthcheck" >"$output" 2>&1
then
  fail 'stalled Compose health command passed its bound'
fi
grep -Fx 'HEALTH_FAIL: nitro-feed-relay' "$output" >/dev/null ||
  fail 'stalled Compose health command did not fail at its named contract'

if PATH="$fake_bin:$PATH" \
  PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_RELEASE_ENV="$release_env" \
  PHOENIX_HEALTH_EXPECTED_MODE=INVALID \
  PHOENIX_HEALTHCHECK_DOCKER_LOG="$docker_log" \
    /bin/sh "$healthcheck" >"$output" 2>&1
then
  fail 'invalid explicit health mode passed'
fi
grep -Fx 'HEALTH_FAIL: invalid expected mode' "$output" >/dev/null ||
  fail 'invalid explicit health mode did not fail explicitly'

if PATH="$fake_bin:$PATH" \
  PHOENIX_DEPLOY_ROOT="$deploy_root" \
  PHOENIX_ENV_FILE="$env_file" \
  PHOENIX_RELEASE_ENV="$release_env" \
  PHOENIX_RELEASE_MANIFEST="$candidate_manifest" \
  PHOENIX_HEALTH_ALLOW_LEGACY_ATLAS_BINARY=invalid \
  PHOENIX_HEALTHCHECK_DOCKER_LOG="$docker_log" \
    /bin/sh "$healthcheck" >"$output" 2>&1
then
  fail 'invalid legacy Atlas allowance passed'
fi
grep -Fx 'HEALTH_FAIL: invalid legacy Atlas allowance' "$output" >/dev/null ||
  fail 'invalid legacy Atlas allowance did not fail explicitly'

echo 'production-healthcheck-tests: ok'
