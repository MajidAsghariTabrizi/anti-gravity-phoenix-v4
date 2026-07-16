use crate::economic::RpcMethod;
use crate::providers::ProviderLease;
use crate::shadow_state::MAX_GATEWAY_RESPONSE_BYTES;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant, SystemTime};
use thiserror::Error;

const JSON_RPC_ID: u64 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(5);
pub const MAX_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct RpcCallResult {
    pub value: Value,
    pub latency_ns: u128,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error("RPC provider request timed out")]
    Timeout,
    #[error("RPC provider HTTP request failed")]
    Http,
    #[error("RPC provider response exceeded the configured bound")]
    Oversized,
    #[error("RPC provider returned an invalid JSON-RPC response")]
    InvalidResponse,
    #[error("RPC provider returned a JSON-RPC error")]
    ProviderError,
    #[error("RPC provider applied an HTTP rate limit")]
    RateLimited { retry_after: Duration },
}

impl TransportError {
    pub const fn class(self) -> &'static str {
        match self {
            Self::Timeout => "provider_timeout",
            Self::Http => "provider_http_failure",
            Self::Oversized => "provider_oversized_response",
            Self::InvalidResponse => "provider_invalid_response",
            Self::ProviderError => "provider_rpc_error",
            Self::RateLimited { .. } => "provider_rate_limited",
        }
    }

    pub const fn timeout(self) -> bool {
        matches!(self, Self::Timeout)
    }

    pub const fn retry_after(self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => Some(retry_after),
            _ => None,
        }
    }
}

#[async_trait]
pub trait JsonRpcClient: Send + Sync {
    async fn call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        timeout: Duration,
    ) -> Result<RpcCallResult, TransportError>;
}

#[derive(Clone, Debug)]
pub struct ReqwestJsonRpcClient {
    client: reqwest::Client,
}

impl ReqwestJsonRpcClient {
    pub fn new() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("anti-gravity-phoenix-rpc-gateway/4")
            .build()
            .map_err(|_| TransportError::Http)?;
        Ok(Self { client })
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcEnvelope {
    jsonrpc: String,
    id: Value,
    result: Option<Value>,
    error: Option<JsonRpcErrorBody>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcErrorBody {
    code: i64,
}

#[async_trait]
impl JsonRpcClient for ReqwestJsonRpcClient {
    async fn call(
        &self,
        provider: &ProviderLease,
        method: RpcMethod,
        params: Value,
        timeout: Duration,
    ) -> Result<RpcCallResult, TransportError> {
        let started = Instant::now();
        let response = self
            .client
            .post(provider.url())
            .timeout(timeout)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": JSON_RPC_ID,
                "method": method.as_str(),
                "params": params
            }))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Http
                }
            })?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(TransportError::RateLimited {
                retry_after: parse_retry_after(
                    response.headers().get(reqwest::header::RETRY_AFTER),
                    SystemTime::now(),
                ),
            });
        }
        if !response.status().is_success() {
            return Err(TransportError::Http);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_GATEWAY_RESPONSE_BYTES as u64)
        {
            return Err(TransportError::Oversized);
        }

        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                if error.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Http
                }
            })?;
            if bytes.len().saturating_add(chunk.len()) > MAX_GATEWAY_RESPONSE_BYTES {
                return Err(TransportError::Oversized);
            }
            bytes.extend_from_slice(&chunk);
        }
        let envelope: JsonRpcEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidResponse)?;
        if envelope.jsonrpc != "2.0"
            || envelope.id.as_u64() != Some(JSON_RPC_ID)
            || (envelope.result.is_some() == envelope.error.is_some())
        {
            return Err(TransportError::InvalidResponse);
        }
        if let Some(error) = envelope.error {
            let _bounded_error_code = error.code;
            return Err(TransportError::ProviderError);
        }
        Ok(RpcCallResult {
            value: envelope.result.ok_or(TransportError::InvalidResponse)?,
            latency_ns: started.elapsed().as_nanos(),
        })
    }
}

fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>, now: SystemTime) -> Duration {
    let parsed = value
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .trim()
                .parse::<u64>()
                .ok()
                .map(Duration::from_secs)
                .or_else(|| {
                    httpdate::parse_http_date(value)
                        .ok()
                        .and_then(|deadline| deadline.duration_since(now).ok())
                })
        })
        .unwrap_or(DEFAULT_RATE_LIMIT_COOLDOWN);
    parsed
        .max(Duration::from_secs(1))
        .min(MAX_RATE_LIMIT_COOLDOWN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::parse_provider_config;
    use std::collections::HashSet;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn transport_errors_are_bounded_and_never_echo_provider_urls() {
        for error in [
            TransportError::Timeout,
            TransportError::Http,
            TransportError::Oversized,
            TransportError::InvalidResponse,
            TransportError::ProviderError,
            TransportError::RateLimited {
                retry_after: Duration::from_secs(10),
            },
        ] {
            let rendered = error.to_string().to_ascii_lowercase();
            assert!(!rendered.contains("https://"));
            assert!(!rendered.contains("token"));
            assert!(error.class().len() <= 64);
        }
    }

    #[test]
    fn retry_after_supports_delta_seconds_and_clamps_provider_input() {
        let ten = reqwest::header::HeaderValue::from_static("10");
        let huge = reqwest::header::HeaderValue::from_static("999999");
        assert_eq!(
            parse_retry_after(Some(&ten), SystemTime::UNIX_EPOCH),
            Duration::from_secs(10)
        );
        assert_eq!(
            parse_retry_after(Some(&huge), SystemTime::UNIX_EPOCH),
            MAX_RATE_LIMIT_COOLDOWN
        );
        assert_eq!(
            parse_retry_after(None, SystemTime::UNIX_EPOCH),
            DEFAULT_RATE_LIMIT_COOLDOWN
        );
    }

    #[test]
    fn client_disables_redirect_following_and_builds_without_credentials() {
        assert!(ReqwestJsonRpcClient::new().is_ok());
    }

    #[tokio::test]
    async fn reqwest_transport_round_trips_a_bounded_json_rpc_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
                if request
                    .windows(b"eth_chainId".len())
                    .any(|window| window == b"eth_chainId")
                {
                    break;
                }
                assert!(request.len() < 4096);
            }
            let body = r#"{"jsonrpc":"2.0","id":1,"result":"0xa4b1"}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let config = parse_provider_config(&format!("http://{address}"), "1").unwrap();
        let mut pool = config.into_pool(Instant::now());
        let provider = pool.reserve_best(Instant::now(), &HashSet::new()).unwrap();
        let result = ReqwestJsonRpcClient::new()
            .unwrap()
            .call(
                &provider,
                RpcMethod::EthChainId,
                json!([]),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(result.value, json!("0xa4b1"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn loopback_http_429_parses_and_clamps_retry_after() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let read = stream.read(&mut request).await.unwrap();
            assert!(read > 0);
            stream
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 120\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let config = parse_provider_config(&format!("http://{address}"), "1").unwrap();
        let mut pool = config.into_pool(Instant::now());
        let provider = pool.reserve_best(Instant::now(), &HashSet::new()).unwrap();
        let result = ReqwestJsonRpcClient::new()
            .unwrap()
            .call(
                &provider,
                RpcMethod::EthChainId,
                json!([]),
                Duration::from_secs(2),
            )
            .await;
        assert!(matches!(
            result,
            Err(TransportError::RateLimited { retry_after })
                if retry_after == MAX_RATE_LIMIT_COOLDOWN
        ));
        server.await.unwrap();
    }
}
