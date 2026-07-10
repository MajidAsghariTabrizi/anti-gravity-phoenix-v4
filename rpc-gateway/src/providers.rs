use std::fmt;
use std::time::{Duration, Instant};

use crate::budget::TokenBucket;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open { until: Instant },
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub name: String,
    pub url: String,
    pub weight: u32,
    pub health_score: i32,
    pub circuit: CircuitState,
    pub cooldown_until: Option<Instant>,
    pub bucket: TokenBucket,
    pub consecutive_failures: u32,
}

impl Provider {
    pub fn new(name: String, url: String, weight: u32, now: Instant) -> Self {
        Self {
            name,
            url,
            weight,
            health_score: 100,
            circuit: CircuitState::Closed,
            bucket: TokenBucket::new(weight, Duration::from_secs(1), now),
            cooldown_until: None,
            consecutive_failures: 0,
        }
    }

    pub fn available(&mut self, now: Instant) -> bool {
        if !self.refresh_eligibility(now) {
            return false;
        }
        self.bucket.refill(now);
        self.bucket.available() > 0
    }

    pub fn reserve(&mut self, now: Instant) -> bool {
        if !self.refresh_eligibility(now) {
            return false;
        }
        self.bucket.try_take(now)
    }

    fn refresh_eligibility(&mut self, now: Instant) -> bool {
        match self.circuit {
            CircuitState::Open { until } if now < until => return false,
            CircuitState::Open { .. } => self.circuit = CircuitState::Closed,
            CircuitState::Closed => {}
        }
        if let Some(until) = self.cooldown_until {
            if now < until {
                return false;
            }
            self.cooldown_until = None;
        }
        true
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.health_score = (self.health_score + 1).min(100);
        self.cooldown_until = None;
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures += 1;
        self.health_score = (self.health_score - 20).max(0);
        let backoff_secs = (1_u64 << self.consecutive_failures.saturating_sub(1).min(5)).min(30);
        self.cooldown_until = Some(now + Duration::from_secs(backoff_secs));
        if self.consecutive_failures >= 3 {
            self.circuit = CircuitState::Open {
                until: now + Duration::from_secs(30),
            };
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderPool {
    providers: Vec<Provider>,
}

impl ProviderPool {
    pub fn new(providers: Vec<Provider>) -> Self {
        Self { providers }
    }

    pub fn choose(&mut self, now: Instant) -> Option<&mut Provider> {
        let mut best_idx: Option<usize> = None;
        let mut best_weight = 0;
        for (idx, provider) in self.providers.iter_mut().enumerate() {
            if !provider.available(now) {
                continue;
            }
            if best_idx.is_none() || provider.weight > best_weight {
                best_weight = provider.weight;
                best_idx = Some(idx);
            }
        }
        if let Some(idx) = best_idx {
            if self.providers[idx].reserve(now) {
                return Some(&mut self.providers[idx]);
            }
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    pub providers: Vec<ProviderSpec>,
    pub global_rps: u32,
}

impl ProviderConfig {
    pub fn into_pool(self, now: Instant) -> ProviderPool {
        ProviderPool::new(
            self.providers
                .into_iter()
                .map(|provider| Provider::new(provider.name, provider.url, provider.priority, now))
                .collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSpec {
    pub name: String,
    pub url: String,
    pub priority: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderConfigError {
    EmptyProviderList,
    EmptyProviderUrl { index: usize },
    InvalidProviderUrl { index: usize },
    EmptyPriorityList,
    CountMismatch { urls: usize, priorities: usize },
    InvalidPriority { index: usize },
    ZeroPriority { index: usize },
    InvalidGlobalRps,
    ZeroGlobalRps,
}

impl fmt::Display for ProviderConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProviderList => write!(f, "RPC provider list is empty"),
            Self::EmptyProviderUrl { index } => {
                write!(f, "RPC provider URL at index {index} is empty")
            }
            Self::InvalidProviderUrl { index } => {
                write!(f, "RPC provider URL at index {index} must be http(s)")
            }
            Self::EmptyPriorityList => write!(f, "RPC provider priority list is empty"),
            Self::CountMismatch { urls, priorities } => write!(
                f,
                "RPC provider URL count ({urls}) does not match priority count ({priorities})"
            ),
            Self::InvalidPriority { index } => {
                write!(f, "RPC provider priority at index {index} is invalid")
            }
            Self::ZeroPriority { index } => {
                write!(
                    f,
                    "RPC provider priority at index {index} must be greater than zero"
                )
            }
            Self::InvalidGlobalRps => write!(f, "RPC_GLOBAL_RPS must be a positive integer"),
            Self::ZeroGlobalRps => write!(f, "RPC_GLOBAL_RPS must be greater than zero"),
        }
    }
}

impl std::error::Error for ProviderConfigError {}

pub fn parse_provider_config(
    urls: &str,
    priorities: &str,
    global_rps: &str,
) -> Result<ProviderConfig, ProviderConfigError> {
    if urls.trim().is_empty() {
        return Err(ProviderConfigError::EmptyProviderList);
    }
    if priorities.trim().is_empty() {
        return Err(ProviderConfigError::EmptyPriorityList);
    }

    let urls: Vec<&str> = urls.split(',').map(str::trim).collect();
    let priorities: Vec<&str> = priorities.split(',').map(str::trim).collect();
    if urls.len() != priorities.len() {
        return Err(ProviderConfigError::CountMismatch {
            urls: urls.len(),
            priorities: priorities.len(),
        });
    }

    let global_rps = parse_global_rps(global_rps)?;
    let mut providers = Vec::with_capacity(urls.len());
    for (index, (url, priority)) in urls.into_iter().zip(priorities).enumerate() {
        if url.is_empty() {
            return Err(ProviderConfigError::EmptyProviderUrl { index });
        }
        if !is_http_url(url) {
            return Err(ProviderConfigError::InvalidProviderUrl { index });
        }
        let priority = parse_priority(index, priority)?;
        providers.push(ProviderSpec {
            name: safe_provider_name(index, url),
            url: url.to_string(),
            priority,
        });
    }
    if providers.is_empty() {
        return Err(ProviderConfigError::EmptyProviderList);
    }
    Ok(ProviderConfig {
        providers,
        global_rps,
    })
}

fn parse_priority(index: usize, value: &str) -> Result<u32, ProviderConfigError> {
    let parsed = value
        .parse::<i64>()
        .map_err(|_| ProviderConfigError::InvalidPriority { index })?;
    if parsed == 0 {
        return Err(ProviderConfigError::ZeroPriority { index });
    }
    if parsed < 0 || parsed > u32::MAX as i64 {
        return Err(ProviderConfigError::InvalidPriority { index });
    }
    Ok(parsed as u32)
}

fn parse_global_rps(value: &str) -> Result<u32, ProviderConfigError> {
    let parsed = value
        .trim()
        .parse::<i64>()
        .map_err(|_| ProviderConfigError::InvalidGlobalRps)?;
    if parsed == 0 {
        return Err(ProviderConfigError::ZeroGlobalRps);
    }
    if parsed < 0 || parsed > u32::MAX as i64 {
        return Err(ProviderConfigError::InvalidGlobalRps);
    }
    Ok(parsed as u32)
}

fn is_http_url(url: &str) -> bool {
    host_from_http_url(url).is_some()
}

fn safe_provider_name(index: usize, url: &str) -> String {
    let host = match host_from_http_url(url) {
        Some(host) => host.to_ascii_lowercase(),
        None => return format!("provider_{index}"),
    };
    if host.contains("quicknode") || host.contains("quiknode") {
        "quicknode".to_string()
    } else if host.contains("drpc") {
        "drpc".to_string()
    } else if host.contains("publicnode") {
        "publicnode".to_string()
    } else if host == "arb1.arbitrum.io" {
        "arbitrum_public".to_string()
    } else {
        format!("provider_{index}")
    }
}

fn host_from_http_url(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = host_port.split(':').next().unwrap_or_default();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}
