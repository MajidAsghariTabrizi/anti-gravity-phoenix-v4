use serde_json::Value;
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::{Command, Output};

const ROUTES: &str = include_str!("../../fixtures/routes/weth_usdc_uniswap_v3.json");

fn candidate_environment() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("PHOENIX_MODE", "SHADOW".to_string()),
        ("LIVE_EXECUTION", "false".to_string()),
        ("SIGNER_PRIVATE_KEY", String::new()),
        ("EXECUTOR_ADDRESS", String::new()),
        ("WALLET_ADDRESS", String::new()),
        ("RECORDER_DAEMON", "true".to_string()),
        ("RECORDER_PERSISTENCE_POLICY", "money_path_v1".to_string()),
        (
            "ENGINE_ROUTER_ADDRESSES",
            money_path_classifier::REVIEWED_ROUTER_ADDRESSES.join(","),
        ),
        ("ENGINE_ROUTE_REGISTRY_JSON", ROUTES.to_string()),
        ("RECORDER_HEALTH_ADDR", "127.0.0.1:19400".to_string()),
        (
            "POSTGRES_DSN",
            "postgresql://recorder.invalid/phoenix".to_string(),
        ),
        ("PGSSLMODE", "prefer".to_string()),
        ("NATS_URL", "nats://recorder.invalid:4222".to_string()),
        ("RECORDER_BATCH_MAX_SIZE", "256".to_string()),
        ("RECORDER_BATCH_MAX_WAIT_MS", "100".to_string()),
        ("RECORDER_AGGREGATE_FLUSH_SECONDS", "60".to_string()),
        ("RECORDER_AGGREGATE_FLUSH_EVENTS", "10000".to_string()),
        ("RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY", "100".to_string()),
        ("RECORDER_MAX_SAMPLE_JSON_BYTES", "1024".to_string()),
    ])
}

fn run_config_check(environment: &BTreeMap<&str, String>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_phoenix-recorder"));
    command.arg("--config-check").env_clear();
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("Recorder config check must run")
}

fn run_daemon_startup(environment: &BTreeMap<&str, String>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_phoenix-recorder"));
    command.env_clear();
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("Recorder daemon startup must run")
}

fn result(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("config check must emit one JSON result")
}

fn assert_error(output: &Output, code: &str, environment_name: &str) {
    assert!(!output.status.success());
    let value = result(output);
    assert_eq!(value["schema"], "phoenix.recorder-config-check.v1");
    assert_eq!(value["status"], "error");
    assert_eq!(value["error_code"], code);
    assert_eq!(value["environment_name"], environment_name);
}

#[test]
fn exact_candidate_environment_passes_without_runtime_side_effects() {
    let postgres = TcpListener::bind("127.0.0.1:0").unwrap();
    let nats = TcpListener::bind("127.0.0.1:0").unwrap();
    let health = TcpListener::bind("127.0.0.1:0").unwrap();
    postgres.set_nonblocking(true).unwrap();
    nats.set_nonblocking(true).unwrap();

    let mut environment = candidate_environment();
    environment.insert(
        "POSTGRES_DSN",
        format!(
            "postgresql://recorder@{}/phoenix",
            postgres.local_addr().unwrap()
        ),
    );
    environment.insert("NATS_URL", format!("nats://{}", nats.local_addr().unwrap()));
    environment.insert(
        "RECORDER_HEALTH_ADDR",
        health.local_addr().unwrap().to_string(),
    );

    let output = run_config_check(&environment);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = result(&output);
    assert_eq!(value["schema"], "phoenix.recorder-config-check.v1");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["error_code"], "ok");
    assert!(value["environment_name"].is_null());
    assert_eq!(postgres.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    assert_eq!(nats.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
fn every_required_environment_name_is_reported_without_its_value() {
    for name in [
        "RECORDER_DAEMON",
        "PHOENIX_MODE",
        "LIVE_EXECUTION",
        "RECORDER_PERSISTENCE_POLICY",
        "ENGINE_ROUTER_ADDRESSES",
        "ENGINE_ROUTE_REGISTRY_JSON",
        "POSTGRES_DSN",
        "NATS_URL",
    ] {
        let mut environment = candidate_environment();
        environment.remove(name);
        let output = run_config_check(&environment);
        assert_error(&output, "required_environment_missing", name);
    }
}

#[test]
fn daemon_startup_preserves_the_specific_bounded_config_failure() {
    let mut environment = candidate_environment();
    environment.remove("POSTGRES_DSN");
    let output = run_daemon_startup(&environment);
    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("recorder_stopped"));
    assert!(combined.contains("required_environment_missing"));
    assert!(combined.contains("POSTGRES_DSN"));
    assert!(!combined.contains("required Recorder environment is missing"));
    assert!(!combined.contains("postgresql://"));
}

#[test]
fn malformed_route_registry_and_router_addresses_are_specific() {
    let mut malformed_route = candidate_environment();
    malformed_route.insert("ENGINE_ROUTE_REGISTRY_JSON", "not-json".to_string());
    let output = run_config_check(&malformed_route);
    assert_error(
        &output,
        "route_registry_invalid",
        "ENGINE_ROUTE_REGISTRY_JSON",
    );

    let mut invalid_router = candidate_environment();
    invalid_router.insert("ENGINE_ROUTER_ADDRESSES", "not-an-address".to_string());
    let output = run_config_check(&invalid_router);
    assert_error(
        &output,
        "router_addresses_invalid",
        "ENGINE_ROUTER_ADDRESSES",
    );
}

#[test]
fn malformed_numeric_settings_identify_the_exact_environment_name() {
    for name in [
        "RECORDER_BATCH_MAX_SIZE",
        "RECORDER_BATCH_MAX_WAIT_MS",
        "RECORDER_AGGREGATE_FLUSH_SECONDS",
        "RECORDER_AGGREGATE_FLUSH_EVENTS",
        "RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY",
        "RECORDER_MAX_SAMPLE_JSON_BYTES",
    ] {
        let mut environment = candidate_environment();
        environment.insert(name, "not-a-number".to_string());
        let output = run_config_check(&environment);
        assert_error(&output, "numeric_environment_invalid", name);
    }
}

#[test]
fn shadow_safety_failures_remain_fail_closed_and_output_is_sanitized() {
    let mut environment = candidate_environment();
    environment.insert("PHOENIX_MODE", "LIVE".to_string());
    environment.insert(
        "SIGNER_PRIVATE_KEY",
        "private-material-must-not-leak".to_string(),
    );
    environment.insert(
        "POSTGRES_DSN",
        "postgresql://operator:credential@example.invalid/phoenix".to_string(),
    );
    let output = run_config_check(&environment);
    assert_error(&output, "shadow_mode_invalid", "PHOENIX_MODE");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for forbidden in [
        "private-material-must-not-leak",
        "operator:credential",
        "not-json",
        money_path_classifier::REVIEWED_ROUTER_ADDRESSES[0],
    ] {
        assert!(!combined.contains(forbidden));
    }
}
