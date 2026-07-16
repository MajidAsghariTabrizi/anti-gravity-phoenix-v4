#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
renderer=$script_dir/render-production-compose.sh
context_validator=$script_dir/validate-production-release-context.sh
helper=$script_dir/production_context.py
test_root=$(mktemp -d "${TMPDIR:-/tmp}/phoenix-production-context-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fail() {
  echo "production-compose-context-tests: $1" >&2
  exit 1
}

if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  python3() {
    python "$@"
  }
fi

release_sha=1111111111111111111111111111111111111111
provider_secret=provider-token-must-not-print
manifest=$test_root/release-manifest.json
release_env=$test_root/release.env
operator_env=$test_root/phoenix.env
compose_file=$test_root/compose.prod.yml
fake_compose=$test_root/fake-compose
compose_output=$test_root/compose-output.json
compose_log=$test_root/compose.log
rendered=$test_root/rendered.json
metadata=$test_root/metadata.json

cp "$repo_dir/compose.prod.yml" "$compose_file"

cat >"$manifest" <<EOF
{
  "schema": "phoenix.release.v1",
  "release_sha": "$release_sha",
  "created_at": "2026-07-15T00:00:00Z",
  "images": {
    "feed-ingestor": {"repository": "ghcr.io/majidasgharitabrizi/feed-ingestor", "tag": "sha-$release_sha", "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111"},
    "phoenix-engine": {"repository": "ghcr.io/majidasgharitabrizi/phoenix-engine", "tag": "sha-$release_sha", "digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222"},
    "rpc-gateway": {"repository": "ghcr.io/majidasgharitabrizi/rpc-gateway", "tag": "sha-$release_sha", "digest": "sha256:3333333333333333333333333333333333333333333333333333333333333333"},
    "recorder": {"repository": "ghcr.io/majidasgharitabrizi/recorder", "tag": "sha-$release_sha", "digest": "sha256:4444444444444444444444444444444444444444444444444444444444444444"},
    "dashboard": {"repository": "ghcr.io/majidasgharitabrizi/dashboard", "tag": "sha-$release_sha", "digest": "sha256:5555555555555555555555555555555555555555555555555555555555555555"}
  }
}
EOF

python3 "$helper" manifest-env \
  --manifest "$manifest" \
  --expected-sha "$release_sha" \
  --output "$release_env"

route_json=$(python3 -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1], encoding="utf-8")), separators=(",", ":")))' \
  "$repo_dir/fixtures/routes/weth_usdc_uniswap_v3.json")
reordered_route_json=$(printf '%s' "$route_json" | python3 -c '
import json, sys
value = json.load(sys.stdin)
value = [{key: route[key] for key in reversed(list(route))} for route in value]
print(json.dumps(value, separators=(",", ":")))
')
mismatched_route_json=$(printf '%s' "$route_json" | python3 -c '
import json, sys
value = json.load(sys.stdin)
value[0]["route_fingerprint"] += "-different"
print(json.dumps(value, separators=(",", ":")))
')

write_operator_env() {
  route_value=$1
  include_route=$2
  cat >"$operator_env" <<EOF
PHOENIX_ENV=production
PHOENIX_MODE=SHADOW
LIVE_EXECUTION=false
CHAIN_ID=42161
SIGNER_PRIVATE_KEY=
WALLET_ADDRESS=
EXECUTOR_ADDRESS=
RPC_PROVIDER_URLS=https://rpc.invalid/$provider_secret
RPC_STATE_REQUESTS_PER_MINUTE=12
EOF
  if [ "$include_route" = yes ]; then
    printf 'ENGINE_ROUTE_REGISTRY_JSON=%s\n' "$route_value" >>"$operator_env"
  fi
}

write_rendered() {
  rendered_route=$1
  budget=$2
  engine_image=${3:-ghcr.io/majidasgharitabrizi/phoenix-engine@sha256:2222222222222222222222222222222222222222222222222222222222222222}
  python3 - "$compose_output" "$rendered_route" "$budget" "$engine_image" <<'PY'
import json
import sys

output, route, budget, engine_image = sys.argv[1:]
images = {
    "nitro-feed-relay": "offchainlabs/nitro-node@sha256:ebc985e3b105980734630744981e1542001c22d74cba57509fe0d5ed8bb84c14",
    "nats": "nats@sha256:b83efabe3e7def1e0a4a31ec6e078999bb17c80363f881df35edc70fcb6bb927",
    "postgres": "postgres@sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777",
    "migration-runner": "ghcr.io/majidasgharitabrizi/feed-ingestor@sha256:1111111111111111111111111111111111111111111111111111111111111111",
    "rpc-gateway": "ghcr.io/majidasgharitabrizi/rpc-gateway@sha256:3333333333333333333333333333333333333333333333333333333333333333",
    "feed-ingestor": "ghcr.io/majidasgharitabrizi/feed-ingestor@sha256:1111111111111111111111111111111111111111111111111111111111111111",
    "phoenix-engine": engine_image,
    "shadow-dispatcher": "ghcr.io/majidasgharitabrizi/recorder@sha256:4444444444444444444444444444444444444444444444444444444444444444",
    "recorder": "ghcr.io/majidasgharitabrizi/recorder@sha256:4444444444444444444444444444444444444444444444444444444444444444",
    "dashboard": "ghcr.io/majidasgharitabrizi/dashboard@sha256:5555555555555555555555555555555555555555555555555555555555555555",
    "prometheus": "prom/prometheus@sha256:075b1ba2c4ebb04bc3a6ab86c06ec8d8099f8fda1c96ef6d104d9bb1def1d8bc",
}
services = {name: {"image": image} for name, image in images.items()}
safety = {
    "CHAIN_ID": "42161",
    "ENGINE_ROUTE_REGISTRY_JSON": route,
    "EXECUTOR_ADDRESS": "",
    "LIVE_EXECUTION": "false",
    "PHOENIX_MODE": "SHADOW",
    "SIGNER_PRIVATE_KEY": "",
    "WALLET_ADDRESS": "",
}
services["phoenix-engine"]["environment"] = safety
services["shadow-dispatcher"]["environment"] = {
    key: value for key, value in safety.items() if key != "ENGINE_ROUTE_REGISTRY_JSON" and key != "CHAIN_ID"
}
services["rpc-gateway"]["environment"] = {"RPC_STATE_REQUESTS_PER_MINUTE": budget}
for service in ("nitro-feed-relay", "nats", "postgres", "migration-runner", "feed-ingestor", "recorder", "dashboard", "prometheus"):
    services[service]["environment"] = {}
with open(output, "w", encoding="utf-8", newline="\n") as handle:
    json.dump({"services": services}, handle, sort_keys=True, separators=(",", ":"))
    handle.write("\n")
PY
}

cat >"$fake_compose" <<'SH'
#!/usr/bin/env sh
set -eu
fake_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf '%s\n' "$*" >>"$fake_dir/compose.log"
cat "$fake_dir/compose-output.json"
SH
chmod +x "$fake_compose"

render() {
  : >"$compose_log"
  PHOENIX_COMPOSE_BIN="$fake_compose" COMPOSE_FILE="$test_root/rogue.yml" \
    "$renderer" \
      --compose-file "$compose_file" \
      --env-file "$operator_env" \
      --release-env "$release_env" \
      --release-manifest "$manifest" \
      --output "$rendered" \
      --metadata-output "$metadata"
}

expect_render_failure() {
  expected_code=$1
  output_file=$test_root/failure.out
  if render >"$output_file" 2>&1; then
    fail "expected $expected_code"
  fi
  grep -F "\"code\":\"$expected_code\"" "$output_file" >/dev/null ||
    fail "$expected_code was not explicit"
  if grep -F "$provider_secret" "$output_file" >/dev/null; then
    fail "$expected_code printed a provider URL"
  fi
}

write_operator_env "$route_json" yes
write_rendered "$route_json" 12
input_checksums_before=$(cksum "$operator_env" "$release_env" "$manifest")
render_stdout=$(render 2>&1) || fail "valid production render failed"
input_checksums_after=$(cksum "$operator_env" "$release_env" "$manifest")
[ "$input_checksums_before" = "$input_checksums_after" ] || fail "renderer rewrote an input file"
printf '%s' "$render_stdout" | python3 -c '
import json, sys
value = json.load(sys.stdin)
assert value["status"] == "ok"
assert value["mode"] == "SHADOW"
assert value["live_execution"] is False
assert value["rpc_state_requests_per_minute"] == 12
' || fail "renderer output is not bounded machine-readable JSON"
if printf '%s' "$render_stdout" | grep -F "$provider_secret" >/dev/null; then
  fail "renderer printed a provider URL"
fi
expected_invocation="--env-file $operator_env --env-file $release_env -f $compose_file config --format json"
grep -Fx -- "$expected_invocation" "$compose_log" >/dev/null || fail "production Compose selection is not exact"
if grep -F 'rogue.yml' "$compose_log" >/dev/null; then
  fail "inherited Compose override changed production selection"
fi
if grep -E '(^| )(up|start|run|pull|restart|stop|down)( |$)' "$compose_log" >/dev/null; then
  fail "renderer touched runtime services"
fi
grep -F '"ENGINE_ROUTE_REGISTRY_JSON"' "$rendered" >/dev/null || fail "route registry is absent from rendered Engine environment"

operator_checksum=$(cksum "$operator_env")
: >"$compose_log"
if output=$(PHOENIX_COMPOSE_BIN="$fake_compose" "$renderer" \
  --compose-file "$compose_file" \
  --env-file "$operator_env" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --output "$operator_env" \
  --metadata-output "$test_root/conflict-metadata.json" 2>&1); then
  fail "renderer allowed an output path to overwrite its operator env input"
fi
printf '%s' "$output" | grep -F '"code":"PRODUCTION_OUTPUT_PATH_CONFLICT"' >/dev/null ||
  fail "output/input path conflict was not explicit"
[ "$operator_checksum" = "$(cksum "$operator_env")" ] || fail "output conflict rewrote the operator env"
[ ! -s "$compose_log" ] || fail "output conflict reached Compose rendering"

manifest_only_render=$test_root/manifest-only-rendered.json
manifest_only_metadata=$test_root/manifest-only-metadata.json
PHOENIX_COMPOSE_BIN="$fake_compose" "$renderer" \
  --compose-file "$compose_file" \
  --env-file "$operator_env" \
  --release-manifest "$manifest" \
  --output "$manifest_only_render" \
  --metadata-output "$manifest_only_metadata" >/dev/null ||
  fail "manifest-only release-state selection failed"

first_hash=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["route_registry_hash"])' "$metadata")
write_operator_env "$reordered_route_json" yes
write_rendered "$reordered_route_json" 12
render >/dev/null || fail "reordered canonical route render failed"
second_hash=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["route_registry_hash"])' "$metadata")
[ "$first_hash" = "$second_hash" ] || fail "route hash is not canonical and deterministic"

write_operator_env "$route_json" no
write_rendered "$route_json" 12
expect_render_failure ROUTE_REGISTRY_MISSING

write_operator_env '[{bad}]' yes
write_rendered "$route_json" 12
expect_render_failure ROUTE_REGISTRY_INVALID_JSON

write_operator_env '[]' yes
write_rendered '[]' 12
expect_render_failure ROUTE_REGISTRY_EMPTY

write_operator_env "$route_json" yes
write_rendered "$mismatched_route_json" 12
expect_render_failure ROUTE_REGISTRY_RENDER_MISMATCH

invalid_route_json=$(printf '%s' "$route_json" | python3 -c '
import json, sys
value = json.load(sys.stdin)
value[0]["route_id"] = "invalid route id"
print(json.dumps(value, separators=(",", ":")))
')
write_operator_env "$invalid_route_json" yes
write_rendered "$invalid_route_json" 12
expect_render_failure ROUTE_REGISTRY_INVALID
write_operator_env "$route_json" yes

for budget in 2 11; do
  write_rendered "$route_json" "$budget"
  expect_render_failure RPC_STATE_BUDGET_TOO_LOW
done
for budget in 12 13; do
  write_rendered "$route_json" "$budget"
  render >/dev/null || fail "RPC budget $budget was rejected"
done

write_rendered "$route_json" 12 app-phoenix-engine
expect_render_failure LOCAL_IMAGE_FALLBACK

for safety_case in \
  'SIGNER_PRIVATE_KEY=forbidden|SIGNER_MUST_BE_EMPTY' \
  'WALLET_ADDRESS=forbidden|WALLET_MUST_BE_EMPTY' \
  'EXECUTOR_ADDRESS=forbidden|EXECUTOR_MUST_BE_EMPTY' \
  'CHAIN_ID=1|CHAIN_ID_MISMATCH' \
  'PHOENIX_MODE=LIVE|SHADOW_MODE_REQUIRED' \
  'LIVE_EXECUTION=true|LIVE_EXECUTION_MUST_BE_FALSE'
do
  safety_assignment=${safety_case%%|*}
  safety_code=${safety_case#*|}
  write_operator_env "$route_json" yes
  printf '%s\n' "$safety_assignment" >>"$operator_env"
  write_rendered "$route_json" 12
  expect_render_failure "$safety_code"
done
write_operator_env "$route_json" yes

saved_manifest=$manifest
manifest=$test_root/missing-manifest.json
write_rendered "$route_json" 12
expect_render_failure RELEASE_MANIFEST_MISSING
manifest=$saved_manifest

bad_release_env=$test_root/bad-release.env
sed 's/sha256:222222/sha256:999999/' "$release_env" >"$bad_release_env"
mv "$release_env" "$test_root/good-release.env"
release_env=$bad_release_env
write_rendered "$route_json" 12
expect_render_failure RELEASE_IMAGE_MISMATCH
release_env=$test_root/good-release.env

write_rendered "$route_json" 12
render >/dev/null || fail "valid render before active-context test failed"
release_state=$test_root/release-state.json
current_release=$test_root/current-release
running_images=$test_root/running-images.json
context_result=$test_root/context-result.json
context_rendered=$test_root/context-rendered.json
context_metadata=$test_root/context-metadata.json
python3 "$helper" write-state \
  --manifest "$manifest" \
  --release-env "$release_env" \
  --render-metadata "$metadata" \
  --compose-config "$rendered" \
  --output "$release_state"
printf '%s\n' "$release_sha" >"$current_release"
python3 - "$metadata" "$running_images" <<'PY'
import json
import sys

metadata = json.load(open(sys.argv[1], encoding="utf-8"))
running = {}
for service, image in metadata["images"].items():
    if service == "migration-runner":
        continue
    running[service] = {"configured_image": image, "image_id": "sha256:" + "a" * 64}
with open(sys.argv[2], "w", encoding="utf-8", newline="\n") as handle:
    json.dump({"schema": "phoenix.running-images.v1", "services": running}, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
PHOENIX_COMPOSE_BIN="$fake_compose" "$context_validator" \
  --compose-file "$compose_file" \
  --env-file "$operator_env" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --current-release "$current_release" \
  --release-state "$release_state" \
  --running-images-file "$running_images" \
  --rendered-output "$context_rendered" \
  --metadata-output "$context_metadata" \
  --output "$context_result" >/dev/null || fail "valid active release context failed"

bad_release_state=$test_root/bad-release-state.json
python3 - "$release_state" "$bad_release_state" <<'PY'
import json
import sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
value["route_registry_hash"] = "sha256:" + "0" * 64
with open(sys.argv[2], "w", encoding="utf-8", newline="\n") as handle:
    json.dump(value, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
if output=$(PHOENIX_COMPOSE_BIN="$fake_compose" "$context_validator" \
  --compose-file "$compose_file" \
  --env-file "$operator_env" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --current-release "$current_release" \
  --release-state "$bad_release_state" \
  --running-images-file "$running_images" \
  --rendered-output "$context_rendered" \
  --metadata-output "$context_metadata" \
  --output "$context_result" 2>&1); then
  fail "route hash mismatch passed active release validation"
fi
printf '%s' "$output" | grep -F '"code":"ROUTE_REGISTRY_HASH_MISMATCH"' >/dev/null ||
  fail "route hash mismatch was not explicit"

python3 - "$running_images" <<'PY'
import json
import sys
path = sys.argv[1]
value = json.load(open(path, encoding="utf-8"))
value["services"]["phoenix-engine"]["configured_image"] = "ghcr.io/majidasgharitabrizi/phoenix-engine@sha256:" + "f" * 64
with open(path, "w", encoding="utf-8", newline="\n") as handle:
    json.dump(value, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
if output=$(PHOENIX_COMPOSE_BIN="$fake_compose" "$context_validator" \
  --compose-file "$compose_file" \
  --env-file "$operator_env" \
  --release-env "$release_env" \
  --release-manifest "$manifest" \
  --current-release "$current_release" \
  --release-state "$release_state" \
  --running-images-file "$running_images" \
  --rendered-output "$context_rendered" \
  --metadata-output "$context_metadata" \
  --output "$context_result" 2>&1); then
  fail "running image mismatch passed"
fi
printf '%s' "$output" | grep -F '"code":"RUNNING_IMAGE_MISMATCH"' >/dev/null ||
  fail "running image mismatch was not explicit"

for generated in \
  deploy/current-release \
  deploy/current-release.env \
  deploy/current-release.json \
  deploy/current-release-context.json \
  deploy/previous-release \
  deploy/release-manifest.json \
  deploy/manifests/test.json \
  deploy/.runtime/test.tmp \
  current-release.env
do
  git -C "$repo_dir" check-ignore -q "$generated" || fail "generated release state is not ignored: $generated"
done

forbidden_repo=$test_root/forbidden-repo
mkdir -p "$forbidden_repo"
git -C "$forbidden_repo" init -q
: >"$forbidden_repo/FETCH_HEAD"
if (cd "$forbidden_repo" && sh "$script_dir/forbidden-file-check.sh" >/dev/null 2>&1); then
  fail "root FETCH_HEAD passed the POSIX forbidden-file scan"
fi
grep -F "Pattern = '^FETCH_HEAD$'" "$script_dir/forbidden-file-check.ps1" >/dev/null ||
  fail "PowerShell forbidden-file scan does not reject root FETCH_HEAD"

if grep -F -- '--remove-orphans' "$script_dir/deploy-release.sh" "$script_dir/rollback-release.sh" >/dev/null; then
  fail "release scripts retain broad remove-orphans"
fi
for release_script in "$script_dir/deploy-release.sh" "$script_dir/rollback-release.sh"; do
  grep -F "trap 'exit 1' HUP INT TERM" "$release_script" >/dev/null ||
    fail "release script can continue after an interrupt: $release_script"
  if grep -E "^trap [^-].*EXIT HUP INT TERM$" "$release_script" >/dev/null; then
    fail "release script conflates cleanup and interrupt handling: $release_script"
  fi
done
for installed_script in \
  production_context.py \
  render-production-compose.sh \
  validate-production-release-context.sh \
  shadow-profitability-report.sh \
  shadow_profitability_report.py \
  shadow-profitability-report.sql \
  shadow-route-discovery.sh \
  shadow_route_discovery.py \
  shadow-route-discovery-enrichment.sql \
  prelive-money-path-report.sh \
  prelive_money_path_report.py \
  prelive-money-path-report.sql \
  prelive-money-path-summary.schema.json \
  arbitrum_uniswap_v3_pool_proofs.json
do
  grep -F "$installed_script" "$script_dir/bootstrap-production.sh" >/dev/null ||
    fail "bootstrap does not install $installed_script"
done

sh "$script_dir/validate-production-env-tests.sh" >/dev/null || fail "production env validator tests failed"

echo "production-compose-context-tests: ok"
