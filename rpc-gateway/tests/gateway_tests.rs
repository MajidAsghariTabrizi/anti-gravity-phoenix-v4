use std::time::{Duration, Instant};

use rpc_gateway::budget::GlobalBudget;
use rpc_gateway::cache::TtlCache;
use rpc_gateway::coalescer::{CoalesceDecision, Coalescer};
use rpc_gateway::providers::{CircuitState, Provider, ProviderPool};

#[test]
fn global_budget_rejects_after_capacity() {
    let now = Instant::now();
    let mut budget = GlobalBudget::new(2, Duration::from_secs(1), now);
    assert!(budget.admit(now));
    assert!(budget.admit(now));
    assert!(!budget.admit(now));
}

#[test]
fn cache_honors_ttl() {
    let now = Instant::now();
    let mut cache = TtlCache::default();
    cache.insert(
        "eth_chainId".to_string(),
        "0xa4b1".to_string(),
        Duration::from_secs(1),
        now,
    );
    assert_eq!(cache.get("eth_chainId", now), Some("0xa4b1".to_string()));
    assert_eq!(cache.get("eth_chainId", now + Duration::from_secs(2)), None);
}

#[test]
fn coalescer_marks_followers() {
    let mut c = Coalescer::default();
    assert_eq!(c.enter("eth_getCode:abc"), CoalesceDecision::Leader);
    assert_eq!(c.enter("eth_getCode:abc"), CoalesceDecision::Follower);
    assert_eq!(c.finish("eth_getCode:abc"), 1);
}

#[test]
fn provider_circuit_opens_after_failures() {
    let now = Instant::now();
    let mut provider = Provider::new("p1".to_string(), "https://example".to_string(), 1, now);
    provider.record_failure(now);
    provider.record_failure(now);
    provider.record_failure(now);
    assert!(matches!(provider.circuit, CircuitState::Open { .. }));
}

#[test]
fn pool_selects_available_provider() {
    let now = Instant::now();
    let p = Provider::new("p1".to_string(), "https://example".to_string(), 1, now);
    let mut pool = ProviderPool::new(vec![p]);
    assert!(pool.choose(now).is_some());
}
