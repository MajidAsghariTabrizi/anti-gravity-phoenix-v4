use money_path_classifier::{ClassifierError, MoneyPathClassifier, ADMISSION_POLICY_VERSION};
use phoenix_recorder::ingress::{IngressBuffer, IngressBufferConfig};
use phoenix_recorder::jetstream::{
    ensure_durable_pipeline, MessageFetcher, CONSUMER_ACK_WAIT, CONSUMER_MAX_ACK_PENDING,
    DURABLE_CONSUMER_NAME, STREAM_NAME,
};
use phoenix_recorder::logging::LogSampler;
use phoenix_recorder::metrics::Metrics;
use phoenix_recorder::persistence::{EventStore, PostgresStore};
use phoenix_recorder::runtime::{
    consume_durable_messages, flush_ingress_evidence, mark_nats_connected, mark_nats_disconnected,
    monitor_database, nats_connect_options, BatchConfig, ConsumerExit, PrePersistenceClassifier,
    RetryPolicy, RuntimeConfigError,
};
use phoenix_recorder::state::Readiness;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const CONFIG_CHECK_SCHEMA: &str = "phoenix.recorder-config-check.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConfigError {
    code: &'static str,
    environment_name: Option<&'static str>,
}

impl ConfigError {
    const fn new(code: &'static str, environment_name: Option<&'static str>) -> Self {
        Self {
            code,
            environment_name,
        }
    }

    const fn missing(environment_name: &'static str) -> Self {
        Self::new("required_environment_missing", Some(environment_name))
    }

    const fn invalid(code: &'static str, environment_name: &'static str) -> Self {
        Self::new(code, Some(environment_name))
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.code)?;
        if let Some(environment_name) = self.environment_name {
            write!(formatter, ":{environment_name}")?;
        }
        Ok(())
    }
}

impl Error for ConfigError {}

#[derive(Debug)]
enum RecorderError {
    Config(ConfigError),
    Runtime(&'static str),
}

impl RecorderError {
    const fn code(&self) -> &'static str {
        match self {
            Self::Config(error) => error.code,
            Self::Runtime(code) => code,
        }
    }

    const fn environment_name(&self) -> Option<&'static str> {
        match self {
            Self::Config(error) => error.environment_name,
            Self::Runtime(_) => None,
        }
    }
}

impl fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => error.fmt(formatter),
            Self::Runtime(code) => write!(formatter, "{code}"),
        }
    }
}

impl Error for RecorderError {}

#[derive(Clone)]
struct Config {
    health_addr: String,
    postgres_dsn: String,
    pg_ssl_mode: String,
    nats_url: String,
    batch: BatchConfig,
    classifier: MoneyPathClassifier,
    ingress: IngressBufferConfig,
    persistence_policy: PersistencePolicy,
}

impl Config {
    fn from_env() -> Result<Self, ConfigError> {
        validate_daemon_mode()?;
        validate_shadow_safety()?;
        let persistence_policy =
            PersistencePolicy::parse(&required_env("RECORDER_PERSISTENCE_POLICY")?)?;
        let router_addresses = parse_router_addresses(&required_env("ENGINE_ROUTER_ADDRESSES")?)?;
        let route_registry = required_env("ENGINE_ROUTE_REGISTRY_JSON")?;
        let classifier = MoneyPathClassifier::from_release(
            persistence_policy.as_str(),
            &router_addresses,
            &route_registry,
        )
        .map_err(classifier_config_error)?;
        let batch = BatchConfig {
            max_size: optional_usize("RECORDER_BATCH_MAX_SIZE", 256)?,
            max_wait: Duration::from_millis(optional_u64("RECORDER_BATCH_MAX_WAIT_MS", 100)?),
        }
        .validate()
        .map_err(|error| match error {
            RuntimeConfigError::BatchSize => ConfigError::invalid(
                "numeric_environment_out_of_range",
                "RECORDER_BATCH_MAX_SIZE",
            ),
            RuntimeConfigError::BatchWait => ConfigError::invalid(
                "numeric_environment_out_of_range",
                "RECORDER_BATCH_MAX_WAIT_MS",
            ),
        })?;
        let ingress = IngressBufferConfig {
            flush_interval: Duration::from_secs(optional_u64(
                "RECORDER_AGGREGATE_FLUSH_SECONDS",
                60,
            )?),
            flush_after_events: optional_usize("RECORDER_AGGREGATE_FLUSH_EVENTS", 10_000)?,
            max_samples_per_detail_per_day: optional_usize(
                "RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY",
                100,
            )?,
            max_sample_json_bytes: optional_usize("RECORDER_MAX_SAMPLE_JSON_BYTES", 1_024)?,
        }
        .validate()
        .map_err(|_| ConfigError::new("ingress_configuration_out_of_range", None))?;
        let health_addr = optional_env("RECORDER_HEALTH_ADDR", "0.0.0.0:9400")?;
        health_addr
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::invalid("health_address_invalid", "RECORDER_HEALTH_ADDR"))?;
        let pg_ssl_mode = optional_env("PGSSLMODE", "prefer")?;
        validate_pg_ssl_mode(&pg_ssl_mode)?;
        Ok(Self {
            health_addr,
            postgres_dsn: required_env("POSTGRES_DSN")?,
            pg_ssl_mode,
            nats_url: required_env("NATS_URL")?,
            batch,
            classifier,
            ingress,
            persistence_policy,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistencePolicy {
    MoneyPathV1,
}

impl PersistencePolicy {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            ADMISSION_POLICY_VERSION => Ok(Self::MoneyPathV1),
            _ => Err(ConfigError::invalid(
                "persistence_policy_invalid",
                "RECORDER_PERSISTENCE_POLICY",
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::MoneyPathV1 => "money_path_v1",
        }
    }
}

fn validate_daemon_mode() -> Result<(), ConfigError> {
    let value = required_env("RECORDER_DAEMON")?;
    if !value.eq_ignore_ascii_case("true") {
        return Err(ConfigError::invalid(
            "daemon_mode_invalid",
            "RECORDER_DAEMON",
        ));
    }
    Ok(())
}

fn validate_shadow_safety() -> Result<(), ConfigError> {
    require_exact_env("PHOENIX_MODE", "SHADOW", "shadow_mode_invalid")?;
    require_exact_env("LIVE_EXECUTION", "false", "live_execution_invalid")?;
    for name in ["SIGNER_PRIVATE_KEY", "EXECUTOR_ADDRESS", "WALLET_ADDRESS"] {
        match std::env::var(name) {
            Ok(value) if value.is_empty() => {}
            Err(std::env::VarError::NotPresent) => {}
            Ok(_) => {
                return Err(ConfigError::invalid(
                    "shadow_execution_configuration_present",
                    name,
                ));
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::invalid("environment_encoding_invalid", name));
            }
        }
    }
    Ok(())
}

fn require_exact_env(
    name: &'static str,
    expected: &str,
    code: &'static str,
) -> Result<(), ConfigError> {
    if required_env(name)? != expected {
        return Err(ConfigError::invalid(code, name));
    }
    Ok(())
}

fn parse_router_addresses(raw: &str) -> Result<Vec<String>, ConfigError> {
    let values = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if values.is_empty() || values.join(",") != raw {
        return Err(ConfigError::invalid(
            "router_addresses_invalid",
            "ENGINE_ROUTER_ADDRESSES",
        ));
    }
    Ok(values)
}

fn classifier_config_error(error: ClassifierError) -> ConfigError {
    match error {
        ClassifierError::AdmissionPolicy => {
            ConfigError::invalid("persistence_policy_invalid", "RECORDER_PERSISTENCE_POLICY")
        }
        ClassifierError::RouterRegistry => {
            ConfigError::invalid("router_addresses_invalid", "ENGINE_ROUTER_ADDRESSES")
        }
        ClassifierError::RouteRegistry => {
            ConfigError::invalid("route_registry_invalid", "ENGINE_ROUTE_REGISTRY_JSON")
        }
        ClassifierError::Invariant => ConfigError::new("classifier_invariant_failed", None),
    }
}

fn optional_usize(name: &'static str, default: usize) -> Result<usize, ConfigError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .map_err(|_| ConfigError::invalid("numeric_environment_invalid", name)),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ConfigError::invalid("environment_encoding_invalid", name))
        }
    }
}

fn optional_u64(name: &'static str, default: u64) -> Result<u64, ConfigError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| ConfigError::invalid("numeric_environment_invalid", name)),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ConfigError::invalid("environment_encoding_invalid", name))
        }
    }
}

fn optional_env(name: &'static str, default: &str) -> Result<String, ConfigError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => Ok(default.to_string()),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ConfigError::invalid("environment_encoding_invalid", name))
        }
    }
}

fn required_env(name: &'static str) -> Result<String, ConfigError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(std::env::VarError::NotPresent) => Err(ConfigError::missing(name)),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(ConfigError::invalid("environment_encoding_invalid", name))
        }
    }
}

fn validate_pg_ssl_mode(value: &str) -> Result<(), ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "disable" | "allow" | "prefer" | "" | "require" | "verify-ca" | "verify-full" => Ok(()),
        _ => Err(ConfigError::invalid("pg_ssl_mode_invalid", "PGSSLMODE")),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().map(String::as_str) == Some("--config-check") {
        if arguments.len() != 1 {
            let error = ConfigError::new("config_check_arguments_invalid", None);
            emit_config_check("error", error.code, error.environment_name);
            return Err(io::Error::other(error).into());
        }
        return match Config::from_env() {
            Ok(_config) => {
                emit_config_check("ok", "ok", None);
                Ok(())
            }
            Err(error) => {
                emit_config_check("error", error.code, error.environment_name);
                Err(io::Error::other(error).into())
            }
        };
    }

    if !daemon_enabled() {
        return run_file_recorder().map_err(Into::into);
    }

    init_logging();
    if let Err(error) = run_daemon().await {
        tracing::error!(
            event = "recorder_stopped",
            error_code = error.code(),
            environment_name = error.environment_name().unwrap_or("none")
        );
        return Err(io::Error::other(error).into());
    }
    Ok(())
}

fn emit_config_check(
    status: &'static str,
    error_code: &'static str,
    environment_name: Option<&'static str>,
) {
    println!(
        "{}",
        serde_json::json!({
            "schema": CONFIG_CHECK_SCHEMA,
            "status": status,
            "error_code": error_code,
            "environment_name": environment_name,
        })
    );
}

fn daemon_enabled() -> bool {
    std::env::var("RECORDER_DAEMON")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .init();
}

async fn run_daemon() -> Result<(), RecorderError> {
    let config = Config::from_env().map_err(RecorderError::Config)?;
    let readiness = Readiness::new();
    let metrics = Metrics::default();
    let sampler = LogSampler::default();
    let shutdown = CancellationToken::new();
    let classifier: Arc<dyn PrePersistenceClassifier> = Arc::new(config.classifier.clone());
    let ingress = IngressBuffer::new(config.ingress.clone())
        .map_err(|_| RecorderError::Runtime("ingress_runtime_configuration_invalid"))?;

    tracing::info!(
        event = "recorder_startup",
        nats_subject = phoenix_recorder::NATS_SUBJECT,
        nats_delivery = "jetstream_durable_pull",
        stream = STREAM_NAME,
        durable_consumer = DURABLE_CONSUMER_NAME,
        batch_max_messages = config.batch.max_size,
        batch_max_wait_ms = config.batch.max_wait.as_millis() as u64,
        max_ack_pending = CONSUMER_MAX_ACK_PENDING,
        ack_wait_seconds = CONSUMER_ACK_WAIT.as_secs(),
        persistence_policy = config.persistence_policy.as_str(),
        aggregate_flush_seconds = config.ingress.flush_interval.as_secs(),
        aggregate_flush_events = config.ingress.flush_after_events,
        max_samples_per_detail_per_day = config.ingress.max_samples_per_detail_per_day
    );

    let health_task = tokio::spawn(serve_health(
        config.health_addr.clone(),
        readiness.clone(),
        metrics.clone(),
        shutdown.clone(),
    ));
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!(event = "recorder_graceful_shutdown_started");
        signal_shutdown.cancel();
    });

    let store = connect_postgres_until_ready(
        &config,
        &readiness,
        &metrics,
        &sampler,
        &shutdown,
        RetryPolicy::default(),
    )
    .await
    .ok_or(RecorderError::Runtime(
        "shutdown_before_postgres_initialization",
    ))?;
    let store: Arc<dyn EventStore> = Arc::new(store);
    let database_monitor = tokio::spawn(monitor_database(
        store.clone(),
        readiness.clone(),
        metrics.clone(),
        sampler.clone(),
        shutdown.clone(),
        Duration::from_secs(5),
    ));
    let ingress_flush = tokio::spawn(flush_ingress_evidence(
        store.clone(),
        ingress.clone(),
        metrics.clone(),
        sampler.clone(),
        shutdown.clone(),
    ));

    let disconnected = Arc::new(AtomicBool::new(false));
    let mut integrity_failure = false;
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        let options = nats_connect_options(
            readiness.clone(),
            metrics.clone(),
            sampler.clone(),
            disconnected.clone(),
        );
        let client = match options.connect(config.nats_url.clone()).await {
            Ok(client) => client,
            Err(_) => {
                readiness.set_jetstream_connected(false);
                if let Some(suppressed) = sampler.sample("jetstream_connect_failure") {
                    tracing::warn!(
                        event = "recorder_jetstream_connect_failed",
                        suppressed,
                        retry_delay_ms = 1_000_u64
                    );
                }
                if sleep_or_shutdown(Duration::from_secs(1), &shutdown).await {
                    break;
                }
                continue;
            }
        };
        mark_nats_connected(&readiness, &metrics, disconnected.as_ref());

        let consumer = match ensure_durable_pipeline(&client).await {
            Ok(consumer) => {
                readiness.set_stream_ready(true);
                readiness.set_consumer_ready(true);
                tracing::info!(
                    event = "recorder_jetstream_pipeline_ready",
                    stream = STREAM_NAME,
                    durable_consumer = DURABLE_CONSUMER_NAME
                );
                consumer
            }
            Err(error) => {
                readiness.set_stream_ready(false);
                readiness.set_consumer_ready(false);
                mark_nats_disconnected(&readiness, disconnected.as_ref());
                if let Some(suppressed) = sampler.sample("jetstream_provision_failure") {
                    tracing::warn!(
                        event = "recorder_jetstream_provision_failed",
                        error_class = %error,
                        suppressed,
                        retry_delay_ms = 1_000_u64
                    );
                }
                if sleep_or_shutdown(Duration::from_secs(1), &shutdown).await {
                    break;
                }
                continue;
            }
        };

        let fetcher: Arc<dyn MessageFetcher> = Arc::new(consumer);
        let exit = consume_durable_messages(
            fetcher,
            store.clone(),
            classifier.clone(),
            ingress.clone(),
            readiness.clone(),
            metrics.clone(),
            sampler.clone(),
            shutdown.clone(),
            config.batch,
            RetryPolicy::default(),
        )
        .await;
        mark_nats_disconnected(&readiness, disconnected.as_ref());
        if exit == ConsumerExit::Shutdown {
            break;
        }
        if exit == ConsumerExit::IntegrityFailure {
            integrity_failure = true;
            break;
        }
        tracing::warn!(
            event = "recorder_jetstream_fetch_loop_ended",
            retry_delay_ms = 1_000_u64
        );
        if sleep_or_shutdown(Duration::from_secs(1), &shutdown).await {
            break;
        }
    }

    shutdown.cancel();
    let _ = database_monitor.await;
    let _ = ingress_flush.await;
    readiness.stop_event_loop();
    let _ = health_task.await;
    tracing::info!(event = "recorder_graceful_shutdown_complete");
    if integrity_failure {
        Err(RecorderError::Runtime("terminal_integrity_condition"))
    } else {
        Ok(())
    }
}

async fn connect_postgres_until_ready(
    config: &Config,
    readiness: &Readiness,
    metrics: &Metrics,
    sampler: &LogSampler,
    shutdown: &CancellationToken,
    retry: RetryPolicy,
) -> Option<PostgresStore> {
    let mut delay = retry.initial;
    let mut failed_attempts = 0_u64;
    loop {
        match PostgresStore::connect(&config.postgres_dsn, &config.pg_ssl_mode).await {
            Ok(store) => {
                readiness.set_postgres_connected(true);
                tracing::info!(event = "recorder_postgres_connected");
                match store.verify_schema().await {
                    Ok(()) => {
                        readiness.set_schema_verified(true);
                        if failed_attempts > 0 {
                            metrics.database_retry_recovered();
                        }
                        tracing::info!(event = "recorder_schema_verified");
                        return Some(store);
                    }
                    Err(error) => {
                        failed_attempts = failed_attempts.saturating_add(1);
                        metrics.database_failure();
                        metrics.database_retry();
                        readiness.set_schema_verified(false);
                        if let Some(suppressed) = sampler.sample("initial_schema_failure") {
                            tracing::error!(
                                event = "recorder_schema_verification_failed",
                                error_class = %error,
                                suppressed,
                                retry_delay_ms = delay.as_millis() as u64
                            );
                        }
                    }
                }
            }
            Err(error) => {
                failed_attempts = failed_attempts.saturating_add(1);
                metrics.database_failure();
                metrics.database_retry();
                readiness.set_postgres_connected(false);
                if let Some(suppressed) = sampler.sample("initial_postgres_failure") {
                    tracing::warn!(
                        event = "recorder_postgres_connect_failed",
                        error_class = %error,
                        suppressed,
                        retry_delay_ms = delay.as_millis() as u64
                    );
                }
            }
        }
        if sleep_or_shutdown(delay, shutdown).await {
            return None;
        }
        delay = delay.saturating_mul(2).min(retry.maximum);
    }
}

async fn sleep_or_shutdown(duration: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = shutdown.cancelled() => true,
        _ = tokio::time::sleep(duration) => false,
    }
}

async fn serve_health(
    addr: String,
    readiness: Readiness,
    metrics: Metrics,
    shutdown: CancellationToken,
) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(_) => {
            readiness.stop_event_loop();
            shutdown.cancel();
            tracing::error!(event = "recorder_health_bind_failed");
            return;
        }
    };
    tracing::info!(event = "recorder_health_listening", address = %addr);
    let permits = Arc::new(Semaphore::new(32));
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, _)) = accepted else {
            continue;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let readiness = readiness.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = handle_health_request(stream, readiness, metrics).await;
        });
    }
}

async fn handle_health_request(
    mut stream: TcpStream,
    readiness: Readiness,
    metrics: Metrics,
) -> io::Result<()> {
    let mut buffer = [0_u8; 2048];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
        .await
        .unwrap_or(Ok(0))?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, content_type, body) = match path {
        "/healthz" if readiness.healthy() => (200, "text/plain", "ok\n".to_string()),
        "/healthz" => (503, "text/plain", "event loop stopped\n".to_string()),
        "/readyz" => match readiness.ready() {
            Ok(()) => (200, "text/plain", "ready\n".to_string()),
            Err(reason) => (503, "text/plain", format!("{reason}\n")),
        },
        "/metrics" => (200, "text/plain; version=0.0.4", metrics.render(&readiness)),
        _ => (404, "text/plain", "not found\n".to_string()),
    };
    write_http_response(&mut stream, status, content_type, &body).await
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate.recv() => {},
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn run_file_recorder() -> io::Result<()> {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/feed/recorded.ndjson".to_string());
    let mut file = File::create(output)?;
    for line in io::stdin().lock().lines() {
        writeln!(file, "{}", line?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(path: &str, readiness: Readiness, metrics: Metrics) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let path = path.to_string();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nhost: recorder\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            String::from_utf8(response).unwrap()
        });
        let (server, _) = listener.accept().await.unwrap();
        handle_health_request(server, readiness, metrics)
            .await
            .unwrap();
        client.await.unwrap()
    }

    #[tokio::test]
    async fn health_is_live_while_readiness_tracks_dependencies() {
        let readiness = Readiness::new();
        let metrics = Metrics::default();
        let health = request("/healthz", readiness.clone(), metrics.clone()).await;
        assert!(health.starts_with("HTTP/1.1 200 OK"));

        let not_ready = request("/readyz", readiness.clone(), metrics.clone()).await;
        assert!(not_ready.starts_with("HTTP/1.1 503 Service Unavailable"));
        assert!(not_ready.ends_with("PostgreSQL unavailable\n"));

        readiness.set_postgres_connected(true);
        readiness.set_schema_verified(true);
        readiness.set_jetstream_connected(true);
        readiness.set_stream_ready(true);
        readiness.set_consumer_ready(true);
        readiness.set_fetching_active(true);
        let ready = request("/readyz", readiness.clone(), metrics.clone()).await;
        assert!(ready.starts_with("HTTP/1.1 200 OK"));
        assert!(ready.ends_with("ready\n"));

        let metric_response = request("/metrics", readiness, metrics).await;
        assert!(metric_response.contains("recorder_readiness 1"));
    }

    #[test]
    fn persistence_policy_is_explicit_and_unknown_values_fail() {
        assert_eq!(
            PersistencePolicy::parse("money_path_v1"),
            Ok(PersistencePolicy::MoneyPathV1)
        );
        assert!(PersistencePolicy::parse("all_events").is_err());
        assert!(PersistencePolicy::parse("").is_err());
    }

    #[test]
    fn reviewed_router_list_is_exact_and_unambiguous() {
        let raw = money_path_classifier::REVIEWED_ROUTER_ADDRESSES.join(",");
        assert_eq!(parse_router_addresses(&raw).unwrap().len(), 3);
        assert!(parse_router_addresses("router, with, spaces").is_err());
        assert!(parse_router_addresses("").is_err());
    }
}
