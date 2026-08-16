use crate::config::transaction_signer_from_file;
use crate::model::{canonical_digest, CanonicalAddress, TransactionHash, ValidatedLeg};
use crate::rpc::{
    ExecutionRpc, ExecutorConfigurationSnapshot, HttpExecutionRpc, RpcError, RpcErrorKind,
    TransactionQuote, TransactionReceipt,
};
use crate::signer::{TransactionDraft, TransactionSigner};
use crate::ARBITRUM_ONE_CHAIN_ID;
use async_trait::async_trait;
use ethabi::{ParamType, Token};
use rpc_gateway::runtime::reviewed_aave_unwind_routes;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fmt;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;
use tokio::time::{sleep, Instant};
use url::Url;

const POLICY: &str = include_str!("../../config/phoenix-route-policy-v1.json");
pub const CONFIGURE_ACK_ENV: &str = "PHOENIX_EXECUTOR_OWNER_BOOTSTRAP_ACK";
pub const CONFIGURE_ACK: &str = "BOOTSTRAP_EXECUTOR_OWNER_42161";
pub const UNPAUSE_ACK_ENV: &str = "PHOENIX_EXECUTOR_OWNER_UNPAUSE_ACK";
pub const UNPAUSE_ACK: &str = "UNPAUSE_CONFIGURED_EXECUTOR_42161";
pub const PAUSE_ACK_ENV: &str = "PHOENIX_EXECUTOR_OWNER_PAUSE_ACK";
pub const PAUSE_ACK: &str = "PAUSE_EXECUTOR_AFTER_FAILED_DEPLOY_42161";
const OWNER_EVIDENCE_SCHEMA: &str = "phoenix.executor-owner-bootstrap-evidence.v2";
const OWNER_PLAN_SCHEMA: &str = "phoenix.executor-owner-plan.v3";

const OWNER_ENVIRONMENT_NAMES: &[&str] = &[
    "CHAIN_ID",
    "WALLET_ADDRESS",
    "EXECUTOR_ADDRESS",
    "PRODUCTION_RPC_URL",
    "LIVE_EXECUTOR_RPC_URL",
    "LIVE_EXECUTOR_RPC_HEADER_NAME",
    "LIVE_EXECUTOR_RPC_HEADER_FILE",
    "LIVE_EXECUTOR_RPC_ALLOWLIST",
    "LIVE_EXECUTOR_EXPECTED_OWNER",
    "LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER",
    "LIVE_EXECUTOR_EXECUTOR_CODE_HASH",
    "LIVE_EXECUTOR_MAX_INPUT_AMOUNT",
    "LIVE_EXECUTOR_MAX_GAS_LIMIT",
    "LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI",
    "LIVE_EXECUTOR_MAX_PRIORITY_FEE_PER_GAS_WEI",
    "LIVE_EXECUTOR_RECEIPT_TIMEOUT_SECONDS",
    "LIVE_EXECUTOR_POLL_INTERVAL_SECONDS",
    "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME",
    "ENGINE_ROUTER_ADDRESSES",
    "SIGNER_PRIVATE_KEY",
    "SIGNER_PRIVATE_KEY_FILE",
    "PHOENIX_RELEASE_SHA",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerMutation {
    Configure,
    Unpause,
    Pause,
}

impl OwnerMutation {
    const fn acknowledgement(self) -> (&'static str, &'static str) {
        match self {
            Self::Configure => (CONFIGURE_ACK_ENV, CONFIGURE_ACK),
            Self::Unpause => (UNPAUSE_ACK_ENV, UNPAUSE_ACK),
            Self::Pause => (PAUSE_ACK_ENV, PAUSE_ACK),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Configure => "configure",
            Self::Unpause => "unpause",
            Self::Pause => "pause",
        }
    }

    const fn success_marker(self) -> &'static str {
        match self {
            Self::Configure => "EXECUTOR_OWNER_CONFIGURE_OK",
            Self::Unpause => "EXECUTOR_OWNER_UNPAUSE_OK",
            Self::Pause => "EXECUTOR_OWNER_PAUSE_OK",
        }
    }
}

#[derive(Clone)]
struct OwnerBootstrapContext {
    chain_id: u64,
    wallet: CanonicalAddress,
    executor: CanonicalAddress,
    expected_owner: CanonicalAddress,
    expected_flash_provider: CanonicalAddress,
    expected_code_hash: String,
    maximum_input: u128,
    maximum_gas_limit: u64,
    maximum_max_fee_per_gas: u128,
    maximum_priority_fee_per_gas: u128,
    receipt_timeout: Duration,
    poll_interval: Duration,
    one_transaction_at_a_time: bool,
    rpc_url: Url,
    rpc_allowlist: Vec<Url>,
    rpc_header_name: String,
    rpc_header_file: String,
    signer_file: Option<String>,
    release_sha: String,
    policy_hash: String,
    settlement_asset: CanonicalAddress,
    assets: Vec<CanonicalAddress>,
    routers: Vec<CanonicalAddress>,
    legs: Vec<ValidatedLeg>,
}

impl fmt::Debug for OwnerBootstrapContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerBootstrapContext")
            .field("chain_id", &self.chain_id)
            .field("wallet", &self.wallet)
            .field("executor", &self.executor)
            .field("expected_owner", &self.expected_owner)
            .field("expected_flash_provider", &self.expected_flash_provider)
            .field("expected_code_hash", &self.expected_code_hash)
            .field("maximum_input", &self.maximum_input)
            .field("rpc", &"<redacted>")
            .field("signer_file", &"<redacted>")
            .field("release_sha", &self.release_sha)
            .field("policy_hash", &self.policy_hash)
            .finish()
    }
}

impl OwnerBootstrapContext {
    fn from_environment() -> Result<Self, OwnerBootstrapError> {
        let values = OWNER_ENVIRONMENT_NAMES
            .iter()
            .filter_map(|name| {
                env::var(name)
                    .ok()
                    .map(|value| ((*name).to_string(), value))
            })
            .collect();
        Self::from_values(&values)
    }

    fn from_values(values: &BTreeMap<String, String>) -> Result<Self, OwnerBootstrapError> {
        if values
            .get("SIGNER_PRIVATE_KEY")
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(OwnerBootstrapError::EnvironmentSignerForbidden);
        }
        let chain_id = required(values, "CHAIN_ID")?
            .parse()
            .map_err(|_| OwnerBootstrapError::InvalidChain)?;
        if chain_id != ARBITRUM_ONE_CHAIN_ID {
            return Err(OwnerBootstrapError::InvalidChain);
        }
        let wallet = parse_address(required(values, "WALLET_ADDRESS")?)?;
        let executor = parse_address(required(values, "EXECUTOR_ADDRESS")?)?;
        let expected_owner = parse_address(required(values, "LIVE_EXECUTOR_EXPECTED_OWNER")?)?;
        let expected_flash_provider =
            parse_address(required(values, "LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER")?)?;
        let expected_code_hash = required(values, "LIVE_EXECUTOR_EXECUTOR_CODE_HASH")?.to_string();
        if !canonical_digest(&expected_code_hash) {
            return Err(OwnerBootstrapError::InvalidCodeHash);
        }
        let rpc_url = parse_production_url(required(values, "PRODUCTION_RPC_URL")?)?;
        if let Some(executor_rpc) = values
            .get("LIVE_EXECUTOR_RPC_URL")
            .filter(|value| !value.trim().is_empty())
        {
            let executor_rpc = parse_production_url(executor_rpc)?;
            if executor_rpc != rpc_url {
                return Err(OwnerBootstrapError::RpcConfigurationMismatch);
            }
        }
        let rpc_allowlist = required(values, "LIVE_EXECUTOR_RPC_ALLOWLIST")?
            .split(',')
            .map(|value| parse_production_url(value.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        if rpc_allowlist.is_empty() || !rpc_allowlist.iter().any(|allowed| allowed == &rpc_url) {
            return Err(OwnerBootstrapError::RpcNotAllowlisted);
        }
        let rpc_header_name = required(values, "LIVE_EXECUTOR_RPC_HEADER_NAME")?.to_string();
        let rpc_header_file = required(values, "LIVE_EXECUTOR_RPC_HEADER_FILE")?.to_string();
        if rpc_header_name != "api-key" || !Path::new(&rpc_header_file).is_absolute() {
            return Err(OwnerBootstrapError::RpcConfigurationMismatch);
        }
        let maximum_input = positive_u128(values, "LIVE_EXECUTOR_MAX_INPUT_AMOUNT")?;
        let maximum_gas_limit = positive_u64(values, "LIVE_EXECUTOR_MAX_GAS_LIMIT")?;
        let maximum_max_fee_per_gas =
            positive_u128(values, "LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI")?;
        let maximum_priority_fee_per_gas =
            positive_u128(values, "LIVE_EXECUTOR_MAX_PRIORITY_FEE_PER_GAS_WEI")?;
        if maximum_priority_fee_per_gas > maximum_max_fee_per_gas {
            return Err(OwnerBootstrapError::InvalidLimit);
        }
        let receipt_timeout = Duration::from_secs(positive_u64(
            values,
            "LIVE_EXECUTOR_RECEIPT_TIMEOUT_SECONDS",
        )?);
        let poll_interval =
            Duration::from_secs(positive_u64(values, "LIVE_EXECUTOR_POLL_INTERVAL_SECONDS")?);
        if receipt_timeout > Duration::from_secs(600) || poll_interval > Duration::from_secs(30) {
            return Err(OwnerBootstrapError::InvalidLimit);
        }
        let one_transaction_at_a_time =
            required(values, "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME")? == "true";
        if !one_transaction_at_a_time {
            return Err(OwnerBootstrapError::ConcurrentTransactionsForbidden);
        }
        let release_sha = required(values, "PHOENIX_RELEASE_SHA")?.to_string();
        if !canonical_release_sha(&release_sha) {
            return Err(OwnerBootstrapError::InvalidReleaseSha);
        }
        let routers = required(values, "ENGINE_ROUTER_ADDRESSES")?
            .split(',')
            .map(|value| parse_address(value.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        if routers.is_empty() || routers.len() > 3 {
            return Err(OwnerBootstrapError::InvalidRouterSet);
        }
        let unique_routers = routers
            .iter()
            .map(ToString::to_string)
            .collect::<HashSet<_>>();
        if unique_routers.len() != routers.len() {
            return Err(OwnerBootstrapError::InvalidRouterSet);
        }
        let policy = parse_policy()?;
        if maximum_input > policy.maximum_input {
            return Err(OwnerBootstrapError::MaximumInputExceedsPolicy);
        }
        let assets = reviewed_owner_assets(&policy)?;
        let legs = reviewed_owner_legs(&policy)?;
        Ok(Self {
            chain_id,
            wallet,
            executor,
            expected_owner,
            expected_flash_provider,
            expected_code_hash,
            maximum_input,
            maximum_gas_limit,
            maximum_max_fee_per_gas,
            maximum_priority_fee_per_gas,
            receipt_timeout,
            poll_interval,
            one_transaction_at_a_time,
            rpc_url,
            rpc_allowlist,
            rpc_header_name,
            rpc_header_file,
            signer_file: values
                .get("SIGNER_PRIVATE_KEY_FILE")
                .filter(|value| !value.trim().is_empty())
                .cloned(),
            release_sha,
            policy_hash: policy.hash,
            settlement_asset: policy.settlement_asset,
            assets,
            routers,
            legs,
        })
    }

    fn load_signer(&self) -> Result<TransactionSigner, OwnerBootstrapError> {
        if self.wallet != self.expected_owner {
            return Err(OwnerBootstrapError::SignerWalletOwnerMismatch);
        }
        let signer_file = self
            .signer_file
            .as_deref()
            .ok_or(OwnerBootstrapError::MissingSignerFile)?;
        let signer = transaction_signer_from_file(signer_file, self.chain_id)
            .map_err(|_| OwnerBootstrapError::InvalidSignerFile)?;
        if signer.address() != self.wallet || signer.address() != self.expected_owner {
            return Err(OwnerBootstrapError::SignerWalletOwnerMismatch);
        }
        Ok(signer)
    }
}

#[derive(Clone, Debug)]
struct RoutePolicy {
    hash: String,
    maximum_input: u128,
    settlement_asset: CanonicalAddress,
    legs: Vec<ValidatedLeg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnerActionKind {
    Searcher,
    Asset(usize),
    Router(usize),
    Factory(usize),
    Pool(usize),
    MaximumInput,
    Unpause,
    Pause,
}

#[derive(Clone, PartialEq, Eq)]
struct OwnerAction {
    kind: OwnerActionKind,
    description: &'static str,
    calldata: Vec<u8>,
}

impl OwnerAction {
    fn calldata_hash(&self) -> String {
        hex::encode(Sha256::digest(&self.calldata))
    }
}

#[derive(Clone, Debug)]
struct ActionEvidence {
    description: &'static str,
    calldata_hash: String,
    status: &'static str,
    transaction_hash: Option<TransactionHash>,
    block_number: Option<u64>,
    receipt_status: Option<u64>,
}

#[derive(Clone, Copy)]
struct OwnerTransactionLimits {
    chain_id: u64,
    maximum_gas_limit: u64,
    maximum_max_fee_per_gas: u128,
    maximum_priority_fee_per_gas: u128,
    receipt_timeout: Duration,
    poll_interval: Duration,
}

#[async_trait]
trait OwnerBootstrapRpc: Send + Sync {
    async fn chain_id(&self) -> Result<u64, RpcError>;
    async fn snapshot(
        &self,
        context: &OwnerBootstrapContext,
        router: CanonicalAddress,
    ) -> Result<ExecutorConfigurationSnapshot, RpcError>;
    async fn quote(
        &self,
        from: CanonicalAddress,
        to: CanonicalAddress,
        calldata: &[u8],
    ) -> Result<TransactionQuote, RpcError>;
    async fn pending_nonce(&self, wallet: CanonicalAddress) -> Result<u64, RpcError>;
    async fn submit(&self, raw_transaction: &[u8]) -> Result<TransactionHash, RpcError>;
    async fn receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError>;
    async fn transaction_known(&self, tx_hash: TransactionHash) -> Result<bool, RpcError>;
}

#[async_trait]
impl OwnerBootstrapRpc for HttpExecutionRpc {
    async fn chain_id(&self) -> Result<u64, RpcError> {
        ExecutionRpc::chain_id(self).await
    }

    async fn snapshot(
        &self,
        context: &OwnerBootstrapContext,
        router: CanonicalAddress,
    ) -> Result<ExecutorConfigurationSnapshot, RpcError> {
        self.executor_configuration_snapshot(
            context.executor,
            context.wallet,
            &context.assets,
            router,
            &context.legs,
        )
        .await
    }

    async fn quote(
        &self,
        from: CanonicalAddress,
        to: CanonicalAddress,
        calldata: &[u8],
    ) -> Result<TransactionQuote, RpcError> {
        self.quote_transaction(from, to, calldata).await
    }

    async fn pending_nonce(&self, wallet: CanonicalAddress) -> Result<u64, RpcError> {
        ExecutionRpc::pending_nonce(self, wallet).await
    }

    async fn submit(&self, raw_transaction: &[u8]) -> Result<TransactionHash, RpcError> {
        self.send_raw_transaction(raw_transaction).await
    }

    async fn receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<Option<TransactionReceipt>, RpcError> {
        self.transaction_receipt(tx_hash).await
    }

    async fn transaction_known(&self, tx_hash: TransactionHash) -> Result<bool, RpcError> {
        ExecutionRpc::transaction_known(self, tx_hash).await
    }
}

async fn submit_owner_action<R: OwnerBootstrapRpc>(
    rpc: &R,
    signer: &TransactionSigner,
    wallet: CanonicalAddress,
    executor: CanonicalAddress,
    calldata: Vec<u8>,
    limits: OwnerTransactionLimits,
) -> Result<TransactionReceipt, OwnerBootstrapError> {
    if signer.address() != wallet {
        return Err(OwnerBootstrapError::SignerWalletOwnerMismatch);
    }
    let quote = rpc
        .quote(wallet, executor, &calldata)
        .await
        .map_err(map_rpc)?;
    if quote.gas_limit > limits.maximum_gas_limit {
        return Err(OwnerBootstrapError::GasLimitExceeded);
    }
    if quote.max_fee_per_gas > limits.maximum_max_fee_per_gas {
        return Err(OwnerBootstrapError::MaximumFeeExceeded);
    }
    if quote.max_priority_fee_per_gas > limits.maximum_priority_fee_per_gas {
        return Err(OwnerBootstrapError::PriorityFeeExceeded);
    }
    let nonce = rpc.pending_nonce(wallet).await.map_err(map_rpc)?;
    let signed = signer
        .sign(TransactionDraft {
            chain_id: limits.chain_id,
            nonce,
            gas_limit: quote.gas_limit,
            max_fee_per_gas: quote.max_fee_per_gas,
            max_priority_fee_per_gas: quote.max_priority_fee_per_gas,
            to: executor,
            calldata,
        })
        .map_err(|_| OwnerBootstrapError::SigningFailed)?;
    let returned_hash = rpc
        .submit(signed.raw_bytes())
        .await
        .map_err(map_submission_rpc)?;
    if returned_hash != signed.tx_hash() {
        return Err(OwnerBootstrapError::TransactionHashMismatch);
    }
    let receipt = wait_for_receipt(
        rpc,
        returned_hash,
        limits.receipt_timeout,
        limits.poll_interval,
    )
    .await?;
    if receipt.transaction_hash != returned_hash {
        return Err(OwnerBootstrapError::RpcDisagreement);
    }
    if receipt.status != 1 {
        return Err(OwnerBootstrapError::TransactionReverted);
    }
    Ok(receipt)
}

/// PhoenixExecutor rotation uses the same quote/nonce/sign/single-submit/hash
/// equality/receipt machinery as normal owner bootstrap, with stricter fixed
/// ceilings.  The raw transaction and signer material never leave this API.
pub(crate) async fn submit_rotation_owner_call(
    rpc: &HttpExecutionRpc,
    signer: &TransactionSigner,
    executor: CanonicalAddress,
    calldata: Vec<u8>,
) -> Result<TransactionReceipt, OwnerBootstrapError> {
    submit_owner_action(
        rpc,
        signer,
        signer.address(),
        executor,
        calldata,
        OwnerTransactionLimits {
            chain_id: 42_161,
            maximum_gas_limit: 5_000_000,
            maximum_max_fee_per_gas: 10_000_000_000,
            maximum_priority_fee_per_gas: 10_000_000_000,
            receipt_timeout: Duration::from_secs(120),
            poll_interval: Duration::from_secs(2),
        },
    )
    .await
}

pub async fn owner_plan_from_environment() -> Result<Value, OwnerBootstrapError> {
    let context = OwnerBootstrapContext::from_environment()?;
    let rpc = production_rpc(&context)?;
    owner_plan(&context, &rpc).await
}

pub async fn configured_preflight_from_environment() -> Result<Value, OwnerBootstrapError> {
    let context = OwnerBootstrapContext::from_environment()?;
    let rpc = production_rpc(&context)?;
    configured_preflight(&context, &rpc).await
}

pub async fn configured_signer_preflight_from_environment() -> Result<Value, OwnerBootstrapError> {
    let context = OwnerBootstrapContext::from_environment()?;
    let signer = context.load_signer()?;
    let rpc = production_rpc(&context)?;
    configured_signer_preflight(&context, &rpc, &signer).await
}

// runtime_preflight validates the exact executor identity and configuration
// without requiring a pause transition. Provider-only recovery consumes this
// read-only evidence and is explicitly forbidden from unpausing the contract.
pub async fn runtime_preflight_from_environment() -> Result<Value, OwnerBootstrapError> {
    let context = OwnerBootstrapContext::from_environment()?;
    let rpc = production_rpc(&context)?;
    runtime_preflight(&context, &rpc).await
}

// Validates the exact configured executor and signer without submitting a
// transaction. This is the post-unpause counterpart to configured_preflight.
pub async fn live_preflight_from_environment() -> Result<Value, OwnerBootstrapError> {
    let context = OwnerBootstrapContext::from_environment()?;
    let signer = context.load_signer()?;
    let rpc = production_rpc(&context)?;
    live_preflight(&context, &rpc, &signer).await
}

async fn live_preflight<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
    signer: &TransactionSigner,
) -> Result<Value, OwnerBootstrapError> {
    if signer.address() != context.wallet || signer.address() != context.expected_owner {
        return Err(OwnerBootstrapError::SignerWalletOwnerMismatch);
    }
    let mut evidence = runtime_preflight(context, rpc).await?;
    if evidence
        .get("final_state")
        .and_then(|state| state.get("paused"))
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Err(OwnerBootstrapError::LivePreflightRequiresUnpaused);
    }
    evidence["command"] = Value::String("live-preflight".to_string());
    evidence["status"] = Value::String("ready-unpaused".to_string());
    evidence["signer_matches_owner"] = Value::Bool(true);
    Ok(evidence)
}

pub async fn execute_from_environment(
    mutation: OwnerMutation,
) -> Result<Value, OwnerBootstrapError> {
    require_acknowledgement_from_environment(mutation)?;
    let context = OwnerBootstrapContext::from_environment()?;
    let signer = context.load_signer()?;
    let rpc = production_rpc(&context)?;
    execute_mutation(&context, &rpc, &signer, mutation).await
}

pub fn require_acknowledgement_from_environment(
    mutation: OwnerMutation,
) -> Result<(), OwnerBootstrapError> {
    require_acknowledgement(mutation, |name| env::var(name).ok())
}

fn production_rpc(
    context: &OwnerBootstrapContext,
) -> Result<HttpExecutionRpc, OwnerBootstrapError> {
    HttpExecutionRpc::new_production_authenticated(
        context.rpc_url.clone(),
        &context.rpc_allowlist,
        &context.rpc_header_name,
        &context.rpc_header_file,
    )
    .map_err(|_| OwnerBootstrapError::RpcNotAllowlisted)
}

fn require_acknowledgement(
    mutation: OwnerMutation,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Result<(), OwnerBootstrapError> {
    let (name, expected) = mutation.acknowledgement();
    if lookup(name).as_deref() != Some(expected) {
        return Err(OwnerBootstrapError::InvalidAcknowledgement);
    }
    Ok(())
}

async fn owner_plan<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
) -> Result<Value, OwnerBootstrapError> {
    ensure_chain(context, rpc).await?;
    let snapshots = read_validated_snapshots(context, rpc).await?;
    if !snapshots[0].paused && !configuration_complete(context, &snapshots) {
        return Err(OwnerBootstrapError::UnpausedConfigurationIncomplete);
    }
    let mut actions = desired_configuration_actions(context)?;
    actions.push(action(
        OwnerActionKind::Unpause,
        "unpause executor after reviewed configuration",
        "setPaused",
        &[ParamType::Bool],
        &[Token::Bool(false)],
    ));
    let transactions = actions
        .iter()
        .filter(|candidate| action_needed(candidate, context, &snapshots))
        .map(|candidate| {
            json!({
                "chain_id": context.chain_id,
                "target": context.executor.to_string(),
                "value": "0",
                "calldata_hash": candidate.calldata_hash(),
                "description": candidate.description
            })
        })
        .collect::<Vec<_>>();
    let status = if transactions.is_empty() {
        "ready"
    } else {
        "EXTERNAL_OWNER_AUTHORIZATION_REQUIRED"
    };
    Ok(json!({
        "schema": OWNER_PLAN_SCHEMA,
        "status": status,
        "chain_id": context.chain_id,
        "target": context.executor.to_string(),
        "value": "0",
        "release_sha": context.release_sha,
        "route_policy_hash": context.policy_hash,
        "transactions": transactions,
        "verification_command": "autonomous-live-control owner-configured-preflight"
    }))
}

async fn configured_preflight<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
) -> Result<Value, OwnerBootstrapError> {
    ensure_chain(context, rpc).await?;
    let snapshots = read_validated_snapshots(context, rpc).await?;
    if !configuration_complete(context, &snapshots) {
        return Err(OwnerBootstrapError::ConfigurationIncomplete);
    }
    if !snapshots[0].paused {
        return Err(OwnerBootstrapError::ConfiguredPreflightRequiresPaused);
    }
    Ok(json!({
        "schema": OWNER_EVIDENCE_SCHEMA,
        "command": "configured-preflight",
        "chain_id": context.chain_id,
        "executor": context.executor.to_string(),
        "release_sha": context.release_sha,
        "route_policy_hash": context.policy_hash,
        "status": "ready-paused",
        "final_state": snapshot_evidence(context, &snapshots)
    }))
}

async fn configured_signer_preflight<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
    signer: &TransactionSigner,
) -> Result<Value, OwnerBootstrapError> {
    if signer.address() != context.wallet || signer.address() != context.expected_owner {
        return Err(OwnerBootstrapError::SignerWalletOwnerMismatch);
    }
    let mut evidence = configured_preflight(context, rpc).await?;
    evidence["command"] = Value::String("configured-signer-preflight".to_string());
    evidence["signer_matches_owner"] = Value::Bool(true);
    Ok(evidence)
}

async fn runtime_preflight<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
) -> Result<Value, OwnerBootstrapError> {
    ensure_chain(context, rpc).await?;
    let snapshots = read_validated_snapshots(context, rpc).await?;
    if !configuration_complete(context, &snapshots) {
        return Err(OwnerBootstrapError::ConfigurationIncomplete);
    }
    let paused = snapshots[0].paused;
    Ok(json!({
        "schema": OWNER_EVIDENCE_SCHEMA,
        "command": "runtime-preflight",
        "chain_id": context.chain_id,
        "executor": context.executor.to_string(),
        "release_sha": context.release_sha,
        "route_policy_hash": context.policy_hash,
        "status": if paused { "ready-paused" } else { "ready-unpaused" },
        "final_state": snapshot_evidence(context, &snapshots)
    }))
}

async fn execute_mutation<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
    signer: &TransactionSigner,
    mutation: OwnerMutation,
) -> Result<Value, OwnerBootstrapError> {
    if !context.one_transaction_at_a_time {
        return Err(OwnerBootstrapError::ConcurrentTransactionsForbidden);
    }
    if signer.address() != context.wallet
        || signer.address() != context.expected_owner
        || context.wallet != context.expected_owner
    {
        return Err(OwnerBootstrapError::SignerWalletOwnerMismatch);
    }
    ensure_chain(context, rpc).await?;
    let initial = read_validated_snapshots(context, rpc).await?;
    let actions = match mutation {
        OwnerMutation::Configure => {
            if !initial[0].paused && !configuration_complete(context, &initial) {
                return Err(OwnerBootstrapError::UnpausedConfigurationIncomplete);
            }
            desired_configuration_actions(context)?
        }
        OwnerMutation::Unpause => {
            if !configuration_complete(context, &initial) {
                return Err(OwnerBootstrapError::ConfigurationIncomplete);
            }
            vec![action(
                OwnerActionKind::Unpause,
                "unpause fully configured executor",
                "setPaused",
                &[ParamType::Bool],
                &[Token::Bool(false)],
            )]
        }
        OwnerMutation::Pause => vec![action(
            OwnerActionKind::Pause,
            "pause executor after failed deployment",
            "setPaused",
            &[ParamType::Bool],
            &[Token::Bool(true)],
        )],
    };

    let mut evidence = Vec::with_capacity(actions.len());
    for candidate in actions {
        let before = read_validated_snapshots(context, rpc).await?;
        if mutation == OwnerMutation::Configure && !before[0].paused {
            return Err(OwnerBootstrapError::ConfigureWouldUnpause);
        }
        if !action_needed(&candidate, context, &before) {
            evidence.push(ActionEvidence {
                description: candidate.description,
                calldata_hash: candidate.calldata_hash(),
                status: "skipped-already-applied",
                transaction_hash: None,
                block_number: None,
                receipt_status: None,
            });
            continue;
        }
        if mutation == OwnerMutation::Unpause && !configuration_complete(context, &before) {
            return Err(OwnerBootstrapError::ConfigurationIncomplete);
        }
        let receipt = submit_owner_action(
            rpc,
            signer,
            context.wallet,
            context.executor,
            candidate.calldata.clone(),
            OwnerTransactionLimits {
                chain_id: context.chain_id,
                maximum_gas_limit: context.maximum_gas_limit,
                maximum_max_fee_per_gas: context.maximum_max_fee_per_gas,
                maximum_priority_fee_per_gas: context.maximum_priority_fee_per_gas,
                receipt_timeout: context.receipt_timeout,
                poll_interval: context.poll_interval,
            },
        )
        .await?;
        let after = read_validated_snapshots(context, rpc).await?;
        if action_needed(&candidate, context, &after) {
            return Err(OwnerBootstrapError::StateTransitionMismatch);
        }
        if mutation == OwnerMutation::Configure && !after[0].paused {
            return Err(OwnerBootstrapError::ConfigureWouldUnpause);
        }
        evidence.push(ActionEvidence {
            description: candidate.description,
            calldata_hash: candidate.calldata_hash(),
            status: "applied",
            transaction_hash: Some(receipt.transaction_hash),
            block_number: Some(receipt.block_number),
            receipt_status: Some(receipt.status),
        });
    }

    let final_snapshots = read_validated_snapshots(context, rpc).await?;
    match mutation {
        OwnerMutation::Configure => {
            if !configuration_complete(context, &final_snapshots) || !final_snapshots[0].paused {
                return Err(OwnerBootstrapError::StateTransitionMismatch);
            }
        }
        OwnerMutation::Unpause if final_snapshots[0].paused => {
            return Err(OwnerBootstrapError::StateTransitionMismatch);
        }
        OwnerMutation::Pause if !final_snapshots[0].paused => {
            return Err(OwnerBootstrapError::StateTransitionMismatch);
        }
        OwnerMutation::Unpause | OwnerMutation::Pause => {}
    }
    Ok(json!({
        "schema": OWNER_EVIDENCE_SCHEMA,
        "command": mutation.label(),
        "chain_id": context.chain_id,
        "executor": context.executor.to_string(),
        "release_sha": context.release_sha,
        "route_policy_hash": context.policy_hash,
        "status": mutation.success_marker(),
        "actions": evidence.into_iter().map(|item| json!({
            "description": item.description,
            "calldata_hash": item.calldata_hash,
            "transaction_hash": item.transaction_hash.map(|hash| hash.to_string()),
            "block_number": item.block_number,
            "receipt_status": item.receipt_status,
            "status": item.status
        })).collect::<Vec<_>>(),
        "final_state": snapshot_evidence(context, &final_snapshots)
    }))
}

async fn ensure_chain<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
) -> Result<(), OwnerBootstrapError> {
    let chain_id = rpc.chain_id().await.map_err(map_rpc)?;
    if chain_id != ARBITRUM_ONE_CHAIN_ID || chain_id != context.chain_id {
        return Err(OwnerBootstrapError::ChainMismatch);
    }
    Ok(())
}

async fn read_validated_snapshots<R: OwnerBootstrapRpc>(
    context: &OwnerBootstrapContext,
    rpc: &R,
) -> Result<Vec<ExecutorConfigurationSnapshot>, OwnerBootstrapError> {
    let mut snapshots = Vec::with_capacity(context.routers.len());
    for router in &context.routers {
        snapshots.push(rpc.snapshot(context, *router).await.map_err(map_rpc)?);
    }
    let first = snapshots
        .first()
        .ok_or(OwnerBootstrapError::InvalidRouterSet)?;
    validate_snapshot_shape(context, first)?;
    validate_identity(context, first)?;
    for snapshot in snapshots.iter().skip(1) {
        validate_snapshot_shape(context, snapshot)?;
        validate_identity(context, snapshot)?;
        if snapshot.runtime_code_hash != first.runtime_code_hash
            || snapshot.owner != first.owner
            || snapshot.flash_provider != first.flash_provider
            || snapshot.paused != first.paused
            || snapshot.maximum_input_amount != first.maximum_input_amount
            || snapshot.searcher_authorized != first.searcher_authorized
            || snapshot.assets_approved != first.assets_approved
            || snapshot.factories_approved != first.factories_approved
            || snapshot.pools_approved != first.pools_approved
        {
            return Err(OwnerBootstrapError::RpcDisagreement);
        }
    }
    Ok(snapshots)
}

fn validate_snapshot_shape(
    context: &OwnerBootstrapContext,
    snapshot: &ExecutorConfigurationSnapshot,
) -> Result<(), OwnerBootstrapError> {
    if snapshot.assets_approved.len() != context.assets.len()
        || snapshot.factories_approved.len() != context.legs.len()
        || snapshot.pools_approved.len() != context.legs.len()
    {
        return Err(OwnerBootstrapError::RpcDisagreement);
    }
    Ok(())
}

fn validate_identity(
    context: &OwnerBootstrapContext,
    snapshot: &ExecutorConfigurationSnapshot,
) -> Result<(), OwnerBootstrapError> {
    if snapshot.runtime_code_hash != context.expected_code_hash {
        return Err(OwnerBootstrapError::CodeHashMismatch);
    }
    if snapshot.owner != Some(context.expected_owner) {
        return Err(OwnerBootstrapError::OnchainOwnerMismatch);
    }
    if snapshot.flash_provider != Some(context.expected_flash_provider) {
        return Err(OwnerBootstrapError::FlashProviderMismatch);
    }
    Ok(())
}

fn configuration_complete(
    context: &OwnerBootstrapContext,
    snapshots: &[ExecutorConfigurationSnapshot],
) -> bool {
    let Some(first) = snapshots.first() else {
        return false;
    };
    first.searcher_authorized
        && first.assets_approved.iter().all(|approved| *approved)
        && first.maximum_input_amount == context.maximum_input
        && first.factories_approved.iter().all(|approved| *approved)
        && first.pools_approved.iter().all(|approved| *approved)
        && snapshots.iter().all(|snapshot| snapshot.router_approved)
}

fn desired_configuration_actions(
    context: &OwnerBootstrapContext,
) -> Result<Vec<OwnerAction>, OwnerBootstrapError> {
    let mut actions = vec![action(
        OwnerActionKind::Searcher,
        "authorize autonomous searcher",
        "setSearcher",
        &[ParamType::Address, ParamType::Bool],
        &[address_token(context.wallet), Token::Bool(true)],
    )];
    for (index, asset) in context.assets.iter().enumerate() {
        actions.push(action(
            OwnerActionKind::Asset(index),
            "approve reviewed execution asset",
            "setAsset",
            &[ParamType::Address, ParamType::Bool],
            &[address_token(*asset), Token::Bool(true)],
        ));
    }
    for (index, router) in context.routers.iter().enumerate() {
        actions.push(action(
            OwnerActionKind::Router(index),
            "approve reviewed router",
            "setRouter",
            &[ParamType::Address, ParamType::Bool],
            &[address_token(*router), Token::Bool(true)],
        ));
    }
    let mut seen_factories = Vec::new();
    for (index, leg) in context.legs.iter().enumerate() {
        let factory = leg.factory.ok_or(OwnerBootstrapError::InvalidPolicy)?;
        if !seen_factories.contains(&factory) {
            seen_factories.push(factory);
            actions.push(action(
                OwnerActionKind::Factory(index),
                "approve reviewed factory",
                "setFactory",
                &[ParamType::Address, ParamType::Bool],
                &[address_token(factory), Token::Bool(true)],
            ));
        }
    }
    for (index, leg) in context.legs.iter().enumerate() {
        let factory = leg.factory.ok_or(OwnerBootstrapError::InvalidPolicy)?;
        let (token0, token1) = ordered_pair(leg.token_in, leg.token_out);
        actions.push(action(
            OwnerActionKind::Pool(index),
            "approve reviewed pool",
            "approvePool",
            &[
                ParamType::Address,
                ParamType::Address,
                ParamType::Address,
                ParamType::Address,
                ParamType::Uint(24),
                ParamType::Bool,
            ],
            &[
                address_token(leg.pool),
                address_token(factory),
                address_token(token0),
                address_token(token1),
                Token::Uint(leg.fee.into()),
                Token::Bool(true),
            ],
        ));
    }
    actions.push(action(
        OwnerActionKind::MaximumInput,
        "set conservative maximum input",
        "setMaximumInputAmount",
        &[ParamType::Uint(256)],
        &[Token::Uint(context.maximum_input.into())],
    ));
    Ok(actions)
}

fn action(
    kind: OwnerActionKind,
    description: &'static str,
    name: &str,
    input_types: &[ParamType],
    arguments: &[Token],
) -> OwnerAction {
    let mut calldata = ethabi::short_signature(name, input_types).to_vec();
    calldata.extend(ethabi::encode(arguments));
    OwnerAction {
        kind,
        description,
        calldata,
    }
}

fn action_needed(
    candidate: &OwnerAction,
    context: &OwnerBootstrapContext,
    snapshots: &[ExecutorConfigurationSnapshot],
) -> bool {
    let first = &snapshots[0];
    match candidate.kind {
        OwnerActionKind::Searcher => !first.searcher_authorized,
        OwnerActionKind::Asset(index) => !first.assets_approved[index],
        OwnerActionKind::Router(index) => !snapshots[index].router_approved,
        OwnerActionKind::Factory(index) => !first.factories_approved[index],
        OwnerActionKind::Pool(index) => !first.pools_approved[index],
        OwnerActionKind::MaximumInput => first.maximum_input_amount != context.maximum_input,
        OwnerActionKind::Unpause => first.paused,
        OwnerActionKind::Pause => !first.paused,
    }
}

#[cfg(test)]
fn enforce_quote(
    context: &OwnerBootstrapContext,
    quote: &TransactionQuote,
) -> Result<(), OwnerBootstrapError> {
    if quote.gas_limit > context.maximum_gas_limit {
        return Err(OwnerBootstrapError::GasLimitExceeded);
    }
    if quote.max_fee_per_gas > context.maximum_max_fee_per_gas {
        return Err(OwnerBootstrapError::MaximumFeeExceeded);
    }
    if quote.max_priority_fee_per_gas > context.maximum_priority_fee_per_gas {
        return Err(OwnerBootstrapError::PriorityFeeExceeded);
    }
    Ok(())
}

async fn wait_for_receipt<R: OwnerBootstrapRpc>(
    rpc: &R,
    transaction_hash: TransactionHash,
    receipt_timeout: Duration,
    poll_interval: Duration,
) -> Result<TransactionReceipt, OwnerBootstrapError> {
    let deadline = Instant::now() + receipt_timeout;
    loop {
        if let Some(receipt) = rpc.receipt(transaction_hash).await.map_err(map_rpc)? {
            return Ok(receipt);
        }
        if Instant::now() >= deadline {
            return if rpc
                .transaction_known(transaction_hash)
                .await
                .map_err(map_rpc)?
            {
                Err(OwnerBootstrapError::ReceiptTimeout)
            } else {
                Err(OwnerBootstrapError::SubmissionUnknown)
            };
        }
        sleep(poll_interval).await;
    }
}

fn snapshot_evidence(
    context: &OwnerBootstrapContext,
    snapshots: &[ExecutorConfigurationSnapshot],
) -> Value {
    let first = &snapshots[0];
    let mut expected_factories = Vec::new();
    for factory in context.legs.iter().filter_map(|leg| leg.factory) {
        let value = factory.to_string();
        if !expected_factories.contains(&value) {
            expected_factories.push(value);
        }
    }
    json!({
        "runtime_code_hash": first.runtime_code_hash,
        "owner": first.owner.map(|value| value.to_string()),
        "flash_provider": first.flash_provider.map(|value| value.to_string()),
        "paused": first.paused,
        "maximum_input_amount": first.maximum_input_amount.to_string(),
        "expected_searcher": context.wallet.to_string(),
        "settlement_asset": context.settlement_asset.to_string(),
        "expected_assets": context.assets.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "expected_routers": context.routers.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "expected_factories": expected_factories,
        "expected_pools": context.legs.iter().map(|leg| leg.pool.to_string()).collect::<Vec<_>>(),
        "searcher_authorized": first.searcher_authorized,
        "assets_approved": first.assets_approved,
        "routers_approved": snapshots.iter().all(|snapshot| snapshot.router_approved),
        "factories_approved": first.factories_approved.iter().all(|value| *value),
        "pools_approved": first.pools_approved.iter().all(|value| *value),
        "configuration_complete": configuration_complete(context, snapshots)
    })
}

fn parse_policy() -> Result<RoutePolicy, OwnerBootstrapError> {
    let policy: Value =
        serde_json::from_str(POLICY).map_err(|_| OwnerBootstrapError::InvalidPolicy)?;
    verify_contract_hash(
        &policy,
        "policy_hash",
        "route-policy",
        "phoenix.route-policy.v1",
    )?;
    if policy.get("chain_id").and_then(Value::as_u64) != Some(ARBITRUM_ONE_CHAIN_ID) {
        return Err(OwnerBootstrapError::InvalidPolicy);
    }
    let settlement_asset = parse_address(value_text(&policy, "settlement_asset")?)?;
    let token_path = policy
        .get("token_path")
        .and_then(Value::as_array)
        .ok_or(OwnerBootstrapError::InvalidPolicy)?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or(OwnerBootstrapError::InvalidPolicy)
                .and_then(parse_address)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let pools = policy
        .get("pool_addresses")
        .and_then(Value::as_array)
        .ok_or(OwnerBootstrapError::InvalidPolicy)?;
    let factories = policy
        .get("factory_addresses")
        .and_then(Value::as_array)
        .ok_or(OwnerBootstrapError::InvalidPolicy)?;
    let fees = policy
        .get("fees")
        .and_then(Value::as_array)
        .ok_or(OwnerBootstrapError::InvalidPolicy)?;
    let directions = policy
        .get("directions")
        .and_then(Value::as_array)
        .ok_or(OwnerBootstrapError::InvalidPolicy)?;
    if pools.len() + 1 != token_path.len()
        || factories.len() != pools.len()
        || fees.len() != pools.len()
        || directions.len() != pools.len()
    {
        return Err(OwnerBootstrapError::InvalidPolicy);
    }
    let legs = (0..pools.len())
        .map(|index| {
            let direction = directions[index]
                .as_str()
                .ok_or(OwnerBootstrapError::InvalidPolicy)?;
            let zero_for_one = match direction {
                "zero_for_one" => true,
                "one_for_zero" => false,
                _ => return Err(OwnerBootstrapError::InvalidPolicy),
            };
            Ok(ValidatedLeg {
                pool: parse_address(
                    pools[index]
                        .as_str()
                        .ok_or(OwnerBootstrapError::InvalidPolicy)?,
                )?,
                factory: Some(parse_address(
                    factories[index]
                        .as_str()
                        .ok_or(OwnerBootstrapError::InvalidPolicy)?,
                )?),
                token_in: token_path[index],
                token_out: token_path[index + 1],
                fee: fees[index]
                    .as_u64()
                    .and_then(|value| value.try_into().ok())
                    .ok_or(OwnerBootstrapError::InvalidPolicy)?,
                zero_for_one,
                min_amount_out: 1,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RoutePolicy {
        hash: value_text(&policy, "policy_hash")?.to_string(),
        maximum_input: value_text(&policy, "maximum_input_amount")?
            .parse()
            .map_err(|_| OwnerBootstrapError::InvalidPolicy)?,
        settlement_asset,
        legs,
    })
}

fn reviewed_owner_assets(
    policy: &RoutePolicy,
) -> Result<Vec<CanonicalAddress>, OwnerBootstrapError> {
    let mut assets = vec![policy.settlement_asset];
    for route in reviewed_aave_unwind_routes().map_err(|_| OwnerBootstrapError::InvalidPolicy)? {
        for token in [route.token0, route.token1] {
            let token = parse_address(&token)?;
            if !assets.contains(&token) {
                assets.push(token);
            }
        }
    }
    if assets.len() != 3 {
        return Err(OwnerBootstrapError::InvalidPolicy);
    }
    Ok(assets)
}

fn reviewed_owner_legs(policy: &RoutePolicy) -> Result<Vec<ValidatedLeg>, OwnerBootstrapError> {
    let mut legs = policy.legs.clone();
    for route in reviewed_aave_unwind_routes().map_err(|_| OwnerBootstrapError::InvalidPolicy)? {
        let pool = parse_address(&route.pool)?;
        let factory = parse_address(&route.factory)?;
        let token0 = parse_address(&route.token0)?;
        let token1 = parse_address(&route.token1)?;
        if let Some(existing) = legs.iter().find(|leg| leg.pool == pool) {
            let same_pair = (existing.token_in == token0 && existing.token_out == token1)
                || (existing.token_in == token1 && existing.token_out == token0);
            if existing.factory != Some(factory) || existing.fee != route.fee || !same_pair {
                return Err(OwnerBootstrapError::InvalidPolicy);
            }
            continue;
        }
        legs.push(ValidatedLeg {
            pool,
            factory: Some(factory),
            token_in: if route.zero_for_one { token0 } else { token1 },
            token_out: if route.zero_for_one { token1 } else { token0 },
            fee: route.fee,
            zero_for_one: route.zero_for_one,
            min_amount_out: 1,
        });
    }
    if legs.len() != 4 {
        return Err(OwnerBootstrapError::InvalidPolicy);
    }
    Ok(legs)
}

fn address_token(address: CanonicalAddress) -> Token {
    Token::Address(primitive_types::H160::from_slice(address.as_bytes()))
}

fn ordered_pair(
    left: CanonicalAddress,
    right: CanonicalAddress,
) -> (CanonicalAddress, CanonicalAddress) {
    if left.as_bytes() < right.as_bytes() {
        (left, right)
    } else {
        (right, left)
    }
}

fn required<'a>(
    values: &'a BTreeMap<String, String>,
    name: &'static str,
) -> Result<&'a str, OwnerBootstrapError> {
    values
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(OwnerBootstrapError::Missing(name))
}

fn positive_u64(
    values: &BTreeMap<String, String>,
    name: &'static str,
) -> Result<u64, OwnerBootstrapError> {
    required(values, name)?
        .parse()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(OwnerBootstrapError::InvalidLimit)
}

fn positive_u128(
    values: &BTreeMap<String, String>,
    name: &'static str,
) -> Result<u128, OwnerBootstrapError> {
    required(values, name)?
        .parse()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(OwnerBootstrapError::InvalidLimit)
}

fn parse_address(value: &str) -> Result<CanonicalAddress, OwnerBootstrapError> {
    CanonicalAddress::parse(value).map_err(|_| OwnerBootstrapError::InvalidAddress)
}

fn parse_production_url(value: &str) -> Result<Url, OwnerBootstrapError> {
    let url = Url::parse(value).map_err(|_| OwnerBootstrapError::InvalidRpcUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(OwnerBootstrapError::InvalidRpcUrl);
    }
    Ok(url)
}

fn canonical_release_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn value_text<'a>(value: &'a Value, field: &str) -> Result<&'a str, OwnerBootstrapError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or(OwnerBootstrapError::InvalidPolicy)
}

fn verify_contract_hash(
    value: &Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<(), OwnerBootstrapError> {
    let mut body = value.clone();
    body.as_object_mut()
        .ok_or(OwnerBootstrapError::InvalidPolicy)?
        .remove(field)
        .ok_or(OwnerBootstrapError::InvalidPolicy)?;
    let prefix = format!("phoenix.canonical-json.v1:{domain}:{schema}\n");
    let digest = hex::encode(Sha256::digest(
        [prefix.as_bytes(), canonical_json(&body)?.as_slice()].concat(),
    ));
    if value_text(value, field)? != digest {
        return Err(OwnerBootstrapError::PolicyHashMismatch);
    }
    Ok(())
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, OwnerBootstrapError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            serde_json::to_vec(value).map_err(|_| OwnerBootstrapError::InvalidPolicy)
        }
        Value::Array(values) => {
            let mut output = vec![b'['];
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(canonical_json(child)?);
            }
            output.push(b']');
            Ok(output)
        }
        Value::Object(values) => {
            let sorted = values.iter().collect::<BTreeMap<_, _>>();
            let mut output = vec![b'{'];
            for (index, (key, child)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(
                    serde_json::to_vec(key).map_err(|_| OwnerBootstrapError::InvalidPolicy)?,
                );
                output.push(b':');
                output.extend(canonical_json(child)?);
            }
            output.push(b'}');
            Ok(output)
        }
    }
}

fn map_rpc(error: RpcError) -> OwnerBootstrapError {
    match error.kind {
        RpcErrorKind::NonceConflict => OwnerBootstrapError::NonceConflict,
        RpcErrorKind::Timeout => OwnerBootstrapError::RpcTimeout,
        _ => OwnerBootstrapError::RpcFailure,
    }
}

fn map_submission_rpc(error: RpcError) -> OwnerBootstrapError {
    match error.kind {
        RpcErrorKind::NonceConflict => OwnerBootstrapError::NonceConflict,
        RpcErrorKind::Timeout | RpcErrorKind::Transport => OwnerBootstrapError::SubmissionUnknown,
        _ => OwnerBootstrapError::RpcFailure,
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OwnerBootstrapError {
    #[error("owner acknowledgement is invalid")]
    InvalidAcknowledgement,
    #[error("missing required owner setting: {0}")]
    Missing(&'static str),
    #[error("environment-backed signer material is forbidden")]
    EnvironmentSignerForbidden,
    #[error("signer file is missing")]
    MissingSignerFile,
    #[error("signer file is invalid or unsafe")]
    InvalidSignerFile,
    #[error("signer, wallet, and expected owner do not match")]
    SignerWalletOwnerMismatch,
    #[error("chain identity is invalid")]
    InvalidChain,
    #[error("RPC chain identity mismatch")]
    ChainMismatch,
    #[error("canonical address is invalid")]
    InvalidAddress,
    #[error("executor code hash is invalid")]
    InvalidCodeHash,
    #[error("executor runtime code hash mismatch")]
    CodeHashMismatch,
    #[error("on-chain executor owner mismatch")]
    OnchainOwnerMismatch,
    #[error("executor flash provider mismatch")]
    FlashProviderMismatch,
    #[error("RPC URL is invalid")]
    InvalidRpcUrl,
    #[error("RPC URL is not allowlisted")]
    RpcNotAllowlisted,
    #[error("executor and production RPC settings differ")]
    RpcConfigurationMismatch,
    #[error("safety limit is invalid")]
    InvalidLimit,
    #[error("one transaction at a time is required")]
    ConcurrentTransactionsForbidden,
    #[error("release SHA is invalid")]
    InvalidReleaseSha,
    #[error("reviewed router set is invalid")]
    InvalidRouterSet,
    #[error("embedded route policy is invalid")]
    InvalidPolicy,
    #[error("embedded route policy hash mismatch")]
    PolicyHashMismatch,
    #[error("configured maximum input exceeds policy")]
    MaximumInputExceedsPolicy,
    #[error("executor configuration is incomplete")]
    ConfigurationIncomplete,
    #[error("configured preflight requires the executor to remain paused")]
    ConfiguredPreflightRequiresPaused,
    #[error("live preflight requires the executor to be unpaused")]
    LivePreflightRequiresUnpaused,
    #[error("unpaused executor has incomplete configuration")]
    UnpausedConfigurationIncomplete,
    #[error("owner configure cannot unpause the executor")]
    ConfigureWouldUnpause,
    #[error("transaction gas limit exceeds the configured cap")]
    GasLimitExceeded,
    #[error("transaction maximum fee exceeds the configured cap")]
    MaximumFeeExceeded,
    #[error("transaction priority fee exceeds the configured cap")]
    PriorityFeeExceeded,
    #[error("transaction signing failed")]
    SigningFailed,
    #[error("transaction nonce conflict")]
    NonceConflict,
    #[error("submitted transaction hash mismatch")]
    TransactionHashMismatch,
    #[error("transaction reverted")]
    TransactionReverted,
    #[error("transaction receipt timed out")]
    ReceiptTimeout,
    #[error("transaction submission is unknown")]
    SubmissionUnknown,
    #[error("RPC providers or responses disagree")]
    RpcDisagreement,
    #[error("RPC request timed out")]
    RpcTimeout,
    #[error("RPC request failed")]
    RpcFailure,
    #[error("on-chain state transition mismatch")]
    StateTransitionMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const EXECUTOR: &str = "0x1111111111111111111111111111111111111111";
    const FLASH: &str = "0x2222222222222222222222222222222222222222";
    const ROUTER: &str = "0x3333333333333333333333333333333333333333";
    const ASSET: &str = "0x4444444444444444444444444444444444444444";
    const TOKEN: &str = "0x5555555555555555555555555555555555555555";
    const POOL_A: &str = "0x6666666666666666666666666666666666666666";
    const POOL_B: &str = "0x7777777777777777777777777777777777777777";
    const FACTORY: &str = "0x8888888888888888888888888888888888888888";

    struct MockRpc {
        chain_id: u64,
        snapshot: Mutex<ExecutorConfigurationSnapshot>,
        post_submit: Mutex<Option<ExecutorConfigurationSnapshot>>,
        quote: Mutex<TransactionQuote>,
        receipt_status: u64,
        return_wrong_hash: bool,
        omit_receipt: bool,
        known: bool,
        submitted_hash: Mutex<Option<TransactionHash>>,
        snapshot_calls: AtomicUsize,
        quote_calls: AtomicUsize,
        nonce_calls: AtomicUsize,
        submit_calls: AtomicUsize,
    }

    impl MockRpc {
        fn new(snapshot: ExecutorConfigurationSnapshot) -> Self {
            Self {
                chain_id: ARBITRUM_ONE_CHAIN_ID,
                snapshot: Mutex::new(snapshot),
                post_submit: Mutex::new(None),
                quote: Mutex::new(test_quote()),
                receipt_status: 1,
                return_wrong_hash: false,
                omit_receipt: false,
                known: true,
                submitted_hash: Mutex::new(None),
                snapshot_calls: AtomicUsize::new(0),
                quote_calls: AtomicUsize::new(0),
                nonce_calls: AtomicUsize::new(0),
                submit_calls: AtomicUsize::new(0),
            }
        }

        fn with_post_submit(self, snapshot: ExecutorConfigurationSnapshot) -> Self {
            *self.post_submit.lock().expect("post submit") = Some(snapshot);
            self
        }
    }

    #[async_trait]
    impl OwnerBootstrapRpc for MockRpc {
        async fn chain_id(&self) -> Result<u64, RpcError> {
            Ok(self.chain_id)
        }

        async fn snapshot(
            &self,
            _context: &OwnerBootstrapContext,
            _router: CanonicalAddress,
        ) -> Result<ExecutorConfigurationSnapshot, RpcError> {
            self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.snapshot.lock().expect("snapshot").clone())
        }

        async fn quote(
            &self,
            _from: CanonicalAddress,
            _to: CanonicalAddress,
            _calldata: &[u8],
        ) -> Result<TransactionQuote, RpcError> {
            self.quote_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.quote.lock().expect("quote").clone())
        }

        async fn pending_nonce(&self, _wallet: CanonicalAddress) -> Result<u64, RpcError> {
            self.nonce_calls.fetch_add(1, Ordering::SeqCst);
            Ok(7)
        }

        async fn submit(&self, raw_transaction: &[u8]) -> Result<TransactionHash, RpcError> {
            self.submit_calls.fetch_add(1, Ordering::SeqCst);
            let local = TransactionHash::from_bytes(alloy_primitives::keccak256(raw_transaction).0);
            *self.submitted_hash.lock().expect("submitted hash") = Some(local);
            if let Some(post_submit) = self.post_submit.lock().expect("post submit").take() {
                *self.snapshot.lock().expect("snapshot") = post_submit;
            }
            if self.return_wrong_hash {
                Ok(TransactionHash::from_bytes([9; 32]))
            } else {
                Ok(local)
            }
        }

        async fn receipt(
            &self,
            _tx_hash: TransactionHash,
        ) -> Result<Option<TransactionReceipt>, RpcError> {
            if self.omit_receipt {
                return Ok(None);
            }
            let transaction_hash = self
                .submitted_hash
                .lock()
                .expect("submitted hash")
                .expect("submitted transaction");
            Ok(Some(TransactionReceipt {
                transaction_hash,
                contract_address: None,
                status: self.receipt_status,
                block_number: 99,
                gas_used: 21_000,
                l1_gas_used: 0,
                l1_fee: 0,
                effective_gas_price: 10,
                logs: Vec::new(),
            }))
        }

        async fn transaction_known(&self, _tx_hash: TransactionHash) -> Result<bool, RpcError> {
            Ok(self.known)
        }
    }

    fn address(value: &str) -> CanonicalAddress {
        CanonicalAddress::parse(value).expect("canonical address")
    }

    fn test_signer() -> TransactionSigner {
        TransactionSigner::from_secret(&hex::encode([7_u8; 32]), ARBITRUM_ONE_CHAIN_ID)
            .expect("test signer")
    }

    fn context() -> OwnerBootstrapContext {
        let signer = test_signer();
        let factory = address(FACTORY);
        OwnerBootstrapContext {
            chain_id: ARBITRUM_ONE_CHAIN_ID,
            wallet: signer.address(),
            executor: address(EXECUTOR),
            expected_owner: signer.address(),
            expected_flash_provider: address(FLASH),
            expected_code_hash: "a".repeat(64),
            maximum_input: 100,
            maximum_gas_limit: 500_000,
            maximum_max_fee_per_gas: 100,
            maximum_priority_fee_per_gas: 10,
            receipt_timeout: Duration::from_millis(1),
            poll_interval: Duration::ZERO,
            one_transaction_at_a_time: true,
            rpc_url: Url::parse("https://rpc.example.invalid").expect("url"),
            rpc_allowlist: vec![Url::parse("https://rpc.example.invalid").expect("url")],
            rpc_header_name: "api-key".to_string(),
            rpc_header_file: "/run/secrets/test-rpc-key".to_string(),
            signer_file: None,
            release_sha: "a".repeat(40),
            policy_hash: "b".repeat(64),
            settlement_asset: address(ASSET),
            assets: vec![address(ASSET), address(TOKEN)],
            routers: vec![address(ROUTER)],
            legs: vec![
                ValidatedLeg {
                    pool: address(POOL_A),
                    factory: Some(factory),
                    token_in: address(ASSET),
                    token_out: address(TOKEN),
                    fee: 500,
                    zero_for_one: true,
                    min_amount_out: 1,
                },
                ValidatedLeg {
                    pool: address(POOL_B),
                    factory: Some(factory),
                    token_in: address(TOKEN),
                    token_out: address(ASSET),
                    fee: 3_000,
                    zero_for_one: false,
                    min_amount_out: 1,
                },
            ],
        }
    }

    fn complete_snapshot(
        context: &OwnerBootstrapContext,
        paused: bool,
    ) -> ExecutorConfigurationSnapshot {
        ExecutorConfigurationSnapshot {
            runtime_code_hash: context.expected_code_hash.clone(),
            owner: Some(context.expected_owner),
            flash_provider: Some(context.expected_flash_provider),
            paused,
            maximum_input_amount: context.maximum_input,
            searcher_authorized: true,
            assets_approved: vec![true; context.assets.len()],
            router_approved: true,
            factories_approved: vec![true; context.legs.len()],
            pools_approved: vec![true; context.legs.len()],
        }
    }

    fn test_quote() -> TransactionQuote {
        TransactionQuote {
            block_number: 1,
            block_hash: format!("0x{}", "1".repeat(64)),
            gas_limit: 100_000,
            l1_gas_units: 0,
            base_fee_per_gas: 10,
            max_fee_per_gas: 30,
            max_priority_fee_per_gas: 5,
            estimated_l1_cost: 0,
            endpoint_identity: "rpc-test".to_string(),
        }
    }

    fn full_values() -> BTreeMap<String, String> {
        let context = context();
        BTreeMap::from([
            ("CHAIN_ID".to_string(), "42161".to_string()),
            ("WALLET_ADDRESS".to_string(), context.wallet.to_string()),
            ("EXECUTOR_ADDRESS".to_string(), context.executor.to_string()),
            (
                "PRODUCTION_RPC_URL".to_string(),
                "https://rpc.example.invalid".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RPC_URL".to_string(),
                "https://rpc.example.invalid".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RPC_ALLOWLIST".to_string(),
                "https://rpc.example.invalid".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RPC_HEADER_NAME".to_string(),
                "api-key".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RPC_HEADER_FILE".to_string(),
                std::env::temp_dir()
                    .join("phoenix-test-rpc-key")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "LIVE_EXECUTOR_EXPECTED_OWNER".to_string(),
                context.expected_owner.to_string(),
            ),
            (
                "LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER".to_string(),
                context.expected_flash_provider.to_string(),
            ),
            (
                "LIVE_EXECUTOR_EXECUTOR_CODE_HASH".to_string(),
                "a".repeat(64),
            ),
            (
                "LIVE_EXECUTOR_MAX_INPUT_AMOUNT".to_string(),
                "100".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_GAS_LIMIT".to_string(),
                "500000".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_MAX_FEE_PER_GAS_WEI".to_string(),
                "100".to_string(),
            ),
            (
                "LIVE_EXECUTOR_MAX_PRIORITY_FEE_PER_GAS_WEI".to_string(),
                "10".to_string(),
            ),
            (
                "LIVE_EXECUTOR_RECEIPT_TIMEOUT_SECONDS".to_string(),
                "10".to_string(),
            ),
            (
                "LIVE_EXECUTOR_POLL_INTERVAL_SECONDS".to_string(),
                "1".to_string(),
            ),
            (
                "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME".to_string(),
                "true".to_string(),
            ),
            ("ENGINE_ROUTER_ADDRESSES".to_string(), ROUTER.to_string()),
            ("PHOENIX_RELEASE_SHA".to_string(), "a".repeat(40)),
        ])
    }

    #[test]
    fn acknowledgement_missing_or_wrong_fails_before_other_lookup() {
        let lookups = AtomicUsize::new(0);
        let result = require_acknowledgement(OwnerMutation::Configure, |_| {
            lookups.fetch_add(1, Ordering::SeqCst);
            None
        });
        assert_eq!(result, Err(OwnerBootstrapError::InvalidAcknowledgement));
        assert_eq!(lookups.load(Ordering::SeqCst), 1);
        assert_eq!(
            require_acknowledgement(OwnerMutation::Configure, |_| Some("wrong".to_string())),
            Err(OwnerBootstrapError::InvalidAcknowledgement)
        );
    }

    #[test]
    fn environment_private_key_is_rejected() {
        let mut values = full_values();
        values.insert("SIGNER_PRIVATE_KEY".to_string(), hex::encode([1_u8; 32]));
        assert_eq!(
            OwnerBootstrapContext::from_values(&values).expect_err("environment signer"),
            OwnerBootstrapError::EnvironmentSignerForbidden
        );
    }

    #[tokio::test]
    async fn signer_wallet_expected_owner_mismatch_fails_before_rpc_mutation() {
        let mut context = context();
        context.expected_owner = address("0x9999999999999999999999999999999999999999");
        let rpc = MockRpc::new(complete_snapshot(&context, true));
        let result =
            execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Configure).await;
        assert_eq!(
            result.expect_err("owner mismatch"),
            OwnerBootstrapError::SignerWalletOwnerMismatch
        );
        assert_eq!(rpc.snapshot_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rpc.nonce_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn onchain_owner_mismatch_fails_closed() {
        let context = context();
        let mut snapshot = complete_snapshot(&context, true);
        snapshot.owner = Some(address("0x9999999999999999999999999999999999999999"));
        let error = configured_preflight(&context, &MockRpc::new(snapshot))
            .await
            .expect_err("owner mismatch");
        assert_eq!(error, OwnerBootstrapError::OnchainOwnerMismatch);
    }

    #[tokio::test]
    async fn chain_mismatch_fails_closed() {
        let context = context();
        let mut rpc = MockRpc::new(complete_snapshot(&context, true));
        rpc.chain_id = 1;
        assert_eq!(
            configured_preflight(&context, &rpc)
                .await
                .expect_err("chain mismatch"),
            OwnerBootstrapError::ChainMismatch
        );
        assert_eq!(rpc.snapshot_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn executor_code_hash_mismatch_fails_closed() {
        let context = context();
        let mut snapshot = complete_snapshot(&context, true);
        snapshot.runtime_code_hash = "c".repeat(64);
        assert_eq!(
            configured_preflight(&context, &MockRpc::new(snapshot))
                .await
                .expect_err("code hash"),
            OwnerBootstrapError::CodeHashMismatch
        );
    }

    #[tokio::test]
    async fn flash_provider_mismatch_fails_closed() {
        let context = context();
        let mut snapshot = complete_snapshot(&context, true);
        snapshot.flash_provider = Some(address("0x9999999999999999999999999999999999999999"));
        assert_eq!(
            configured_preflight(&context, &MockRpc::new(snapshot))
                .await
                .expect_err("flash provider"),
            OwnerBootstrapError::FlashProviderMismatch
        );
    }

    #[test]
    fn arbitrary_target_value_and_calldata_are_not_inputs() {
        let context = context();
        let actions = desired_configuration_actions(&context).expect("actions");
        assert!(!actions.is_empty());
        assert!(actions
            .iter()
            .all(|candidate| !candidate.calldata.is_empty()));
        assert_eq!(context.executor, address(EXECUTOR));
        assert!(OWNER_ENVIRONMENT_NAMES
            .iter()
            .all(|name| !matches!(*name, "TARGET" | "CALLDATA" | "VALUE" | "NONCE")));
    }

    #[test]
    fn gas_limit_cap_is_enforced() {
        let context = context();
        let mut quote = test_quote();
        quote.gas_limit = context.maximum_gas_limit + 1;
        assert_eq!(
            enforce_quote(&context, &quote),
            Err(OwnerBootstrapError::GasLimitExceeded)
        );
    }

    #[test]
    fn maximum_fee_cap_is_enforced() {
        let context = context();
        let mut quote = test_quote();
        quote.max_fee_per_gas = context.maximum_max_fee_per_gas + 1;
        assert_eq!(
            enforce_quote(&context, &quote),
            Err(OwnerBootstrapError::MaximumFeeExceeded)
        );
    }

    #[test]
    fn priority_fee_cap_is_enforced() {
        let context = context();
        let mut quote = test_quote();
        quote.max_priority_fee_per_gas = context.maximum_priority_fee_per_gas + 1;
        assert_eq!(
            enforce_quote(&context, &quote),
            Err(OwnerBootstrapError::PriorityFeeExceeded)
        );
    }

    #[tokio::test]
    async fn returned_hash_mismatch_stops_execution() {
        let context = context();
        let before = complete_snapshot(&context, false);
        let mut after = before.clone();
        after.paused = true;
        let mut rpc = MockRpc::new(before).with_post_submit(after);
        rpc.return_wrong_hash = true;
        assert_eq!(
            execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Pause)
                .await
                .expect_err("hash mismatch"),
            OwnerBootstrapError::TransactionHashMismatch
        );
    }

    #[tokio::test]
    async fn reverted_receipt_stops_execution() {
        let context = context();
        let before = complete_snapshot(&context, false);
        let mut after = before.clone();
        after.paused = true;
        let mut rpc = MockRpc::new(before).with_post_submit(after);
        rpc.receipt_status = 0;
        assert_eq!(
            execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Pause)
                .await
                .expect_err("revert"),
            OwnerBootstrapError::TransactionReverted
        );
    }

    #[tokio::test]
    async fn receipt_timeout_stops_execution() {
        let mut context = context();
        context.receipt_timeout = Duration::ZERO;
        let before = complete_snapshot(&context, false);
        let mut rpc = MockRpc::new(before);
        rpc.omit_receipt = true;
        assert_eq!(
            execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Pause)
                .await
                .expect_err("receipt timeout"),
            OwnerBootstrapError::ReceiptTimeout
        );
    }

    #[tokio::test]
    async fn unknown_submission_stops_execution() {
        let mut context = context();
        context.receipt_timeout = Duration::ZERO;
        let before = complete_snapshot(&context, false);
        let mut rpc = MockRpc::new(before);
        rpc.omit_receipt = true;
        rpc.known = false;
        assert_eq!(
            execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Pause)
                .await
                .expect_err("unknown submission"),
            OwnerBootstrapError::SubmissionUnknown
        );
    }

    #[tokio::test]
    async fn partial_configuration_resumes_only_missing_action() {
        let context = context();
        let mut before = complete_snapshot(&context, true);
        before.maximum_input_amount = 1;
        let after = complete_snapshot(&context, true);
        let rpc = MockRpc::new(before).with_post_submit(after);
        let evidence = execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Configure)
            .await
            .expect("configure");
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 1);
        let actions = evidence["actions"].as_array().expect("actions");
        assert_eq!(
            actions
                .iter()
                .filter(|item| item["status"] == "applied")
                .count(),
            1
        );
    }

    #[test]
    fn duplicate_factory_is_configured_once() {
        let context = context();
        let actions = desired_configuration_actions(&context).expect("actions");
        assert_eq!(
            actions
                .iter()
                .filter(|candidate| matches!(candidate.kind, OwnerActionKind::Factory(_)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn configure_never_unpauses() {
        let context = context();
        let actions = desired_configuration_actions(&context).expect("actions");
        assert!(actions
            .iter()
            .all(|candidate| candidate.kind != OwnerActionKind::Unpause));
        let rpc = MockRpc::new(complete_snapshot(&context, true));
        let evidence = execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Configure)
            .await
            .expect("idempotent configure");
        assert_eq!(evidence["final_state"]["paused"], true);
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn configured_preflight_requires_paused() {
        let context = context();
        assert_eq!(
            configured_preflight(&context, &MockRpc::new(complete_snapshot(&context, false)))
                .await
                .expect_err("unpaused"),
            OwnerBootstrapError::ConfiguredPreflightRequiresPaused
        );
    }

    #[tokio::test]
    async fn configured_signer_preflight_requires_paused_matching_owner_without_mutation() {
        let context = context();
        let signer = test_signer();
        let rpc = MockRpc::new(complete_snapshot(&context, true));
        let evidence = configured_signer_preflight(&context, &rpc, &signer)
            .await
            .expect("paused signer preflight");
        assert_eq!(evidence["status"], "ready-paused");
        assert_eq!(evidence["signer_matches_owner"], true);
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);

        let other = TransactionSigner::from_secret(&hex::encode([8_u8; 32]), ARBITRUM_ONE_CHAIN_ID)
            .expect("other signer");
        assert_eq!(
            configured_signer_preflight(&context, &rpc, &other)
                .await
                .expect_err("signer mismatch"),
            OwnerBootstrapError::SignerWalletOwnerMismatch
        );
        let unpaused = MockRpc::new(complete_snapshot(&context, false));
        assert_eq!(
            configured_signer_preflight(&context, &unpaused, &signer)
                .await
                .expect_err("paused contract required"),
            OwnerBootstrapError::ConfiguredPreflightRequiresPaused
        );
    }

    #[tokio::test]
    async fn runtime_preflight_accepts_exact_paused_or_unpaused_configuration_without_mutation() {
        let context = context();
        for paused in [true, false] {
            let rpc = MockRpc::new(complete_snapshot(&context, paused));
            let evidence = runtime_preflight(&context, &rpc)
                .await
                .expect("runtime preflight");
            assert_eq!(evidence["release_sha"], context.release_sha);
            assert_eq!(evidence["final_state"]["paused"], paused);
            assert_eq!(evidence["final_state"]["configuration_complete"], true);
            assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn live_preflight_requires_unpaused_and_matching_signer_without_mutation() {
        let context = context();
        let signer = test_signer();
        let paused = MockRpc::new(complete_snapshot(&context, true));
        assert_eq!(
            live_preflight(&context, &paused, &signer)
                .await
                .expect_err("paused"),
            OwnerBootstrapError::LivePreflightRequiresUnpaused
        );
        assert_eq!(paused.submit_calls.load(Ordering::SeqCst), 0);

        let rpc = MockRpc::new(complete_snapshot(&context, false));
        let evidence = live_preflight(&context, &rpc, &signer)
            .await
            .expect("live preflight");
        assert_eq!(evidence["command"], "live-preflight");
        assert_eq!(evidence["status"], "ready-unpaused");
        assert_eq!(evidence["final_state"]["paused"], false);
        assert_eq!(evidence["signer_matches_owner"], true);
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);

        let other = TransactionSigner::from_secret(&hex::encode([8_u8; 32]), ARBITRUM_ONE_CHAIN_ID)
            .expect("other signer");
        assert_eq!(
            live_preflight(&context, &rpc, &other)
                .await
                .expect_err("signer mismatch"),
            OwnerBootstrapError::SignerWalletOwnerMismatch
        );
    }

    #[tokio::test]
    async fn unpause_is_blocked_until_configuration_is_complete() {
        let context = context();
        let mut snapshot = complete_snapshot(&context, true);
        snapshot.assets_approved[1] = false;
        let rpc = MockRpc::new(snapshot);
        assert_eq!(
            execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Unpause)
                .await
                .expect_err("incomplete"),
            OwnerBootstrapError::ConfigurationIncomplete
        );
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unpause_is_the_final_owner_mutation() {
        let context = context();
        let before = complete_snapshot(&context, true);
        let after = complete_snapshot(&context, false);
        let rpc = MockRpc::new(before).with_post_submit(after);
        let evidence = execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Unpause)
            .await
            .expect("unpause");
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(evidence["final_state"]["paused"], false);
    }

    #[tokio::test]
    async fn pause_is_idempotent() {
        let context = context();
        let rpc = MockRpc::new(complete_snapshot(&context, true));
        let evidence = execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Pause)
            .await
            .expect("pause");
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);
        assert_eq!(evidence["actions"][0]["status"], "skipped-already-applied");
    }

    #[tokio::test]
    async fn owner_plan_is_read_only() {
        let context = context();
        let rpc = MockRpc::new(complete_snapshot(&context, true));
        let plan = owner_plan(&context, &rpc).await.expect("plan");
        assert_eq!(rpc.quote_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rpc.nonce_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rpc.submit_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            plan["transactions"].as_array().expect("transactions").len(),
            1
        );
    }

    #[tokio::test]
    async fn evidence_never_contains_secrets_raw_transactions_or_calldata() {
        let context = context();
        let rpc = MockRpc::new(complete_snapshot(&context, true));
        let evidence = execute_mutation(&context, &rpc, &test_signer(), OwnerMutation::Configure)
            .await
            .expect("evidence");
        let output = serde_json::to_string(&evidence).expect("serialize");
        assert!(!output.contains(&hex::encode([7_u8; 32])));
        assert!(!output.contains("\"raw\""));
        assert!(!output.contains("\"data\""));
        assert!(!output.contains("rpc.example.invalid"));
    }

    #[test]
    fn canonical_router_set_rejects_duplicates() {
        let mut values = full_values();
        values.insert(
            "ENGINE_ROUTER_ADDRESSES".to_string(),
            format!("{ROUTER},{ROUTER}"),
        );
        assert_eq!(
            OwnerBootstrapContext::from_values(&values).expect_err("duplicate routers"),
            OwnerBootstrapError::InvalidRouterSet
        );
    }

    #[test]
    fn owner_context_includes_reviewed_aave_assets_and_all_verified_unwind_pools() {
        let context = OwnerBootstrapContext::from_values(&full_values()).expect("owner context");
        assert_eq!(context.assets.len(), 3);
        assert!(context
            .assets
            .contains(&address("0x82af49447d8a07e3bd95bd0d56f35241523fbab1")));
        assert!(context
            .assets
            .contains(&address("0xaf88d065e77c8cc2239327c5edb3a432268e5831")));
        assert!(context
            .assets
            .contains(&address("0xff970a61a04b1ca14834a43f5de4533ebddb5cc8")));
        assert_eq!(context.legs.len(), 4);
        let fee_100 = context
            .legs
            .iter()
            .find(|leg| leg.fee == 100)
            .expect("reviewed fee-100 Aave unwind");
        assert_eq!(
            fee_100.pool,
            address("0x6f38e884725a116c9c7fbf208e79fe8828a2595f")
        );
        assert!(!fee_100.zero_for_one);
        let usdc_e = context
            .legs
            .iter()
            .find(|leg| leg.pool == address("0xc31e54c7a869b9fcbecc14363cf510d1c41fa443"))
            .expect("reviewed WETH/USDC.e fee-500 Aave unwind");
        assert_eq!(usdc_e.fee, 500);
        assert!(usdc_e.zero_for_one);
    }

    #[test]
    fn maximum_input_cannot_exceed_embedded_policy() {
        let mut values = full_values();
        values.insert(
            "LIVE_EXECUTOR_MAX_INPUT_AMOUNT".to_string(),
            "10000000000000001".to_string(),
        );
        assert_eq!(
            OwnerBootstrapContext::from_values(&values).expect_err("policy maximum"),
            OwnerBootstrapError::MaximumInputExceedsPolicy
        );
    }

    #[test]
    fn owner_commands_do_not_accept_database_configuration() {
        assert!(!OWNER_ENVIRONMENT_NAMES.contains(&"POSTGRES_DSN"));
        assert!(!OWNER_ENVIRONMENT_NAMES.contains(&"DATABASE_URL"));
    }
}
