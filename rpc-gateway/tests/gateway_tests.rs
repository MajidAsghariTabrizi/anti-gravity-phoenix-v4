use std::time::{Duration, Instant};

use rpc_gateway::budget::GlobalBudget;
use rpc_gateway::cache::TtlCache;
use rpc_gateway::coalescer::{CoalesceDecision, Coalescer};
use rpc_gateway::providers::{
    parse_provider_config, CircuitState, Provider, ProviderConfigError, ProviderPool,
};

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
fn cache_evicts_deterministically_at_its_configured_bound() {
    let now = Instant::now();
    let mut cache = TtlCache::new(2);
    cache.insert(
        "old".to_string(),
        "1".to_string(),
        Duration::from_secs(1),
        now,
    );
    cache.insert(
        "new".to_string(),
        "2".to_string(),
        Duration::from_secs(2),
        now,
    );
    cache.insert(
        "newest".to_string(),
        "3".to_string(),
        Duration::from_secs(3),
        now,
    );
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.get("old", now), None);
    assert_eq!(cache.get("new", now), Some("2".to_string()));
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

#[test]
fn highest_priority_healthy_provider_is_selected() {
    let now = Instant::now();
    let mut pool = priority_pool(now);
    assert_eq!(pool.choose(now).map(|p| p.name.as_str()), Some("quicknode"));
}

#[test]
fn preferred_provider_failure_selects_drpc() {
    let now = Instant::now();
    let mut pool = priority_pool(now);
    pool.choose(now).unwrap().record_failure(now);
    assert_eq!(pool.choose(now).map(|p| p.name.as_str()), Some("drpc"));
}

#[test]
fn preferred_and_drpc_failure_selects_publicnode() {
    let now = Instant::now();
    let mut pool = priority_pool(now);
    pool.choose(now).unwrap().record_failure(now);
    pool.choose(now).unwrap().record_failure(now);
    assert_eq!(
        pool.choose(now).map(|p| p.name.as_str()),
        Some("publicnode")
    );
}

#[test]
fn only_emergency_provider_healthy_selects_arbitrum_public_rpc() {
    let now = Instant::now();
    let mut pool = priority_pool(now);
    pool.choose(now).unwrap().record_failure(now);
    pool.choose(now).unwrap().record_failure(now);
    pool.choose(now).unwrap().record_failure(now);
    assert_eq!(
        pool.choose(now).map(|p| p.name.as_str()),
        Some("arbitrum_public")
    );
}

#[test]
fn circuit_open_provider_is_skipped() {
    let now = Instant::now();
    let mut quicknode = Provider::new(
        "quicknode".to_string(),
        "https://example.quiknode.pro/token".to_string(),
        4,
        now,
    );
    quicknode.circuit = CircuitState::Open {
        until: now + Duration::from_secs(30),
    };
    let drpc = Provider::new(
        "drpc".to_string(),
        "https://arbitrum.drpc.org".to_string(),
        3,
        now,
    );
    let mut pool = ProviderPool::new(vec![quicknode, drpc]);
    assert_eq!(pool.choose(now).map(|p| p.name.as_str()), Some("drpc"));
}

#[test]
fn cooldown_provider_is_skipped() {
    let now = Instant::now();
    let mut pool = priority_pool(now);
    pool.choose(now).unwrap().record_failure(now);
    assert_eq!(pool.choose(now).map(|p| p.name.as_str()), Some("drpc"));
}

#[test]
fn recovered_preferred_provider_becomes_selectable_again() {
    let now = Instant::now();
    let mut pool = priority_pool(now);
    pool.choose(now).unwrap().record_failure(now);
    assert_eq!(pool.choose(now).map(|p| p.name.as_str()), Some("drpc"));

    let recovered_at = now + Duration::from_secs(2);
    assert_eq!(
        pool.choose(recovered_at).map(|p| p.name.as_str()),
        Some("quicknode")
    );
}

#[test]
fn equal_priorities_preserve_configured_order() {
    let now = Instant::now();
    let cfg =
        parse_provider_config("https://first.example,https://second.example", "1,1", "5").unwrap();
    let mut pool = cfg.into_pool(now);
    assert_eq!(
        pool.choose(now).map(|p| p.name.as_str()),
        Some("provider_0")
    );
}

#[test]
fn url_priority_count_mismatch_fails() {
    let err = parse_provider_config("https://first.example,https://second.example", "1", "5")
        .unwrap_err();
    assert_eq!(
        err,
        ProviderConfigError::CountMismatch {
            urls: 2,
            priorities: 1
        }
    );
}

#[test]
fn invalid_priority_fails() {
    let err = parse_provider_config("https://first.example", "not-a-number", "5").unwrap_err();
    assert_eq!(err, ProviderConfigError::InvalidPriority { index: 0 });
}

#[test]
fn negative_priority_fails() {
    let err = parse_provider_config("https://first.example", "-1", "5").unwrap_err();
    assert_eq!(err, ProviderConfigError::InvalidPriority { index: 0 });
}

#[test]
fn zero_priority_fails() {
    let err = parse_provider_config("https://first.example", "0", "5").unwrap_err();
    assert_eq!(err, ProviderConfigError::ZeroPriority { index: 0 });
}

#[test]
fn malformed_global_rps_fails() {
    let err = parse_provider_config("https://first.example", "1", "fast").unwrap_err();
    assert_eq!(err, ProviderConfigError::InvalidGlobalRps);
}

#[test]
fn zero_global_rps_fails() {
    let err = parse_provider_config("https://first.example", "1", "0").unwrap_err();
    assert_eq!(err, ProviderConfigError::ZeroGlobalRps);
}

#[test]
fn provider_names_and_errors_do_not_expose_credential_bearing_urls() {
    let credential_url = "https://secret-token.quiknode.pro/credential/path?api_key=hidden";
    let cfg = parse_provider_config(credential_url, "4", "5").unwrap();
    assert_eq!(cfg.providers[0].name, "quicknode");
    assert!(!cfg.providers[0].name.contains("secret-token"));
    assert!(!cfg.providers[0].name.contains("api_key"));

    let err = parse_provider_config(credential_url, "0", "5")
        .unwrap_err()
        .to_string();
    assert!(!err.contains("secret-token"));
    assert!(!err.contains("api_key"));
    assert!(!err.contains(credential_url));

    let rendered = format!("{cfg:?}");
    assert!(!rendered.contains("secret-token"));
    assert!(!rendered.contains("api_key"));
}

fn priority_pool(now: Instant) -> ProviderPool {
    parse_provider_config(
        "https://secret-token.quiknode.pro/credential/path,https://arbitrum.drpc.org,https://arbitrum-one-rpc.publicnode.com,https://arb1.arbitrum.io/rpc",
        "4,3,1,1",
        "5",
    )
    .unwrap()
    .into_pool(now)
}
