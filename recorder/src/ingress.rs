use chrono::{DateTime, NaiveDate, Utc};
use money_path_classifier::{ClassificationResult, IngressClassification};
use serde::Serialize;
use serde_json::Value;
use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Notify;

pub const INGRESS_SCHEMA_VERSION: &str = "money_path.ingress.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngressBufferConfig {
    pub flush_interval: Duration,
    pub flush_after_events: usize,
    pub max_samples_per_detail_per_day: usize,
    pub max_sample_json_bytes: usize,
}

impl Default for IngressBufferConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(60),
            flush_after_events: 10_000,
            max_samples_per_detail_per_day: 100,
            max_sample_json_bytes: 1_024,
        }
    }
}

impl IngressBufferConfig {
    pub fn validate(self) -> Result<Self, IngressError> {
        if !(Duration::from_secs(10)..=Duration::from_secs(300)).contains(&self.flush_interval)
            || !(100..=100_000).contains(&self.flush_after_events)
            || !(1..=1_000).contains(&self.max_samples_per_detail_per_day)
            || !(256..=4_096).contains(&self.max_sample_json_bytes)
        {
            return Err(IngressError::Configuration);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct IngressAggregateKey {
    pub bucket_date: NaiveDate,
    pub classification: String,
    pub detail_class: String,
    pub router_kind: String,
    pub wrapper_kind: String,
    pub selector_kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngressAggregate {
    pub key: IngressAggregateKey,
    pub event_count: u64,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IngressSample {
    pub key: IngressAggregateKey,
    pub safe_decoder_summary: Value,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngressFlushBatch {
    pub aggregates: Vec<IngressAggregate>,
    pub samples: Vec<IngressSample>,
}

impl IngressFlushBatch {
    pub fn is_empty(&self) -> bool {
        self.aggregates.is_empty() && self.samples.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordOutcome {
    pub sample_buffered: bool,
    pub sample_limit_reached: bool,
}

#[derive(Clone, Debug)]
pub struct IngressBuffer {
    inner: Arc<Mutex<BufferState>>,
    notify: Arc<Notify>,
    config: IngressBufferConfig,
}

#[derive(Debug, Default)]
struct BufferState {
    aggregates: BTreeMap<IngressAggregateKey, IngressAggregate>,
    samples: BTreeMap<(NaiveDate, String), Vec<IngressSample>>,
    buffered_events: usize,
}

impl IngressBuffer {
    pub fn new(config: IngressBufferConfig) -> Result<Self, IngressError> {
        Ok(Self {
            inner: Arc::new(Mutex::new(BufferState::default())),
            notify: Arc::new(Notify::new()),
            config: config.validate()?,
        })
    }

    pub fn config(&self) -> &IngressBufferConfig {
        &self.config
    }

    pub fn record(
        &self,
        result: &ClassificationResult,
        observed_at: DateTime<Utc>,
    ) -> Result<RecordOutcome, IngressError> {
        let key = aggregate_key(result, observed_at.date_naive())?;
        let mut state = self.inner.lock().map_err(|_| IngressError::Invariant)?;
        let aggregate = state
            .aggregates
            .entry(key.clone())
            .or_insert_with(|| IngressAggregate {
                key: key.clone(),
                event_count: 0,
                first_seen_at: observed_at,
                last_seen_at: observed_at,
            });
        aggregate.event_count = aggregate.event_count.saturating_add(1);
        aggregate.first_seen_at = aggregate.first_seen_at.min(observed_at);
        aggregate.last_seen_at = aggregate.last_seen_at.max(observed_at);
        state.buffered_events = state.buffered_events.saturating_add(1);

        let mut outcome = RecordOutcome::default();
        if result.classification == IngressClassification::UnsupportedInteresting {
            let summary =
                serde_json::to_value(&result.summary).map_err(|_| IngressError::Invariant)?;
            let encoded = serde_json::to_vec(&summary).map_err(|_| IngressError::Invariant)?;
            if encoded.len() > self.config.max_sample_json_bytes {
                return Err(IngressError::SampleOversized);
            }
            let sample_key = (key.bucket_date, key.detail_class.clone());
            let samples = state.samples.entry(sample_key).or_default();
            if samples.len() < self.config.max_samples_per_detail_per_day {
                samples.push(IngressSample {
                    key,
                    safe_decoder_summary: summary,
                    observed_at,
                });
                outcome.sample_buffered = true;
            } else {
                outcome.sample_limit_reached = true;
            }
        }
        if state.buffered_events >= self.config.flush_after_events {
            self.notify.notify_one();
        }
        Ok(outcome)
    }

    pub async fn wait_for_flush_request(&self) {
        self.notify.notified().await;
    }

    pub fn take(&self) -> Result<IngressFlushBatch, IngressError> {
        let mut state = self.inner.lock().map_err(|_| IngressError::Invariant)?;
        let drained = std::mem::take(&mut *state);
        Ok(IngressFlushBatch {
            aggregates: drained.aggregates.into_values().collect(),
            samples: drained.samples.into_values().flatten().collect(),
        })
    }

    pub fn restore(&self, batch: IngressFlushBatch) -> Result<(), IngressError> {
        let mut state = self.inner.lock().map_err(|_| IngressError::Invariant)?;
        for incoming in batch.aggregates {
            let count = incoming.event_count;
            match state.aggregates.entry(incoming.key.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(incoming);
                }
                Entry::Occupied(mut entry) => {
                    let aggregate = entry.get_mut();
                    aggregate.event_count = aggregate.event_count.saturating_add(count);
                    aggregate.first_seen_at =
                        aggregate.first_seen_at.min(incoming.first_seen_at);
                    aggregate.last_seen_at = aggregate.last_seen_at.max(incoming.last_seen_at);
                }
            }
            state.buffered_events = state.buffered_events.saturating_add(count as usize);
        }
        for sample in batch.samples {
            let sample_key = (sample.key.bucket_date, sample.key.detail_class.clone());
            let samples = state.samples.entry(sample_key).or_default();
            if samples.len() < self.config.max_samples_per_detail_per_day {
                samples.push(sample);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize) {
        let state = self.inner.lock().unwrap();
        (
            state.aggregates.len(),
            state.samples.values().map(Vec::len).sum(),
        )
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum IngressError {
    #[error("money-path ingress buffer configuration is invalid")]
    Configuration,
    #[error("money-path ingress evidence exceeded its safe bound")]
    SampleOversized,
    #[error("money-path ingress evidence invariant failed")]
    Invariant,
}

fn aggregate_key(
    result: &ClassificationResult,
    bucket_date: NaiveDate,
) -> Result<IngressAggregateKey, IngressError> {
    if result.detail_class.is_empty() || result.detail_class.len() > 128 {
        return Err(IngressError::Invariant);
    }
    Ok(IngressAggregateKey {
        bucket_date,
        classification: result.classification.as_str().to_string(),
        detail_class: result.detail_class.to_string(),
        router_kind: result
            .summary
            .router_kind
            .map(|kind| kind.as_str())
            .unwrap_or("none")
            .to_string(),
        wrapper_kind: result.summary.wrapper_kind.as_str().to_string(),
        selector_kind: result.summary.outer_selector_kind.as_str().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use money_path_classifier::{
        DecodedSwapKind, OuterSelectorKind, RouterKind, SafeDecoderSummary, UnsupportedReason,
        WrapperKind,
    };

    fn result(classification: IngressClassification) -> ClassificationResult {
        ClassificationResult {
            classification,
            detail_class: "known_router_unsupported_exact_output",
            summary: SafeDecoderSummary {
                router_kind: Some(RouterKind::LegacySwapRouter),
                outer_selector_kind: OuterSelectorKind::LegacyExactOutputSingle,
                wrapper_kind: WrapperKind::Direct,
                decoded_swap_kind: DecodedSwapKind::None,
                unsupported_reason: UnsupportedReason::ExactOutput,
                command_count: 1,
                v3_hop_count: 0,
                reviewed_pool_matches: 0,
            },
        }
    }

    #[test]
    fn unsupported_samples_are_bounded_and_raw_evidence_is_absent() {
        let buffer = IngressBuffer::new(IngressBufferConfig {
            max_samples_per_detail_per_day: 2,
            ..IngressBufferConfig::default()
        })
        .unwrap();
        let observed = DateTime::parse_from_rfc3339("2026-07-18T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            buffer
                .record(
                    &result(IngressClassification::UnsupportedInteresting),
                    observed
                )
                .unwrap()
                .sample_buffered
        );
        assert!(
            buffer
                .record(
                    &result(IngressClassification::UnsupportedInteresting),
                    observed
                )
                .unwrap()
                .sample_buffered
        );
        assert!(
            buffer
                .record(
                    &result(IngressClassification::UnsupportedInteresting),
                    observed
                )
                .unwrap()
                .sample_limit_reached
        );
        assert_eq!(buffer.counts(), (1, 2));

        let encoded = serde_json::to_string(&buffer.take().unwrap().samples).unwrap();
        for forbidden in [
            "tx_hash",
            "raw_tx",
            "calldata",
            "0x111111",
            "postgres://",
            "http://",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn aggregate_keys_are_low_cardinality_and_failed_flush_can_be_restored() {
        let buffer = IngressBuffer::new(IngressBufferConfig::default()).unwrap();
        let observed = Utc::now();
        for _ in 0..10 {
            buffer
                .record(&result(IngressClassification::Irrelevant), observed)
                .unwrap();
        }
        let batch = buffer.take().unwrap();
        assert_eq!(batch.aggregates.len(), 1);
        assert_eq!(batch.aggregates[0].event_count, 10);
        buffer.restore(batch).unwrap();
        assert_eq!(buffer.counts(), (1, 0));
    }

    #[test]
    fn configuration_bounds_flush_and_sample_memory() {
        assert!(IngressBufferConfig::default().validate().is_ok());
        assert!(IngressBufferConfig {
            flush_interval: Duration::from_secs(9),
            ..IngressBufferConfig::default()
        }
        .validate()
        .is_err());
        assert!(IngressBufferConfig {
            max_samples_per_detail_per_day: 1_001,
            ..IngressBufferConfig::default()
        }
        .validate()
        .is_err());
    }
}
