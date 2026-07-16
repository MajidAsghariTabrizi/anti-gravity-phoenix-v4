use primitive_types::U256;
use serde::Serialize;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ChainId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SequenceNumber(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TxHash(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Address(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PoolId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TokenAddress(pub Address);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Amount(pub u128);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SqrtPriceX96(pub U256);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Tick(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Liquidity(pub u128);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct OpportunityId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RouteId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ZeroForOne,
    OneForZero,
}

impl Address {
    pub fn parse(input: &str) -> Result<Self, DomainError> {
        let s = input.to_ascii_lowercase();
        if s.len() != 42 || !s.starts_with("0x") || !s[2..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(DomainError::InvalidAddress(input.to_string()));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Amount {
    pub const ZERO: Self = Self(0);

    pub fn checked_add(self, rhs: Amount) -> Result<Amount, DomainError> {
        self.0
            .checked_add(rhs.0)
            .map(Amount)
            .ok_or(DomainError::ArithmeticOverflow)
    }

    pub fn checked_sub(self, rhs: Amount) -> Result<Amount, DomainError> {
        self.0
            .checked_sub(rhs.0)
            .map(Amount)
            .ok_or(DomainError::ArithmeticUnderflow)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissReason {
    UnsupportedOrigin,
    StateIncomplete,
    NoAffectedRoute,
    NoSpread,
    OptimizedBelowThreshold,
    StaleState,
    Expired,
    SigningError,
    SubmissionError,
    NotIncluded,
    ReceiptRevert,
    CompetitorOrStateChanged,
    ReconciliationError,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    InvalidAddress(String),
    InvalidCalldata(String),
    ArithmeticOverflow,
    ArithmeticUnderflow,
    StateIncomplete,
    UnsupportedOrigin,
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAddress(v) => write!(f, "invalid address: {v}"),
            Self::InvalidCalldata(v) => write!(f, "invalid calldata: {v}"),
            Self::ArithmeticOverflow => write!(f, "arithmetic overflow"),
            Self::ArithmeticUnderflow => write!(f, "arithmetic underflow"),
            Self::StateIncomplete => write!(f, "state incomplete"),
            Self::UnsupportedOrigin => write!(f, "unsupported origin"),
        }
    }
}

impl std::error::Error for DomainError {}
