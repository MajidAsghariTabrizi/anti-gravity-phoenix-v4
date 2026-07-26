#!/usr/bin/env sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)

gateway_installer=$repo_root/scripts/install-autonomous-live-deploy-gateway.sh
context_installer=$repo_root/scripts/install-production-release-context.sh
build_workflow=$repo_root/.github/workflows/build-images.yml
ci_workflow=$repo_root/.github/workflows/ci.yml
dockerfile=$repo_root/deploy/rust.Dockerfile
runtime_verifier=$repo_root/scripts/verify-image-runtime.sh
compose_contract=$repo_root/compose.live-autonomous.yml
control_source=$repo_root/live-executor/src/autonomous_live_control_main.rs

fail() {
  echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_FAILED: $1" >&2
  exit 1
}

for required in \
  "$gateway_installer" \
  "$context_installer" \
  "$build_workflow" \
  "$ci_workflow" \
  "$dockerfile" \
  "$runtime_verifier" \
  "$compose_contract" \
  "$control_source"
do
  [ -f "$required" ] && [ ! -L "$required" ] ||
    fail "required_file_missing:$required"
done

for specification in \
  'release_assets.py:0600' \
  'release_components.py:0600' \
  'release_provenance.py:0600' \
  'install-release-assets.sh:0700' \
  'install-production-release-context.sh:0700' \
  'production-healthcheck.sh:0700' \
  'production_mode.py:0700' \
  'prelive-protected-maintenance.sh:0700' \
  'prelive_protected_maintenance.py:0600' \
  'prelive-protected-maintenance-launch.sh:0700' \
  'prelive-protected-maintenance-unit.sh:0700' \
  'rollback-release.sh:0700'
do
  grep -F "'$specification'" "$gateway_installer" >/dev/null ||
    fail "gateway_runtime_missing:$specification"
done

for safety_script in \
  prelive-protected-maintenance.sh \
  prelive_protected_maintenance.py \
  prelive-protected-maintenance-launch.sh \
  prelive-protected-maintenance-unit.sh
do
  grep -F "$safety_script" "$context_installer" >/dev/null ||
    fail "context_installer_runtime_missing:$safety_script"
done

PYTHONDONTWRITEBYTECODE=1 \
  python3 -I -B "$repo_root/scripts/release_provenance.py" --help >/dev/null ||
  fail release_provenance_isolated_import_failed

grep -F 'libgcc-s1' "$dockerfile" >/dev/null ||
  fail rust_runtime_dependency_missing

grep -F 'autonomous-live-control __image_runtime_probe__' "$dockerfile" >/dev/null ||
  fail dockerfile_runtime_probe_missing

PYTHONDONTWRITEBYTECODE=1 python3 -I -B - \
  "$compose_contract" "$control_source" "$dockerfile" "$runtime_verifier" <<'PY' ||
import re
import sys
from pathlib import Path


def fail(message: str) -> None:
    raise SystemExit(f"AUTONOMOUS_CONTROL_CONTRACT_INVALID:{message}")


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


compose_path, control_path, dockerfile_path, verifier_path = map(Path, sys.argv[1:])
compose = compose_path.read_text(encoding="utf-8")
control = control_path.read_text(encoding="utf-8")
dockerfile = dockerfile_path.read_text(encoding="utf-8")
verifier = verifier_path.read_text(encoding="utf-8")

service_match = re.search(
    r"(?ms)^  live-executor:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
    compose,
)
require(service_match is not None, "compose_live_executor_service_missing")
service = service_match.group("body")
require(
    len(
        re.findall(
            r"(?m)^\s+WALLET_ADDRESS:\s*"
            r"\$\{LIVE_EXECUTOR_WALLET_ADDRESS:[^}]+\}\s*$",
            service,
        )
    )
    == 1,
    "compose_wallet_mapping_not_canonical",
)
require(
    len(
        re.findall(
            r"(?m)^\s+EXECUTOR_ADDRESS:\s*"
            r"\$\{LIVE_EXECUTOR_EXECUTOR_ADDRESS:[^}]+\}\s*$",
            service,
        )
    )
    == 1,
    "compose_executor_mapping_not_canonical",
)

require(
    '"LIVE_EXECUTOR_WALLET_ADDRESS"' not in control,
    "control_deprecated_wallet_lookup_present",
)
require(
    '"LIVE_EXECUTOR_EXECUTOR_ADDRESS"' not in control,
    "control_deprecated_executor_lookup_present",
)
require(
    control.count("control_address_environment_with(") == 2,
    "control_canonical_address_helper_usage_invalid",
)

run_start = control.find("async fn run()")
pool_start = control.find("async fn database_pool()")
require(run_start >= 0 and pool_start > run_start, "control_dispatch_structure_missing")
run_body = control[run_start:pool_start]
dispatch = run_body.find("match command.as_str()")
require(dispatch >= 0, "control_command_dispatch_missing")
require("POSTGRES_DSN" not in run_body, "control_database_initialized_before_dispatch")
require("PgPoolOptions" not in run_body, "control_pool_initialized_before_dispatch")
for command in (
    "IMAGE_RUNTIME_PROBE_COMMAND",
    '"preflight"',
    '"owner-plan"',
    '"migrate"',
    '"activate"',
    '"disarm"',
    '"status"',
    '"reconciliation-status"',
):
    require(command in run_body[dispatch:], f"control_command_missing:{command}")
require(
    run_body.find('"unsupported command"') > dispatch,
    "unsupported_command_not_rejected_by_dispatch",
)
require(
    control.find('required("POSTGRES_DSN")') > pool_start,
    "control_dsn_lookup_not_isolated",
)

for label, gate in (("dockerfile", dockerfile), ("verifier", verifier)):
    require("__image_runtime_probe__" in gate, f"{label}_probe_command_missing")
    require(
        "AUTONOMOUS_CONTROL_RUNTIME_OK" in gate,
        f"{label}_success_marker_missing",
    )
    require(
        "__image_runtime_contract_probe__" not in gate,
        f"{label}_obsolete_probe_command_present",
    )
    require(
        'probe_status" -lt 125' not in gate,
        f"{label}_nonzero_probe_status_accepted",
    )
    require(
        '*AUTONOMOUS_CONTROL_FAILED:*)' in gate,
        f"{label}_failure_marker_not_rejected",
    )
    require(
        '[ ! -s "$probe_stderr" ]' in gate,
        f"{label}_probe_stderr_not_required_empty",
    )

require(
    '[ "$probe_output" = "AUTONOMOUS_CONTROL_RUNTIME_OK" ]' in dockerfile,
    "dockerfile_probe_stdout_not_exact",
)
require(
    "set +e" not in dockerfile[dockerfile.find("FROM debian:bookworm-slim") :],
    "dockerfile_probe_nonzero_status_tolerated",
)
require(
    '[ "$probe_status" -eq 0 ]' in verifier,
    "verifier_probe_zero_status_not_required",
)
require(
    "[ \"$probe_output\" = AUTONOMOUS_CONTROL_RUNTIME_OK ]" in verifier,
    "verifier_probe_stdout_not_exact",
)
PY
  fail autonomous_control_contract_checker_failed

grep -F 'Verify immutable image runtime contract' "$build_workflow" >/dev/null ||
  fail immutable_build_runtime_gate_missing

grep -F 'scripts/verify-image-runtime.sh' "$build_workflow" >/dev/null ||
  fail immutable_build_runtime_verifier_missing

grep -F 'scripts/verify-image-runtime.sh' "$ci_workflow" >/dev/null ||
  fail source_ci_runtime_verifier_missing

echo "AUTONOMOUS_LIVE_RELEASE_CONTRACT_TEST_OK"
