use phoenix_live_executor::control_environment::{
    control_address_environment_with, EXECUTOR_ADDRESS_ENV, WALLET_ADDRESS_ENV,
};
use std::process::{Command, Output};

const WALLET_ADDRESS: &str = "0x1111111111111111111111111111111111111111";
const EXECUTOR_ADDRESS: &str = "0x2222222222222222222222222222222222222222";

fn control_command(argument: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_autonomous-live-control"));
    command.arg(argument).env_clear();
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn image_runtime_probe_is_exact_and_environment_independent() {
    let output = control_command("__image_runtime_probe__")
        .output()
        .expect("run image runtime probe");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"AUTONOMOUS_CONTROL_RUNTIME_OK\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn unsupported_command_fails_before_environment_or_database_access() {
    let output = control_command("__unsupported_contract_command__")
        .output()
        .expect("run unsupported command");
    let error = stderr(&output);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(error.contains("AUTONOMOUS_CONTROL_FAILED: unsupported command"));
    assert!(!error.contains("POSTGRES_DSN"));
    assert!(!error.contains("required environment"));
    assert!(!error.contains("database"));
}

#[test]
fn no_database_control_commands_use_only_canonical_address_environment_names() {
    for argument in ["preflight", "owner-plan", "owner-configured-preflight"] {
        let missing = |name| {
            if argument == "preflight" {
                format!("required environment is missing: {name}")
            } else {
                format!("missing required owner setting: {name}")
            }
        };
        let deprecated_only = control_command(argument)
            .env("CHAIN_ID", "42161")
            .env("LIVE_EXECUTOR_WALLET_ADDRESS", WALLET_ADDRESS)
            .env("LIVE_EXECUTOR_EXECUTOR_ADDRESS", EXECUTOR_ADDRESS)
            .output()
            .expect("run control command with deprecated names");
        let deprecated_error = stderr(&deprecated_only);
        assert!(!deprecated_only.status.success(), "{argument}");
        assert!(
            deprecated_error.contains(&missing("WALLET_ADDRESS")),
            "{argument}: {deprecated_error}"
        );
        assert!(
            !deprecated_error.contains("LIVE_EXECUTOR_WALLET_ADDRESS"),
            "{argument}"
        );
        assert!(!deprecated_error.contains(WALLET_ADDRESS), "{argument}");

        let wallet_only = control_command(argument)
            .env("CHAIN_ID", "42161")
            .env(WALLET_ADDRESS_ENV, WALLET_ADDRESS)
            .output()
            .expect("run control command with canonical wallet");
        let wallet_error = stderr(&wallet_only);
        assert!(!wallet_only.status.success(), "{argument}");
        assert!(
            wallet_error.contains(&missing("EXECUTOR_ADDRESS")),
            "{argument}: {wallet_error}"
        );
        assert!(!wallet_error.contains(EXECUTOR_ADDRESS), "{argument}");

        let canonical = control_command(argument)
            .env("CHAIN_ID", "42161")
            .env(WALLET_ADDRESS_ENV, WALLET_ADDRESS)
            .env(EXECUTOR_ADDRESS_ENV, EXECUTOR_ADDRESS)
            .output()
            .expect("run control command with canonical addresses");
        let canonical_error = stderr(&canonical);
        assert!(!canonical.status.success(), "{argument}");
        let next_required = if argument == "preflight" {
            "PRODUCTION_RPC_URL"
        } else {
            "LIVE_EXECUTOR_EXPECTED_OWNER"
        };
        assert!(
            canonical_error.contains(&missing(next_required)),
            "{argument}: {canonical_error}"
        );
        assert!(!canonical_error.contains(WALLET_ADDRESS), "{argument}");
        assert!(!canonical_error.contains(EXECUTOR_ADDRESS), "{argument}");
    }
}

#[test]
fn mutating_owner_commands_reject_missing_acknowledgement_before_environment_access() {
    for argument in ["owner-configure", "owner-unpause", "owner-pause"] {
        let output = control_command(argument)
            .output()
            .expect("run mutating owner command");
        let error = stderr(&output);
        assert!(!output.status.success(), "{argument}");
        assert!(
            error.contains("AUTONOMOUS_CONTROL_FAILED: owner acknowledgement is invalid"),
            "{argument}: {error}"
        );
        assert!(!error.contains("POSTGRES_DSN"), "{argument}");
        assert!(!error.contains("required owner setting"), "{argument}");
    }
}

#[test]
fn missing_environment_diagnostic_names_variable_without_value() {
    let sensitive_value = "sensitive-wallet-value-must-not-appear";
    let error = control_address_environment_with(|name| match name {
        WALLET_ADDRESS_ENV => Some(sensitive_value.to_string()),
        EXECUTOR_ADDRESS_ENV => None,
        _ => unreachable!("unexpected environment lookup"),
    })
    .expect_err("missing canonical executor address");

    assert_eq!(error.name(), EXECUTOR_ADDRESS_ENV);
    assert_eq!(
        error.to_string(),
        "required environment is missing: EXECUTOR_ADDRESS"
    );
    assert!(!error.to_string().contains(sensitive_value));
    assert!(!format!("{error:?}").contains(sensitive_value));
}
