use rpc_gateway::economic::MethodTimeouts;
use rpc_gateway::metrics::RuntimeRpcMetrics;
use rpc_gateway::providers::parse_provider_config;
use rpc_gateway::runtime::{GatewayError, GatewayLimits, GatewayRuntime};
use rpc_gateway::runtime_state::GatewayReadiness;
use rpc_gateway::shadow_state::{
    ShadowStateRequest, MAX_GATEWAY_REQUEST_BYTES, SHADOW_STATE_SCHEMA_VERSION,
};
use rpc_gateway::transport::ReqwestJsonRpcClient;
use serde::Serialize;
use std::error::Error;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(3);
const STATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Clone, Debug)]
struct Config {
    address: String,
    providers: rpc_gateway::providers::ProviderConfig,
    timeouts: MethodTimeouts,
    limits: GatewayLimits,
    provider_probe_interval: Duration,
}

impl Config {
    fn from_env() -> Result<Self, &'static str> {
        let urls = required_env("RPC_PROVIDER_URLS")?;
        let priorities = required_env("RPC_PROVIDER_WEIGHTS")?;
        let providers = parse_provider_config(&urls, &priorities)
            .map_err(|_| "invalid RPC provider configuration")?;
        Ok(Self {
            address: std::env::var("RPC_GATEWAY_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9300".to_string()),
            providers,
            timeouts: MethodTimeouts {
                eth_call: timeout_from_env("RPC_ETH_CALL_TIMEOUT_MS", 3_000)?,
                state_read: timeout_from_env("RPC_STATE_READ_TIMEOUT_MS", 2_000)?,
                logs: timeout_from_env("RPC_LOGS_TIMEOUT_MS", 10_000)?,
            },
            limits: GatewayLimits {
                upstream_calls_per_second: positive_u32_from_env(
                    "RPC_UPSTREAM_CALLS_PER_SECOND",
                    1,
                )?,
                upstream_call_burst: positive_u32_from_env("RPC_UPSTREAM_CALL_BURST", 4)?,
                state_requests_per_minute: positive_u32_from_env(
                    "RPC_STATE_REQUESTS_PER_MINUTE",
                    12,
                )?,
            },
            provider_probe_interval: Duration::from_secs(u64::from(positive_u32_from_env(
                "RPC_PROVIDER_PROBE_INTERVAL_SECONDS",
                60,
            )?)),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging();
    if let Err(error) = run().await {
        tracing::error!(event = "rpc_gateway_stopped", error_class = error);
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

async fn run() -> Result<(), &'static str> {
    let config = Config::from_env()?;
    let readiness = GatewayReadiness::new(true);
    let metrics = RuntimeRpcMetrics::default();
    let client =
        ReqwestJsonRpcClient::new().map_err(|_| "RPC HTTP client initialization failed")?;
    let runtime = Arc::new(GatewayRuntime::with_limits(
        config.providers,
        Arc::new(client),
        config.timeouts,
        metrics.clone(),
        readiness.clone(),
        config.limits,
    ));
    let shutdown = CancellationToken::new();

    tracing::info!(
        event = "rpc_gateway_startup",
        shadow_state_schema = SHADOW_STATE_SCHEMA_VERSION,
        provider_urls_logged = false
    );
    let probe_task = tokio::spawn(monitor_providers(
        runtime.clone(),
        config.provider_probe_interval,
        shutdown.clone(),
    ));
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        signal_shutdown.cancel();
    });

    serve(
        config.address,
        runtime,
        readiness.clone(),
        metrics,
        shutdown.clone(),
    )
    .await;
    shutdown.cancel();
    let _ = probe_task.await;
    readiness.stop_event_loop();
    Ok(())
}

async fn monitor_providers(
    runtime: Arc<GatewayRuntime>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        if let Err(error) = runtime.probe().await {
            tracing::warn!(
                event = "rpc_gateway_provider_probe_failed",
                error_class = error.class()
            );
        }
        if sleep_or_shutdown(interval, &shutdown).await {
            return;
        }
    }
}

async fn serve(
    address: String,
    runtime: Arc<GatewayRuntime>,
    readiness: GatewayReadiness,
    metrics: RuntimeRpcMetrics,
    shutdown: CancellationToken,
) {
    let listener = match TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(_) => {
            readiness.stop_event_loop();
            shutdown.cancel();
            tracing::error!(event = "rpc_gateway_bind_failed");
            return;
        }
    };
    tracing::info!(event = "rpc_gateway_listening", address = %address);
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
        let runtime = runtime.clone();
        let readiness = readiness.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = handle_request(stream, runtime, readiness, metrics).await;
        });
    }
}

async fn handle_request(
    mut stream: TcpStream,
    runtime: Arc<GatewayRuntime>,
    readiness: GatewayReadiness,
    metrics: RuntimeRpcMetrics,
) -> io::Result<()> {
    let request = match read_request(&mut stream).await {
        Ok(request) => request,
        Err(HttpReadError::Oversized) => {
            return write_text(&mut stream, 413, "request too large\n").await;
        }
        Err(HttpReadError::Invalid) => {
            return write_text(&mut stream, 400, "invalid request\n").await;
        }
    };
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/healthz") if readiness.healthy() => write_text(&mut stream, 200, "ok\n").await,
        ("GET", "/healthz") => write_text(&mut stream, 503, "event loop stopped\n").await,
        ("GET", "/readyz") => match readiness.ready() {
            Ok(()) => write_text(&mut stream, 200, "ready\n").await,
            Err(reason) => write_text(&mut stream, 503, &format!("{reason}\n")).await,
        },
        ("GET", "/metrics") => {
            write_response(
                &mut stream,
                200,
                "text/plain; version=0.0.4",
                metrics.render(&readiness).as_bytes(),
            )
            .await
        }
        ("POST", "/v1/shadow/state") => {
            let parsed: ShadowStateRequest = match serde_json::from_slice(&request.body) {
                Ok(parsed) => parsed,
                Err(_) => {
                    return write_json(&mut stream, 400, &GatewayError::InvalidRequest.response())
                        .await;
                }
            };
            match tokio::time::timeout(STATE_REQUEST_TIMEOUT, runtime.resolve_shadow_state(parsed))
                .await
            {
                Ok(Ok(response)) => write_json(&mut stream, 200, &response).await,
                Ok(Err(error)) => {
                    write_json(&mut stream, error.http_status(), &error.response()).await
                }
                Err(_) => {
                    let error = GatewayError::ProviderUnavailable;
                    write_json(&mut stream, error.http_status(), &error.response()).await
                }
            }
        }
        _ => write_text(&mut stream, 404, "not found\n").await,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpReadError {
    Invalid,
    Oversized,
}

async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpReadError> {
    let mut bytes = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 2048];
    let header_end = loop {
        let read = tokio::time::timeout(HTTP_READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| HttpReadError::Invalid)?
            .map_err(|_| HttpReadError::Invalid)?;
        if read == 0 {
            return Err(HttpReadError::Invalid);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_end(&bytes) {
            break end;
        }
        if bytes.len() > MAX_HTTP_HEADER_BYTES {
            return Err(HttpReadError::Oversized);
        }
    };
    if header_end > MAX_HTTP_HEADER_BYTES {
        return Err(HttpReadError::Oversized);
    }
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|_| HttpReadError::Invalid)?;
    let mut lines = header.split("\r\n");
    let mut request_line = lines
        .next()
        .ok_or(HttpReadError::Invalid)?
        .split_whitespace();
    let method = request_line
        .next()
        .ok_or(HttpReadError::Invalid)?
        .to_string();
    let path = request_line
        .next()
        .ok_or(HttpReadError::Invalid)?
        .to_string();
    let version = request_line.next().ok_or(HttpReadError::Invalid)?;
    if request_line.next().is_some()
        || !matches!(method.as_str(), "GET" | "POST")
        || !path.starts_with('/')
        || version != "HTTP/1.1"
    {
        return Err(HttpReadError::Invalid);
    }

    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(HttpReadError::Invalid)?;
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(HttpReadError::Invalid);
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| HttpReadError::Invalid)?,
            );
        }
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    if chunked {
        return Err(HttpReadError::Invalid);
    }
    let content_length = match method.as_str() {
        "POST" => content_length.ok_or(HttpReadError::Invalid)?,
        _ => content_length.unwrap_or(0),
    };
    if content_length > MAX_GATEWAY_REQUEST_BYTES {
        return Err(HttpReadError::Oversized);
    }
    let body_start = header_end + 4;
    let total = body_start
        .checked_add(content_length)
        .ok_or(HttpReadError::Oversized)?;
    while bytes.len() < total {
        let read = tokio::time::timeout(HTTP_READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| HttpReadError::Invalid)?
            .map_err(|_| HttpReadError::Invalid)?;
        if read == 0 {
            return Err(HttpReadError::Invalid);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > total {
            return Err(HttpReadError::Invalid);
        }
    }
    Ok(HttpRequest {
        method,
        path,
        body: bytes[body_start..total].to_vec(),
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| {
        b"{\"schema_version\":\"phoenix.rpc.shadow_state.v1\",\"error_class\":\"response_serialization_failure\",\"retryable\":false}".to_vec()
    });
    write_response(stream, status, "application/json", &body).await
}

async fn write_text(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    write_response(stream, status, "text/plain", body.as_bytes()).await
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await
}

fn timeout_from_env(name: &'static str, default_ms: u64) -> Result<Duration, &'static str> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| "invalid RPC timeout configuration")?,
        Err(_) => default_ms,
    };
    if !(100..=30_000).contains(&value) {
        return Err("invalid RPC timeout configuration");
    }
    Ok(Duration::from_millis(value))
}

fn positive_u32_from_env(name: &'static str, default: u32) -> Result<u32, &'static str> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<u32>()
            .map_err(|_| "invalid RPC budget configuration")?,
        Err(_) => default,
    };
    if value == 0 {
        return Err("invalid RPC budget configuration");
    }
    Ok(value)
}

fn required_env(name: &'static str) -> Result<String, &'static str> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(name)
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
    use rpc_gateway::metrics::REQUIRED_RPC_METRICS;

    #[test]
    fn parser_finds_headers_and_rejects_invalid_request_lines() {
        assert_eq!(
            find_header_end(b"GET /healthz HTTP/1.1\r\nhost: gateway\r\n\r\n"),
            Some(36)
        );
        assert_eq!(find_header_end(b"incomplete"), None);
    }

    #[test]
    fn every_declared_metric_is_rendered_by_runtime_metrics() {
        let metrics = RuntimeRpcMetrics::default();
        let rendered = metrics.render(&GatewayReadiness::new(true));
        assert!(REQUIRED_RPC_METRICS
            .iter()
            .all(|metric| rendered.contains(metric)));
    }

    #[test]
    fn source_does_not_log_provider_configuration_or_add_execution_capability() {
        let source = include_str!("main.rs");
        for forbidden in [
            ["provider", "_urls", " ="].concat(),
            ["send", "_raw", "_transaction"].concat(),
            ["PRIVATE", "_KEY"].concat(),
        ] {
            assert!(!source.to_ascii_lowercase().contains(&forbidden));
        }
    }
}
