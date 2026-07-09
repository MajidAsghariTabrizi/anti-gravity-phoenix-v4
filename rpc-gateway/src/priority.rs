use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    P0Reconciliation = 0,
    P1PoolState = 1,
    P2Bootstrap = 2,
    P3Metadata = 3,
    P4Dashboard = 4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedRequest {
    pub priority: Priority,
    pub enqueued_seq: u64,
    pub method: String,
    pub cache_key: String,
}

impl Ord for QueuedRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| other.enqueued_seq.cmp(&self.enqueued_seq))
    }
}

impl PartialOrd for QueuedRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
