use phoenix_engine::config::EngineConfig;
use phoenix_engine::domain::Address;
use phoenix_engine::engine_jetstream::{JetStreamFetcher, MessageFetcher};
use phoenix_engine::execution::ExecutionMode;
use phoenix_engine::metrics::RuntimeMetrics;
use phoenix_engine::origin::{reviewed_router_kind, REVIEWED_ROUTER_ADDRESSES};
use phoenix_engine::persistence::{PostgresShadowStore, ShadowStore};
use phoenix_engine::readiness::initialize_runtime;
use phoenix_engine::rpc_evaluator::{RpcCandidateEvaluator, RpcGatewayClient, ShadowStateClient};
use phoenix_engine::runtime::{consume_engine_messages, RuntimeExit};
use phoenix_engine::runtime_state::RuntimeReadiness;
use phoenix_engine::shadow_processor::{RouteRegistry, ShadowProcessor};
use phoenix_recorder::engine_stream::{
    ensure_engine_pipeline, ENGINE_DURABLE_NAME, ENGINE_STREAM_NAME,
};
use phoenix_recorder::logging::LogSampler;
use std::collections::HashSet;
use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const DEFAULT_ROUTERS: &str = "0xe592427a0aece92de3edee1f18e0157c05861564,0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45,0xa51afafe0263b40edaef0df8781ea9aa03e381a3";
const RETRY_INITIAL: Duration = Duration::from_secs(1);
const RETRY_MAXIMUM: Duration = Duration::from_secs(30);
const DATABASE_MONITOR_INTERVAL: Duration = Duration::from_secs(5);
const RPC_MONITOR_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
struct DaemonConfig {
    engine: EngineConfig,
    health_addr: String,
    postgres_dsn: String,
    pg_ssl_mode: String,
    routers: Vec<Address>,
    routes: RouteRegistry,
    rpc_gateway_url: String,
    code_version: String,
}

impl DaemonConfig {
    fn from_env() -> Result<Self, &'static str> {
        let engine = EngineConfig::from_env();
        let initialized =
            initialize_runtime(&engine).map_err(|_| "invalid Engine configuration")?;
        if initialized.mode != ExecutionMode::Shadow {
            return Err("Engine durable runtime requires SHADOW mode");
        }
        let postgres_dsn = required_env("POSTGRES_DSN")?;
        let routers = parse_routers(
            &std::env::var("ENGINE_ROUTER_ADDRESSES")
                .unwrap_or_else(|_| DEFAULT_ROUTERS.to_string()),
        )?;
        let routes = RouteRegistry::from_json(
            &std::env::var("ENGINE_ROUTE_REGISTRY_JSON").unwrap_or_else(|_| "[]".to_string()),
        )
        .map_err(|_| "invalid Engine route registry")?;
        Ok(Self {
            engine,
            health_addr: std::env::var("ENGINE_HEALTH_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9200".to_string()),
            postgres_dsn,
            pg_ssl_mode: std::env::var("PGSSLMODE").unwrap_or_else(|_| "prefer".to_string()),
            routers,
            routes,
            rpc_gateway_url: std::env::var("RPC_GATEWAY_URL")
                .unwrap_or_else(|_| "http://rpc-gateway:9300".to_string()),
            code_version: std::env::var("PHOENIX_CODE_VERSION")
                .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string()),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging();
    if let Err(error) = run_daemon().await {
        tracing::error!(event = "phoenix_engine_stopped", error_class = error);
        return Err(io::Error::other(error).into());
    }
    Ok(())
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
    let config = DaemonConfig::from_env()?;
    let readiness = RuntimeReadiness::new();
    let metrics = RuntimeMetrics::default();
    let sampler = LogSampler::default();
    let shutdown = CancellationToken::new();

    let strategy_configured = !config.routes.is_empty();
    readiness.set_strategy_configured(strategy_configured);
    readiness.set_evaluation_dependencies_ready(false);
    let rpc_client = Arc::new(
        RpcGatewayClient::new(&config.rpc_gateway_url)
            .map_err(|_| "invalid RPC Gateway configuration")?,
    );
    let evaluator = Arc::new(
        RpcCandidateEvaluator::with_metrics(
            rpc_client.clone(),
            config.code_version.clone(),
            metrics.clone(),
        )
        .map_err(|_| "invalid Engine evaluator configuration")?,
    );
    let processor = Arc::new(
        ShadowProcessor::new(config.routers.clone(), config.routes.clone(), evaluator)
            .map_err(|_| "invalid Engine router registry")?,
    );

    tracing::info!(
        event = "phoenix_engine_startup",
        mode = "SHADOW",
        stream = ENGINE_STREAM_NAME,
        durable_consumer = ENGINE_DURABLE_NAME,
        strategy_configured,
        evaluation_backend = "block_pinned_rpc_state",
        simulation_level = "state_based",
        live_execution = false
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
        tracing::info!(event = "phoenix_engine_graceful_shutdown_started");
        signal_shutdown.cancel();
    });
    let rpc_monitor = tokio::spawn(monitor_rpc_gateway(
        rpc_client,
        readiness.clone(),
        sampler.clone(),
        shutdown.clone(),
    ));

    if std::env::var("PHOENIX_ONESHOT")
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        shutdown.cancel();
        let _ = rpc_monitor.await;
        let _ = health_task.await;
        return Ok(());
    }

    let store = connect_postgres_until_ready(&config, &readiness, &metrics, &sampler, &shutdown)
        .await
        .ok_or("Engine shutdown before PostgreSQL initialization")?;
    let store: Arc<dyn ShadowStore> = Arc::new(store);
    let database_monitor = tokio::spawn(monitor_database(
        store.clone(),
        readiness.clone(),
        metrics.clone(),
        sampler.clone(),
        shutdown.clone(),
    ));

    let mut integrity_failure = false;
    let mut connection_attempt = 0_u64;
    loop {
        if shutdown.is_cancelled() {
            break;
        }
        connection_attempt = connection_attempt.saturating_add(1);
        if connection_attempt > 1 {
            tracing::warn!(
                event = "phoenix_engine_nats_reconnect_attempt",
                reconnect_attempt = connection_attempt - 1
            );
        }
        let client = match async_nats::connect(config.engine.nats_url.clone()).await {
            Ok(client) => {
                readiness.set_nats_connected(true);
                client
            }
            Err(_) => {
                readiness.set_nats_connected(false);
                sampled_warning(
                    &sampler,
                    "engine_nats_connect_failure",
                    "phoenix_engine_nats_connect_failed",
                );
                if sleep_or_shutdown(RETRY_INITIAL, &shutdown).await {
                    break;
                }
                continue;
            }
        };

        let consumer = match ensure_engine_pipeline(&client).await {
            Ok(consumer) => {
                readiness.set_stream_ready(true);
                readiness.set_consumer_ready(true);
                tracing::info!(
                    event = "phoenix_engine_pipeline_ready",
                    stream = ENGINE_STREAM_NAME,
                    durable_consumer = ENGINE_DURABLE_NAME
                );
                consumer
            }
            Err(error) => {
                readiness.set_stream_ready(false);
                readiness.set_consumer_ready(false);
                if error.terminal() {
                    readiness.mark_integrity_loss();
                    tracing::error!(
                        event = "phoenix_engine_pipeline_incompatible",
                        error_class = %error
                    );
                    integrity_failure = true;
                    break;
                }
                sampled_warning(
                    &sampler,
                    "engine_pipeline_failure",
                    "phoenix_engine_pipeline_unavailable",
                );
                readiness.set_nats_connected(false);
                if sleep_or_shutdown(RETRY_INITIAL, &shutdown).await {
                    break;
                }
                continue;
            }
        };

        let fetcher: Arc<dyn MessageFetcher> = Arc::new(JetStreamFetcher::new(consumer));
        let exit = consume_engine_messages(
            fetcher,
            store.clone(),
            processor.clone(),
            readiness.clone(),
            metrics.clone(),
            sampler.clone(),
            shutdown.clone(),
        )
        .await;
        readiness.set_nats_connected(false);
        match exit {
            RuntimeExit::Shutdown => break,
            RuntimeExit::IntegrityFailure => {
                integrity_failure = true;
                break;
            }
            RuntimeExit::StoreFailed => {
                wait_for_store_recovery(store.as_ref(), &readiness, &metrics, &sampler, &shutdown)
                    .await;
            }
            RuntimeExit::FetchFailed | RuntimeExit::AcknowledgementFailed => {}
        }
        if sleep_or_shutdown(RETRY_INITIAL, &shutdown).await {
            break;
        }
    }

    shutdown.cancel();
    let _ = database_monitor.await;
    let _ = rpc_monitor.await;
    readiness.stop_event_loop();
    let _ = health_task.await;
    tracing::info!(event = "phoenix_engine_graceful_shutdown_complete");
    if integrity_failure {
        Err("Engine stopped on a terminal integrity condition")
    } else {
        Ok(())
    }
}

async fn monitor_rpc_gateway(
    client: Arc<RpcGatewayClient>,
    readiness: RuntimeReadiness,
    sampler: LogSampler,
    shutdown: CancellationToken,
) {
    loop {
        match client.ready().await {
            Ok(true) => readiness.set_evaluation_dependencies_ready(true),
            Ok(false) | Err(_) => {
                readiness.set_evaluation_dependencies_ready(false);
                sampled_warning(
                    &sampler,
                    "engine_rpc_gateway_monitor_failure",
                    "phoenix_engine_rpc_gateway_unready",
                );
            }
        }
        if sleep_or_shutdown(RPC_MONITOR_INTERVAL, &shutdown).await {
            return;
        }
    }
}

async fn connect_postgres_until_ready(
    config: &DaemonConfig,
    readiness: &RuntimeReadiness,
    metrics: &RuntimeMetrics,
    sampler: &LogSampler,
    shutdown: &CancellationToken,
) -> Option<PostgresShadowStore> {
    let mut delay = RETRY_INITIAL;
    loop {
        match PostgresShadowStore::connect(&config.postgres_dsn, &config.pg_ssl_mode).await {
            Ok(store) => {
                readiness.set_postgres_connected(true);
                match store.verify_schema().await {
                    Ok(()) => {
                        readiness.set_schema_verified(true);
                        readiness.set_persistence_healthy(true);
                        tracing::info!(event = "phoenix_engine_schema_verified");
                        return Some(store);
                    }
                    Err(_) => {
                        metrics.processing_failure();
                        readiness.set_schema_verified(false);
                        sampled_warning(
                            sampler,
                            "engine_initial_schema_failure",
                            "phoenix_engine_schema_verification_failed",
                        );
                    }
                }
            }
            Err(_) => {
                metrics.processing_failure();
                readiness.set_postgres_connected(false);
                sampled_warning(
                    sampler,
                    "engine_initial_postgres_failure",
                    "phoenix_engine_postgres_connect_failed",
                );
            }
        }
        if sleep_or_shutdown(delay, shutdown).await {
            return None;
        }
        delay = delay.saturating_mul(2).min(RETRY_MAXIMUM);
    }
}

async fn monitor_database(
    store: Arc<dyn ShadowStore>,
    readiness: RuntimeReadiness,
    metrics: RuntimeMetrics,
    sampler: LogSampler,
    shutdown: CancellationToken,
) {
    loop {
        if sleep_or_shutdown(DATABASE_MONITOR_INTERVAL, &shutdown).await {
            return;
        }
        match store.ping().await {
            Ok(()) => {
                readiness.set_postgres_connected(true);
                readiness.set_persistence_healthy(true);
            }
            Err(_) => {
                metrics.processing_failure();
                readiness.set_postgres_connected(false);
                sampled_warning(
                    &sampler,
                    "engine_postgres_monitor_failure",
                    "phoenix_engine_postgres_health_failed",
                );
            }
        }
    }
}

async fn wait_for_store_recovery(
    store: &dyn ShadowStore,
    readiness: &RuntimeReadiness,
    metrics: &RuntimeMetrics,
    sampler: &LogSampler,
    shutdown: &CancellationToken,
) {
    let mut delay = RETRY_INITIAL;
    loop {
        match store.ping().await {
            Ok(()) => {
                readiness.set_postgres_connected(true);
                match store.verify_schema().await {
                    Ok(()) => {
                        readiness.set_schema_verified(true);
                        readiness.set_persistence_healthy(true);
                        return;
                    }
                    Err(_) => readiness.set_schema_verified(false),
                }
            }
            Err(_) => readiness.set_postgres_connected(false),
        }
        metrics.processing_failure();
        sampled_warning(
            sampler,
            "engine_postgres_recovery_failure",
            "phoenix_engine_postgres_recovery_waiting",
        );
        if sleep_or_shutdown(delay, shutdown).await {
            return;
        }
        delay = delay.saturating_mul(2).min(RETRY_MAXIMUM);
    }
}

async fn serve_health(
    addr: String,
    readiness: RuntimeReadiness,
    metrics: RuntimeMetrics,
    shutdown: CancellationToken,
) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(_) => {
            readiness.stop_event_loop();
            shutdown.cancel();
            tracing::error!(event = "phoenix_engine_health_bind_failed");
            return;
        }
    };
    tracing::info!(event = "phoenix_engine_health_listening", address = %addr);
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
    readiness: RuntimeReadiness,
    metrics: RuntimeMetrics,
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

fn parse_routers(raw: &str) -> Result<Vec<Address>, &'static str> {
    let values = raw.split(',').map(str::trim).collect::<Vec<_>>();
    if values.is_empty()
        || values.len() > REVIEWED_ROUTER_ADDRESSES.len()
        || values.iter().any(|value| value.is_empty())
    {
        return Err("invalid Engine router registry");
    }
    let routers = values
        .into_iter()
        .map(|value| Address::parse(value).map_err(|_| "invalid Engine router registry"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = HashSet::new();
    if routers
        .iter()
        .any(|router| reviewed_router_kind(router).is_none() || !seen.insert(router.clone()))
    {
        return Err("invalid Engine router registry");
    }
    Ok(routers)
}

fn required_env(name: &'static str) -> Result<String, &'static str> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(name)
}

fn sampled_warning(sampler: &LogSampler, class: &'static str, event: &'static str) {
    if let Some(suppressed) = sampler.sample(class) {
        tracing::warn!(event = event, failure_class = class, suppressed);
    }
}

async fn sleep_or_shutdown(duration: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = shutdown.cancelled() => true,
        _ = tokio::time::sleep(duration) => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(path: &str, readiness: RuntimeReadiness, metrics: RuntimeMetrics) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let path = path.to_string();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nhost: engine\r\n\r\n").as_bytes())
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

    #[test]
    fn router_configuration_is_bounded_and_canonicalized() {
        let routers = parse_routers(DEFAULT_ROUTERS).unwrap();
        assert_eq!(routers.len(), REVIEWED_ROUTER_ADDRESSES.len());
        assert_eq!(routers[0].as_str(), REVIEWED_ROUTER_ADDRESSES[0]);
        assert!(parse_routers("").is_err());
        assert!(parse_routers("not-an-address").is_err());
        assert!(parse_routers("0x1b81d678ffb9c0263b24a97847620c99d213eb14").is_err());
        assert!(parse_routers(&format!(
            "{},{}",
            REVIEWED_ROUTER_ADDRESSES[0], REVIEWED_ROUTER_ADDRESSES[0]
        ))
        .is_err());
    }

    #[tokio::test]
    async fn health_stays_live_while_runtime_readiness_is_fail_closed() {
        let readiness = RuntimeReadiness::new();
        let metrics = RuntimeMetrics::default();
        let health = request("/healthz", readiness.clone(), metrics.clone()).await;
        assert!(health.starts_with("HTTP/1.1 200 OK"));
        let not_ready = request("/readyz", readiness.clone(), metrics.clone()).await;
        assert!(not_ready.starts_with("HTTP/1.1 503 Service Unavailable"));
        let metric_response = request("/metrics", readiness, metrics).await;
        assert!(metric_response.contains("phoenix_engine_readiness 0"));
    }

    #[test]
    fn daemon_source_contains_no_signing_or_submission_capability() {
        let source = include_str!("main.rs");
        for forbidden in [
            ["send", "_raw", "_transaction"].concat(),
            ["sign", "_transaction"].concat(),
            ["PRIVATE", "_KEY"].concat(),
            ["SIGN", "ER"].concat(),
        ] {
            assert!(!source.contains(&forbidden));
        }
    }
}
