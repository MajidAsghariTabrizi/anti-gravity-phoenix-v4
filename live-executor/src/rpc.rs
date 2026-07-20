use crate::abi::RpcLog;
use crate::model::{CanonicalAddress, TransactionHash};
use async_trait::async_trait;
use reqwest::redirect::Policy;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;
use url::Url;

const MAX_RPC_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const RPC_TIMEOUT_SECONDS: u64 = 10;
const ISOLATED_FORK_MARKER: &str = "CONFIRMED_LOCAL_ANVIL";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionReceipt {
    pub transaction_hash: TransactionHash,
    pub status: u64,
    pub block_number: u64,
    pub gas_used: u64,
    pub effective_gas_price: u128,
    pub logs: Vec<RpcLog>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcErrorKind {
    Transport,
    Timeout,
    ResponseTooLarge,
    MalformedResponse,
    RemoteFailure,
    NonceConflict,
    ChainMismatch,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("RPC request failed ({kind:?})")]
pub struct RpcError {
    pub kind: RpcErrorKind,
    pub remote_code: Option<i64>,
}

impl RpcError {
    fn new(kind: RpcErrorKind) -> Self {
        Self {
            kind,
            remote_code: None,
        }
    }
}

#[async_trait]
pub trait ExecutionRpc: Send + Sync {
    async fn chain_id(&self) -> Result<u64, RpcError>;
    async fn pending_nonce(&self, wallet: CanonicalAddress) -> Result<u64, RpcError>;
    async fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
    ) -> Result<TransactionHash, RpcError>;
    async fn transaction_receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError>;
    async fn transaction_known(&self, tx_hash: TransactionHash) -> Result<bool, RpcError>;
}

pub struct HttpExecutionRpc {
    client: Client,
    endpoint: Url,
    next_id: AtomicU64,
}

impl HttpExecutionRpc {
    pub fn new_production(endpoint: Url, allowlist: &[Url]) -> Result<Self, RpcError> {
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || !allowlist.iter().any(|allowed| allowed == &endpoint)
        {
            return Err(RpcError::new(RpcErrorKind::Transport));
        }
        Self::new(endpoint)
    }

    pub fn new_isolated_fork(endpoint: Url, marker: &str) -> Result<Self, RpcError> {
        let loopback = endpoint
            .host_str()
            .is_some_and(|host| host == "127.0.0.1" || host == "localhost" || host == "::1");
        if marker != ISOLATED_FORK_MARKER
            || endpoint.scheme() != "http"
            || !loopback
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
        {
            return Err(RpcError::new(RpcErrorKind::Transport));
        }
        Self::new(endpoint)
    }

    fn new(endpoint: Url) -> Result<Self, RpcError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(RPC_TIMEOUT_SECONDS))
            .https_only(endpoint.scheme() == "https")
            .redirect(Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| RpcError::new(RpcErrorKind::Transport))?;
        Ok(Self {
            client,
            endpoint,
            next_id: AtomicU64::new(1),
        })
    }

    async fn call(&self, method: &'static str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .map_err(classify_reqwest_error)?;
        if !response.status().is_success() {
            return Err(RpcError::new(RpcErrorKind::Transport));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RPC_RESPONSE_BYTES as u64)
        {
            return Err(RpcError::new(RpcErrorKind::ResponseTooLarge));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(classify_reqwest_error)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RPC_RESPONSE_BYTES {
                return Err(RpcError::new(RpcErrorKind::ResponseTooLarge));
            }
            bytes.extend_from_slice(&chunk);
        }
        let envelope: Value = serde_json::from_slice(&bytes)
            .map_err(|_| RpcError::new(RpcErrorKind::MalformedResponse))?;
        if envelope.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || envelope.get("id").and_then(Value::as_u64) != Some(id)
        {
            return Err(RpcError::new(RpcErrorKind::MalformedResponse));
        }
        if let Some(error) = envelope.get("error") {
            let code = error.get("code").and_then(Value::as_i64);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let kind = if message.contains("nonce too low")
                || message.contains("nonce too high")
                || message.contains("already known")
                || message.contains("replacement transaction underpriced")
            {
                RpcErrorKind::NonceConflict
            } else {
                RpcErrorKind::RemoteFailure
            };
            return Err(RpcError {
                kind,
                remote_code: code,
            });
        }
        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| RpcError::new(RpcErrorKind::MalformedResponse))
    }
}

#[async_trait]
impl ExecutionRpc for HttpExecutionRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        let value = self.call("eth_chainId", json!([])).await?;
        parse_hex_u64(value.as_str().ok_or_else(malformed)?)
    }

    async fn pending_nonce(&self, wallet: CanonicalAddress) -> Result<u64, RpcError> {
        let value = self
            .call(
                "eth_getTransactionCount",
                json!([wallet.to_string(), "pending"]),
            )
            .await?;
        parse_hex_u64(value.as_str().ok_or_else(malformed)?)
    }

    async fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
    ) -> Result<TransactionHash, RpcError> {
        let value = self
            .call(
                "eth_sendRawTransaction",
                json!([format!("0x{}", hex::encode(raw_transaction))]),
            )
            .await?;
        TransactionHash::parse(value.as_str().ok_or_else(malformed)?)
            .map_err(|_| RpcError::new(RpcErrorKind::MalformedResponse))
    }

    async fn transaction_receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError> {
        let value = self
            .call("eth_getTransactionReceipt", json!([tx_hash.to_string()]))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        let object = value.as_object().ok_or_else(malformed)?;
        let returned_hash = parse_hash_field(object, "transactionHash")?;
        let status = parse_u64_field(object, "status")?;
        if status > 1 {
            return Err(malformed());
        }
        let block_number = parse_u64_field(object, "blockNumber")?;
        let gas_used = parse_u64_field(object, "gasUsed")?;
        let effective_gas_price = parse_u128_field(object, "effectiveGasPrice")?;
        let logs = object
            .get("logs")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?
            .iter()
            .map(parse_log)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(TransactionReceipt {
            transaction_hash: returned_hash,
            status,
            block_number,
            gas_used,
            effective_gas_price,
            logs,
        }))
    }

    async fn transaction_known(&self, tx_hash: TransactionHash) -> Result<bool, RpcError> {
        let value = self
            .call("eth_getTransactionByHash", json!([tx_hash.to_string()]))
            .await?;
        if value.is_null() {
            Ok(false)
        } else if value.is_object() {
            Ok(true)
        } else {
            Err(malformed())
        }
    }
}

fn parse_log(value: &Value) -> Result<RpcLog, RpcError> {
    let object = value.as_object().ok_or_else(malformed)?;
    let address = object
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(malformed)
        .and_then(|value| {
            CanonicalAddress::parse(value)
                .map_err(|_| RpcError::new(RpcErrorKind::MalformedResponse))
        })?;
    let topics = object
        .get("topics")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?
        .iter()
        .map(|topic| {
            let value = topic.as_str().ok_or_else(malformed)?;
            parse_fixed_hex::<32>(value).ok_or_else(malformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data = object
        .get("data")
        .and_then(Value::as_str)
        .and_then(parse_hex_bytes)
        .ok_or_else(malformed)?;
    Ok(RpcLog {
        address,
        topics,
        data,
    })
}

fn parse_hash_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<TransactionHash, RpcError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(malformed)
        .and_then(|value| {
            TransactionHash::parse(value)
                .map_err(|_| RpcError::new(RpcErrorKind::MalformedResponse))
        })
}

fn parse_u64_field(object: &serde_json::Map<String, Value>, name: &str) -> Result<u64, RpcError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(malformed)
        .and_then(parse_hex_u64)
}

fn parse_u128_field(object: &serde_json::Map<String, Value>, name: &str) -> Result<u128, RpcError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(malformed)
        .and_then(parse_hex_u128)
}

fn parse_hex_u64(value: &str) -> Result<u64, RpcError> {
    let digits = canonical_hex_digits(value)?;
    u64::from_str_radix(digits, 16).map_err(|_| malformed())
}

fn parse_hex_u128(value: &str) -> Result<u128, RpcError> {
    let digits = canonical_hex_digits(value)?;
    u128::from_str_radix(digits, 16).map_err(|_| malformed())
}

fn canonical_hex_digits(value: &str) -> Result<&str, RpcError> {
    let digits = value.strip_prefix("0x").ok_or_else(malformed)?;
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(malformed());
    }
    Ok(digits)
}

fn parse_fixed_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != 2 + N * 2 || !value.starts_with("0x") {
        return None;
    }
    let decoded = hex::decode(&value[2..]).ok()?;
    decoded.try_into().ok()
}

fn parse_hex_bytes(value: &str) -> Option<Vec<u8>> {
    let digits = value.strip_prefix("0x")?;
    if digits.len() % 2 != 0 {
        return None;
    }
    hex::decode(digits).ok()
}

fn classify_reqwest_error(error: reqwest::Error) -> RpcError {
    if error.is_timeout() {
        RpcError::new(RpcErrorKind::Timeout)
    } else {
        RpcError::new(RpcErrorKind::Transport)
    }
}

fn malformed() -> RpcError {
    RpcError::new(RpcErrorKind::MalformedResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_rpc_requires_exact_https_allowlist_entry() {
        let endpoint = Url::parse("https://rpc.example.invalid/path").expect("url");
        assert!(HttpExecutionRpc::new_production(endpoint.clone(), &[]).is_err());
        assert!(HttpExecutionRpc::new_production(endpoint.clone(), &[endpoint]).is_ok());
    }

    #[test]
    fn isolated_fork_transport_is_loopback_and_explicit() {
        let loopback = Url::parse("http://127.0.0.1:8545").expect("url");
        assert!(HttpExecutionRpc::new_isolated_fork(loopback.clone(), "wrong").is_err());
        assert!(HttpExecutionRpc::new_isolated_fork(loopback, ISOLATED_FORK_MARKER).is_ok());
        let remote = Url::parse("http://example.invalid:8545").expect("url");
        assert!(HttpExecutionRpc::new_isolated_fork(remote, ISOLATED_FORK_MARKER).is_err());
    }

    #[test]
    fn canonical_quantities_reject_padding_and_uppercase() {
        assert_eq!(parse_hex_u64("0x0").expect("zero"), 0);
        assert_eq!(parse_hex_u64("0x2a").expect("value"), 42);
        assert!(parse_hex_u64("0x00").is_err());
        assert!(parse_hex_u64("0x2A").is_err());
        assert!(parse_hex_u64("2a").is_err());
    }
}
