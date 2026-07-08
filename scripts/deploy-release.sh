#!/usr/bin/env sh
set -eu

release_sha="${1:-}"
deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
manifest="$deploy_dir/manifests/$release_sha.json"
release_env="$deploy_dir/manifests/$release_sha.env"
current_file="$deploy_dir/current-release"
previous_file="$deploy_dir/previous-release"

fail() {
  echo "DEPLOY_FAILED: $1"
  exit 1
}

case "$release_sha" in
  *[!0-9a-f]*|"") fail "release SHA must be 40 lowercase hex characters" ;;
esac
[ "${#release_sha}" -eq 40 ] || fail "release SHA must be 40 lowercase hex characters"
[ -f "$manifest" ] || fail "missing release manifest $manifest"
[ -f "$compose_file" ] || fail "missing production compose file $compose_file"

if [ -s "$current_file" ]; then
  cp "$current_file" "$previous_file"
fi

python3 - "$manifest" "$release_env" "$release_sha" <<'PY'
import json
import sys
from pathlib import Path

manifest_path, env_path, expected_sha = sys.argv[1:4]
manifest = json.loads(Path(manifest_path).read_text())
if manifest.get("release_sha") != expected_sha:
    raise SystemExit("manifest release_sha does not match requested SHA")
required = {
    "feed-ingestor": "FEED_INGESTOR_IMAGE",
    "phoenix-engine": "PHOENIX_ENGINE_IMAGE",
    "rpc-gateway": "RPC_GATEWAY_IMAGE",
    "recorder": "RECORDER_IMAGE",
    "dashboard": "DASHBOARD_IMAGE",
}
lines = []
for image_name, env_name in required.items():
    image = manifest.get("images", {}).get(image_name)
    if not image:
        raise SystemExit(f"manifest missing {image_name}")
    repository = image.get("repository", "")
    tag = image.get("tag", "")
    digest = image.get("digest", "")
    if tag != f"sha-{expected_sha}":
        raise SystemExit(f"{image_name} tag does not match requested SHA")
    if "latest" in tag or tag == "":
        raise SystemExit(f"{image_name} uses a mutable or empty tag")
    if not digest.startswith("sha256:"):
        raise SystemExit(f"{image_name} is missing an immutable digest")
    lines.append(f"{env_name}={repository}@{digest}")
Path(env_path).write_text("\n".join(lines) + "\n")
PY
chmod 0640 "$release_env"
cp "$release_env" "$deploy_dir/current-release.env"

"$deploy_dir/validate-production-env.sh" "$env_file"

compose() {
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$deploy_dir/current-release.env" docker compose --env-file "$env_file" --env-file "$deploy_dir/current-release.env" -f "$compose_file" "$@"
}

rollback_on_failure() {
  code="$?"
  if [ "$code" -ne 0 ]; then
    echo "DEPLOY_FAILED: invoking rollback"
    "$deploy_dir/rollback-release.sh" || echo "ROLLBACK_FAILED"
    exit "$code"
  fi
}
trap rollback_on_failure EXIT

compose config >/dev/null
compose pull
compose run --rm migration-runner
compose up -d --remove-orphans
"$deploy_dir/production-healthcheck.sh"

tmp="$(mktemp "$deploy_dir/current-release.XXXXXX")"
printf '%s\n' "$release_sha" > "$tmp"
mv "$tmp" "$current_file"
trap - EXIT
echo "DEPLOY_OK: $release_sha"
