use phoenix_recorder::jetstream::{
    ensure_durable_pipeline, MessageFetcher, CONSUMER_ACK_WAIT, CONSUMER_MAX_ACK_PENDING,
    DURABLE_CONSUMER_NAME, STREAM_NAME,
};
use phoenix_recorder::logging::LogSampler;
use phoenix_recorder::metrics::Metrics;
use phoenix_recorder::persistence::{EventStore, PostgresStore};
use phoenix_recorder::runtime::{
    consume_durable_messages, mark_nats_connected, mark_nats_disconnected, monitor_database,
    nats_connect_options, BatchConfig, ConsumerExit, RetryPolicy,
};
use phoenix_recorder::state::Readiness;
use std::error::Error;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Config {
    health_addr: String,
    postgres_dsn: String,
    pg_ssl_mode: String,
    nats_url: String,
    batch: BatchConfig,
}

impl Config {
    fn from_env() -> Result<Self, &'static str> {
        let batch = BatchConfig {
            max_size: optional_usize("RECORDER_BATCH_MAX_SIZE", 256)?,
            max_wait: Duration::from_millis(optional_u64("RECORDER_BATCH_MAX_WAIT_MS", 100)?),
        }
        .validate()
        .map_err(|_| "invalid Recorder batch configuration")?;
        Ok(Self {
            health_addr: std::env::var("RECORDER_HEALTH_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9400".to_string()),
            postgres_dsn: required_env("POSTGRES_DSN")?,
            pg_ssl_mode: std::env::var("PGSSLMODE").unwrap_or_else(|_| "prefer".to_string()),
            nats_url: required_env("NATS_URL")?,
            batch,
        })
    }
}

fn optional_usize(name: &'static str, default: usize) -> Result<usize, &'static str> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .map_err(|_| "invalid Recorder batch configuration"),
        Err(_) => Ok(default),
    }
}

fn optional_u64(name: &'static str, default: u64) -> Result<u64, &'static str> {
    match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| "invalid Recorder batch configuration"),
        Err(_) => Ok(default),
    }
}

fn required_env(name: &'static str) -> Result<String, &'static str> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(name)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if !daemon_enabled() {
        return run_file_recorder().map_err(Into::into);
    }

    init_logging();
    if let Err(error) = run_daemon().await {
        tracing::error!(event = "recorder_stopped", error_class = error);
        return Err(io::Error::other(error).into());
    }
    Ok(())
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

async fn run_daemon() -> Result<(), &'static str> {
    let config = Config::from_env().map_err(|_| "required Recorder environment is missing")?;
    let readiness = Readiness::new();
    let metrics = Metrics::default();
    let sampler = LogSampler::default();
    let shutdown = CancellationToken::new();

    tracing::info!(
        event = "recorder_startup",
        nats_subject = phoenix_recorder::NATS_SUBJECT,
        nats_delivery = "jetstream_durable_pull",
        stream = STREAM_NAME,
        durable_consumer = DURABLE_CONSUMER_NAME,
        batch_max_messages = config.batch.max_size,
        batch_max_wait_ms = config.batch.max_wait.as_millis() as u64,
        max_ack_pending = CONSUMER_MAX_ACK_PENDING,
        ack_wait_seconds = CONSUMER_ACK_WAIT.as_secs()
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
    .ok_or("Recorder shutdown before PostgreSQL initialization")?;
    let store: Arc<dyn EventStore> = Arc::new(store);
    let database_monitor = tokio::spawn(monitor_database(
        store.clone(),
        readiness.clone(),
        metrics.clone(),
        sampler.clone(),
        shutdown.clone(),
        Duration::from_secs(5),
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
    readiness.stop_event_loop();
    let _ = health_task.await;
    tracing::info!(event = "recorder_graceful_shutdown_complete");
    if integrity_failure {
        Err("Recorder stopped on a terminal integrity condition")
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
    loop {
        match PostgresStore::connect(&config.postgres_dsn, &config.pg_ssl_mode).await {
            Ok(store) => {
                readiness.set_postgres_connected(true);
                tracing::info!(event = "recorder_postgres_connected");
                match store.verify_schema().await {
                    Ok(()) => {
                        readiness.set_schema_verified(true);
                        tracing::info!(event = "recorder_schema_verified");
                        return Some(store);
                    }
                    Err(error) => {
                        metrics.database_failure();
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
                metrics.database_failure();
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
}
