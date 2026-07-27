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
owner_bootstrap_source=$repo_root/live-executor/src/owner_bootstrap.rs
deploy_release=$repo_root/scripts/deploy-release.sh

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
  "$control_source" \
  "$owner_bootstrap_source" \
  "$deploy_release"
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
  "$compose_contract" "$control_source" "$owner_bootstrap_source" \
  "$deploy_release" "$dockerfile" "$runtime_verifier" <<'PY' ||
import re
import sys
from pathlib import Path


def fail(message: str) -> None:
    raise SystemExit(f"AUTONOMOUS_CONTROL_CONTRACT_INVALID:{message}")


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


(
    compose_path,
    control_path,
    owner_bootstrap_path,
    deploy_release_path,
    dockerfile_path,
    verifier_path,
) = map(Path, sys.argv[1:])
compose = compose_path.read_text(encoding="utf-8")
control = control_path.read_text(encoding="utf-8")
owner_bootstrap = owner_bootstrap_path.read_text(encoding="utf-8")
owner_runtime = owner_bootstrap.split("#[cfg(test)]", 1)[0]
deploy_release = deploy_release_path.read_text(encoding="utf-8")
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
    control.count("control_address_environment_with(") == 1,
    "control_preflight_canonical_address_helper_usage_invalid",
)
require(
    '"WALLET_ADDRESS"' in owner_runtime
    and '"EXECUTOR_ADDRESS"' in owner_runtime,
    "owner_bootstrap_canonical_address_names_missing",
)
require(
    '"LIVE_EXECUTOR_WALLET_ADDRESS"' not in owner_runtime
    and '"LIVE_EXECUTOR_EXECUTOR_ADDRESS"' not in owner_runtime,
    "owner_bootstrap_deprecated_address_names_present",
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
    '"owner-configure"',
    '"owner-configured-preflight"',
    '"owner-unpause"',
    '"owner-pause"',
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
for forbidden in ("POSTGRES_DSN", "DATABASE_URL"):
    require(
        forbidden not in owner_runtime,
        f"owner_bootstrap_database_dependency_present:{forbidden}",
    )

for required_owner_contract in (
    "BOOTSTRAP_EXECUTOR_OWNER_42161",
    "UNPAUSE_CONFIGURED_EXECUTOR_42161",
    "PAUSE_EXECUTOR_AFTER_FAILED_DEPLOY_42161",
    "EXECUTOR_OWNER_CONFIGURE_OK",
    "EXECUTOR_OWNER_CONFIGURED_PREFLIGHT_OK",
    "EXECUTOR_OWNER_UNPAUSE_OK",
    "EXECUTOR_OWNER_PAUSE_OK",
    "transaction_signer_from_file",
    "quote_transaction",
    "pending_nonce",
    "send_raw_transaction",
    "transaction_receipt",
    "transaction_known",
    '"receipt_status"',
):
    require(
        required_owner_contract in owner_runtime or required_owner_contract in control,
        f"owner_bootstrap_contract_missing:{required_owner_contract}",
    )

for forbidden_input in (
    '"TARGET"',
    '"CALLDATA"',
    '"RAW_TRANSACTION"',
    '"TRANSACTION_NONCE"',
):
    require(
        forbidden_input not in owner_runtime,
        f"owner_bootstrap_arbitrary_input_present:{forbidden_input}",
    )

authorization_absent = deploy_release.find('if [ ! -e "$owner_authorization" ]')
authorization = deploy_release.find(
    "validate_owner_authorization", authorization_absent
)
consume = deploy_release.find("consume_owner_authorization", authorization)
configure = deploy_release.find("live-executor owner-configure")
configured_preflight = deploy_release.find("live-executor owner-configured-preflight")
production_mode = deploy_release.find(
    'python3 "$deploy_dir/production_mode.py" live --env-file "$env_file"'
)
live_reload = deploy_release.find("reload_environment", production_mode)
burn_in = deploy_release.find("run_live_engine_burn_in", live_reload)
activation = deploy_release.find("live-executor activate")
unpause = deploy_release.find("live-executor owner-unpause")
normal_preflight = deploy_release.rfind("live-executor preflight")
executor_start = deploy_release.find("compose up -d --no-deps live-executor")
require(
    min(
        authorization,
        authorization_absent,
        consume,
        configure,
        configured_preflight,
        production_mode,
        live_reload,
        burn_in,
        activation,
        unpause,
        normal_preflight,
        executor_start,
    )
    >= 0,
    "owner_bootstrap_deployment_sequence_missing",
)
require(
    authorization_absent
    < authorization
    < consume
    < configure
    < configured_preflight
    < production_mode
    < live_reload
    < burn_in
    < activation
    < unpause
    < normal_preflight
    < executor_start,
    "owner_bootstrap_deployment_sequence_invalid",
)
require(
    "engine_burn_in_seconds=${PHOENIX_ENGINE_BURN_IN_SECONDS:-120}"
    in deploy_release
    and '[ "$engine_burn_in_seconds" -ge 120 ]' in deploy_release,
    "engine_burn_in_minimum_missing",
)
burn_in_body = deploy_release[
    deploy_release.find("run_live_engine_burn_in()") : deploy_release.find(
        "install_active_file()", deploy_release.find("run_live_engine_burn_in()")
    )
]
for burn_in_contract in (
    "{{.RestartCount}}",
    "http://127.0.0.1:9200/readyz",
    "http://127.0.0.1:9300/readyz",
    "compose ps -q live-executor",
):
    require(
        burn_in_contract in burn_in_body,
        f"engine_burn_in_contract_missing:{burn_in_contract}",
    )
require(
    "engine_terminal_integrity_total" in burn_in_body
    and "phoenix_engine_terminal_integrity_total" in deploy_release,
    "engine_burn_in_terminal_integrity_gate_missing",
)
require(
    deploy_release.find("compose stop -t 30 live-executor", production_mode)
    < burn_in
    < activation
    < unpause
    < executor_start,
    "executor_not_stopped_through_engine_burn_in",
)
require(
    deploy_release.count("reload_environment") >= 3
    and deploy_release.count("assert_live_environment") >= 3,
    "live_environment_reload_contract_missing",
)
require(
    'unset PHOENIX_MODE LIVE_EXECUTION AUTONOMOUS_EXECUTION' in deploy_release,
    "stale_mode_environment_not_cleared",
)
require(
    "validate_live_rpc_rendering" in deploy_release
    and 'urls != [sys.argv[2], sys.argv[3]]' in deploy_release
    and "len(priorities) != 2" in deploy_release,
    "rendered_live_rpc_parity_gate_missing",
)
preflight_render_start = deploy_release.find(
    '"$deploy_dir/render-production-compose.sh"',
    deploy_release.find('verify_active_release_coherence "$rollback_sha" ""'),
)
preflight_render_end = deploy_release.find(
    'capture_protected_ids "$protected_before"',
    preflight_render_start,
)
preflight_render_body = (
    deploy_release[preflight_render_start:preflight_render_end]
    if 0 <= preflight_render_start < preflight_render_end
    else ""
)
require(
    preflight_render_body
    and "validate_live_rpc_inputs" in preflight_render_body
    and "validate_live_rpc_inputs()" in deploy_release,
    "prelive_rpc_inputs_not_validated",
)
require(
    'candidate_live_env="$state_dir/candidate-live.env"' in deploy_release
    and 'cp "$env_file" "$candidate_live_env"' in preflight_render_body
    and 'production_mode.py" live --env-file "$candidate_live_env"' in preflight_render_body
    and 'validate-production-env.sh" "$candidate_live_env"' in preflight_render_body
    and '--overlay-file "$overlay_file"' in preflight_render_body
    and '--env-file "$candidate_live_env"' in preflight_render_body
    and "validate_live_rpc_rendering" in preflight_render_body,
    "candidate_live_overlay_preflight_missing",
)
require(
    "production_environment_identity()" in deploy_release
    and "active_environment_identity_before" in preflight_render_body
    and "active_environment_identity_after" in preflight_render_body
    and (
        '[ "$active_environment_identity_after" = '
        '"$active_environment_identity_before" ]'
    )
    in preflight_render_body,
    "candidate_preflight_does_not_prove_active_environment_unchanged",
)
preflight_rpc_gate = deploy_release.find(
    'fail "preflight LIVE RPC provider and priority configuration is invalid"'
)
first_container_creation = deploy_release.find("compose pull")
require(
    0 <= preflight_rpc_gate < first_container_creation,
    "live_rpc_parity_not_validated_before_container_creation",
)
require(
    "candidate-release-assets.sha" in deploy_release
    and "verify_active_release_coherence" in deploy_release
    and 'rollback_release_root="$release_root/$rollback_sha"' in deploy_release
    and 'PHOENIX_CONTEXT_INSTALLER="$rollback_context_installer"' in deploy_release,
    "coherent_version_matched_rollback_contract_missing",
)
require(
    "owner_authorization=/etc/phoenix/authorizations/executor-owner-bootstrap.json"
    in deploy_release,
    "owner_authorization_path_not_exact",
)
require(
    "PHOENIX_EXECUTOR_OWNER_BOOTSTRAP_AUTHORIZATION_FILE" not in deploy_release,
    "owner_authorization_path_is_operator_overridable",
)
require(
    'mv -n "$owner_authorization" "$consumed_owner_authorization"'
    in deploy_release
    and '[ ! -e "$owner_authorization" ] && [ -f "$consumed_owner_authorization" ]'
    in deploy_release,
    "owner_authorization_not_consumed_exactly_once",
)
require(
    deploy_release.find("owner_bootstrap_started=1", consume)
    < configure,
    "owner_bootstrap_not_marked_before_first_mutation",
)
owner_unpause_attempt = deploy_release.find(
    "owner_unpause_attempted=1", configured_preflight
)
owner_unpause_code = deploy_release.find("owner_unpause_code=$?")
owner_unpause_applied = deploy_release.find("owner_unpaused=1", owner_unpause_code)
owner_unpause_failure = deploy_release.find(
    '[ "$owner_unpause_code" -eq 0 ] || fail "executor owner unpause failed"',
    owner_unpause_code,
)
require(
    configured_preflight
    < owner_unpause_attempt
    < owner_unpause_code
    < owner_unpause_applied
    < owner_unpause_failure,
    "owner_unpause_compensation_state_not_captured_before_failure",
)
require(
    deploy_release.find("live-executor owner-pause")
    < deploy_release.find("invoking rollback"),
    "owner_pause_not_before_rollback",
)
for authorization_contract in (
    "phoenix.executor-owner-bootstrap-authorization.v1",
    '{"schema", "release_sha", "chain_id", "acknowledgement"}',
    "0:0:600:1",
    "cannot be consumed atomically",
    "EXTERNAL_OWNER_AUTHORIZATION_REQUIRED",
):
    require(
        authorization_contract in deploy_release,
        f"owner_authorization_contract_missing:{authorization_contract}",
    )

owner_mutation = control[control.find("async fn owner_mutation") :]
require(
    owner_mutation.find("execute_from_environment(mutation).await?")
    < owner_mutation.find("mutation == OwnerMutation::Unpause")
    < owner_mutation.find("preflight().await?")
    < owner_mutation.find('println!("{marker}")'),
    "owner_unpause_normal_preflight_sequence_invalid",
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
