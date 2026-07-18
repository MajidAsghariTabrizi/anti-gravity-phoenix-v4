use phoenix_recorder::dispatcher::{
    dispatch_once, refresh_backlog_telemetry, DispatchConfig, DispatcherError, DispatcherMetrics,
    DispatcherReadiness,
};
use phoenix_recorder::engine_outbox::{OutboxError, OutboxStore, PostgresOutbox};
use phoenix_recorder::engine_stream::{
    ensure_engine_stream, JetStreamEnginePublisher, ENGINE_STREAM_NAME, ENGINE_SUBJECT,
};
use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const DEPENDENCY_RETRY: Duration = Duration::from_secs(1);
const NATS_PROBE_INTERVAL: Duration = Duration::from_secs(5);
const BACKLOG_STATEMENT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
struct Config {
    health_addr: String,
    postgres_dsn: String,
    pg_ssl_mode: String,
    nats_url: String,
    dispatch: DispatchConfig,
    idle_poll: Duration,
    backlog_refresh: Duration,
}

impl Config {
    fn from_env() -> Result<Self, &'static str> {
        let mode = std::env::var("PHOENIX_MODE").unwrap_or_else(|_| "SHADOW".to_string());
        let live_execution = std::env::var("LIVE_EXECUTION")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !mode.eq_ignore_ascii_case("SHADOW") || live_execution {
            return Err("Shadow Dispatcher requires fail-closed SHADOW mode");
        }
        let owner = std::env::var("SHADOW_DISPATCHER_INSTANCE_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| std::env::var("HOSTNAME").ok())
            .unwrap_or_else(|| "shadow-dispatcher".to_string());
        if !valid_owner(&owner) {
            return Err("invalid Shadow Dispatcher instance identity");
        }
        let dispatch = DispatchConfig {
            owner,
            batch_size: optional_usize("SHADOW_DISPATCHER_BATCH_SIZE", 64)?,
            lease: Duration::from_secs(optional_u64("SHADOW_DISPATCHER_LEASE_SECONDS", 30)?),
            retry_initial: Duration::from_millis(optional_u64(
                "SHADOW_DISPATCHER_RETRY_INITIAL_MS",
                1_000,
            )?),
            retry_maximum: Duration::from_millis(optional_u64(
                "SHADOW_DISPATCHER_RETRY_MAX_MS",
                60_000,
            )?),
        }
        .validate()
        .map_err(|_| "invalid Shadow Dispatcher delivery configuration")?;
        let idle_poll = Duration::from_millis(optional_u64("SHADOW_DISPATCHER_IDLE_POLL_MS", 250)?);
        if idle_poll < Duration::from_millis(25) || idle_poll > Duration::from_secs(5) {
            return Err("invalid Shadow Dispatcher polling configuration");
        }
        let backlog_refresh = Duration::from_secs(optional_u64(
            "SHADOW_DISPATCHER_BACKLOG_REFRESH_SECONDS",
            60,
        )?);
        if !valid_backlog_refresh(backlog_refresh) {
            return Err("invalid Shadow Dispatcher backlog refresh configuration");
        }
        Ok(Self {
            health_addr: std::env::var("SHADOW_DISPATCHER_HEALTH_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9500".to_string()),
            postgres_dsn: required_env("POSTGRES_DSN")?,
            pg_ssl_mode: std::env::var("PGSSLMODE").unwrap_or_else(|_| "prefer".to_string()),
            nats_url: required_env("NATS_URL")?,
            dispatch,
            idle_poll,
            backlog_refresh,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging();
    if let Err(error) = run().await {
        tracing::error!(event = "shadow_dispatcher_stopped", error_class = error);
        return Err(io::Error::other(error).into());
    }
    Ok(())
}

async fn run() -> Result<(), &'static str> {
    let config = Config::from_env().map_err(|_| "Shadow Dispatcher configuration invalid")?;
    let readiness = DispatcherReadiness::new();
    let metrics = DispatcherMetrics::default();
    let shutdown = CancellationToken::new();

    tracing::info!(
        event = "shadow_dispatcher_startup",
        stream = ENGINE_STREAM_NAME,
        subject = ENGINE_SUBJECT,
        batch_max_messages = config.dispatch.batch_size,
        lease_seconds = config.dispatch.lease.as_secs(),
        backlog_refresh_seconds = config.backlog_refresh.as_secs(),
        backlog_statement_timeout_ms = BACKLOG_STATEMENT_TIMEOUT.as_millis() as u64
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
        tracing::info!(event = "shadow_dispatcher_graceful_shutdown_started");
        signal_shutdown.cancel();
    });

    let mut terminal_failure = false;
    while !shutdown.is_cancelled() {
        let store = match PostgresOutbox::connect(&config.postgres_dsn, &config.pg_ssl_mode).await {
            Ok(store) => store,
            Err(error) => {
                readiness.set_postgres_connected(false);
                log_dependency_failure("postgres_connect", error.class());
                if sleep_or_shutdown(DEPENDENCY_RETRY, &shutdown).await {
                    break;
                }
                continue;
            }
        };
        readiness.set_postgres_connected(true);
        if let Err(error) = store.verify_schema().await {
            readiness.set_schema_verified(false);
            if matches!(error, OutboxError::Schema | OutboxError::Configuration) {
                readiness.mark_terminal_integrity();
                terminal_failure = true;
                break;
            }
            readiness.set_postgres_connected(false);
            log_dependency_failure("schema_verify", error.class());
            if sleep_or_shutdown(DEPENDENCY_RETRY, &shutdown).await {
                break;
            }
            continue;
        }
        readiness.set_schema_verified(true);

        let client = match async_nats::connect(config.nats_url.clone()).await {
            Ok(client) => client,
            Err(_) => {
                readiness.set_jetstream_connected(false);
                log_dependency_failure("nats_connect", "connection");
                if sleep_or_shutdown(DEPENDENCY_RETRY, &shutdown).await {
                    break;
                }
                continue;
            }
        };
        readiness.set_jetstream_connected(true);
        match ensure_engine_stream(&client).await {
            Ok(_) => readiness.set_stream_compatible(true),
            Err(error) if error.terminal() => {
                readiness.mark_terminal_integrity();
                tracing::error!(
                    event = "shadow_dispatcher_stream_incompatible",
                    error_class = error.class()
                );
                terminal_failure = true;
                break;
            }
            Err(error) => {
                readiness.set_jetstream_connected(false);
                log_dependency_failure("stream_verify", error.class());
                if sleep_or_shutdown(DEPENDENCY_RETRY, &shutdown).await {
                    break;
                }
                continue;
            }
        }

        let publisher = JetStreamEnginePublisher::new(client.clone());
        let telemetry_shutdown = shutdown.child_token();
        let telemetry_task = tokio::spawn(run_backlog_telemetry(
            store.clone(),
            metrics.clone(),
            telemetry_shutdown.clone(),
            config.backlog_refresh,
            BACKLOG_STATEMENT_TIMEOUT,
        ));
        let mut last_nats_probe = Instant::now();
        loop {
            if shutdown.is_cancelled() {
                break;
            }
            if last_nats_probe.elapsed() >= NATS_PROBE_INTERVAL {
                if client.flush().await.is_err() {
                    readiness.set_jetstream_connected(false);
                    log_dependency_failure("nats_probe", "connection");
                    break;
                }
                last_nats_probe = Instant::now();
            }
            match dispatch_once(&store, &publisher, &config.dispatch, &readiness, &metrics).await {
                Ok(0) => {
                    if sleep_or_shutdown(config.idle_poll, &shutdown).await {
                        break;
                    }
                }
                Ok(rows) => {
                    tracing::info!(event = "shadow_dispatcher_batch_published", rows);
                }
                Err(DispatcherError::TerminalIntegrity) => {
                    terminal_failure = true;
                    break;
                }
                Err(DispatcherError::Stream(error)) => {
                    readiness.set_jetstream_connected(false);
                    log_dependency_failure("publish", error.class());
                    break;
                }
                Err(DispatcherError::Outbox(
                    OutboxError::Integrity | OutboxError::Configuration,
                )) => {
                    readiness.mark_terminal_integrity();
                    terminal_failure = true;
                    break;
                }
                Err(DispatcherError::Outbox(error)) => {
                    readiness.set_postgres_connected(false);
                    log_dependency_failure("outbox", error.class());
                    break;
                }
                Err(DispatcherError::Configuration) => {
                    readiness.mark_terminal_integrity();
                    terminal_failure = true;
                    break;
                }
            }
        }
        telemetry_shutdown.cancel();
        let _ = telemetry_task.await;
        if terminal_failure || shutdown.is_cancelled() {
            break;
        }
        if sleep_or_shutdown(DEPENDENCY_RETRY, &shutdown).await {
            break;
        }
    }

    shutdown.cancel();
    readiness.stop_event_loop();
    let _ = health_task.await;
    tracing::info!(event = "shadow_dispatcher_graceful_shutdown_complete");
    if terminal_failure {
        Err("Shadow Dispatcher stopped on terminal integrity failure")
    } else {
        Ok(())
    }
}

async fn run_backlog_telemetry(
    store: PostgresOutbox,
    metrics: DispatcherMetrics,
    shutdown: CancellationToken,
    refresh_interval: Duration,
    statement_timeout: Duration,
) {
    loop {
        if let Err(error) = refresh_backlog_telemetry(&store, &metrics, statement_timeout).await {
            tracing::warn!(
                event = "shadow_dispatcher_backlog_refresh_failed",
                error_class = error.class(),
                retry_delay_seconds = refresh_interval.as_secs()
            );
        }
        if sleep_or_shutdown(refresh_interval, &shutdown).await {
            return;
        }
    }
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

fn log_dependency_failure(dependency: &'static str, error_class: &'static str) {
    tracing::warn!(
        event = "shadow_dispatcher_dependency_failure",
        dependency,
        error_class,
        retry_delay_ms = DEPENDENCY_RETRY.as_millis() as u64
    );
}

async fn sleep_or_shutdown(duration: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = shutdown.cancelled() => true,
        _ = tokio::time::sleep(duration) => false,
    }
}

async fn serve_health(
    addr: String,
    readiness: DispatcherReadiness,
    metrics: DispatcherMetrics,
    shutdown: CancellationToken,
) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(_) => {
            readiness.stop_event_loop();
            shutdown.cancel();
            tracing::error!(event = "shadow_dispatcher_health_bind_failed");
            return;
        }
    };
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
    readiness: DispatcherReadiness,
    metrics: DispatcherMetrics,
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

fn valid_owner(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_backlog_refresh(value: Duration) -> bool {
    (Duration::from_secs(10)..=Duration::from_secs(300)).contains(&value)
}

fn optional_usize(name: &'static str, default: usize) -> Result<usize, &'static str> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .map_err(|_| "invalid Shadow Dispatcher numeric configuration"),
        Err(_) => Ok(default),
    }
}

fn optional_u64(name: &'static str, default: u64) -> Result<u64, &'static str> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| "invalid Shadow Dispatcher numeric configuration"),
        Err(_) => Ok(default),
    }
}

fn required_env(name: &'static str) -> Result<String, &'static str> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(name)
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(
        path: &str,
        readiness: DispatcherReadiness,
        metrics: DispatcherMetrics,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let path = path.to_string();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nhost: dispatcher\r\n\r\n").as_bytes())
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
    async fn health_is_independent_while_readiness_tracks_dispatcher_dependencies() {
        let readiness = DispatcherReadiness::new();
        let metrics = DispatcherMetrics::default();
        assert!(request("/healthz", readiness.clone(), metrics.clone())
            .await
            .starts_with("HTTP/1.1 200 OK"));
        assert!(request("/readyz", readiness, metrics)
            .await
            .starts_with("HTTP/1.1 503 Service Unavailable"));
    }

    #[tokio::test]
    async fn cancellation_interrupts_idle_backoff() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        assert!(sleep_or_shutdown(Duration::from_secs(60), &shutdown).await);
    }

    #[test]
    fn instance_identity_is_bounded_and_log_safe() {
        assert!(valid_owner("shadow-dispatcher-1"));
        assert!(!valid_owner("dispatcher with spaces"));
        assert!(!valid_owner(&"a".repeat(129)));
    }

    #[test]
    fn backlog_refresh_interval_is_bounded() {
        assert!(valid_backlog_refresh(Duration::from_secs(10)));
        assert!(valid_backlog_refresh(Duration::from_secs(60)));
        assert!(valid_backlog_refresh(Duration::from_secs(300)));
        assert!(!valid_backlog_refresh(Duration::from_secs(9)));
        assert!(!valid_backlog_refresh(Duration::from_secs(301)));
    }

    #[test]
    fn source_does_not_log_payloads_or_add_execution_capability() {
        let source = include_str!("shadow-dispatcher.rs");
        let payload_log = ["payload", " ="].concat();
        let signing_call = ["sign", "_transaction"].concat();
        let submission_call = ["send", "_raw", "_transaction"].concat();
        assert!(!source.contains(&payload_log));
        assert!(!source.contains(&signing_call));
        assert!(!source.contains(&submission_call));
    }
}
