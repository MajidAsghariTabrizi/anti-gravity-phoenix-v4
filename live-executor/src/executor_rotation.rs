//! PhoenixExecutor-specific rotation guardrails.
//!
//! This module deliberately contains no generic deployment primitive and no
//! RPC mutation.  It validates the immutable inputs that a separately
//! authorized operator must present before using the signer CREATE API.

use crate::model::CanonicalAddress;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use thiserror::Error;

pub const ROTATION_SCHEMA: &str = "phoenix.executor-rotation.v1";
pub const ARBITRUM_CHAIN_ID: u64 = 42_161;
pub const REVIEWED_MAX_INPUT: u128 = 10_000_000_000_000_000;
pub const OLD_EXECUTOR: &str = "0x634f62d7cd28d1c4dcf503d901b88d666c2626ad";
pub const REVIEWED_OWNER: &str = "0x9f30c00b68f7c0edb4b4117b9f04e0ca2eb2c17a";
pub const REVIEWED_FLASH_PROVIDER: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
pub const REVIEWED_ATLAS: &str = "0x8ad1ae9d97c79aa68a0a151e83ff3942f68f86c1";
pub const REVIEWED_WETH: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";

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
    pub cutover_tx_hashes: Vec<String>,
    pub rollback_tx_hash: Option<String>,
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
}

/// Narrow host-operation boundary.  The implementation is intentionally
/// injected: production transport, compose/context orchestration, and signer
/// material stay outside this state machine and cannot be replaced by a
/// generic deployer.
pub trait RotationBackend {
    type Error;

    fn deploy(&mut self) -> Result<(), Self::Error>;
    fn mirror(&mut self) -> Result<(), Self::Error>;
    fn verify(&mut self) -> Result<(), Self::Error>;
    fn spl_gate(&mut self) -> Result<bool, Self::Error>;
    fn drain_old_bound_work(&mut self) -> Result<bool, Self::Error>;
    fn cutover_live_identity(&mut self) -> Result<(), Self::Error>;
    fn reconcile(&mut self) -> Result<bool, Self::Error>;
    fn rollback_identity_once(&mut self) -> Result<(), Self::Error>;
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
    pub fn execute(&mut self) -> Result<(), RotationError> {
        self.backend
            .deploy()
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .mirror()
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .verify()
            .map_err(|_| RotationError::LifecycleRejected)?;
        if !self
            .backend
            .spl_gate()
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            return Err(RotationError::LifecycleRejected);
        }
        if !self
            .backend
            .drain_old_bound_work()
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            return Err(RotationError::LifecycleRejected);
        }
        self.backend
            .cutover_live_identity()
            .map_err(|_| RotationError::LifecycleRejected)?;
        if !self
            .backend
            .reconcile()
            .map_err(|_| RotationError::LifecycleRejected)?
        {
            self.rollback_once()?;
        }
        Ok(())
    }

    pub fn rollback_once(&mut self) -> Result<(), RotationError> {
        if self.rollback_used {
            return Err(RotationError::LifecycleRejected);
        }
        self.rollback_used = true;
        self.backend
            .rollback_identity_once()
            .map_err(|_| RotationError::LifecycleRejected)
    }

    pub fn prepare(&mut self) -> Result<(), RotationError> {
        self.backend
            .deploy()
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .mirror()
            .map_err(|_| RotationError::LifecycleRejected)?;
        self.backend
            .verify()
            .map_err(|_| RotationError::LifecycleRejected)
    }
}

pub fn validate_plan(plan: &RotationPlan) -> Result<(), RotationError> {
    if plan.schema != ROTATION_SCHEMA
        || plan.chain_id != ARBITRUM_CHAIN_ID
        || !sha256_hex(&plan.source_sha)
        || !sha256_hex(&plan.base_release_sha)
        || !sha256_hex(&plan.old_runtime_sha256)
        || !sha256_hex(&plan.expected_new_runtime_sha256)
        || !sha256_hex(&plan.creation_bytecode_sha256)
        || !sha256_hex(&plan.config_digest)
        || normalize(&plan.old_executor) != normalize(OLD_EXECUTOR)
        || normalize(&plan.owner) != normalize(REVIEWED_OWNER)
        || normalize(&plan.flash_provider) != normalize(REVIEWED_FLASH_PROVIDER)
        || normalize(&plan.atlas) != normalize(REVIEWED_ATLAS)
        || normalize(&plan.weth) != normalize(REVIEWED_WETH)
        || plan.maximum_input_amount != REVIEWED_MAX_INPUT
        || !valid_address(&plan.searcher)
        || plan.assets.is_empty()
        || plan.routers.is_empty()
        || !valid_address(&plan.factory)
        || plan.pools.is_empty()
        || plan.assets.iter().any(|value| !valid_address(value))
        || plan.routers.iter().any(|value| !valid_address(value))
        || plan.pools.iter().any(|pool| {
            !valid_address(&pool.address)
                || pool.fee == 0
                || !valid_address(&pool.token0)
                || !valid_address(&pool.token1)
        })
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
    validate_plan(plan)?;
    if !tx_hash(deployment_tx_hash) {
        return Err(RotationError::InvalidPlan);
    }
    Ok(RotationProvenance {
        schema: ROTATION_SCHEMA.to_string(),
        tooling_source_sha: normalize(&plan.source_sha),
        base_release_sha: normalize(&plan.base_release_sha),
        chain_id: plan.chain_id,
        old_executor: normalize(&plan.old_executor),
        new_executor: new_executor.to_string(),
        old_runtime_sha256: normalize(&plan.old_runtime_sha256),
        new_runtime_sha256: normalize(&plan.expected_new_runtime_sha256),
        creation_bytecode_sha256: normalize(&plan.creation_bytecode_sha256),
        config_digest: normalize(&plan.config_digest),
        deployment_tx_hash: deployment_tx_hash.to_ascii_lowercase(),
        cutover_tx_hashes: Vec::new(),
        rollback_tx_hash: None,
    })
}

fn normalize(value: &str) -> String {
    value.trim().trim_start_matches("0x").to_ascii_lowercase()
}

fn sha256_hex(value: &str) -> bool {
    normalize(value).len() == 64 && normalize(value).chars().all(|c| c.is_ascii_hexdigit())
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
            source_sha: "11".repeat(32),
            base_release_sha: "22".repeat(32),
            old_executor: OLD_EXECUTOR.to_string(),
            old_runtime_sha256: "33".repeat(32),
            expected_new_runtime_sha256: "44".repeat(32),
            creation_bytecode_sha256: "55".repeat(32),
            config_digest: "66".repeat(32),
            owner: REVIEWED_OWNER.to_string(),
            flash_provider: REVIEWED_FLASH_PROVIDER.to_string(),
            atlas: REVIEWED_ATLAS.to_string(),
            weth: REVIEWED_WETH.to_string(),
            maximum_input_amount: REVIEWED_MAX_INPUT,
            searcher: REVIEWED_OWNER.to_string(),
            assets: vec![REVIEWED_WETH.to_string()],
            routers: vec!["0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45".to_string()],
            factory: "0x1f98431c8ad98523631ae4a59f267346ea31f984".to_string(),
            pools: vec![RotationPool {
                address: "0x6f3841a2d5c8e1fcb1ca9c42d6b8c5d0f2d7595f".to_string(),
                fee: 100,
                token0: REVIEWED_WETH.to_string(),
                token1: "0xaf88d065e77c8cc2239327c5edb3a432268e5831".to_string(),
            }],
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
    fn unknown_old_executor_is_rejected() {
        let mut value = plan();
        value.old_executor = REVIEWED_OWNER.to_string();
        assert_eq!(validate_plan(&value), Err(RotationError::InvalidPlan));
    }

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<&'static str>,
        spl: bool,
        drained: bool,
        reconciled: bool,
        rollbacks: u8,
    }

    impl RotationBackend for FakeBackend {
        type Error = ();
        fn deploy(&mut self) -> Result<(), Self::Error> {
            self.calls.push("deploy");
            Ok(())
        }
        fn mirror(&mut self) -> Result<(), Self::Error> {
            self.calls.push("mirror");
            Ok(())
        }
        fn verify(&mut self) -> Result<(), Self::Error> {
            self.calls.push("verify");
            Ok(())
        }
        fn spl_gate(&mut self) -> Result<bool, Self::Error> {
            self.calls.push("spl");
            Ok(self.spl)
        }
        fn drain_old_bound_work(&mut self) -> Result<bool, Self::Error> {
            self.calls.push("drain");
            Ok(self.drained)
        }
        fn cutover_live_identity(&mut self) -> Result<(), Self::Error> {
            self.calls.push("cutover");
            Ok(())
        }
        fn reconcile(&mut self) -> Result<bool, Self::Error> {
            self.calls.push("reconcile");
            Ok(self.reconciled)
        }
        fn rollback_identity_once(&mut self) -> Result<(), Self::Error> {
            self.calls.push("rollback");
            self.rollbacks += 1;
            Ok(())
        }
    }

    #[test]
    fn lifecycle_requires_spl_and_drain_before_cutover() {
        let backend = FakeBackend {
            spl: true,
            drained: true,
            reconciled: true,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        operator.execute().expect("execute");
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

    #[test]
    fn reconciliation_failure_performs_exactly_one_identity_rollback() {
        let backend = FakeBackend {
            spl: true,
            drained: true,
            reconciled: false,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        operator.execute().expect("rollback");
        assert!(operator.rollback_once().is_err());
        let backend = operator.into_backend();
        assert_eq!(backend.rollbacks, 1);
    }

    #[test]
    fn spl_failure_blocks_cutover_and_drain() {
        let backend = FakeBackend {
            spl: false,
            drained: true,
            reconciled: true,
            ..Default::default()
        };
        let mut operator = RotationOperator::new(backend);
        assert_eq!(operator.execute(), Err(RotationError::LifecycleRejected));
        let backend = operator.into_backend();
        assert_eq!(backend.calls, ["deploy", "mirror", "verify", "spl"]);
    }
}
