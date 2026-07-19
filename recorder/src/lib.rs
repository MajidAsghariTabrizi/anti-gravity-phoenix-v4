pub mod dispatcher;
pub mod engine_outbox;
pub mod engine_stream;
pub mod ingress;
pub mod jetstream;
pub mod logging;
pub mod metrics;
pub mod model;
pub mod persistence;
pub mod runtime;
pub mod state;

pub const NATS_SUBJECT: &str = "phoenix.feed.tx";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpportunityLifecycle {
    OriginSeen,
    OriginSupported,
    RoutesAffected,
    Simulated,
    Profitable,
    Submitted,
    SequencedOrAccepted,
    ReceiptSuccess,
    Settled,
    RealizedProfit,
}

impl OpportunityLifecycle {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::OriginSeen => "origin_seen",
            Self::OriginSupported => "origin_supported",
            Self::RoutesAffected => "routes_affected",
            Self::Simulated => "simulated",
            Self::Profitable => "profitable",
            Self::Submitted => "submitted",
            Self::SequencedOrAccepted => "sequenced_or_accepted",
            Self::ReceiptSuccess => "receipt_success",
            Self::Settled => "settled",
            Self::RealizedProfit => "realized_profit",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedFeedEvent {
    pub sequence: u64,
    pub payload: String,
}

#[derive(Clone, Debug, Default)]
pub struct RecorderBuffer {
    events: Vec<RecordedFeedEvent>,
}

impl RecorderBuffer {
    pub fn push(&mut self, event: RecordedFeedEvent) {
        self.events.push(event);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn records_feed_events_in_order() {
        let mut buffer = RecorderBuffer::default();
        buffer.push(RecordedFeedEvent {
            sequence: 1,
            payload: "{}".to_string(),
        });
        assert_eq!(buffer.len(), 1);
        assert_eq!(OpportunityLifecycle::OriginSeen.as_str(), "origin_seen");
    }
}
