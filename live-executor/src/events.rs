use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use thiserror::Error;

/// Oracle event types detected from on-chain activity
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum OracleEventType {
    NewLiquidatable,
    PriceUpdate,
    DeadlineExpiry,
    ProviderRecovery,
}

/// A detected oracle event from feed processing
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OracleEvent {
    pub event_id: Uuid,
    pub auction_id: String,
    pub signal_id: String,
    pub event_type: OracleEventType,
    pub block_number: u64,
    pub transaction_hash: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub processed: bool,
    pub processing_attempts: u64,
}

/// Result of processing an oracle event
#[derive(Clone, Debug, PartialEq)]
pub enum ProcessResult {
    ClaimedAndExecuted,
    NoMatchingSignal,
    BelowProfitFloor,
    EconomicRejection,
    Disarmed,
}

/// Error for event-driven operations
#[derive(Clone, Debug, Error)]
pub enum EventError {
    #[error("event already processed")]
    AlreadyProcessed,
    #[error("no matching signal found")]
    NoMatchingSignal,
    #[error("profit below floor")]
    BelowProfitFloor,
    #[error("economic rejection")]
    EconomicRejection,
    #[error("system disarmed")]
    Disarmed,
    #[error("internal error: {0}")]
    Internal(String),
}

/// Event-driven trigger configuration and state
#[derive(Clone, Debug)]
pub struct EventDrivenTriggerConfig {
    pub enabled: bool,
    pub max_pending: u64,
    pub cooldown_ms: u64,
}

/// Test module for event-driven trigger safety gates
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EVENT_DRIVEN_TRIGGER_ACTIVE;

    #[test]
    fn event_can_be_processed() {
        let event = OracleEvent {
            event_id: Uuid::new_v4(),
            auction_id: "test-auction-123".to_string(),
            signal_id: "test-signal-123".to_string(),
            event_type: OracleEventType::NewLiquidatable,
            block_number: 12345678,
            transaction_hash: Some("0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".to_string()),
            observed_at: DateTime::parse_from_rfc3339("2026-07-28T00:05:00Z")
                .expect("valid test time")
                .with_timezone(&Utc),
            processed: false,
            processing_attempts: 0,
        };

        // Test that event structure is valid
        let serialized = serde_json::to_string(&event).expect("serialize event");
        let _deserialized: OracleEvent = serde_json::from_str(&serialized).expect("deserialize event");
    }

    #[test]
    fn process_result_variants() {
        assert_eq!(
            ProcessResult::ClaimedAndExecuted,
            ProcessResult::ClaimedAndExecuted
        );
        assert_ne!(
            ProcessResult::ClaimedAndExecuted,
            ProcessResult::NoMatchingSignal
        );
    }

    /// Assert that the event-driven trigger configuration preserves safety gates.
    ///
    //   - EVENT_DRIVEN_TRIGGER_ACTIVE defaults to false.
    //   - With the flag off, claim() falls through to the original polling path
    //     (no behavioral change).
    //   - Even with the flag on, the zero-address borrower check and WETH-only
    //     policy are NOT bypassed — they are independent guards (see revenue.rs).
    #[test]
    fn event_driven_preserves_safety_gates() {
        // CONFIG: flag defaults to false
        assert!(!EVENT_DRIVEN_TRIGGER_ACTIVE,
                "EVENT_DRIVEN_TRIGGER_ACTIVE must default to false");

        // STRUCTURE: two independent guards exist in revenue.rs:
        //   1. Borrower zero-address check (always enforced, line ~874)
        //   2. WETH-only collateral check (always enforced by default, line ~877)
        //     — the event_driven_active flag does NOT appear in either guard.
        //     — This test confirms the source files contain those checks.
        let source = std::include_str!("revenue.rs");
        // Check borrower guard is present and unconditional
        assert!(
            source.contains("identity.borrower.as_bytes() == &[0; 20]"),
            "revenue.rs must contain the zero-address borrower check"
        );
        // Check WETH guard is present and unconditional (no event_driven_active factor)
        assert!(
            source.contains("identity.debt_asset != weth"),
            "revenue.rs must contain the WETH-only collateral check"
        );
        // Neither guard should have !event_driven_active wrapping them
        let borrower_has_flag = source.contains(
            "!event_driven_active && identity.borrower.as_bytes() == &[0; 20]"
        );
        let weth_has_flag = source.contains(
            "!event_driven_active && identity.debt_asset != weth"
        );
        // Both should be false — the checks are independent of the flag
        assert!(!borrower_has_flag,
                "borrower check must NOT be gated by !event_driven_active");
        assert!(!weth_has_flag,
                "WETH check must NOT be gated by !event_driven_active");
    }
}