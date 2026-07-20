use crate::{ARBITRUM_ONE_CHAIN_ID, REQUEST_SCHEMA_VERSION};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use thiserror::Error;
use uuid::Uuid;

pub const MAX_ROUTE_LEGS: usize = 4;
pub const MAX_APPROVER_BYTES: usize = 128;
pub const MAX_POLICY_VERSION_BYTES: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct CanonicalAddress([u8; 20]);

impl CanonicalAddress {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        if value.len() != 42
            || !value.starts_with("0x")
            || !value[2..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ModelError::InvalidAddress);
        }
        let decoded = hex::decode(&value[2..]).map_err(|_| ModelError::InvalidAddress)?;
        let bytes: [u8; 20] = decoded.try_into().map_err(|_| ModelError::InvalidAddress)?;
        if bytes == [0; 20] {
            return Err(ModelError::InvalidAddress);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Debug for CanonicalAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for CanonicalAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{}", hex::encode(self.0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransactionHash([u8; 32]);

impl TransactionHash {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        if value.len() != 66 || !value.starts_with("0x") {
            return Err(ModelError::InvalidTransactionHash);
        }
        let decoded = hex::decode(&value[2..]).map_err(|_| ModelError::InvalidTransactionHash)?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| ModelError::InvalidTransactionHash)?;
        Ok(Self(bytes))
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for TransactionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for TransactionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{}", hex::encode(self.0))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLeg {
    pub pool: String,
    pub token_in: String,
    pub token_out: String,
    pub fee: u32,
    pub zero_for_one: bool,
    pub min_amount_out: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedLeg {
    pub pool: CanonicalAddress,
    pub token_in: CanonicalAddress,
    pub token_out: CanonicalAddress,
    pub fee: u32,
    pub zero_for_one: bool,
    pub min_amount_out: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionRequest {
    pub id: Uuid,
    pub opportunity_id: Uuid,
    pub schema_version: String,
    pub chain_id: u64,
    pub route_id: [u8; 32],
    pub origin_router: CanonicalAddress,
    pub flash_asset: CanonicalAddress,
    pub flash_amount: u128,
    pub maximum_input_amount: u128,
    pub minimum_profit: u128,
    pub expected_profit: u128,
    pub deadline: DateTime<Utc>,
    pub legs: Vec<ValidatedLeg>,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub policy_version: String,
    pub approval_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawExecutionRequest {
    pub id: Uuid,
    pub opportunity_id: Uuid,
    pub schema_version: String,
    pub chain_id: i64,
    pub route_id: String,
    pub origin_router: String,
    pub flash_asset: String,
    pub flash_amount: String,
    pub maximum_input_amount: String,
    pub minimum_profit: String,
    pub expected_profit: String,
    pub deadline: DateTime<Utc>,
    pub legs: Vec<ExecutionLeg>,
    pub gas_limit: i64,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub policy_version: String,
    pub approval_digest: String,
}

#[derive(Serialize)]
struct ApprovalBody<'a> {
    schema_version: &'a str,
    request_id: String,
    opportunity_id: String,
    chain_id: u64,
    route_id: String,
    origin_router: String,
    flash_asset: String,
    flash_amount: String,
    maximum_input_amount: String,
    minimum_profit: String,
    expected_profit: String,
    deadline_unix_seconds: i64,
    legs: &'a [ValidatedLeg],
    gas_limit: u64,
    max_fee_per_gas: String,
    max_priority_fee_per_gas: String,
    approved_by: &'a str,
    approved_at: String,
    policy_version: &'a str,
}

impl Serialize for ValidatedLeg {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ExecutionLeg {
            pool: self.pool.to_string(),
            token_in: self.token_in.to_string(),
            token_out: self.token_out.to_string(),
            fee: self.fee,
            zero_for_one: self.zero_for_one,
            min_amount_out: self.min_amount_out.to_string(),
        }
        .serialize(serializer)
    }
}

impl RawExecutionRequest {
    pub fn validate(self) -> Result<ExecutionRequest, ModelError> {
        if self.schema_version != REQUEST_SCHEMA_VERSION {
            return Err(ModelError::SchemaVersion);
        }
        let chain_id = u64::try_from(self.chain_id).map_err(|_| ModelError::WrongChain)?;
        if chain_id != ARBITRUM_ONE_CHAIN_ID {
            return Err(ModelError::WrongChain);
        }
        let route_id = parse_fixed_hex::<32>(&self.route_id).ok_or(ModelError::InvalidRoute)?;
        let flash_amount = parse_positive_u128(&self.flash_amount)?;
        let maximum_input_amount = parse_positive_u128(&self.maximum_input_amount)?;
        let minimum_profit = parse_positive_u128(&self.minimum_profit)?;
        let expected_profit = parse_positive_u128(&self.expected_profit)?;
        let gas_limit = u64::try_from(self.gas_limit)
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ModelError::InvalidGas)?;
        let max_fee_per_gas = parse_positive_u128(&self.max_fee_per_gas)?;
        let max_priority_fee_per_gas = parse_positive_u128(&self.max_priority_fee_per_gas)?;
        if max_priority_fee_per_gas > max_fee_per_gas {
            return Err(ModelError::InvalidGas);
        }
        if self.legs.is_empty() || self.legs.len() > MAX_ROUTE_LEGS {
            return Err(ModelError::InvalidLegs);
        }
        let legs = self
            .legs
            .into_iter()
            .map(ValidatedLeg::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        validate_route(&legs, CanonicalAddress::parse(&self.flash_asset)?)?;
        if self.approved_by.trim().is_empty()
            || self.approved_by.len() > MAX_APPROVER_BYTES
            || self.policy_version.trim().is_empty()
            || self.policy_version.len() > MAX_POLICY_VERSION_BYTES
            || !canonical_digest(&self.approval_digest)
        {
            return Err(ModelError::InvalidApproval);
        }
        let request = ExecutionRequest {
            id: self.id,
            opportunity_id: self.opportunity_id,
            schema_version: self.schema_version,
            chain_id,
            route_id,
            origin_router: CanonicalAddress::parse(&self.origin_router)?,
            flash_asset: CanonicalAddress::parse(&self.flash_asset)?,
            flash_amount,
            maximum_input_amount,
            minimum_profit,
            expected_profit,
            deadline: self.deadline,
            legs,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            approved_by: self.approved_by,
            approved_at: self.approved_at,
            policy_version: self.policy_version,
            approval_digest: self.approval_digest,
        };
        if request.canonical_approval_digest()? != request.approval_digest {
            return Err(ModelError::ApprovalDigestMismatch);
        }
        Ok(request)
    }
}

impl ExecutionRequest {
    pub fn canonical_approval_digest(&self) -> Result<String, ModelError> {
        let body = ApprovalBody {
            schema_version: &self.schema_version,
            request_id: self.id.to_string(),
            opportunity_id: self.opportunity_id.to_string(),
            chain_id: self.chain_id,
            route_id: format!("0x{}", hex::encode(self.route_id)),
            origin_router: self.origin_router.to_string(),
            flash_asset: self.flash_asset.to_string(),
            flash_amount: self.flash_amount.to_string(),
            maximum_input_amount: self.maximum_input_amount.to_string(),
            minimum_profit: self.minimum_profit.to_string(),
            expected_profit: self.expected_profit.to_string(),
            deadline_unix_seconds: self.deadline.timestamp(),
            legs: &self.legs,
            gas_limit: self.gas_limit,
            max_fee_per_gas: self.max_fee_per_gas.to_string(),
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.to_string(),
            approved_by: &self.approved_by,
            approved_at: self
                .approved_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            policy_version: &self.policy_version,
        };
        let encoded = serde_json::to_vec(&body).map_err(|_| ModelError::InvalidApproval)?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }
}

impl TryFrom<ExecutionLeg> for ValidatedLeg {
    type Error = ModelError;

    fn try_from(value: ExecutionLeg) -> Result<Self, Self::Error> {
        if value.fee == 0 || value.fee > 1_000_000 {
            return Err(ModelError::InvalidLegs);
        }
        Ok(Self {
            pool: CanonicalAddress::parse(&value.pool)?,
            token_in: CanonicalAddress::parse(&value.token_in)?,
            token_out: CanonicalAddress::parse(&value.token_out)?,
            fee: value.fee,
            zero_for_one: value.zero_for_one,
            min_amount_out: parse_positive_u128(&value.min_amount_out)?,
        })
    }
}

fn validate_route(legs: &[ValidatedLeg], flash_asset: CanonicalAddress) -> Result<(), ModelError> {
    let mut expected_input = flash_asset;
    for leg in legs {
        if leg.token_in != expected_input || leg.token_in == leg.token_out {
            return Err(ModelError::InvalidLegs);
        }
        expected_input = leg.token_out;
    }
    if expected_input != flash_asset {
        return Err(ModelError::InvalidLegs);
    }
    Ok(())
}

fn parse_positive_u128(value: &str) -> Result<u128, ModelError> {
    value
        .parse::<u128>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or(ModelError::InvalidAmount)
}

fn parse_fixed_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != 2 + N * 2 || !value.starts_with("0x") {
        return None;
    }
    let decoded = hex::decode(&value[2..]).ok()?;
    decoded.try_into().ok()
}

pub fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptStatus {
    Claimed,
    NonceAllocated,
    SubmissionUnknown,
    Pending,
    Confirmed,
    Reverted,
    Replaced,
    TimedOut,
    Failed,
}

impl AttemptStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::NonceAllocated => "nonce_allocated",
            Self::SubmissionUnknown => "submission_unknown",
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Reverted => "reverted",
            Self::Replaced => "replaced",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveAttempt {
    pub request: ExecutionRequest,
    pub status: AttemptStatus,
    pub nonce: Option<u64>,
    pub tx_hash: Option<TransactionHash>,
    pub submitted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settlement {
    pub asset: CanonicalAddress,
    pub flash_amount: u128,
    pub premium: u128,
    pub realized_profit: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptOutcome {
    pub receipt_status: u64,
    pub settled_event_found: bool,
    pub block_number: u64,
    pub gas_used: u64,
    pub effective_gas_price: u128,
    pub actual_fee_wei: u128,
    pub settlement: Settlement,
    pub net_pnl_wei: i128,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("invalid canonical address")]
    InvalidAddress,
    #[error("invalid transaction hash")]
    InvalidTransactionHash,
    #[error("unsupported request schema")]
    SchemaVersion,
    #[error("unsupported chain")]
    WrongChain,
    #[error("invalid route identity")]
    InvalidRoute,
    #[error("invalid amount")]
    InvalidAmount,
    #[error("invalid gas policy")]
    InvalidGas,
    #[error("invalid route legs")]
    InvalidLegs,
    #[error("invalid approval")]
    InvalidApproval,
    #[error("approval digest mismatch")]
    ApprovalDigestMismatch,
}
