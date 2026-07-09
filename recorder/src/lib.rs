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
mod tests {
    use super::*;

    #[test]
    fn records_feed_events_in_order() {
        let mut buffer = RecorderBuffer::default();
        buffer.push(RecordedFeedEvent {
            sequence: 1,
            payload: "{}".to_string(),
        });
        assert_eq!(buffer.len(), 1);
    }
}
