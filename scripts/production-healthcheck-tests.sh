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
mkdir -p "$deploy_dir" "$fake_bin"
: >"$env_file"
: >"$release_env"
: >"$deploy_dir/compose.prod.yml"
: >"$docker_log"

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

echo 'production-healthcheck-tests: ok'
