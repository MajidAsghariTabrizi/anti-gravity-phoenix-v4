use async_trait::async_trait;
use reqwest::{redirect::Policy, Client, Url};
use rpc_gateway::shadow_state::PoolStateRequest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

const MAX_RPC_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CODE_BYTES: usize = 512 * 1024;
const MAX_STATE_BYTES: usize = 4096;

pub const ALLOWED_RPC_METHODS: &[&str] = &[
    "anvil_metadata",
    "eth_getBlockByNumber",
    "eth_getCode",
    "eth_call",
    "eth_estimateGas",
    "debug_traceCall",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkMetadata {
    pub chain_id: u64,
    pub fork_block_number: u64,
    pub fork_block_hash: String,
    pub instance_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockObservation {
    pub number: u64,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolObservation {
    pub token0: String,
    pub token1: String,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub fee: u32,
    pub tick_spacing: i32,
    pub slot0: String,
    pub liquidity: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SimulationCall {
    pub from: String,
    pub to: String,
    pub data: String,
    pub value: String,
    pub gas: u64,
}

impl SimulationCall {
    fn rpc_value(&self) -> Result<Value, RpcError> {
        let value = self
            .value
            .parse::<u128>()
            .map_err(|_| RpcError::Integrity)?;
        Ok(json!({
            "from": self.from,
            "to": self.to,
            "data": self.data,
            "value": quantity_u128(value),
            "gas": quantity_u64(self.gas),
        }))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TraceLog {
    pub address: String,
    pub topics: Vec<String>,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceObservation {
    pub gas_used: u64,
    pub output: String,
    pub logs: Vec<TraceLog>,
    pub revert_reason: Option<String>,
    pub trace_hash: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RpcError {
    #[error("fork RPC endpoint configuration is invalid")]
    Configuration,
    #[error("fork RPC transport is unavailable")]
    Transport,
    #[error("fork RPC response failed integrity validation")]
    Integrity,
    #[error("fork simulation reverted: {reason}")]
    Reverted {
        reason: String,
        data: Option<String>,
    },
}

#[async_trait]
pub trait ForkRpc: Send + Sync {
    async fn metadata(&self) -> Result<ForkMetadata, RpcError>;
    async fn latest_block(&self) -> Result<BlockObservation, RpcError>;
    async fn code(&self, address: &str) -> Result<String, RpcError>;
    async fn observe_pool(&self, pool: &PoolStateRequest) -> Result<PoolObservation, RpcError>;
    async fn estimate_gas(&self, call: &SimulationCall) -> Result<u64, RpcError>;
    async fn call(&self, call: &SimulationCall) -> Result<String, RpcError>;
    async fn trace_call(&self, call: &SimulationCall) -> Result<TraceObservation, RpcError>;
}

#[derive(Clone)]
pub struct HttpForkRpc {
    endpoint: Url,
    client: Client,
}

impl HttpForkRpc {
    pub fn new(endpoint: &str, timeout: Duration) -> Result<Self, RpcError> {
        let endpoint = Url::parse(endpoint).map_err(|_| RpcError::Configuration)?;
        if endpoint.scheme() != "http"
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.path(), "" | "/")
            || !loopback_host(&endpoint)
            || timeout.is_zero()
            || timeout > Duration::from_secs(30)
        {
            return Err(RpcError::Configuration);
        }
        let client = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|_| RpcError::Configuration)?;
        Ok(Self { endpoint, client })
    }

    async fn request(&self, method: RpcMethod, params: Value) -> Result<Value, RpcError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method.as_str(),
                "params": params,
            }))
            .send()
            .await
            .map_err(|_| RpcError::Transport)?;
        if !response.status().is_success() {
            return Err(RpcError::Transport);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RPC_RESPONSE_BYTES as u64)
        {
            return Err(RpcError::Integrity);
        }
        let bytes = response.bytes().await.map_err(|_| RpcError::Transport)?;
        if bytes.len() > MAX_RPC_RESPONSE_BYTES {
            return Err(RpcError::Integrity);
        }
        let envelope: RpcEnvelope =
            serde_json::from_slice(&bytes).map_err(|_| RpcError::Integrity)?;
        if envelope.jsonrpc != "2.0" || envelope.id != json!(1) {
            return Err(RpcError::Integrity);
        }
        if let Some(error) = envelope.error {
            return Err(RpcError::Reverted {
                reason: bounded_reason(&error.message),
                data: error.data.as_ref().and_then(extract_revert_data),
            });
        }
        envelope.result.ok_or(RpcError::Integrity)
    }

    async fn pool_call(&self, address: &str, selector: &str) -> Result<String, RpcError> {
        self.request(
            RpcMethod::EthCall,
            json!([{"to": address, "data": selector}, "latest"]),
        )
        .await?
        .as_str()
        .map(str::to_ascii_lowercase)
        .ok_or(RpcError::Integrity)
    }
}

#[async_trait]
impl ForkRpc for HttpForkRpc {
    async fn metadata(&self) -> Result<ForkMetadata, RpcError> {
        let value = self.request(RpcMethod::AnvilMetadata, json!([])).await?;
        parse_metadata(&value)
    }

    async fn latest_block(&self) -> Result<BlockObservation, RpcError> {
        let value = self
            .request(RpcMethod::EthGetBlockByNumber, json!(["latest", false]))
            .await?;
        Ok(BlockObservation {
            number: parse_quantity_value(value.get("number").ok_or(RpcError::Integrity)?)?,
            hash: canonical_hash_value(value.get("hash").ok_or(RpcError::Integrity)?)?,
        })
    }

    async fn code(&self, address: &str) -> Result<String, RpcError> {
        let value = self
            .request(RpcMethod::EthGetCode, json!([address, "latest"]))
            .await?;
        let code = value
            .as_str()
            .map(str::to_ascii_lowercase)
            .ok_or(RpcError::Integrity)?;
        validate_data(&code, 0, None, MAX_CODE_BYTES)?;
        Ok(code)
    }

    async fn observe_pool(&self, pool: &PoolStateRequest) -> Result<PoolObservation, RpcError> {
        let token0 = decode_address_word(&self.pool_call(&pool.address, "0x0dfe1681").await?)?;
        let token1 = decode_address_word(&self.pool_call(&pool.address, "0xd21220a7").await?)?;
        let fee = decode_u32_word(&self.pool_call(&pool.address, "0xddca3f43").await?)?;
        let tick_spacing = decode_i24_word(&self.pool_call(&pool.address, "0xd0c93a7c").await?)?;
        let token0_decimals = decode_u8_word(&self.pool_call(&token0, "0x313ce567").await?)?;
        let token1_decimals = decode_u8_word(&self.pool_call(&token1, "0x313ce567").await?)?;
        let slot0 = self.pool_call(&pool.address, "0x3850c7bd").await?;
        validate_data(&slot0, 64, None, MAX_STATE_BYTES)?;
        let liquidity = self.pool_call(&pool.address, "0x1a686502").await?;
        validate_data(&liquidity, 32, Some(32), MAX_STATE_BYTES)?;
        Ok(PoolObservation {
            token0,
            token1,
            token0_decimals,
            token1_decimals,
            fee,
            tick_spacing,
            slot0,
            liquidity,
        })
    }

    async fn estimate_gas(&self, call: &SimulationCall) -> Result<u64, RpcError> {
        let value = self
            .request(RpcMethod::EthEstimateGas, json!([call.rpc_value()?]))
            .await?;
        parse_quantity_value(&value)
    }

    async fn call(&self, call: &SimulationCall) -> Result<String, RpcError> {
        let output = self
            .request(RpcMethod::EthCall, json!([call.rpc_value()?, "latest"]))
            .await?
            .as_str()
            .map(str::to_ascii_lowercase)
            .ok_or(RpcError::Integrity)?;
        validate_data(&output, 0, None, MAX_STATE_BYTES)?;
        Ok(output)
    }

    async fn trace_call(&self, call: &SimulationCall) -> Result<TraceObservation, RpcError> {
        let value = self
            .request(
                RpcMethod::DebugTraceCall,
                json!([
                    call.rpc_value()?,
                    "latest",
                    {"tracer": "callTracer", "tracerConfig": {"withLog": true}}
                ]),
            )
            .await?;
        parse_trace(value)
    }
}

#[derive(Clone, Copy)]
enum RpcMethod {
    AnvilMetadata,
    EthGetBlockByNumber,
    EthGetCode,
    EthCall,
    EthEstimateGas,
    DebugTraceCall,
}

impl RpcMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::AnvilMetadata => "anvil_metadata",
            Self::EthGetBlockByNumber => "eth_getBlockByNumber",
            Self::EthGetCode => "eth_getCode",
            Self::EthCall => "eth_call",
            Self::EthEstimateGas => "eth_estimateGas",
            Self::DebugTraceCall => "debug_traceCall",
        }
    }
}

#[derive(Deserialize)]
struct RpcEnvelope {
    jsonrpc: String,
    id: Value,
    result: Option<Value>,
    error: Option<RpcErrorEnvelope>,
}

#[derive(Deserialize)]
struct RpcErrorEnvelope {
    message: String,
    data: Option<Value>,
}

fn loopback_host(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback()),
        None => false,
    }
}

fn parse_quantity_value(value: &Value) -> Result<u64, RpcError> {
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    let raw = value.as_str().ok_or(RpcError::Integrity)?;
    let body = raw.strip_prefix("0x").ok_or(RpcError::Integrity)?;
    if body.is_empty()
        || (body.len() > 1 && body.starts_with('0'))
        || !body.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(RpcError::Integrity);
    }
    u64::from_str_radix(body, 16).map_err(|_| RpcError::Integrity)
}

fn canonical_hash_value(value: &Value) -> Result<String, RpcError> {
    let hash = value
        .as_str()
        .map(str::to_ascii_lowercase)
        .ok_or(RpcError::Integrity)?;
    if hash.len() != 66
        || !hash.starts_with("0x")
        || !hash[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RpcError::Integrity);
    }
    Ok(hash)
}

fn parse_metadata(value: &Value) -> Result<ForkMetadata, RpcError> {
    let fork = value.get("forkedNetwork").ok_or(RpcError::Integrity)?;
    let chain_id = parse_quantity_value(
        fork.get("chainId")
            .or_else(|| value.get("chainId"))
            .ok_or(RpcError::Integrity)?,
    )?;
    let fork_block_number =
        parse_quantity_value(fork.get("forkBlockNumber").ok_or(RpcError::Integrity)?)?;
    let fork_block_hash =
        canonical_hash_value(fork.get("forkBlockHash").ok_or(RpcError::Integrity)?)?;
    let instance_id = canonical_hash_value(value.get("instanceId").ok_or(RpcError::Integrity)?)?;
    Ok(ForkMetadata {
        chain_id,
        fork_block_number,
        fork_block_hash,
        instance_hash: instance_id[2..].to_string(),
    })
}

fn validate_data(
    value: &str,
    minimum_bytes: usize,
    exact_bytes: Option<usize>,
    maximum_bytes: usize,
) -> Result<Vec<u8>, RpcError> {
    let body = value.strip_prefix("0x").ok_or(RpcError::Integrity)?;
    if body.len() % 2 != 0 || !body.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RpcError::Integrity);
    }
    let decoded = hex::decode(body).map_err(|_| RpcError::Integrity)?;
    if decoded.len() < minimum_bytes
        || decoded.len() > maximum_bytes
        || exact_bytes.is_some_and(|expected| decoded.len() != expected)
    {
        return Err(RpcError::Integrity);
    }
    Ok(decoded)
}

fn decode_address_word(value: &str) -> Result<String, RpcError> {
    let word = validate_data(value, 32, Some(32), 32)?;
    if word[..12].iter().any(|byte| *byte != 0) {
        return Err(RpcError::Integrity);
    }
    Ok(format!("0x{}", hex::encode(&word[12..])))
}

fn decode_u32_word(value: &str) -> Result<u32, RpcError> {
    let word = validate_data(value, 32, Some(32), 32)?;
    if word[..28].iter().any(|byte| *byte != 0) {
        return Err(RpcError::Integrity);
    }
    Ok(u32::from_be_bytes(
        word[28..].try_into().map_err(|_| RpcError::Integrity)?,
    ))
}

fn decode_u8_word(value: &str) -> Result<u8, RpcError> {
    u8::try_from(decode_u32_word(value)?).map_err(|_| RpcError::Integrity)
}

fn decode_i24_word(value: &str) -> Result<i32, RpcError> {
    let word = validate_data(value, 32, Some(32), 32)?;
    let negative = word[29] & 0x80 != 0;
    let expected_prefix = if negative { 0xff } else { 0x00 };
    if word[..29].iter().any(|byte| *byte != expected_prefix) {
        return Err(RpcError::Integrity);
    }
    let raw = u32::from_be_bytes([0, word[29], word[30], word[31]]);
    Ok(if negative {
        raw as i32 - (1_i32 << 24)
    } else {
        raw as i32
    })
}

fn parse_trace(value: Value) -> Result<TraceObservation, RpcError> {
    let encoded = serde_json::to_vec(&value).map_err(|_| RpcError::Integrity)?;
    let gas_used = parse_quantity_value(value.get("gasUsed").ok_or(RpcError::Integrity)?)?;
    let output = value
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or("0x")
        .to_ascii_lowercase();
    validate_data(&output, 0, None, MAX_STATE_BYTES)?;
    let mut logs = Vec::new();
    collect_logs(&value, &mut logs)?;
    let revert_reason = value
        .get("revertReason")
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(bounded_reason);
    Ok(TraceObservation {
        gas_used,
        output,
        logs,
        revert_reason,
        trace_hash: hex::encode(Sha256::digest(encoded)),
    })
}

fn collect_logs(frame: &Value, output: &mut Vec<TraceLog>) -> Result<(), RpcError> {
    if let Some(logs) = frame.get("logs") {
        let logs: Vec<TraceLog> =
            serde_json::from_value(logs.clone()).map_err(|_| RpcError::Integrity)?;
        output.extend(logs);
    }
    if let Some(calls) = frame.get("calls") {
        for call in calls.as_array().ok_or(RpcError::Integrity)? {
            collect_logs(call, output)?;
        }
    }
    Ok(())
}

fn extract_revert_data(value: &Value) -> Option<String> {
    if let Some(data) = value.as_str() {
        return (data.len() <= 4098 && data.starts_with("0x")).then(|| data.to_ascii_lowercase());
    }
    value
        .get("data")
        .and_then(extract_revert_data)
        .or_else(|| value.get("result").and_then(extract_revert_data))
}

fn bounded_reason(value: &str) -> String {
    let clean = value
        .chars()
        .filter(|character| !character.is_control())
        .take(1024)
        .collect::<String>();
    if clean.is_empty() {
        "execution reverted".to_string()
    } else {
        clean
    }
}

fn quantity_u64(value: u64) -> String {
    format!("0x{value:x}")
}

fn quantity_u128(value: u128) -> String {
    format!("0x{value:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_canonical_anvil_metadata_shape() {
        let block_hash = format!("0x{}", "a".repeat(64));
        let instance_id = format!("0x{}", "b".repeat(64));
        let metadata = parse_metadata(&json!({
            "chainId": 42161,
            "instanceId": instance_id,
            "forkedNetwork": {
                "chainId": 42161,
                "forkBlockNumber": "0x64",
                "forkBlockHash": block_hash,
            }
        }))
        .expect("canonical metadata");
        assert_eq!(metadata.chain_id, 42161);
        assert_eq!(metadata.fork_block_number, 100);
        assert_eq!(metadata.fork_block_hash, format!("0x{}", "a".repeat(64)));
        assert_eq!(metadata.instance_hash, "b".repeat(64));
    }

    #[test]
    fn rejects_unbounded_or_untyped_anvil_instance_identity() {
        let value = json!({
            "chainId": 42161,
            "instanceId": {"unexpected": true},
            "forkedNetwork": {
                "chainId": 42161,
                "forkBlockNumber": 100,
                "forkBlockHash": format!("0x{}", "a".repeat(64)),
            }
        });
        assert_eq!(parse_metadata(&value), Err(RpcError::Integrity));
    }
}
