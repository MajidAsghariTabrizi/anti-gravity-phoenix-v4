use crate::domain::Address;
use crate::engine_input::decode_engine_input;
use crate::origin::{
    OriginClassification, OriginDetector, OriginEvidence, RouterKind, UnsupportedReason,
    REVIEWED_ROUTER_ADDRESSES,
};
use crate::shadow_processor::RouteRegistry;
use phoenix_recorder::model::{decode_message, engine_event_identity, ENGINE_INPUT_SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use thiserror::Error;

pub const POSTGRES_FEED_EVENT_SOURCE: &str = "postgresql.feed_events";
pub const POSITIVE_ROUTE_EVIDENCE_FOUND: &str = "POSITIVE_ROUTE_EVIDENCE_FOUND";
pub const POSITIVE_ROUTE_EVIDENCE_NOT_FOUND: &str = "POSITIVE_ROUTE_EVIDENCE_NOT_FOUND";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StoredTransactionEvidence {
    pub provenance: TransactionProvenance,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransactionProvenance {
    pub source: String,
    pub feed_event_id: i64,
    pub recorded_at: String,
    pub source_block_number: Option<u64>,
    pub source_block_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PositiveRouteSummary {
    pub transaction_hash: String,
    pub source_sequence: u64,
    pub source_block_number: Option<u64>,
    pub source_block_hash: Option<String>,
    pub router_address: Option<String>,
    pub router_kind: Option<String>,
    pub selector: Option<String>,
    pub command_family: Vec<String>,
    pub supported: bool,
    pub exact_input: Option<bool>,
    pub exact_output: bool,
    pub decoded_token_path: Vec<String>,
    pub decoded_fee_path: Vec<u32>,
    pub decoded_pool_ids: Vec<String>,
    pub affected_configured_pool_ids: Vec<String>,
    pub matched_route_ids: Vec<String>,
    pub matched_route_fingerprints: Vec<String>,
    pub route_match_result: String,
    pub rejection_detail_class: Option<String>,
    pub candidate_count: usize,
    pub candidate_produced: bool,
    pub production_evidence: bool,
    pub shadow_only: bool,
    pub execution_request_created: bool,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct DiscoveryStatistics {
    pub inspected_transactions: u64,
    pub router_counts: BTreeMap<String, u64>,
    pub selector_counts: BTreeMap<String, u64>,
    pub result_counts: BTreeMap<String, u64>,
    pub candidate_count: u64,
    pub production_candidate_count: u64,
}

impl DiscoveryStatistics {
    pub fn observe(&mut self, summary: &PositiveRouteSummary) {
        self.inspected_transactions = self.inspected_transactions.saturating_add(1);
        increment(
            &mut self.router_counts,
            summary
                .router_kind
                .as_deref()
                .unwrap_or("unreviewed_router"),
        );
        increment(
            &mut self.selector_counts,
            summary
                .selector
                .as_deref()
                .unwrap_or("selector_unavailable"),
        );
        increment(&mut self.result_counts, &summary.route_match_result);
        if summary.candidate_produced {
            self.candidate_count = self.candidate_count.saturating_add(1);
        }
        if summary.production_evidence {
            self.production_candidate_count = self.production_candidate_count.saturating_add(1);
        }
    }

    pub fn terminal_result(&self) -> &'static str {
        if self.production_candidate_count > 0 {
            POSITIVE_ROUTE_EVIDENCE_FOUND
        } else {
            POSITIVE_ROUTE_EVIDENCE_NOT_FOUND
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EvidenceAnalysisError {
    #[error("stored transaction evidence is not a valid normalized event")]
    InvalidStoredPayload,
    #[error("stored transaction evidence failed the Engine input contract")]
    InvalidEngineInput,
    #[error("reviewed router configuration is invalid")]
    InvalidRouterConfiguration,
}

pub fn analyze_stored_transaction(
    stored: &StoredTransactionEvidence,
    routes: &RouteRegistry,
) -> Result<PositiveRouteSummary, EvidenceAnalysisError> {
    let payload = serde_json::to_vec(&stored.payload)
        .map_err(|_| EvidenceAnalysisError::InvalidStoredPayload)?;
    let validated =
        decode_message(&payload).map_err(|_| EvidenceAnalysisError::InvalidStoredPayload)?;
    let identity = engine_event_identity(&validated.tx);
    let input = decode_engine_input(
        &payload,
        Some(ENGINE_INPUT_SCHEMA_VERSION),
        Some(&identity),
        validated.tx.sequence,
    )
    .map_err(|_| EvidenceAnalysisError::InvalidEngineInput)?;
    let detector = OriginDetector::new(reviewed_router_addresses()?)
        .map_err(|_| EvidenceAnalysisError::InvalidRouterConfiguration)?;
    let router_address = input
        .normalized
        .to
        .as_ref()
        .map(|address| address.as_str().to_string());
    let router_kind = input
        .normalized
        .to
        .as_ref()
        .and_then(crate::origin::reviewed_router_kind)
        .map(router_kind_label)
        .map(str::to_string);
    let selector = bounded_selector(&input.normalized.calldata);
    let trusted_source = trusted_provenance(&stored.provenance);

    let mut summary = PositiveRouteSummary {
        transaction_hash: input.identity.tx_hash.clone(),
        source_sequence: input.identity.source_sequence,
        source_block_number: stored.provenance.source_block_number,
        source_block_hash: stored.provenance.source_block_hash.clone(),
        router_address,
        router_kind,
        selector,
        command_family: Vec::new(),
        supported: false,
        exact_input: None,
        exact_output: false,
        decoded_token_path: Vec::new(),
        decoded_fee_path: Vec::new(),
        decoded_pool_ids: Vec::new(),
        affected_configured_pool_ids: Vec::new(),
        matched_route_ids: Vec::new(),
        matched_route_fingerprints: Vec::new(),
        route_match_result: "irrelevant_origin".to_string(),
        rejection_detail_class: Some("irrelevant_origin".to_string()),
        candidate_count: 0,
        candidate_produced: false,
        production_evidence: false,
        shadow_only: true,
        execution_request_created: false,
    };

    match detector.classify(&input.normalized) {
        OriginClassification::SupportedSwapOrigin(origin) => {
            let matched = routes.affected_routes(&origin.candidate_touched_pools);
            let configured_pool_ids = matched
                .iter()
                .flat_map(|route| route.route.legs.iter())
                .map(|leg| leg.pool_id.0.as_str())
                .collect::<HashSet<_>>();
            summary.command_family = origin.decoded_commands;
            summary.supported = true;
            summary.exact_input = Some(origin.exact_in);
            summary.decoded_token_path = origin
                .swap_path
                .iter()
                .map(|token| token.0.as_str().to_string())
                .collect();
            summary.decoded_fee_path = origin
                .candidate_touched_pools
                .iter()
                .filter_map(|pool| pool.0.rsplit(':').next()?.parse().ok())
                .collect();
            summary.decoded_pool_ids = origin
                .candidate_touched_pools
                .iter()
                .map(|pool| pool.0.clone())
                .collect();
            summary.affected_configured_pool_ids = origin
                .candidate_touched_pools
                .iter()
                .filter(|pool| configured_pool_ids.contains(pool.0.as_str()))
                .map(|pool| pool.0.clone())
                .collect();
            summary.matched_route_ids = matched
                .iter()
                .map(|route| route.route.route_id.0.clone())
                .collect();
            summary.matched_route_fingerprints = matched
                .iter()
                .map(|route| route.fingerprint.clone())
                .collect();
            summary.candidate_count = matched.len();
            summary.candidate_produced = !matched.is_empty();
            if summary.candidate_produced {
                summary.route_match_result = "matched_configured_two_pool_route".to_string();
                summary.rejection_detail_class = None;
                summary.production_evidence = trusted_source;
            } else {
                summary.route_match_result = "decoded_but_irrelevant_pool".to_string();
                summary.rejection_detail_class = Some("no_affected_two_pool_route".to_string());
            }
        }
        OriginClassification::KnownRouterUnsupportedCommand(evidence) => {
            apply_unsupported(&mut summary, &evidence);
        }
        OriginClassification::PossibleAggregator => {
            summary.route_match_result = "unsupported_router".to_string();
            summary.rejection_detail_class = Some("possible_aggregator".to_string());
        }
        OriginClassification::Irrelevant => {}
        OriginClassification::Malformed(evidence) => {
            summary.command_family = vec![enum_label(&evidence.outer_selector_kind)];
            summary.exact_input = evidence.exact_in;
            summary.route_match_result = "malformed_calldata".to_string();
            summary.rejection_detail_class = Some("malformed_origin_calldata".to_string());
        }
    }
    Ok(summary)
}

fn apply_unsupported(summary: &mut PositiveRouteSummary, evidence: &OriginEvidence) {
    summary.command_family = vec![enum_label(&evidence.outer_selector_kind)];
    summary.exact_input = evidence.exact_in;
    summary.exact_output = evidence.unsupported_reason == UnsupportedReason::ExactOutput;
    summary.route_match_result = match evidence.unsupported_reason {
        UnsupportedReason::ExactOutput => "unsupported_exact_output",
        _ => "unsupported_command",
    }
    .to_string();
    summary.rejection_detail_class = Some(
        match evidence.unsupported_reason {
            UnsupportedReason::ExactOutput => "known_router_unsupported_exact_output",
            UnsupportedReason::AmbiguousMultiSwap => "known_router_ambiguous_multi_swap",
            _ => "known_router_unsupported_command",
        }
        .to_string(),
    );
}

fn reviewed_router_addresses() -> Result<Vec<Address>, EvidenceAnalysisError> {
    REVIEWED_ROUTER_ADDRESSES
        .iter()
        .map(|value| {
            Address::parse(value).map_err(|_| EvidenceAnalysisError::InvalidRouterConfiguration)
        })
        .collect()
}

fn trusted_provenance(provenance: &TransactionProvenance) -> bool {
    provenance.source == POSTGRES_FEED_EVENT_SOURCE
        && provenance.feed_event_id > 0
        && (10..=64).contains(&provenance.recorded_at.len())
        && !provenance.recorded_at.chars().any(char::is_control)
}

fn bounded_selector(calldata: &str) -> Option<String> {
    if calldata.len() < 10
        || !calldata.starts_with("0x")
        || !calldata[2..10]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(calldata[..10].to_string())
}

fn router_kind_label(kind: RouterKind) -> &'static str {
    match kind {
        RouterKind::LegacySwapRouter => "legacy_swap_router",
        RouterKind::SwapRouter02 => "swap_router02",
        RouterKind::UniversalRouter => "universal_router",
    }
}

fn enum_label(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn increment(counts: &mut BTreeMap<String, u64>, key: &str) {
    let value = counts.entry(key.to_string()).or_default();
    *value = value.saturating_add(1);
}
