use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MINIMUM_OBSERVATIONS: u64 = 100;
pub const MINIMUM_VALID_ACCEPTANCE_BPS: u16 = 9_990;
pub const MINIMUM_FORK_PASS_RATE_BPS: u16 = 9_500;
pub const MAXIMUM_PREDICTION_ERROR_BPS: u16 = 1_000;
pub const MINIMUM_PROMOTION_OUTCOMES: u64 = 20;
pub const MINIMUM_SUCCESS_RATE_BPS: u16 = 9_500;
pub const MAXIMUM_STATE_AGE_BLOCKS: u64 = 1;
pub const MAXIMUM_QUOTE_AGE_MS: u64 = 2_000;
pub const MAXIMUM_CANDIDATE_AGE_MS: u64 = 3_000;
pub const READINESS_TTL_SECONDS: i64 = 600;
pub const COOLDOWN_SECONDS: i64 = 900;
pub const MAXIMUM_REVIEWED_INPUT_WEI: u128 = 10_000_000_000_000_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EconomicPhase {
    DisarmedDeploy,
    DisarmedEvidence,
    CanaryReady,
    LiveCanaryMin,
    LiveScaleL1,
    LiveScaleL2,
    LiveScaleL3,
    LiveScaleL4,
    LiveScaleL5,
    LiveMaxReviewed,
    Cooldown,
    DisarmedFailure,
}

impl EconomicPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DisarmedDeploy => "DISARMED_DEPLOY",
            Self::DisarmedEvidence => "DISARMED_EVIDENCE",
            Self::CanaryReady => "CANARY_READY",
            Self::LiveCanaryMin => "LIVE_CANARY_MIN",
            Self::LiveScaleL1 => "LIVE_SCALE_L1",
            Self::LiveScaleL2 => "LIVE_SCALE_L2",
            Self::LiveScaleL3 => "LIVE_SCALE_L3",
            Self::LiveScaleL4 => "LIVE_SCALE_L4",
            Self::LiveScaleL5 => "LIVE_SCALE_L5",
            Self::LiveMaxReviewed => "LIVE_MAX_REVIEWED",
            Self::Cooldown => "COOLDOWN",
            Self::DisarmedFailure => "DISARMED_FAILURE",
        }
    }

    pub const fn is_live(self) -> bool {
        matches!(
            self,
            Self::LiveCanaryMin
                | Self::LiveScaleL1
                | Self::LiveScaleL2
                | Self::LiveScaleL3
                | Self::LiveScaleL4
                | Self::LiveScaleL5
                | Self::LiveMaxReviewed
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum SizeLevel {
    #[serde(rename = "MIN")]
    Min,
    #[serde(rename = "L1")]
    L1,
    #[serde(rename = "L2")]
    L2,
    #[serde(rename = "L3")]
    L3,
    #[serde(rename = "L4")]
    L4,
    #[serde(rename = "L5")]
    L5,
    #[serde(rename = "MAX_REVIEWED")]
    MaxReviewed,
}

impl SizeLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Min => "MIN",
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
            Self::L4 => "L4",
            Self::L5 => "L5",
            Self::MaxReviewed => "MAX_REVIEWED",
        }
    }

    pub const fn amount_wei(self) -> u128 {
        match self {
            Self::Min => 100_000_000_000_000,
            Self::L1 => 250_000_000_000_000,
            Self::L2 => 500_000_000_000_000,
            Self::L3 => 1_000_000_000_000_000,
            Self::L4 => 2_500_000_000_000_000,
            Self::L5 => 5_000_000_000_000_000,
            Self::MaxReviewed => MAXIMUM_REVIEWED_INPUT_WEI,
        }
    }

    pub const fn phase(self) -> EconomicPhase {
        match self {
            Self::Min => EconomicPhase::LiveCanaryMin,
            Self::L1 => EconomicPhase::LiveScaleL1,
            Self::L2 => EconomicPhase::LiveScaleL2,
            Self::L3 => EconomicPhase::LiveScaleL3,
            Self::L4 => EconomicPhase::LiveScaleL4,
            Self::L5 => EconomicPhase::LiveScaleL5,
            Self::MaxReviewed => EconomicPhase::LiveMaxReviewed,
        }
    }

    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Min => Some(Self::L1),
            Self::L1 => Some(Self::L2),
            Self::L2 => Some(Self::L3),
            Self::L3 => Some(Self::L4),
            Self::L4 => Some(Self::L5),
            Self::L5 => Some(Self::MaxReviewed),
            Self::MaxReviewed => None,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Min | Self::L1 => Self::Min,
            Self::L2 => Self::L1,
            Self::L3 => Self::L2,
            Self::L4 => Self::L3,
            Self::L5 => Self::L4,
            Self::MaxReviewed => Self::L5,
        }
    }
}

impl TryFrom<&str> for SizeLevel {
    type Error = EconomicControlError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "MIN" => Ok(Self::Min),
            "L1" => Ok(Self::L1),
            "L2" => Ok(Self::L2),
            "L3" => Ok(Self::L3),
            "L4" => Ok(Self::L4),
            "L5" => Ok(Self::L5),
            "MAX_REVIEWED" => Ok(Self::MaxReviewed),
            _ => Err(EconomicControlError::InvalidLevel),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceGate {
    pub supported_observations: u64,
    pub valid_acceptance_bps: u16,
    pub process_fatal_integrity_exits: u64,
    pub quarantine_progress_proven: bool,
    pub consumer_pending_bounded: bool,
    pub ack_pending_bounded: bool,
    pub stale_outbox_rows: u64,
    pub primary_rpc_healthy: bool,
    pub secondary_rpc_healthy: bool,
    pub rpc_providers_independent: bool,
    pub eligible_rpc_disagreements: u64,
    pub maximum_state_age_blocks: u64,
    pub maximum_quote_age_ms: u64,
    pub maximum_candidate_age_ms: u64,
    pub fork_attempts: u64,
    pub fork_passes: u64,
    pub prediction_error_bps: u16,
    pub secondary_skips: u64,
    pub fork_skips: u64,
    pub execution_requests: u64,
    pub active_attempts: u64,
    pub positive_independent_fork_candidates: u64,
}

impl EvidenceGate {
    pub fn validate(&self) -> Result<(), EconomicControlError> {
        if self.supported_observations < MINIMUM_OBSERVATIONS {
            return Err(EconomicControlError::InsufficientObservations);
        }
        if self.valid_acceptance_bps < MINIMUM_VALID_ACCEPTANCE_BPS {
            return Err(EconomicControlError::AcceptanceRate);
        }
        if self.process_fatal_integrity_exits != 0 || !self.quarantine_progress_proven {
            return Err(EconomicControlError::Integrity);
        }
        if !self.consumer_pending_bounded || !self.ack_pending_bounded {
            return Err(EconomicControlError::Backlog);
        }
        if self.stale_outbox_rows != 0 {
            return Err(EconomicControlError::Outbox);
        }
        if !self.primary_rpc_healthy
            || !self.secondary_rpc_healthy
            || !self.rpc_providers_independent
            || self.eligible_rpc_disagreements != 0
        {
            return Err(EconomicControlError::RpcAgreement);
        }
        if self.maximum_state_age_blocks > MAXIMUM_STATE_AGE_BLOCKS
            || self.maximum_quote_age_ms > MAXIMUM_QUOTE_AGE_MS
            || self.maximum_candidate_age_ms > MAXIMUM_CANDIDATE_AGE_MS
        {
            return Err(EconomicControlError::StaleEvidence);
        }
        if rate_bps(self.fork_passes, self.fork_attempts) < MINIMUM_FORK_PASS_RATE_BPS {
            return Err(EconomicControlError::ForkRate);
        }
        if self.prediction_error_bps > MAXIMUM_PREDICTION_ERROR_BPS {
            return Err(EconomicControlError::PredictionError);
        }
        if self.secondary_skips != 0 || self.fork_skips != 0 {
            return Err(EconomicControlError::SkippedVerification);
        }
        if self.execution_requests != 0 || self.active_attempts != 0 {
            return Err(EconomicControlError::ExecutionWhileDisarmed);
        }
        if self.positive_independent_fork_candidates == 0 {
            return Err(EconomicControlError::NoPositiveCandidate);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReadinessBinding {
    pub release_sha: String,
    pub engine_image_digest: String,
    pub route_fingerprint: String,
    pub route_universe_hash: String,
    pub route_policy_hash: String,
    pub risk_policy_hash: String,
    pub economic_control_epoch: u64,
    pub global_control_epoch: u64,
    pub route_control_epoch: u64,
    pub executor_code_hash: String,
    pub contract_identity_hash: String,
    pub wallet_gas_reserve_wei: u128,
    pub gas_reserve_floor_wei: u128,
    pub current_daily_loss_wei: u128,
    pub daily_loss_limit_wei: u128,
    pub observed_from: DateTime<Utc>,
    pub observed_until: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub candidate_evidence_hashes: Vec<String>,
}

impl ReadinessBinding {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), EconomicControlError> {
        if !is_hex(&self.release_sha, 40)
            || !is_digest(&self.engine_image_digest)
            || !is_hex(&self.route_universe_hash, 64)
            || !is_hex(&self.route_policy_hash, 64)
            || !is_hex(&self.risk_policy_hash, 64)
            || !is_hex(&self.executor_code_hash, 64)
            || !is_hex(&self.contract_identity_hash, 64)
            || self.route_fingerprint.is_empty()
            || self.route_fingerprint.len() > 256
            || self.candidate_evidence_hashes.is_empty()
            || self
                .candidate_evidence_hashes
                .iter()
                .any(|value| !is_hex(value, 64))
        {
            return Err(EconomicControlError::InvalidBinding);
        }
        if self.observed_from >= self.observed_until
            || self.observed_until > self.created_at
            || self.expires_at <= self.created_at
            || self.expires_at - self.created_at > Duration::seconds(READINESS_TTL_SECONDS)
            || now >= self.expires_at
        {
            return Err(EconomicControlError::ExpiredReadiness);
        }
        if self.wallet_gas_reserve_wei <= self.gas_reserve_floor_wei
            || self.current_daily_loss_wei >= self.daily_loss_limit_wei
        {
            return Err(EconomicControlError::RiskReserve);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutomationAuthorization {
    pub route_fingerprint: String,
    pub route_policy_hash: String,
    pub maximum_reviewed_input_wei: u128,
    pub executor_code_hash: String,
    pub release_family: String,
    pub one_transaction_at_a_time: bool,
    pub reviewed_ladder_only: bool,
    pub automatic_disarm_required: bool,
    pub expires_at: DateTime<Utc>,
}

impl AutomationAuthorization {
    pub fn validate(
        &self,
        readiness: &ReadinessBinding,
        now: DateTime<Utc>,
    ) -> Result<(), EconomicControlError> {
        if self.route_fingerprint != readiness.route_fingerprint
            || self.route_policy_hash != readiness.route_policy_hash
            || self.executor_code_hash != readiness.executor_code_hash
            || self.maximum_reviewed_input_wei != MAXIMUM_REVIEWED_INPUT_WEI
            || !self.one_transaction_at_a_time
            || !self.reviewed_ladder_only
            || !self.automatic_disarm_required
            || self.release_family.is_empty()
            || now >= self.expires_at
        {
            return Err(EconomicControlError::Authorization);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromotionEvidence {
    pub reconciled_outcomes: u64,
    pub aggregate_realized_net_pnl_wei: i128,
    pub successful_outcomes: u64,
    pub fork_attempts: u64,
    pub fork_passes: u64,
    pub rpc_disagreements: u64,
    pub unknown_submissions: u64,
    pub duplicate_submissions: u64,
    pub nonce_gaps: u64,
    pub control_violations: u64,
    pub unreconciled_receipts: u64,
    pub process_fatal_integrity_events: u64,
    pub identity_mismatches: u64,
    pub prediction_error_bps: u16,
    pub daily_loss_wei: u128,
    pub daily_loss_limit_wei: u128,
    pub consecutive_losses: u64,
    pub wallet_gas_reserve_wei: u128,
    pub gas_reserve_floor_wei: u128,
    pub maximum_quote_age_ms: u64,
    pub maximum_candidate_age_ms: u64,
}

impl PromotionEvidence {
    pub fn validate(&self) -> Result<(), EconomicControlError> {
        if self.reconciled_outcomes < MINIMUM_PROMOTION_OUTCOMES
            || self.aggregate_realized_net_pnl_wei <= 0
            || rate_bps(self.successful_outcomes, self.reconciled_outcomes)
                < MINIMUM_SUCCESS_RATE_BPS
            || rate_bps(self.fork_passes, self.fork_attempts) < MINIMUM_FORK_PASS_RATE_BPS
            || self.prediction_error_bps > MAXIMUM_PREDICTION_ERROR_BPS
            || self.rpc_disagreements != 0
            || self.unknown_submissions != 0
            || self.duplicate_submissions != 0
            || self.nonce_gaps != 0
            || self.control_violations != 0
            || self.unreconciled_receipts != 0
            || self.process_fatal_integrity_events != 0
            || self.identity_mismatches != 0
            || self.daily_loss_wei >= self.daily_loss_limit_wei
            || self.consecutive_losses >= 3
            || self.wallet_gas_reserve_wei <= self.gas_reserve_floor_wei
            || self.maximum_quote_age_ms > MAXIMUM_QUOTE_AGE_MS
            || self.maximum_candidate_age_ms > MAXIMUM_CANDIDATE_AGE_MS
        {
            return Err(EconomicControlError::PromotionGate);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureSignal {
    RealizedNegative,
    EconomicQualityDegraded,
    ForkRateBelowMinimum,
    PredictionErrorAboveMaximum,
    RepeatedRpcDisagreement,
    ThreeConsecutiveLosses,
    DailyLossLimitReached,
    UnknownSubmission,
    DuplicateSubmission,
    NonceInconsistency,
    ReceiptUnreconciled,
    ProcessFatalIntegrity,
    SignerMismatch,
    ExecutorCodeMismatch,
    OwnerMismatch,
    FlashProviderMismatch,
    ContractMismatch,
    Rollback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transition {
    Stay {
        phase: EconomicPhase,
        level: SizeLevel,
    },
    Promote {
        phase: EconomicPhase,
        level: SizeLevel,
    },
    Cooldown {
        level: SizeLevel,
        until: DateTime<Utc>,
    },
    RouteDisarm {
        reason: &'static str,
    },
    GlobalDisarm {
        reason: &'static str,
    },
}

pub fn activate_canary(
    phase: EconomicPhase,
    readiness: &ReadinessBinding,
    evidence: &EvidenceGate,
    authorization: &AutomationAuthorization,
    now: DateTime<Utc>,
) -> Result<Transition, EconomicControlError> {
    if phase != EconomicPhase::CanaryReady {
        return Err(EconomicControlError::InvalidTransition);
    }
    evidence.validate()?;
    readiness.validate(now)?;
    authorization.validate(readiness, now)?;
    Ok(Transition::Promote {
        phase: EconomicPhase::LiveCanaryMin,
        level: SizeLevel::Min,
    })
}

pub fn evaluate_promotion(
    phase: EconomicPhase,
    level: SizeLevel,
    evidence: &PromotionEvidence,
) -> Result<Transition, EconomicControlError> {
    if phase != level.phase() {
        return Err(EconomicControlError::InvalidTransition);
    }
    evidence.validate()?;
    let Some(next) = level.next() else {
        return Ok(Transition::Stay { phase, level });
    };
    Ok(Transition::Promote {
        phase: next.phase(),
        level: next,
    })
}

pub fn evaluate_failure(level: SizeLevel, signal: FailureSignal, now: DateTime<Utc>) -> Transition {
    match signal {
        FailureSignal::RealizedNegative | FailureSignal::EconomicQualityDegraded => {
            Transition::Cooldown {
                level: level.previous(),
                until: now + Duration::seconds(COOLDOWN_SECONDS),
            }
        }
        FailureSignal::ForkRateBelowMinimum => Transition::RouteDisarm {
            reason: "fork_pass_rate",
        },
        FailureSignal::PredictionErrorAboveMaximum => Transition::RouteDisarm {
            reason: "prediction_error",
        },
        FailureSignal::RepeatedRpcDisagreement => Transition::RouteDisarm {
            reason: "rpc_disagreement",
        },
        FailureSignal::ThreeConsecutiveLosses => Transition::RouteDisarm {
            reason: "maximum_consecutive_losses",
        },
        FailureSignal::DailyLossLimitReached => Transition::GlobalDisarm {
            reason: "daily_loss_budget",
        },
        FailureSignal::UnknownSubmission => Transition::GlobalDisarm {
            reason: "submission_unknown",
        },
        FailureSignal::DuplicateSubmission => Transition::GlobalDisarm {
            reason: "duplicate_submission",
        },
        FailureSignal::NonceInconsistency => Transition::GlobalDisarm {
            reason: "nonce_inconsistency",
        },
        FailureSignal::ReceiptUnreconciled => Transition::GlobalDisarm {
            reason: "receipt_unreconciled",
        },
        FailureSignal::ProcessFatalIntegrity => Transition::GlobalDisarm {
            reason: "process_fatal_integrity",
        },
        FailureSignal::SignerMismatch => Transition::GlobalDisarm {
            reason: "signer_mismatch",
        },
        FailureSignal::ExecutorCodeMismatch => Transition::GlobalDisarm {
            reason: "executor_code_mismatch",
        },
        FailureSignal::OwnerMismatch => Transition::GlobalDisarm {
            reason: "owner_mismatch",
        },
        FailureSignal::FlashProviderMismatch => Transition::GlobalDisarm {
            reason: "flash_provider_mismatch",
        },
        FailureSignal::ContractMismatch => Transition::GlobalDisarm {
            reason: "contract_mismatch",
        },
        FailureSignal::Rollback => Transition::GlobalDisarm { reason: "rollback" },
    }
}

fn rate_bps(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 || numerator > denominator {
        return 0;
    }
    numerator
        .saturating_mul(10_000)
        .checked_div(denominator)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0)
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| is_hex(digest, 64))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EconomicControlError {
    #[error("insufficient supported observations")]
    InsufficientObservations,
    #[error("valid-input acceptance is below the required rate")]
    AcceptanceRate,
    #[error("integrity evidence is incomplete or failed")]
    Integrity,
    #[error("consumer backlog is not bounded")]
    Backlog,
    #[error("stale outbox rows remain")]
    Outbox,
    #[error("independent RPC agreement is not proven")]
    RpcAgreement,
    #[error("evidence is stale")]
    StaleEvidence,
    #[error("fork pass rate is below the required rate")]
    ForkRate,
    #[error("prediction error exceeds the bound")]
    PredictionError,
    #[error("secondary or fork verification was skipped")]
    SkippedVerification,
    #[error("execution activity exists while disarmed")]
    ExecutionWhileDisarmed,
    #[error("no positive independently verified fork candidate exists")]
    NoPositiveCandidate,
    #[error("readiness binding is invalid")]
    InvalidBinding,
    #[error("readiness is expired")]
    ExpiredReadiness,
    #[error("gas reserve or daily-loss state is unsafe")]
    RiskReserve,
    #[error("automation authorization is invalid")]
    Authorization,
    #[error("promotion evidence does not satisfy every gate")]
    PromotionGate,
    #[error("economic transition is invalid")]
    InvalidTransition,
    #[error("size level is invalid")]
    InvalidLevel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-28T00:05:00Z")
            .expect("valid test time")
            .with_timezone(&Utc)
    }

    fn gate() -> EvidenceGate {
        EvidenceGate {
            supported_observations: 100,
            valid_acceptance_bps: 9_990,
            process_fatal_integrity_exits: 0,
            quarantine_progress_proven: true,
            consumer_pending_bounded: true,
            ack_pending_bounded: true,
            stale_outbox_rows: 0,
            primary_rpc_healthy: true,
            secondary_rpc_healthy: true,
            rpc_providers_independent: true,
            eligible_rpc_disagreements: 0,
            maximum_state_age_blocks: 1,
            maximum_quote_age_ms: 2_000,
            maximum_candidate_age_ms: 3_000,
            fork_attempts: 100,
            fork_passes: 95,
            prediction_error_bps: 1_000,
            secondary_skips: 0,
            fork_skips: 0,
            execution_requests: 0,
            active_attempts: 0,
            positive_independent_fork_candidates: 1,
        }
    }

    fn readiness() -> ReadinessBinding {
        let current = now();
        ReadinessBinding {
            release_sha: "a".repeat(40),
            engine_image_digest: format!("sha256:{}", "b".repeat(64)),
            route_fingerprint: "route".to_string(),
            route_universe_hash: "c".repeat(64),
            route_policy_hash: "d".repeat(64),
            risk_policy_hash: "e".repeat(64),
            economic_control_epoch: 1,
            global_control_epoch: 2,
            route_control_epoch: 3,
            executor_code_hash: "f".repeat(64),
            contract_identity_hash: "1".repeat(64),
            wallet_gas_reserve_wei: 2,
            gas_reserve_floor_wei: 1,
            current_daily_loss_wei: 0,
            daily_loss_limit_wei: 1,
            observed_from: current - Duration::minutes(5),
            observed_until: current - Duration::seconds(1),
            created_at: current,
            expires_at: current + Duration::seconds(READINESS_TTL_SECONDS),
            candidate_evidence_hashes: vec!["2".repeat(64)],
        }
    }

    fn authorization() -> AutomationAuthorization {
        AutomationAuthorization {
            route_fingerprint: "route".to_string(),
            route_policy_hash: "d".repeat(64),
            maximum_reviewed_input_wei: MAXIMUM_REVIEWED_INPUT_WEI,
            executor_code_hash: "f".repeat(64),
            release_family: "phoenix-v4".to_string(),
            one_transaction_at_a_time: true,
            reviewed_ladder_only: true,
            automatic_disarm_required: true,
            expires_at: now() + Duration::minutes(5),
        }
    }

    fn promotion() -> PromotionEvidence {
        PromotionEvidence {
            reconciled_outcomes: 20,
            aggregate_realized_net_pnl_wei: 1,
            successful_outcomes: 19,
            fork_attempts: 20,
            fork_passes: 19,
            rpc_disagreements: 0,
            unknown_submissions: 0,
            duplicate_submissions: 0,
            nonce_gaps: 0,
            control_violations: 0,
            unreconciled_receipts: 0,
            process_fatal_integrity_events: 0,
            identity_mismatches: 0,
            prediction_error_bps: 1_000,
            daily_loss_wei: 0,
            daily_loss_limit_wei: 1,
            consecutive_losses: 0,
            wallet_gas_reserve_wei: 2,
            gas_reserve_floor_wei: 1,
            maximum_quote_age_ms: 2_000,
            maximum_candidate_age_ms: 3_000,
        }
    }

    #[test]
    fn exact_ladder_never_exceeds_reviewed_maximum() {
        let levels = [
            SizeLevel::Min,
            SizeLevel::L1,
            SizeLevel::L2,
            SizeLevel::L3,
            SizeLevel::L4,
            SizeLevel::L5,
            SizeLevel::MaxReviewed,
        ];
        assert_eq!(levels[0].amount_wei(), 100_000_000_000_000);
        assert_eq!(
            levels.last().expect("level").amount_wei(),
            MAXIMUM_REVIEWED_INPUT_WEI
        );
        assert!(levels
            .windows(2)
            .all(|pair| pair[0].amount_wei() < pair[1].amount_wei()));
    }

    #[test]
    fn readiness_rejects_each_incomplete_evidence_class() {
        let mut evidence = gate();
        evidence.execution_requests = 1;
        assert_eq!(
            evidence.validate(),
            Err(EconomicControlError::ExecutionWhileDisarmed)
        );
        evidence = gate();
        evidence.secondary_skips = 1;
        assert_eq!(
            evidence.validate(),
            Err(EconomicControlError::SkippedVerification)
        );
        evidence = gate();
        evidence.positive_independent_fork_candidates = 0;
        assert_eq!(
            evidence.validate(),
            Err(EconomicControlError::NoPositiveCandidate)
        );
    }

    #[test]
    fn expired_or_wrong_binding_cannot_activate() {
        let mut stale = readiness();
        stale.expires_at = now();
        assert_eq!(
            activate_canary(
                EconomicPhase::CanaryReady,
                &stale,
                &gate(),
                &authorization(),
                now()
            ),
            Err(EconomicControlError::ExpiredReadiness)
        );
        let mut wrong = authorization();
        wrong.route_policy_hash = "0".repeat(64);
        assert_eq!(
            activate_canary(
                EconomicPhase::CanaryReady,
                &readiness(),
                &gate(),
                &wrong,
                now()
            ),
            Err(EconomicControlError::Authorization)
        );
    }

    #[test]
    fn first_authorized_live_transition_is_exactly_minimum() {
        assert_eq!(
            activate_canary(
                EconomicPhase::CanaryReady,
                &readiness(),
                &gate(),
                &authorization(),
                now()
            ),
            Ok(Transition::Promote {
                phase: EconomicPhase::LiveCanaryMin,
                level: SizeLevel::Min
            })
        );
    }

    #[test]
    fn promotion_requires_twenty_reconciled_positive_outcomes() {
        assert_eq!(
            evaluate_promotion(EconomicPhase::LiveCanaryMin, SizeLevel::Min, &promotion()),
            Ok(Transition::Promote {
                phase: EconomicPhase::LiveScaleL1,
                level: SizeLevel::L1
            })
        );
        let mut failed = promotion();
        failed.reconciled_outcomes = 19;
        assert_eq!(
            evaluate_promotion(EconomicPhase::LiveCanaryMin, SizeLevel::Min, &failed),
            Err(EconomicControlError::PromotionGate)
        );
        failed = promotion();
        failed.aggregate_realized_net_pnl_wei = 0;
        assert_eq!(
            evaluate_promotion(EconomicPhase::LiveCanaryMin, SizeLevel::Min, &failed),
            Err(EconomicControlError::PromotionGate)
        );
    }

    #[test]
    fn negative_result_steps_down_and_cools_down() {
        assert_eq!(
            evaluate_failure(SizeLevel::L3, FailureSignal::RealizedNegative, now()),
            Transition::Cooldown {
                level: SizeLevel::L2,
                until: now() + Duration::seconds(COOLDOWN_SECONDS)
            }
        );
    }

    #[test]
    fn hard_failures_never_rearm() {
        let signals = [
            FailureSignal::ThreeConsecutiveLosses,
            FailureSignal::DailyLossLimitReached,
            FailureSignal::UnknownSubmission,
            FailureSignal::DuplicateSubmission,
            FailureSignal::NonceInconsistency,
            FailureSignal::ReceiptUnreconciled,
            FailureSignal::ProcessFatalIntegrity,
            FailureSignal::SignerMismatch,
            FailureSignal::ExecutorCodeMismatch,
            FailureSignal::OwnerMismatch,
            FailureSignal::FlashProviderMismatch,
            FailureSignal::ContractMismatch,
            FailureSignal::Rollback,
        ];
        for signal in signals {
            assert!(matches!(
                evaluate_failure(SizeLevel::L5, signal, now()),
                Transition::RouteDisarm { .. } | Transition::GlobalDisarm { .. }
            ));
        }
    }
}
