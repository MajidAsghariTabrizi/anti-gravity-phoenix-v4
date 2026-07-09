#!/usr/bin/env sh
set -eu

deploy_root="${PHOENIX_DEPLOY_ROOT:-/opt/phoenix}"
deploy_dir="$deploy_root/deploy"
env_file="${PHOENIX_ENV_FILE:-/etc/phoenix/phoenix.env}"
compose_file="$deploy_dir/compose.prod.yml"
previous_file="$deploy_dir/previous-release"

[ -s "$previous_file" ] || { echo "ROLLBACK_FAILED: previous-release is missing"; exit 1; }
release_sha="$(tr -d '\r\n' < "$previous_file")"
case "$release_sha" in
  *[!0-9a-f]*|"") echo "ROLLBACK_FAILED: previous release SHA is invalid"; exit 1 ;;
esac
[ "${#release_sha}" -eq 40 ] || { echo "ROLLBACK_FAILED: previous release SHA is invalid"; exit 1; }

manifest="$deploy_dir/manifests/$release_sha.json"
release_env="$deploy_dir/manifests/$release_sha.env"
[ -f "$manifest" ] || { echo "ROLLBACK_FAILED: missing manifest $manifest"; exit 1; }

python3 - "$manifest" "$release_env" "$release_sha" <<'PY'
import json
import sys
from pathlib import Path

manifest_path, env_path, expected_sha = sys.argv[1:4]
manifest = json.loads(Path(manifest_path).read_text())
if manifest.get("release_sha") != expected_sha:
    raise SystemExit("manifest release_sha does not match rollback SHA")
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
    digest = image.get("digest", "")
    if image.get("tag") != f"sha-{expected_sha}" or not digest.startswith("sha256:"):
        raise SystemExit(f"{image_name} does not use the exact rollback SHA digest")
    lines.append(f"{env_name}={image['repository']}@{digest}")
Path(env_path).write_text("\n".join(lines) + "\n")
PY
chmod 0640 "$release_env"
cp "$release_env" "$deploy_dir/current-release.env"

"$deploy_dir/validate-production-env.sh" "$env_file"

compose() {
  PHOENIX_ENV_FILE="$env_file" PHOENIX_RELEASE_ENV="$deploy_dir/current-release.env" docker compose --env-file "$env_file" --env-file "$deploy_dir/current-release.env" -f "$compose_file" "$@"
}

compose config >/dev/null
compose pull
compose up -d --remove-orphans
"$deploy_dir/production-healthcheck.sh"
printf '%s\n' "$release_sha" > "$deploy_dir/current-release"
echo "ROLLBACK_OK: $release_sha"
