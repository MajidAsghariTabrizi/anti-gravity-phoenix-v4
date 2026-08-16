//! PhoenixExecutor-specific rotation guardrails.
//!
//! This module deliberately contains no generic deployment primitive and no
//! RPC mutation.  It validates the immutable inputs that a separately
//! authorized operator must present before using the signer CREATE API.

use crate::model::{CanonicalAddress, TransactionHash};
use crate::owner_bootstrap::submit_rotation_owner_call;
use crate::rpc::{ExecutionRpc, HttpExecutionRpc, PhoenixExecutorMapping, TransactionReceipt};
use crate::signer::{ContractCreationDraft, TransactionSigner};
use alloy_primitives::Address;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

pub const ROTATION_SCHEMA: &str = "phoenix.executor-rotation.v1";
pub const ARBITRUM_CHAIN_ID: u64 = 42_161;
pub const REVIEWED_MAX_INPUT: u128 = 10_000_000_000_000_000;
pub const OLD_EXECUTOR: &str = "0x634f62d7cd28d1c4dcf503d901b88d666c2626ad";
pub const REVIEWED_OWNER: &str = "0x9f30c00b68f7c0edb4b4117b9f04e0ca2eb2c17a";
pub const REVIEWED_FLASH_PROVIDER: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
pub const REVIEWED_ATLAS: &str = "0x8ad1ae9d97c79aa68a0a151e83ff3942f68f86c1";
pub const REVIEWED_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
pub const REVIEWED_NATIVE_USDC: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
pub const REVIEWED_USDC_E: &str = "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8";
pub const REVIEWED_ROUTER_1: &str = "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45";
pub const REVIEWED_ROUTER_2: &str = "0xa51afafe0263b40edaef0df8781ea9aa03e381a3";
pub const REVIEWED_ROUTER_3: &str = "0xe592427a0aece92de3edee1f18e0157c05861564";
pub const REVIEWED_FACTORY: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
pub const REVIEWED_OLD_RUNTIME_SHA256: &str =
    "99a485d5a711180b4455028620bf4d5374558f85ef185ba00a51481c7c239c58";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RotationPlan {
    pub schema: String,
    pub chain_id: u64,
    pub source_sha: String,
    pub base_release_sha: String,
    pub old_executor: String,
    pub old_runtime_sha256: String,
    pub expected_new_runtime_sha256: String,
    pub creation_bytecode_sha256: String,
    pub config_digest: String,
    pub owner: String,
    pub flash_provider: String,
    pub atlas: String,
    pub weth: String,
    pub maximum_input_amount: u128,
    pub searcher: String,
    pub assets: Vec<String>,
    pub routers: Vec<String>,
    pub factory: String,
    pub pools: Vec<RotationPool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RotationPool {
    pub address: String,
    pub fee: u32,
    pub token0: String,
    pub token1: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RotationProvenance {
    pub schema: String,
    pub tooling_source_sha: String,
    pub base_release_sha: String,
    pub chain_id: u64,
    pub old_executor: String,
    pub new_executor: String,
    pub old_runtime_sha256: String,
    pub new_runtime_sha256: String,
    pub creation_bytecode_sha256: String,
    pub config_digest: String,
    pub deployment_tx_hash: String,
    pub deployment_block_number: u64,
    pub cutover_tx_hashes: Vec<String>,
    pub rollback_tx_hash: Option<String>,
    pub config_tx_hashes: Vec<String>,
    pub config_verified: bool,
    pub pre_cutover_spl_absent: bool,
    pub old_bound_work_drained: bool,
    pub fenced_old_requests: u64,
    pub cutover_started: bool,
    pub cutover_completed: bool,
    pub identity_consumers: Vec<String>,
    pub identity_consumers_verified: bool,
    pub rollback_used: bool,
    pub rollback_completed: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RotationError {
    #[error("rotation plan is invalid")]
    InvalidPlan,
    #[error("creation bytecode does not match the immutable plan")]
    BytecodeMismatch,
    #[error("creation bytecode could not be read")]
    BytecodeUnreadable,
    #[error("rotation lifecycle guard rejected the operation")]
    LifecycleRejected,
    #[error("rotation provenance is unavailable or invalid")]
    InvalidProvenance,
}

#[derive(Debug, Error)]
pub enum RotationTransportError {
    #[error("rotation precondition failed")]
    Precondition,
    #[error("rotation RPC failed")]
    Rpc,
    #[error("rotation transaction failed")]
    Transaction,
    #[error("rotation host operation failed")]
    Host,
}

pub struct AuthenticatedPhoenixExecutorRotationTransport {
    rpc: HttpExecutionRpc,
    signer: TransactionSigner,
    creation_bytecode: Vec<u8>,
    new_executor: Option<CanonicalAddress>,
    deployment_tx: Option<TransactionHash>,
    plan_path: PathBuf,
    host_script: PathBuf,
    provenance_path: PathBuf,
    provenance: Option<RotationProvenance>,
}

impl AuthenticatedPhoenixExecutorRotationTransport {
    pub fn new(
        plan: &RotationPlan,
        rpc: HttpExecutionRpc,
        signer: TransactionSigner,
        creation_bytecode: Vec<u8>,
        plan_path: PathBuf,
        host_script: PathBuf,
        provenance_path: PathBuf,
    ) -> Result<Self, RotationError> {
        validate_plan(plan)?;
        if !signer_matches_owner(signer.address(), &plan.owner) {
            return Err(RotationError::InvalidPlan);
        }
        if creation_bytecode.is_empty()
            || hex::encode(Sha256::digest(&creation_bytecode))
                != normalize(&plan.creation_bytecode_sha256)
        {
            return Err(RotationError::BytecodeMismatch);
        }
        let provenance = load_provenance_if_present(&provenance_path, plan)?;
        let new_executor = provenance
            .as_ref()
            .map(|value| CanonicalAddress::parse(&value.new_executor))
            .transpose()
            .map_err(|_| RotationError::InvalidProvenance)?;
        let deployment_tx = provenance
            .as_ref()
            .map(|value| TransactionHash::parse(&value.deployment_tx_hash))
            .transpose()
            .map_err(|_| RotationError::InvalidProvenance)?;
        Ok(Self {
            rpc,
            signer,
            creation_bytecode,
            new_executor,
            deployment_tx,
            plan_path,
            host_script,
            provenance_path,
            provenance,
        })
    }

    pub fn new_executor(&self) -> Option<CanonicalAddress> {
        self.new_executor
    }

    pub fn provenance(&self) -> Option<&RotationProvenance> {
        self.provenance.as_ref()
    }

    fn persist(&self) -> Result<(), RotationTransportError> {
        let value = self
            .provenance
            .as_ref()
            .ok_or(RotationTransportError::Precondition)?;
        persist_provenance(&self.provenance_path, value).map_err(|_| RotationTransportError::Host)
    }

    fn reload(&mut self) -> Result<(), RotationTransportError> {
        let bytes = fs::read(&self.provenance_path).map_err(|_| RotationTransportError::Host)?;
        self.provenance =
            Some(serde_json::from_slice(&bytes).map_err(|_| RotationTransportError::Host)?);
        Ok(())
    }

    async fn wait_receipt(
        &self,
        tx_hash: TransactionHash,
    ) -> Result<TransactionReceipt, RotationTransportError> {
        for _ in 0..60 {
            if let Some(receipt) = self
                .rpc
                .transaction_receipt(tx_hash)
                .await
                .map_err(|_| RotationTransportError::Rpc)?
            {
                return if receipt.status == 1 && receipt.transaction_hash == tx_hash {
                    Ok(receipt)
                } else {
                    Err(RotationTransportError::Transaction)
                };
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(RotationTransportError::Transaction)
    }

    async fn owner_call(
        &mut self,
        executor: CanonicalAddress,
        calldata: Vec<u8>,
    ) -> Result<(), RotationTransportError> {
        let receipt = submit_rotation_owner_call(&self.rpc, &self.signer, executor, calldata)
            .await
            .map_err(|_| RotationTransportError::Transaction)?;
        let returned = receipt.transaction_hash;
        let value = self
            .provenance
            .as_mut()
            .ok_or(RotationTransportError::Precondition)?;
        value.config_tx_hashes.push(returned.to_string());
        self.persist()?;
        Ok(())
    }

    async fn run_host_mode(&self, mode: &str) -> Result<bool, RotationTransportError> {
        let status = tokio::process::Command::new(&self.host_script)
            .arg(mode)
            .arg(&self.plan_path)
            .arg(&self.provenance_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map_err(|_| RotationTransportError::Host)?;
        Ok(status.success())
    }

    async fn verify_exact_config(
        &self,
        executor: CanonicalAddress,
        plan: &RotationPlan,
        expected_paused: bool,
    ) -> Result<(), RotationTransportError> {
        let owner = address(&plan.owner)?;
        let core = self
            .rpc
            .phoenix_executor_core_snapshot(executor)
            .await
            .map_err(|_| RotationTransportError::Rpc)?;
        if core.owner != Some(owner)
            || core.pending_owner.is_some()
            || core.flash_provider != Some(address(&plan.flash_provider)?)
            || core.atlas != Some(address(&plan.atlas)?)
            || core.weth != Some(address(&plan.weth)?)
            || core.paused != expected_paused
            || core.maximum_input_amount != plan.maximum_input_amount
            || !self
                .mapping(executor, PhoenixExecutorMapping::Searcher, &plan.searcher)
                .await?
        {
            return Err(RotationTransportError::Precondition);
        }
        for value in &plan.assets {
            if !self
                .mapping(executor, PhoenixExecutorMapping::Asset, value)
                .await?
            {
                return Err(RotationTransportError::Precondition);
            }
        }
        for value in &plan.routers {
            if !self
                .mapping(executor, PhoenixExecutorMapping::Router, value)
                .await?
            {
                return Err(RotationTransportError::Precondition);
            }
        }
        if !self
            .mapping(executor, PhoenixExecutorMapping::Factory, &plan.factory)
            .await?
        {
            return Err(RotationTransportError::Precondition);
        }
        for pool in &plan.pools {
            if !self
                .rpc
                .phoenix_executor_pool_matches(
                    executor,
                    address(&pool.address)?,
                    address(&plan.factory)?,
                    address(&pool.token0)?,
                    address(&pool.token1)?,
                    pool.fee,
                )
                .await
                .map_err(|_| RotationTransportError::Rpc)?
            {
                return Err(RotationTransportError::Precondition);
            }
        }
        Ok(())
    }

    async fn mapping(
        &self,
        executor: CanonicalAddress,
        mapping: PhoenixExecutorMapping,
        value: &str,
    ) -> Result<bool, RotationTransportError> {
        self.rpc
            .phoenix_executor_mapping(executor, mapping, address(value)?)
            .await
            .map_err(|_| RotationTransportError::Rpc)
    }
}

fn address(value: &str) -> Result<CanonicalAddress, RotationTransportError> {
    CanonicalAddress::parse(value).map_err(|_| RotationTransportError::Precondition)
}

fn call_data(name: &str, types: &[ethabi::ParamType], tokens: &[ethabi::Token]) -> Vec<u8> {
    let mut data = ethabi::short_signature(name, types).to_vec();
    data.extend(ethabi::encode(tokens));
    data
}

fn address_token(value: &str) -> Result<ethabi::Token, RotationTransportError> {
    let parsed = address(value)?;
    Ok(ethabi::Token::Address(primitive_types::H160::from_slice(
        parsed.as_bytes(),
    )))
}

/// Narrow host-operation boundary.  The implementation is intentionally
/// injected: production transport, compose/context orchestration, and signer
/// material stay outside this state machine and cannot be replaced by a
/// generic deployer.
#[async_trait]
pub trait RotationBackend: Send {
    type Error;

    async fn deploy(&mut self) -> Result<(), Self::Error>;
    async fn mirror(&mut self) -> Result<(), Self::Error>;
    async fn verify(&mut self) -> Result<(), Self::Error>;
    async fn spl_gate(&mut self) -> Result<bool, Self::Error>;
    async fn drain_old_bound_work(&mut self) -> Result<bool, Self::Error>;
    async fn cutover_live_identity(&mut self) -> Result<(), Self::Error>;
    async fn reconcile(&mut self) -> Result<bool, Self::Error>;
    async fn rollback_identity_once(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationMode {
    Validate,
    Prepare,
    Execute,
    Rollback,
}

pub struct RotationOperator<B> {
    backend: B,
    rollback_used: bool,
}

/// Production-facing PhoenixExecutor adapter boundary.  The transport is
/// intentionally PhoenixExecutor-specific: it owns the authenticated RPC,
/// protected artifact, canonical compose/context and DB drain checks.  No
/// arbitrary destination, bytecode, authority, or economics parameters are
/// exposed here.
#[async_trait]
pub trait PhoenixExecutorRotationTransport: Send {
    type Error;
    async fn deploy_phoenix_executor(&mut self, plan: &RotationPlan) -> Result<(), Self::Error>;
    async fn mirror_phoenix_config(&mut self, plan: &RotationPlan) -> Result<(), Self::Error>;
    async fn verify_phoenix_pair(&mut self, plan: &RotationPlan) -> Result<(), Self::Error>;
    async fn prove_spl_absent(&mut self, plan: &RotationPlan) -> Result<bool, Self::Error>;
    async fn prove_old_bound_work_drained(&mut self) -> Result<bool, Self::Error>;
    async fn cutover_phoenix_identity(&mut self, plan: &RotationPlan) -> Result<(), Self::Error>;
    async fn reconcile_phoenix_identity(
        &mut self,
        plan: &RotationPlan,
    ) -> Result<bool, Self::Error>;
    async fn rollback_phoenix_identity_once(
        &mut self,
        plan: &RotationPlan,
    ) -> Result<(), Self::Error>;
}

#[async_trait]
impl PhoenixExecutorRotationTransport for AuthenticatedPhoenixExecutorRotationTransport {
    type Error = RotationTransportError;

    async fn deploy_phoenix_executor(&mut self, plan: &RotationPlan) -> Result<(), Self::Error> {
        if let Some(existing) = self.new_executor {
            if self
                .rpc
                .runtime_code_hash(existing)
                .await
                .map_err(|_| RotationTransportError::Rpc)?
                != plan.expected_new_runtime_sha256
            {
                return Err(RotationTransportError::Precondition);
            }
            return Ok(());
        }
        if self
            .rpc
            .chain_id()
            .await
            .map_err(|_| RotationTransportError::Rpc)?
            != ARBITRUM_CHAIN_ID
            || self.signer.address() != address(&plan.owner)?
            || self
                .rpc
                .runtime_code_hash(address(&plan.old_executor)?)
                .await
                .map_err(|_| RotationTransportError::Rpc)?
                != plan.old_runtime_sha256
        {
            return Err(RotationTransportError::Precondition);
        }
        self.verify_exact_config(address(&plan.old_executor)?, plan, false)
            .await?;
        let mut init_code = self.creation_bytecode.clone();
        init_code.extend(ethabi::encode(&[
            address_token(&plan.owner)?,
            address_token(&plan.flash_provider)?,
            address_token(&plan.atlas)?,
            address_token(&plan.weth)?,
        ]));
        let quote = self
            .rpc
            .quote_contract_creation(self.signer.address(), &init_code)
            .await
            .map_err(|_| RotationTransportError::Rpc)?;
        if quote.gas_limit > 15_000_000 || quote.max_fee_per_gas > 10_000_000_000 {
            return Err(RotationTransportError::Precondition);
        }
        let nonce = self
            .rpc
            .pending_nonce(self.signer.address())
            .await
            .map_err(|_| RotationTransportError::Rpc)?;
        let signed = self
            .signer
            .sign_contract_creation(ContractCreationDraft {
                chain_id: ARBITRUM_CHAIN_ID,
                nonce,
                gas_limit: quote.gas_limit,
                max_fee_per_gas: quote.max_fee_per_gas,
                max_priority_fee_per_gas: quote.max_priority_fee_per_gas,
                creation_bytecode: init_code,
            })
            .map_err(|_| RotationTransportError::Transaction)?;
        let returned = self
            .rpc
            .send_raw_transaction(signed.raw_bytes())
            .await
            .map_err(|_| RotationTransportError::Rpc)?;
        if returned != signed.tx_hash() {
            return Err(RotationTransportError::Transaction);
        }
        let receipt = self.wait_receipt(returned).await?;
        let deployed = Address::from_slice(self.signer.address().as_bytes()).create(nonce);
        let deployed = CanonicalAddress::parse(&deployed.to_string().to_ascii_lowercase())
            .map_err(|_| RotationTransportError::Transaction)?;
        if receipt.contract_address != Some(deployed)
            || self
                .rpc
                .runtime_code_hash(deployed)
                .await
                .map_err(|_| RotationTransportError::Rpc)?
                != plan.expected_new_runtime_sha256
        {
            return Err(RotationTransportError::Precondition);
        }
        let core = self
            .rpc
            .phoenix_executor_core_snapshot(deployed)
            .await
            .map_err(|_| RotationTransportError::Rpc)?;
        if core.owner != Some(address(&plan.owner)?)
            || core.pending_owner.is_some()
            || core.flash_provider != Some(address(&plan.flash_provider)?)
            || core.atlas != Some(address(&plan.atlas)?)
            || core.weth != Some(address(&plan.weth)?)
            || !core.paused
            || core.maximum_input_amount != 0
        {
            return Err(RotationTransportError::Precondition);
        }
        self.new_executor = Some(deployed);
        self.deployment_tx = Some(returned);
        self.provenance = Some(
            provenance_with_block(plan, deployed, &returned.to_string(), receipt.block_number)
                .map_err(|_| RotationTransportError::Precondition)?,
        );
        self.persist()?;
        Ok(())
    }

    async fn mirror_phoenix_config(&mut self, plan: &RotationPlan) -> Result<(), Self::Error> {
        let executor = self
            .new_executor
            .ok_or(RotationTransportError::Precondition)?;
        if self
            .provenance
            .as_ref()
            .is_some_and(|value| value.config_verified)
        {
            return self.verify_exact_config(executor, plan, false).await;
        }
        self.owner_call(
            executor,
            call_data(
                "setSearcher",
                &[ethabi::ParamType::Address, ethabi::ParamType::Bool],
                &[address_token(&plan.searcher)?, ethabi::Token::Bool(true)],
            ),
        )
        .await?;
        if !self
            .mapping(executor, PhoenixExecutorMapping::Searcher, &plan.searcher)
            .await?
        {
            return Err(RotationTransportError::Precondition);
        }
        for value in &plan.assets {
            self.owner_call(
                executor,
                call_data(
                    "setAsset",
                    &[ethabi::ParamType::Address, ethabi::ParamType::Bool],
                    &[address_token(value)?, ethabi::Token::Bool(true)],
                ),
            )
            .await?;
            if !self
                .mapping(executor, PhoenixExecutorMapping::Asset, value)
                .await?
            {
                return Err(RotationTransportError::Precondition);
            }
        }
        for value in &plan.routers {
            self.owner_call(
                executor,
                call_data(
                    "setRouter",
                    &[ethabi::ParamType::Address, ethabi::ParamType::Bool],
                    &[address_token(value)?, ethabi::Token::Bool(true)],
                ),
            )
            .await?;
            if !self
                .mapping(executor, PhoenixExecutorMapping::Router, value)
                .await?
            {
                return Err(RotationTransportError::Precondition);
            }
        }
        self.owner_call(
            executor,
            call_data(
                "setFactory",
                &[ethabi::ParamType::Address, ethabi::ParamType::Bool],
                &[address_token(&plan.factory)?, ethabi::Token::Bool(true)],
            ),
        )
        .await?;
        if !self
            .mapping(executor, PhoenixExecutorMapping::Factory, &plan.factory)
            .await?
        {
            return Err(RotationTransportError::Precondition);
        }
        for pool in &plan.pools {
            self.owner_call(
                executor,
                call_data(
                    "approvePool",
                    &[
                        ethabi::ParamType::Address,
                        ethabi::ParamType::Address,
                        ethabi::ParamType::Address,
                        ethabi::ParamType::Address,
                        ethabi::ParamType::Uint(24),
                        ethabi::ParamType::Bool,
                    ],
                    &[
                        address_token(&pool.address)?,
                        address_token(&plan.factory)?,
                        address_token(&pool.token0)?,
                        address_token(&pool.token1)?,
                        ethabi::Token::Uint(pool.fee.into()),
                        ethabi::Token::Bool(true),
                    ],
                ),
            )
            .await?;
            if !self
                .rpc
                .phoenix_executor_pool_matches(
                    executor,
                    address(&pool.address)?,
                    address(&plan.factory)?,
                    address(&pool.token0)?,
                    address(&pool.token1)?,
                    pool.fee,
                )
                .await
                .map_err(|_| RotationTransportError::Rpc)?
            {
                return Err(RotationTransportError::Precondition);
            }
        }
        self.owner_call(
            executor,
            call_data(
                "setMaximumInputAmount",
                &[ethabi::ParamType::Uint(256)],
                &[ethabi::Token::Uint(plan.maximum_input_amount.into())],
            ),
        )
        .await?;
        let core = self
            .rpc
            .phoenix_executor_core_snapshot(executor)
            .await
            .map_err(|_| RotationTransportError::Rpc)?;
        if !core.paused || core.maximum_input_amount != plan.maximum_input_amount {
            return Err(RotationTransportError::Precondition);
        }
        self.verify_exact_config(executor, plan, true).await?;
        self.owner_call(
            executor,
            call_data(
                "setPaused",
                &[ethabi::ParamType::Bool],
                &[ethabi::Token::Bool(false)],
            ),
        )
        .await?;
        self.verify_exact_config(executor, plan, false).await?;
        let value = self
            .provenance
            .as_mut()
            .ok_or(RotationTransportError::Precondition)?;
        value.config_verified = true;
        self.persist()
    }

    async fn verify_phoenix_pair(&mut self, plan: &RotationPlan) -> Result<(), Self::Error> {
        self.verify_exact_config(address(&plan.old_executor)?, plan, false)
            .await?;
        let new_executor = self
            .new_executor
            .ok_or(RotationTransportError::Precondition)?;
        if self
            .rpc
            .runtime_code_hash(new_executor)
            .await
            .map_err(|_| RotationTransportError::Rpc)?
            != plan.expected_new_runtime_sha256
        {
            return Err(RotationTransportError::Precondition);
        }
        self.verify_exact_config(new_executor, plan, false).await
    }

    async fn prove_spl_absent(&mut self, _plan: &RotationPlan) -> Result<bool, Self::Error> {
        let success = self.run_host_mode("spl-gate").await?;
        if success {
            self.reload()?;
        }
        Ok(success
            && self
                .provenance
                .as_ref()
                .is_some_and(|value| value.pre_cutover_spl_absent))
    }
    async fn prove_old_bound_work_drained(&mut self) -> Result<bool, Self::Error> {
        let success = self.run_host_mode("drain").await?;
        if success {
            self.reload()?;
        }
        Ok(success
            && self
                .provenance
                .as_ref()
                .is_some_and(|value| value.old_bound_work_drained))
    }
    async fn cutover_phoenix_identity(&mut self, _plan: &RotationPlan) -> Result<(), Self::Error> {
        if self.run_host_mode("cutover").await? {
            self.reload()?;
            Ok(())
        } else {
            Err(RotationTransportError::Host)
        }
    }
    async fn reconcile_phoenix_identity(
        &mut self,
        _plan: &RotationPlan,
    ) -> Result<bool, Self::Error> {
        let success = self.run_host_mode("reconcile").await?;
        if success {
            self.reload()?;
        }
        Ok(success
            && self
                .provenance
                .as_ref()
                .is_some_and(|value| value.identity_consumers_verified))
    }
    async fn rollback_phoenix_identity_once(
        &mut self,
        _plan: &RotationPlan,
    ) -> Result<(), Self::Error> {
        if self.run_host_mode("rollback").await? {
            self.reload()?;
            Ok(())
        } else {
            Err(RotationTransportError::Host)
        }
    }
}

pub struct ProductionPhoenixExecutorRotationBackend<T> {
    plan: RotationPlan,
    transport: T,
}

impl<T> ProductionPhoenixExecutorRotationBackend<T> {
    pub fn new(plan: RotationPlan, transport: T) -> Result<Self, RotationError> {
        validate_plan(&plan)?;
        Ok(Self { plan, transport })
    }

    pub fn into_transport(self) -> T {
        self.transport
    }
}

#[async_trait]
impl<T: PhoenixExecutorRotationTransport> RotationBackend
    for ProductionPhoenixExecutorRotationBackend<T>
{
    type Error = T::Error;

    async fn deploy(&mut self) -> Result<(), Self::Error> {
        self.transport.deploy_phoenix_executor(&self.plan).await
    }
    async fn mirror(&mut self) -> Result<(), Self::Error> {
        self.transport.mirror_phoenix_config(&self.plan).await
    }
    async fn verify(&mut self) -> Result<(), Self::Error> {
        self.transport.verify_phoenix_pair(&self.plan).await
    }
    async fn spl_gate(&mut self) -> Result<bool, Self::Error> {
        self.transport.prove_spl_absent(&self.plan).await
    }
    async fn drain_old_bound_work(&mut self) -> Result<bool, Self::Error> {
        self.transport.prove_old_bound_work_drained().await
    }
    async fn cutover_live_identity(&mut self) -> Result<(), Self::Error> {
        self.transport.cutover_phoenix_identity(&self.plan).await
    }
    async fn reconcile(&mut self) -> Result<bool, Self::Error> {
        self.transport.reconcile_phoenix_identity(&self.plan).await
    }
    async fn rollback_identity_once(&mut self) -> Result<(), Self::Error> {
        self.transport
            .rollback_phoenix_identity_once(&self.plan)
            .await
    }
}

impl<B> RotationOperator<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            rollback_used: false,
        }
    }

    pub fn into_backend(self) -> B {
        self.backend
    }
}

impl<B: RotationBackend> RotationOperator<B> {
    pub async fn execute(&mut self) -> Result<(), RotationError> {
        self.backend
            .deploy()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .mirror()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .verify()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?;
        if !self
            .backend
            .spl_gate()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            return Err(RotationError::LifecycleRejected);
        }
        if !self
            .backend
            .drain_old_bound_work()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            return Err(RotationError::LifecycleRejected);
        }
        if self.backend.cutover_live_identity().await.is_err() {
            let _ = self.rollback_once().await;
            return Err(RotationError::LifecycleRejected);
        }
        if !self
            .backend
            .reconcile()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            self.rollback_once().await?;
            return Err(RotationError::LifecycleRejected);
        }
        Ok(())
    }

    pub async fn rollback_once(&mut self) -> Result<(), RotationError> {
        if self.rollback_used {
            return Err(RotationError::LifecycleRejected);
        }
        self.rollback_used = true;
        self.backend
            .rollback_identity_once()
            .await
            .map_err(|_| RotationError::LifecycleRejected)
    }

    pub async fn prepare(&mut self) -> Result<(), RotationError> {
        self.backend
            .deploy()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .mirror()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .verify()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?;
        if !self
            .backend
            .spl_gate()
            .await
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            return Err(RotationError::LifecycleRejected);
        }
        Ok(())
    }
}

pub fn validate_plan(plan: &RotationPlan) -> Result<(), RotationError> {
    if plan.schema != ROTATION_SCHEMA
        || plan.chain_id != ARBITRUM_CHAIN_ID
        || !commit_sha(&plan.source_sha)
        || !commit_sha(&plan.base_release_sha)
        || !sha256_hex(&plan.old_runtime_sha256)
        || !sha256_hex(&plan.expected_new_runtime_sha256)
        || !sha256_hex(&plan.creation_bytecode_sha256)
        || !sha256_hex(&plan.config_digest)
        || normalize(&plan.old_executor) != normalize(OLD_EXECUTOR)
        || normalize(&plan.owner) != normalize(REVIEWED_OWNER)
        || normalize(&plan.flash_provider) != normalize(REVIEWED_FLASH_PROVIDER)
        || normalize(&plan.atlas) != normalize(REVIEWED_ATLAS)
        || normalize(&plan.weth) != normalize(REVIEWED_WETH)
        || normalize(&plan.old_runtime_sha256) != REVIEWED_OLD_RUNTIME_SHA256
        || plan.maximum_input_amount != REVIEWED_MAX_INPUT
        || !valid_address(&plan.searcher)
        || plan.assets != reviewed_assets()
        || plan.routers != reviewed_routers()
        || normalize(&plan.factory) != normalize(REVIEWED_FACTORY)
        || plan.pools != reviewed_pools()
        || plan.config_digest != canonical_config_digest()
    {
        return Err(RotationError::InvalidPlan);
    }
    Ok(())
}

pub fn validate_creation_bytecode(
    plan: &RotationPlan,
    bytecode_path: impl AsRef<Path>,
) -> Result<Vec<u8>, RotationError> {
    validate_plan(plan)?;
    let bytes = fs::read(bytecode_path).map_err(|_| RotationError::BytecodeUnreadable)?;
    if bytes.is_empty()
        || hex::encode(Sha256::digest(&bytes)) != normalize(&plan.creation_bytecode_sha256)
    {
        return Err(RotationError::BytecodeMismatch);
    }
    Ok(bytes)
}

pub fn provenance(
    plan: &RotationPlan,
    new_executor: CanonicalAddress,
    deployment_tx_hash: &str,
) -> Result<RotationProvenance, RotationError> {
    provenance_with_block(plan, new_executor, deployment_tx_hash, 0)
}

pub fn provenance_with_block(
    plan: &RotationPlan,
    new_executor: CanonicalAddress,
    deployment_tx_hash: &str,
    deployment_block_number: u64,
) -> Result<RotationProvenance, RotationError> {
    validate_plan(plan)?;
    if !tx_hash(deployment_tx_hash) {
        return Err(RotationError::InvalidPlan);
    }
    Ok(RotationProvenance {
        schema: ROTATION_SCHEMA.to_string(),
        tooling_source_sha: normalize(&plan.source_sha),
        base_release_sha: normalize(&plan.base_release_sha),
        chain_id: plan.chain_id,
        old_executor: CanonicalAddress::parse(&plan.old_executor)
            .map_err(|_| RotationError::InvalidPlan)?
            .to_string(),
        new_executor: new_executor.to_string(),
        old_runtime_sha256: normalize(&plan.old_runtime_sha256),
        new_runtime_sha256: normalize(&plan.expected_new_runtime_sha256),
        creation_bytecode_sha256: normalize(&plan.creation_bytecode_sha256),
        config_digest: normalize(&plan.config_digest),
        deployment_tx_hash: deployment_tx_hash.to_ascii_lowercase(),
        deployment_block_number,
        cutover_tx_hashes: Vec::new(),
        rollback_tx_hash: None,
        config_tx_hashes: Vec::new(),
        config_verified: false,
        pre_cutover_spl_absent: false,
        old_bound_work_drained: false,
        fenced_old_requests: 0,
        cutover_completed: false,
        cutover_started: false,
        identity_consumers: Vec::new(),
        identity_consumers_verified: false,
        rollback_used: false,
        rollback_completed: false,
    })
}

pub fn validate_provenance(
    value: &RotationProvenance,
    plan: &RotationPlan,
) -> Result<(), RotationError> {
    validate_plan(plan)?;
    if value.schema != ROTATION_SCHEMA
        || normalize(&value.tooling_source_sha) != normalize(&plan.source_sha)
        || normalize(&value.base_release_sha) != normalize(&plan.base_release_sha)
        || value.chain_id != plan.chain_id
        || normalize(&value.old_executor) != normalize(&plan.old_executor)
        || normalize(&value.old_runtime_sha256) != normalize(&plan.old_runtime_sha256)
        || normalize(&value.new_runtime_sha256) != normalize(&plan.expected_new_runtime_sha256)
        || normalize(&value.creation_bytecode_sha256) != normalize(&plan.creation_bytecode_sha256)
        || normalize(&value.config_digest) != normalize(&plan.config_digest)
        || !valid_address(&value.new_executor)
        || normalize(&value.new_executor) == normalize(&value.old_executor)
        || !tx_hash(&value.deployment_tx_hash)
        || value.config_tx_hashes.len() > 32
        || value.cutover_tx_hashes.len() > 16
        || value.identity_consumers.len() > 8
        || value.identity_consumers.iter().any(|name| {
            !matches!(
                name.as_str(),
                "atlas-observer" | "phoenix-engine" | "economic-supervisor" | "live-executor"
            )
        })
        || (value.cutover_started
            && value.identity_consumers
                != vec![
                    "atlas-observer".to_string(),
                    "economic-supervisor".to_string(),
                    "live-executor".to_string(),
                    "phoenix-engine".to_string(),
                ])
        || value.config_tx_hashes.iter().any(|hash| !tx_hash(hash))
        || value.cutover_tx_hashes.iter().any(|hash| !tx_hash(hash))
        || value
            .rollback_tx_hash
            .as_deref()
            .is_some_and(|hash| !tx_hash(hash))
        || (value.rollback_completed && !value.rollback_used)
        || (value.rollback_used && !value.cutover_started)
        || (value.cutover_completed && !value.cutover_started)
        || (value.identity_consumers_verified && !value.cutover_completed)
    {
        return Err(RotationError::InvalidProvenance);
    }
    Ok(())
}

fn load_provenance_if_present(
    path: &Path,
    plan: &RotationPlan,
) -> Result<Option<RotationProvenance>, RotationError> {
    if !path.exists() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| RotationError::InvalidProvenance)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(RotationError::InvalidProvenance);
    }
    let value: RotationProvenance =
        serde_json::from_slice(&fs::read(path).map_err(|_| RotationError::InvalidProvenance)?)
            .map_err(|_| RotationError::InvalidProvenance)?;
    validate_provenance(&value, plan)?;
    Ok(Some(value))
}

pub fn read_provenance(
    path: &Path,
    plan: &RotationPlan,
) -> Result<RotationProvenance, RotationError> {
    load_provenance_if_present(path, plan)?.ok_or(RotationError::InvalidProvenance)
}

pub fn persist_provenance(path: &Path, value: &RotationProvenance) -> Result<(), RotationError> {
    let parent = path.parent().ok_or(RotationError::InvalidProvenance)?;
    if !parent.is_dir() || parent.is_symlink() {
        return Err(RotationError::InvalidProvenance);
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| RotationError::InvalidProvenance)?;
    let temporary = parent.join(format!(
        ".phoenix-executor-rotation.{}.tmp",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| RotationError::InvalidProvenance)?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|_| RotationError::InvalidProvenance)?;
        file.write_all(b"\n")
            .map_err(|_| RotationError::InvalidProvenance)?;
        file.sync_all()
            .map_err(|_| RotationError::InvalidProvenance)?;
        fs::rename(&temporary, path).map_err(|_| RotationError::InvalidProvenance)?;
        #[cfg(unix)]
        {
            let directory = fs::File::open(parent).map_err(|_| RotationError::InvalidProvenance)?;
            directory
                .sync_all()
                .map_err(|_| RotationError::InvalidProvenance)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn normalize(value: &str) -> String {
    value.trim().trim_start_matches("0x").to_ascii_lowercase()
}

fn signer_matches_owner(signer: CanonicalAddress, owner: &str) -> bool {
    normalize(&signer.to_string()) == normalize(owner)
}

fn sha256_hex(value: &str) -> bool {
    normalize(value).len() == 64 && normalize(value).chars().all(|c| c.is_ascii_hexdigit())
}

fn commit_sha(value: &str) -> bool {
    normalize(value).len() == 40 && normalize(value).chars().all(|c| c.is_ascii_hexdigit())
}

pub fn canonical_config_digest() -> String {
    let canonical = serde_json::json!({
        "owner": REVIEWED_OWNER,
        "flash_provider": REVIEWED_FLASH_PROVIDER,
        "atlas": REVIEWED_ATLAS,
        "weth": REVIEWED_WETH,
        "maximum_input_amount": REVIEWED_MAX_INPUT,
        "searcher": REVIEWED_OWNER,
        "assets": reviewed_assets(),
        "routers": reviewed_routers(),
        "factory": REVIEWED_FACTORY,
        "pools": reviewed_pools(),
    });
    hex::encode(Sha256::digest(
        serde_json::to_vec(&canonical).expect("canonical JSON"),
    ))
}

fn reviewed_assets() -> Vec<String> {
    [REVIEWED_WETH, REVIEWED_NATIVE_USDC, REVIEWED_USDC_E]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn reviewed_routers() -> Vec<String> {
    [REVIEWED_ROUTER_1, REVIEWED_ROUTER_2, REVIEWED_ROUTER_3]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn reviewed_pools() -> Vec<RotationPool> {
    vec![
        RotationPool {
            address: "0x6f38e884725a116c9c7fbf208e79fe8828a2595f".into(),
            fee: 100,
            token0: REVIEWED_WETH.into(),
            token1: REVIEWED_NATIVE_USDC.into(),
        },
        RotationPool {
            address: "0xc6962004f452be9203591991d15f6b388e09e8d0".into(),
            fee: 500,
            token0: REVIEWED_WETH.into(),
            token1: REVIEWED_NATIVE_USDC.into(),
        },
        RotationPool {
            address: "0xc473e2aee3441bf9240be85eb122abb059a3b57c".into(),
            fee: 3000,
            token0: REVIEWED_WETH.into(),
            token1: REVIEWED_NATIVE_USDC.into(),
        },
        RotationPool {
            address: "0xc31e54c7a869b9fcbecc14363cf510d1c41fa443".into(),
            fee: 500,
            token0: REVIEWED_WETH.into(),
            token1: REVIEWED_USDC_E.into(),
        },
    ]
}

fn tx_hash(value: &str) -> bool {
    let normalized = normalize(value);
    normalized.len() == 64 && normalized.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_address(value: &str) -> bool {
    CanonicalAddress::parse(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> RotationPlan {
        RotationPlan {
            schema: ROTATION_SCHEMA.to_string(),
            chain_id: ARBITRUM_CHAIN_ID,
            source_sha: "11".repeat(20),
            base_release_sha: "22".repeat(20),
            old_executor: OLD_EXECUTOR.to_string(),
            old_runtime_sha256: REVIEWED_OLD_RUNTIME_SHA256.to_string(),
            expected_new_runtime_sha256: "44".repeat(32),
            creation_bytecode_sha256: "55".repeat(32),
            config_digest: canonical_config_digest(),
            owner: REVIEWED_OWNER.to_string(),
            flash_provider: REVIEWED_FLASH_PROVIDER.to_string(),
            atlas: REVIEWED_ATLAS.to_string(),
            weth: REVIEWED_WETH.to_string(),
            maximum_input_amount: REVIEWED_MAX_INPUT,
            searcher: REVIEWED_OWNER.to_string(),
            assets: reviewed_assets(),
            routers: reviewed_routers(),
            factory: REVIEWED_FACTORY.to_string(),
            pools: reviewed_pools(),
        }
    }

    #[test]
    fn plan_is_bound_to_phoenix_executor_identity_and_maximum() {
        let mut value = plan();
        assert_eq!(validate_plan(&value), Ok(()));
        value.maximum_input_amount = REVIEWED_MAX_INPUT + 1;
        assert_eq!(validate_plan(&value), Err(RotationError::InvalidPlan));
    }

    #[test]
    fn canonical_signer_identity_matches_the_reviewed_owner() {
        let signer = CanonicalAddress::parse(REVIEWED_OWNER).expect("reviewed owner");
        assert!(signer_matches_owner(signer, REVIEWED_OWNER));
        assert!(!signer_matches_owner(signer, OLD_EXECUTOR));
    }

    #[test]
    fn unknown_old_executor_is_rejected() {
        let mut value = plan();
        value.old_executor = REVIEWED_OWNER.to_string();
        assert_eq!(validate_plan(&value), Err(RotationError::InvalidPlan));
    }

    #[test]
    fn authenticated_transport_rejects_a_non_owner_signer_before_rpc_or_artifact_use() {
        let plan = plan();
        let rpc = HttpExecutionRpc::new_isolated_fork(
            url::Url::parse("http://127.0.0.1:8545").expect("URL"),
            "CONFIRMED_LOCAL_ANVIL",
        )
        .expect("isolated RPC");
        let signer = TransactionSigner::from_secret(&"01".repeat(32), ARBITRUM_CHAIN_ID)
            .expect("test signer");
        let result = AuthenticatedPhoenixExecutorRotationTransport::new(
            &plan,
            rpc,
            signer,
            Vec::new(),
            PathBuf::from("plan"),
            PathBuf::from("host"),
            PathBuf::from("provenance"),
        );
        assert!(matches!(result, Err(RotationError::InvalidPlan)));
    }

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<&'static str>,
        spl: bool,
        drained: bool,
        reconciled: bool,
        rollbacks: u8,
    }

    #[async_trait]
    impl RotationBackend for FakeBackend {
        type Error = ();
        async fn deploy(&mut self) -> Result<(), Self::Error> {
            self.calls.push("deploy");
            Ok(())
        }
        async fn mirror(&mut self) -> Result<(), Self::Error> {
            self.calls.push("mirror");
            Ok(())
        }
        async fn verify(&mut self) -> Result<(), Self::Error> {
            self.calls.push("verify");
            Ok(())
        }
        async fn spl_gate(&mut self) -> Result<bool, Self::Error> {
            self.calls.push("spl");
            Ok(self.spl)
        }
        async fn drain_old_bound_work(&mut self) -> Result<bool, Self::Error> {
            self.calls.push("drain");
            Ok(self.drained)
        }
        async fn cutover_live_identity(&mut self) -> Result<(), Self::Error> {
            self.calls.push("cutover");
            Ok(())
        }
        async fn reconcile(&mut self) -> Result<bool, Self::Error> {
            self.calls.push("reconcile");
            Ok(self.reconciled)
        }
        async fn rollback_identity_once(&mut self) -> Result<(), Self::Error> {
            self.calls.push("rollback");
            self.rollbacks += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn lifecycle_requires_spl_and_drain_before_cutover() {
        let backend = FakeBackend {
            spl: true,
            drained: true,
            reconciled: true,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        operator.execute().await.expect("execute");
        let backend = operator.into_backend();
        assert_eq!(
            backend.calls,
            [
                "deploy",
                "mirror",
                "verify",
                "spl",
                "drain",
                "cutover",
                "reconcile"
            ]
        );
        assert_eq!(backend.rollbacks, 0);
    }

    #[tokio::test]
    async fn reconciliation_failure_performs_exactly_one_identity_rollback() {
        let backend = FakeBackend {
            spl: true,
            drained: true,
            reconciled: false,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        assert_eq!(
            operator.execute().await,
            Err(RotationError::LifecycleRejected)
        );
        assert!(operator.rollback_once().await.is_err());
        let backend = operator.into_backend();
        assert_eq!(backend.rollbacks, 1);
    }

    #[tokio::test]
    async fn spl_failure_blocks_cutover_and_drain() {
        let backend = FakeBackend {
            spl: false,
            drained: true,
            reconciled: true,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        assert_eq!(
            operator.execute().await,
            Err(RotationError::LifecycleRejected)
        );
        let backend = operator.into_backend();
        assert_eq!(backend.calls, ["deploy", "mirror", "verify", "spl"]);
    }

    #[tokio::test]
    async fn prepare_includes_the_real_spl_gate_and_never_drains_or_cuts_over() {
        let backend = FakeBackend {
            spl: true,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        operator.prepare().await.expect("prepare");
        assert_eq!(
            operator.into_backend().calls,
            ["deploy", "mirror", "verify", "spl"]
        );
    }

    #[test]
    fn durable_provenance_rejects_a_forged_identity_and_second_rollback_shape() {
        let plan = plan();
        let new_executor =
            CanonicalAddress::parse("0x6666666666666666666666666666666666666666").expect("address");
        let mut value =
            provenance(&plan, new_executor, &format!("0x{}", "7".repeat(64))).expect("provenance");
        assert_eq!(value.old_executor, OLD_EXECUTOR);
        assert_eq!(validate_provenance(&value, &plan), Ok(()));
        value.rollback_completed = true;
        assert_eq!(
            validate_provenance(&value, &plan),
            Err(RotationError::InvalidProvenance)
        );
        value.rollback_used = true;
        value.cutover_started = true;
        value.identity_consumers = vec![
            "atlas-observer".to_string(),
            "economic-supervisor".to_string(),
            "live-executor".to_string(),
            "phoenix-engine".to_string(),
        ];
        assert_eq!(validate_provenance(&value, &plan), Ok(()));
        value.new_executor = OLD_EXECUTOR.to_string();
        assert_eq!(
            validate_provenance(&value, &plan),
            Err(RotationError::InvalidProvenance)
        );
    }

    #[derive(Default)]
    struct FakeTransport {
        calls: Vec<&'static str>,
    }

    #[async_trait]
    impl PhoenixExecutorRotationTransport for FakeTransport {
        type Error = ();
        async fn deploy_phoenix_executor(&mut self, _: &RotationPlan) -> Result<(), ()> {
            self.calls.push("authenticated-create");
            Ok(())
        }
        async fn mirror_phoenix_config(&mut self, _: &RotationPlan) -> Result<(), ()> {
            self.calls.push("owner-mirror-readback");
            Ok(())
        }
        async fn verify_phoenix_pair(&mut self, _: &RotationPlan) -> Result<(), ()> {
            self.calls.push("runtime-pair");
            Ok(())
        }
        async fn prove_spl_absent(&mut self, _: &RotationPlan) -> Result<bool, ()> {
            self.calls.push("spl");
            Ok(true)
        }
        async fn prove_old_bound_work_drained(&mut self) -> Result<bool, ()> {
            self.calls.push("drain");
            Ok(true)
        }
        async fn cutover_phoenix_identity(&mut self, _: &RotationPlan) -> Result<(), ()> {
            self.calls.push("cutover");
            Ok(())
        }
        async fn reconcile_phoenix_identity(&mut self, _: &RotationPlan) -> Result<bool, ()> {
            self.calls.push("reconcile");
            Ok(true)
        }
        async fn rollback_phoenix_identity_once(&mut self, _: &RotationPlan) -> Result<(), ()> {
            self.calls.push("rollback");
            Ok(())
        }
    }

    #[tokio::test]
    async fn production_backend_wires_every_async_transport_operation() {
        let backend =
            ProductionPhoenixExecutorRotationBackend::new(plan(), FakeTransport::default())
                .expect("backend");
        let mut operator = RotationOperator::new(backend);
        operator.execute().await.expect("execute");
        let transport = operator.into_backend().into_transport();
        assert_eq!(
            transport.calls,
            [
                "authenticated-create",
                "owner-mirror-readback",
                "runtime-pair",
                "spl",
                "drain",
                "cutover",
                "reconcile"
            ]
        );
    }
}
