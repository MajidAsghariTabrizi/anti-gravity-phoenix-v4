use crate::abi::RpcLog;
use crate::model::{CanonicalAddress, ExecutionRequest, TransactionHash, ValidatedLeg};
use crate::{ARBITRUM_AAVE_V3_POOL_ADDRESS, ARBITRUM_ATLAS_V1_6_4_ADDRESS, ARBITRUM_WETH_ADDRESS};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{fs, path::Path};
use thiserror::Error;
use url::Url;

const MAX_RPC_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const RPC_TIMEOUT_SECONDS: u64 = 10;
const ISOLATED_FORK_MARKER: &str = "CONFIRMED_LOCAL_ANVIL";
const ARBITRUM_NODE_INTERFACE: &str = "0x00000000000000000000000000000000000000c8";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionReceipt {
    pub transaction_hash: TransactionHash,
    pub status: u64,
    pub block_number: u64,
    pub gas_used: u64,
    pub l1_gas_used: u64,
    pub l1_fee: u128,
    pub effective_gas_price: u128,
    pub logs: Vec<RpcLog>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionQuote {
    pub block_number: u64,
    pub block_hash: String,
    pub gas_limit: u64,
    pub l1_gas_units: u64,
    pub base_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub estimated_l1_cost: u128,
    pub endpoint_identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedRpcLog {
    pub transaction_hash: TransactionHash,
    pub block_number: u64,
    pub log_index: u64,
    pub log: RpcLog,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutorConfigurationSnapshot {
    pub runtime_code_hash: String,
    pub owner: Option<CanonicalAddress>,
    pub flash_provider: Option<CanonicalAddress>,
    pub paused: bool,
    pub maximum_input_amount: u128,
    pub searcher_authorized: bool,
    pub assets_approved: Vec<bool>,
    pub router_approved: bool,
    pub factories_approved: Vec<bool>,
    pub pools_approved: Vec<bool>,
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
    async fn finalized_block_identity(&self) -> Result<(u64, String), RpcError>;
    async fn execution_contract_ready(
        &self,
        request: &ExecutionRequest,
        wallet: CanonicalAddress,
        expected_code_hash: &str,
    ) -> Result<bool, RpcError>;
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

#[derive(Clone)]
pub struct HttpExecutionRpc {
    client: Client,
    endpoint: Url,
    next_id: Arc<AtomicU64>,
    endpoint_identity: String,
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

    pub fn new_production_authenticated(
        endpoint: Url,
        allowlist: &[Url],
        header_name: &str,
        header_file: &str,
    ) -> Result<Self, RpcError> {
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || !allowlist.iter().any(|allowed| allowed == &endpoint)
        {
            return Err(RpcError::new(RpcErrorKind::Transport));
        }
        let path = Path::new(header_file);
        let metadata =
            fs::symlink_metadata(path).map_err(|_| RpcError::new(RpcErrorKind::Transport))?;
        if !path.is_absolute()
            || metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > 4096
        {
            return Err(RpcError::new(RpcErrorKind::Transport));
        }
        let header_text =
            fs::read_to_string(path).map_err(|_| RpcError::new(RpcErrorKind::Transport))?;
        let header_text = header_text.trim();
        let name = HeaderName::from_bytes(header_name.as_bytes())
            .map_err(|_| RpcError::new(RpcErrorKind::Transport))?;
        let value = HeaderValue::from_str(header_text)
            .map_err(|_| RpcError::new(RpcErrorKind::Transport))?;
        if header_text.is_empty() {
            return Err(RpcError::new(RpcErrorKind::Transport));
        }
        let mut headers = HeaderMap::new();
        headers.insert(name, value);
        Self::new_with_headers(endpoint, headers)
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
        Self::new_with_headers(endpoint, HeaderMap::new())
    }

    fn new_with_headers(endpoint: Url, headers: HeaderMap) -> Result<Self, RpcError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(RPC_TIMEOUT_SECONDS))
            .https_only(endpoint.scheme() == "https")
            .redirect(Policy::none())
            .no_proxy()
            .default_headers(headers)
            .build()
            .map_err(|_| RpcError::new(RpcErrorKind::Transport))?;
        Ok(Self {
            client,
            endpoint_identity: endpoint_identity(&endpoint),
            endpoint,
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub async fn quote_transaction(
        &self,
        from: CanonicalAddress,
        to: CanonicalAddress,
        calldata: &[u8],
    ) -> Result<TransactionQuote, RpcError> {
        if calldata.is_empty() || calldata.len() > 64 * 1024 {
            return Err(malformed());
        }
        let block_number = self
            .call("eth_blockNumber", json!([]))
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u64)?;
        let block_tag = format!("0x{block_number:x}");
        let block = self
            .call("eth_getBlockByNumber", json!([block_tag, false]))
            .await?;
        let block_hash = block
            .get("hash")
            .and_then(Value::as_str)
            .filter(|value| canonical_hash(value))
            .ok_or_else(malformed)?
            .to_string();
        let base_fee_per_gas = block
            .get("baseFeePerGas")
            .and_then(Value::as_str)
            .ok_or_else(malformed)
            .and_then(parse_hex_u128)?;
        let transaction = json!({
            "from": from.to_string(),
            "to": to.to_string(),
            "data": format!("0x{}", hex::encode(calldata)),
            "value": "0x0"
        });
        let estimated_gas = self
            .call("eth_estimateGas", json!([transaction, block_tag]))
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u64)?;
        let max_priority_fee_per_gas = self
            .call("eth_maxPriorityFeePerGas", json!([]))
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u128)?;
        let max_fee_per_gas = base_fee_per_gas
            .checked_mul(2)
            .and_then(|value| value.checked_add(max_priority_fee_per_gas))
            .ok_or_else(malformed)?;
        let components = self
            .call(
                "eth_call",
                gas_estimate_components_params(from, to, calldata, &block_tag),
            )
            .await?;
        let (component_gas, l1_gas_units) =
            decode_gas_estimate_components(components.as_str().ok_or_else(malformed)?)?;
        if estimated_gas == 0 || component_gas == 0 || estimated_gas < l1_gas_units {
            return Err(malformed());
        }
        let gas_limit = estimated_gas
            .checked_mul(120)
            .and_then(|value| value.checked_add(99))
            .map(|value| value / 100)
            .ok_or_else(malformed)?;
        let estimated_l1_cost = u128::from(l1_gas_units)
            .checked_mul(max_fee_per_gas)
            .ok_or_else(malformed)?;
        Ok(TransactionQuote {
            block_number,
            block_hash,
            gas_limit,
            l1_gas_units,
            base_fee_per_gas,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            estimated_l1_cost,
            endpoint_identity: self.endpoint_identity.clone(),
        })
    }

    /// PhoenixExecutor-only CREATE quote.  This is deliberately separate
    /// from the normal call quote and accepts no destination or value.
    pub async fn quote_contract_creation(
        &self,
        from: CanonicalAddress,
        init_code: &[u8],
    ) -> Result<TransactionQuote, RpcError> {
        if init_code.is_empty() || init_code.len() > 512 * 1024 {
            return Err(malformed());
        }
        let block_number = self.latest_block_number().await?;
        let block_tag = format!("0x{block_number:x}");
        let block = self
            .call("eth_getBlockByNumber", json!([block_tag, false]))
            .await?;
        let block_hash = block
            .get("hash")
            .and_then(Value::as_str)
            .filter(|value| canonical_hash(value))
            .ok_or_else(malformed)?
            .to_string();
        let base_fee_per_gas = block
            .get("baseFeePerGas")
            .and_then(Value::as_str)
            .ok_or_else(malformed)
            .and_then(parse_hex_u128)?;
        let estimate = self
            .call(
                "eth_estimateGas",
                json!([{
                    "from": from.to_string(),
                    "data": format!("0x{}", hex::encode(init_code)),
                    "value": "0x0"
                }, block_tag]),
            )
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u64)?;
        let max_priority_fee_per_gas = self
            .call("eth_maxPriorityFeePerGas", json!([]))
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u128)?;
        let max_fee_per_gas = base_fee_per_gas
            .checked_mul(2)
            .and_then(|value| value.checked_add(max_priority_fee_per_gas))
            .ok_or_else(malformed)?;
        let gas_limit = estimate
            .checked_mul(120)
            .and_then(|value| value.checked_add(99))
            .map(|value| value / 100)
            .ok_or_else(malformed)?;
        Ok(TransactionQuote {
            block_number,
            block_hash,
            gas_limit,
            l1_gas_units: 0,
            base_fee_per_gas,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            estimated_l1_cost: 0,
            endpoint_identity: self.endpoint_identity.clone(),
        })
    }

    pub async fn runtime_code_hash(&self, executor: CanonicalAddress) -> Result<String, RpcError> {
        let code = self
            .call("eth_getCode", json!([executor.to_string(), "latest"]))
            .await?
            .as_str()
            .and_then(parse_hex_bytes)
            .ok_or_else(malformed)?;
        if code.is_empty() {
            return Err(malformed());
        }
        Ok(hex::encode(Sha256::digest(code)))
    }

    pub async fn wallet_balance(&self, wallet: CanonicalAddress) -> Result<u128, RpcError> {
        self.call("eth_getBalance", json!([wallet.to_string(), "latest"]))
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u128)
    }

    pub async fn latest_block_number(&self) -> Result<u64, RpcError> {
        self.call("eth_blockNumber", json!([]))
            .await?
            .as_str()
            .ok_or_else(malformed)
            .and_then(parse_hex_u64)
    }

    pub async fn contract_uint_at(
        &self,
        contract: CanonicalAddress,
        name: &str,
        account: CanonicalAddress,
        block_number: u64,
    ) -> Result<u128, RpcError> {
        let mut data = ethabi::short_signature(name, &[ethabi::ParamType::Address]).to_vec();
        data.extend(ethabi::encode(&[address_token(account)]));
        let value = self
            .call(
                "eth_call",
                json!([{
                    "to": contract.to_string(),
                    "data": format!("0x{}", hex::encode(data))
                }, format!("0x{block_number:x}")]),
            )
            .await?;
        let bytes = value
            .as_str()
            .and_then(parse_hex_bytes)
            .ok_or_else(malformed)?;
        ethabi::decode(&[ethabi::ParamType::Uint(256)], &bytes)
            .map_err(|_| malformed())?
            .into_iter()
            .next()
            .and_then(ethabi::Token::into_uint)
            .and_then(|value| u128::try_from(value).ok())
            .ok_or_else(malformed)
    }

    pub async fn exact_logs(
        &self,
        address: CanonicalAddress,
        topics: &[[u8; 32]],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<IndexedRpcLog>, RpcError> {
        if topics.is_empty()
            || topics.len() > 4
            || from_block > to_block
            || to_block - from_block > 512
        {
            return Err(malformed());
        }
        let value = self
            .call(
                "eth_getLogs",
                json!([{
                    "address": address.to_string(),
                    "fromBlock": format!("0x{from_block:x}"),
                    "toBlock": format!("0x{to_block:x}"),
                    "topics": topics.iter().map(|topic| format!("0x{}", hex::encode(topic))).collect::<Vec<_>>()
                }]),
            )
            .await?;
        let logs = value.as_array().ok_or_else(malformed)?;
        if logs.len() > 128 {
            return Err(malformed());
        }
        logs.iter().map(parse_indexed_log).collect()
    }

    pub async fn transaction_input(
        &self,
        tx_hash: TransactionHash,
        expected_to: CanonicalAddress,
    ) -> Result<Option<Vec<u8>>, RpcError> {
        let value = self
            .call("eth_getTransactionByHash", json!([tx_hash.to_string()]))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        let object = value.as_object().ok_or_else(malformed)?;
        let returned_hash = parse_hash_field(object, "hash")?;
        let to = object
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(malformed)
            .and_then(|value| CanonicalAddress::parse(value).map_err(|_| malformed()))?;
        let input = object
            .get("input")
            .and_then(Value::as_str)
            .and_then(parse_hex_bytes)
            .ok_or_else(malformed)?;
        if returned_hash != tx_hash || to != expected_to || input.len() > 512 * 1024 {
            return Err(malformed());
        }
        Ok(Some(input))
    }

    pub async fn executor_owner_and_flash_provider(
        &self,
        executor: CanonicalAddress,
    ) -> Result<(CanonicalAddress, CanonicalAddress), RpcError> {
        let owner = call_address(self, executor, "owner", &[])
            .await?
            .ok_or_else(malformed)?;
        let flash_provider = call_address(self, executor, "flashProvider", &[])
            .await?
            .ok_or_else(malformed)?;
        Ok((owner, flash_provider))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn atlas_execution_contract_ready(
        &self,
        executor: CanonicalAddress,
        expected_owner: CanonicalAddress,
        expected_code_hash: &str,
        maximum_input_amount: u128,
        debt_asset: CanonicalAddress,
        collateral_asset: CanonicalAddress,
        legs: &[ValidatedLeg],
    ) -> Result<bool, RpcError> {
        let code = self
            .call("eth_getCode", json!([executor.to_string(), "latest"]))
            .await?
            .as_str()
            .and_then(parse_hex_bytes)
            .ok_or_else(malformed)?;
        if code.is_empty() || hex::encode(Sha256::digest(&code)) != expected_code_hash {
            return Ok(false);
        }
        let expected_flash_provider =
            CanonicalAddress::parse(ARBITRUM_AAVE_V3_POOL_ADDRESS).map_err(|_| malformed())?;
        let expected_atlas =
            CanonicalAddress::parse(ARBITRUM_ATLAS_V1_6_4_ADDRESS).map_err(|_| malformed())?;
        let expected_weth =
            CanonicalAddress::parse(ARBITRUM_WETH_ADDRESS).map_err(|_| malformed())?;
        if call_address(self, executor, "owner", &[]).await? != Some(expected_owner)
            || call_address(self, executor, "flashProvider", &[]).await?
                != Some(expected_flash_provider)
            || call_address(self, executor, "atlas", &[]).await? != Some(expected_atlas)
            || call_address(self, executor, "weth", &[]).await? != Some(expected_weth)
            || call_bool(self, executor, "paused", &[]).await?
            || call_uint(self, executor, "maximumInputAmount", &[]).await? < maximum_input_amount
            || !call_bool(
                self,
                executor,
                "approvedAssets",
                &[address_token(debt_asset)],
            )
            .await?
            || !call_bool(
                self,
                executor,
                "approvedAssets",
                &[address_token(collateral_asset)],
            )
            .await?
        {
            return Ok(false);
        }
        for leg in legs {
            let Some(factory) = leg.factory else {
                return Ok(false);
            };
            if !call_bool(
                self,
                executor,
                "approvedFactories",
                &[address_token(factory)],
            )
            .await?
                || !call_pool(self, executor, leg).await?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn executor_configuration_snapshot(
        &self,
        executor: CanonicalAddress,
        searcher: CanonicalAddress,
        assets: &[CanonicalAddress],
        router: CanonicalAddress,
        legs: &[ValidatedLeg],
    ) -> Result<ExecutorConfigurationSnapshot, RpcError> {
        let code = self
            .call("eth_getCode", json!([executor.to_string(), "latest"]))
            .await?
            .as_str()
            .and_then(parse_hex_bytes)
            .ok_or_else(malformed)?;
        if code.is_empty() {
            return Err(malformed());
        }
        let mut factories_approved = Vec::with_capacity(legs.len());
        let mut pools_approved = Vec::with_capacity(legs.len());
        for leg in legs {
            let Some(factory) = leg.factory else {
                return Err(malformed());
            };
            factories_approved.push(
                call_bool(
                    self,
                    executor,
                    "approvedFactories",
                    &[address_token(factory)],
                )
                .await?,
            );
            pools_approved.push(call_pool(self, executor, leg).await?);
        }
        let mut assets_approved = Vec::with_capacity(assets.len());
        for asset in assets {
            assets_approved
                .push(call_bool(self, executor, "approvedAssets", &[address_token(*asset)]).await?);
        }
        Ok(ExecutorConfigurationSnapshot {
            runtime_code_hash: hex::encode(Sha256::digest(&code)),
            owner: call_address(self, executor, "owner", &[]).await?,
            flash_provider: call_address(self, executor, "flashProvider", &[]).await?,
            paused: call_bool(self, executor, "paused", &[]).await?,
            maximum_input_amount: call_uint(self, executor, "maximumInputAmount", &[]).await?,
            searcher_authorized: call_bool(
                self,
                executor,
                "authorizedSearchers",
                &[address_token(searcher)],
            )
            .await?,
            assets_approved,
            router_approved: call_bool(self, executor, "approvedRouters", &[address_token(router)])
                .await?,
            factories_approved,
            pools_approved,
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

    async fn finalized_block_identity(&self) -> Result<(u64, String), RpcError> {
        let value = self
            .call("eth_getBlockByNumber", json!(["finalized", false]))
            .await?;
        parse_block_identity(&value)
    }

    async fn execution_contract_ready(
        &self,
        request: &ExecutionRequest,
        wallet: CanonicalAddress,
        expected_code_hash: &str,
    ) -> Result<bool, RpcError> {
        let code = self
            .call(
                "eth_getCode",
                json!([request.executor_address.to_string(), "latest"]),
            )
            .await?
            .as_str()
            .and_then(parse_hex_bytes)
            .ok_or_else(malformed)?;
        if code.is_empty() || hex::encode(Sha256::digest(&code)) != expected_code_hash {
            return Ok(false);
        }
        let flash_provider =
            call_address(self, request.executor_address, "flashProvider", &[]).await?;
        let flash_asset_approved = call_bool(
            self,
            request.executor_address,
            "approvedAssets",
            &[address_token(request.flash_asset)],
        )
        .await?;
        let is_aave_liquidation = request.aave_liquidation.is_some();
        let router_approved = if is_aave_liquidation {
            false
        } else {
            call_bool(
                self,
                request.executor_address,
                "approvedRouters",
                &[address_token(request.origin_router)],
            )
            .await?
        };
        if call_address(self, request.executor_address, "owner", &[])
            .await?
            .is_none()
            || flash_provider.is_none()
            || call_bool(self, request.executor_address, "paused", &[]).await?
            || call_uint(self, request.executor_address, "maximumInputAmount", &[]).await?
                < request.maximum_input_amount
            || !call_bool(
                self,
                request.executor_address,
                "authorizedSearchers",
                &[address_token(wallet)],
            )
            .await?
            || !flash_asset_approved
            || !execution_router_mapping_ready(is_aave_liquidation, router_approved)
        {
            return Ok(false);
        }
        if let Some(route) = request.aave_liquidation.as_ref() {
            let expected_pool =
                CanonicalAddress::parse(ARBITRUM_AAVE_V3_POOL_ADDRESS).map_err(|_| malformed())?;
            let collateral_approved = call_bool(
                self,
                request.executor_address,
                "approvedAssets",
                &[address_token(route.collateral_asset)],
            )
            .await?;
            if !direct_aave_contract_state_ready(
                flash_provider,
                request.origin_router,
                expected_pool,
                flash_asset_approved,
                collateral_approved,
            ) {
                return Ok(false);
            }
        }
        for leg in &request.legs {
            let Some(factory) = leg.factory else {
                return Ok(false);
            };
            if !call_bool(
                self,
                request.executor_address,
                "approvedFactories",
                &[address_token(factory)],
            )
            .await?
                || !call_pool(self, request.executor_address, leg).await?
            {
                return Ok(false);
            }
        }
        Ok(true)
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
        let l1_gas_used = optional_u64_field(object, "gasUsedForL1")?.unwrap_or(0);
        let effective_gas_price = parse_u128_field(object, "effectiveGasPrice")?;
        let l1_fee = optional_u128_field(object, "l1Fee")?
            .unwrap_or_else(|| u128::from(l1_gas_used).saturating_mul(effective_gas_price));
        if l1_fee == u128::MAX {
            return Err(malformed());
        }
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
            l1_gas_used,
            l1_fee,
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

fn parse_block_identity(value: &Value) -> Result<(u64, String), RpcError> {
    let object = value.as_object().ok_or_else(malformed)?;
    let number = parse_u64_field(object, "number")?;
    let hash = object
        .get("hash")
        .and_then(Value::as_str)
        .filter(|value| canonical_hash(value))
        .ok_or_else(malformed)?
        .to_string();
    Ok((number, hash))
}

fn parse_indexed_log(value: &Value) -> Result<IndexedRpcLog, RpcError> {
    let object = value.as_object().ok_or_else(malformed)?;
    if object.get("removed").and_then(Value::as_bool) != Some(false) {
        return Err(malformed());
    }
    Ok(IndexedRpcLog {
        transaction_hash: parse_hash_field(object, "transactionHash")?,
        block_number: parse_u64_field(object, "blockNumber")?,
        log_index: parse_u64_field(object, "logIndex")?,
        log: parse_log(value)?,
    })
}

fn address_token(address: CanonicalAddress) -> ethabi::Token {
    ethabi::Token::Address(primitive_types::H160::from_slice(address.as_bytes()))
}

fn direct_aave_contract_state_ready(
    flash_provider: Option<CanonicalAddress>,
    origin_router: CanonicalAddress,
    expected_pool: CanonicalAddress,
    debt_asset_approved: bool,
    collateral_asset_approved: bool,
) -> bool {
    flash_provider == Some(expected_pool)
        && origin_router == expected_pool
        && debt_asset_approved
        && collateral_asset_approved
}

fn execution_router_mapping_ready(is_aave_liquidation: bool, router_approved: bool) -> bool {
    is_aave_liquidation || router_approved
}

async fn contract_call(
    rpc: &HttpExecutionRpc,
    contract: CanonicalAddress,
    name: &str,
    arguments: &[ethabi::Token],
    outputs: &[ethabi::ParamType],
) -> Result<Vec<ethabi::Token>, RpcError> {
    let input_types = arguments
        .iter()
        .map(|argument| match argument {
            ethabi::Token::Address(_) => Ok(ethabi::ParamType::Address),
            _ => Err(malformed()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut data = ethabi::short_signature(name, &input_types).to_vec();
    data.extend(ethabi::encode(arguments));
    let result = rpc
        .call(
            "eth_call",
            json!([{
                "to": contract.to_string(),
                "data": format!("0x{}", hex::encode(data))
            }, "latest"]),
        )
        .await?;
    let bytes = result
        .as_str()
        .and_then(parse_hex_bytes)
        .ok_or_else(malformed)?;
    ethabi::decode(outputs, &bytes).map_err(|_| malformed())
}

async fn call_bool(
    rpc: &HttpExecutionRpc,
    contract: CanonicalAddress,
    name: &str,
    arguments: &[ethabi::Token],
) -> Result<bool, RpcError> {
    contract_call(rpc, contract, name, arguments, &[ethabi::ParamType::Bool])
        .await?
        .into_iter()
        .next()
        .and_then(ethabi::Token::into_bool)
        .ok_or_else(malformed)
}

async fn call_uint(
    rpc: &HttpExecutionRpc,
    contract: CanonicalAddress,
    name: &str,
    arguments: &[ethabi::Token],
) -> Result<u128, RpcError> {
    contract_call(
        rpc,
        contract,
        name,
        arguments,
        &[ethabi::ParamType::Uint(256)],
    )
    .await?
    .into_iter()
    .next()
    .and_then(ethabi::Token::into_uint)
    .and_then(|value| u128::try_from(value).ok())
    .ok_or_else(malformed)
}

async fn call_address(
    rpc: &HttpExecutionRpc,
    contract: CanonicalAddress,
    name: &str,
    arguments: &[ethabi::Token],
) -> Result<Option<CanonicalAddress>, RpcError> {
    let address = contract_call(
        rpc,
        contract,
        name,
        arguments,
        &[ethabi::ParamType::Address],
    )
    .await?
    .into_iter()
    .next()
    .and_then(ethabi::Token::into_address)
    .ok_or_else(malformed)?;
    if address == primitive_types::H160::zero() {
        return Ok(None);
    }
    CanonicalAddress::parse(&format!("0x{}", hex::encode(address.as_bytes())))
        .map(Some)
        .map_err(|_| malformed())
}

async fn call_pool(
    rpc: &HttpExecutionRpc,
    contract: CanonicalAddress,
    leg: &ValidatedLeg,
) -> Result<bool, RpcError> {
    let Some(factory) = leg.factory else {
        return Ok(false);
    };
    let decoded = contract_call(
        rpc,
        contract,
        "approvedPools",
        &[address_token(leg.pool)],
        &[
            ethabi::ParamType::Address,
            ethabi::ParamType::Address,
            ethabi::ParamType::Address,
            ethabi::ParamType::Uint(24),
            ethabi::ParamType::Bool,
        ],
    )
    .await?;
    let returned_factory = decoded
        .first()
        .and_then(|value| value.clone().into_address())
        .ok_or_else(malformed)?;
    let returned_token0 = decoded
        .get(1)
        .and_then(|value| value.clone().into_address())
        .ok_or_else(malformed)?;
    let returned_token1 = decoded
        .get(2)
        .and_then(|value| value.clone().into_address())
        .ok_or_else(malformed)?;
    let returned_fee = decoded
        .get(3)
        .and_then(|value| value.clone().into_uint())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(malformed)?;
    let approved = decoded
        .get(4)
        .and_then(|value| value.clone().into_bool())
        .ok_or_else(malformed)?;
    let (token0, token1) = if leg.token_in.as_bytes() < leg.token_out.as_bytes() {
        (leg.token_in, leg.token_out)
    } else {
        (leg.token_out, leg.token_in)
    };
    Ok(approved
        && returned_factory == primitive_types::H160::from_slice(factory.as_bytes())
        && returned_token0 == primitive_types::H160::from_slice(token0.as_bytes())
        && returned_token1 == primitive_types::H160::from_slice(token1.as_bytes())
        && returned_fee == leg.fee)
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

fn optional_u64_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<u64>, RpcError> {
    object
        .get(name)
        .map(|value| value.as_str().ok_or_else(malformed).and_then(parse_hex_u64))
        .transpose()
}

fn optional_u128_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<Option<u128>, RpcError> {
    object
        .get(name)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(malformed)
                .and_then(parse_hex_u128)
        })
        .transpose()
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

fn canonical_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn endpoint_identity(endpoint: &Url) -> String {
    let authority = format!(
        "{}://{}:{}",
        endpoint.scheme(),
        endpoint.host_str().unwrap_or("invalid"),
        endpoint.port_or_known_default().unwrap_or(0)
    );
    format!("rpc-{}", &hex::encode(Sha256::digest(authority))[..16])
}

fn encode_gas_estimate_components(to: CanonicalAddress, calldata: &[u8]) -> String {
    use ethabi::{ParamType, Token};
    use primitive_types::H160;

    let mut data = ethabi::short_signature(
        "gasEstimateComponents",
        &[ParamType::Address, ParamType::Bool, ParamType::Bytes],
    )
    .to_vec();
    data.extend(ethabi::encode(&[
        Token::Address(H160::from_slice(to.as_bytes())),
        Token::Bool(false),
        Token::Bytes(calldata.to_vec()),
    ]));
    format!("0x{}", hex::encode(data))
}

fn gas_estimate_components_params(
    from: CanonicalAddress,
    to: CanonicalAddress,
    calldata: &[u8],
    block_tag: &str,
) -> Value {
    json!([
        {
            "from": from.to_string(),
            "to": ARBITRUM_NODE_INTERFACE,
            "data": encode_gas_estimate_components(to, calldata)
        },
        block_tag
    ])
}

fn decode_gas_estimate_components(value: &str) -> Result<(u64, u64), RpcError> {
    use ethabi::ParamType;

    let bytes = parse_hex_bytes(value).ok_or_else(malformed)?;
    let decoded = ethabi::decode(
        &[
            ParamType::Uint(64),
            ParamType::Uint(64),
            ParamType::Uint(256),
            ParamType::Uint(256),
        ],
        &bytes,
    )
    .map_err(|_| malformed())?;
    let gas = decoded
        .first()
        .and_then(|value| value.clone().into_uint())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(malformed)?;
    let l1 = decoded
        .get(1)
        .and_then(|value| value.clone().into_uint())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(malformed)?;
    Ok((gas, l1))
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

    #[test]
    fn finalized_block_identity_requires_canonical_number_and_hash() {
        let hash = format!("0x{}", "a".repeat(64));
        assert_eq!(
            parse_block_identity(&json!({"number": "0x2a", "hash": hash})).expect("block"),
            (42, format!("0x{}", "a".repeat(64)))
        );
        assert!(parse_block_identity(&json!({
            "number": "0x02a",
            "hash": format!("0x{}", "a".repeat(64))
        }))
        .is_err());
        assert!(parse_block_identity(&json!({
            "number": "0x2a",
            "hash": format!("0x{}", "A".repeat(64))
        }))
        .is_err());
    }

    #[test]
    fn node_interface_quote_preserves_sender_block_and_calldata() {
        let from =
            CanonicalAddress::parse("0x1111111111111111111111111111111111111111").expect("from");
        let to = CanonicalAddress::parse("0x2222222222222222222222222222222222222222").expect("to");
        let calldata = [0xde, 0xad, 0xbe, 0xef];
        let block_tag = "0x1234";
        let encoded = encode_gas_estimate_components(to, &calldata);

        let params = gas_estimate_components_params(from, to, &calldata, block_tag);

        assert_eq!(params[0]["from"], from.to_string());
        assert_eq!(params[0]["to"], ARBITRUM_NODE_INTERFACE);
        assert_eq!(params[0]["data"], encoded);
        assert_eq!(params[1], block_tag);
    }

    #[test]
    fn direct_aave_readiness_rejects_provider_router_or_asset_drift() {
        let pool = CanonicalAddress::parse(ARBITRUM_AAVE_V3_POOL_ADDRESS).expect("pool");
        let drift =
            CanonicalAddress::parse("0x1111111111111111111111111111111111111111").expect("drift");

        assert!(direct_aave_contract_state_ready(
            Some(pool),
            pool,
            pool,
            true,
            true,
        ));
        assert!(!direct_aave_contract_state_ready(
            Some(drift),
            pool,
            pool,
            true,
            true,
        ));
        assert!(!direct_aave_contract_state_ready(
            Some(pool),
            drift,
            pool,
            true,
            true,
        ));
        assert!(!direct_aave_contract_state_ready(
            Some(pool),
            pool,
            pool,
            false,
            true,
        ));
        assert!(!direct_aave_contract_state_ready(
            Some(pool),
            pool,
            pool,
            true,
            false,
        ));
        assert!(execution_router_mapping_ready(true, false));
        assert!(!execution_router_mapping_ready(false, false));
        assert!(execution_router_mapping_ready(false, true));
    }
}
