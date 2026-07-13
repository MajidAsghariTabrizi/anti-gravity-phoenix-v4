use std::collections::HashSet;
use std::fmt;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open { until: Instant },
}

#[derive(Clone)]
pub struct Provider {
    pub name: String,
    pub url: String,
    pub weight: u32,
    pub health_score: i32,
    pub circuit: CircuitState,
    pub cooldown_until: Option<Instant>,
    pub consecutive_failures: u32,
}

impl fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Provider")
            .field("name", &self.name)
            .field("weight", &self.weight)
            .field("health_score", &self.health_score)
            .field("circuit", &self.circuit)
            .field("cooldown_until", &self.cooldown_until)
            .field("consecutive_failures", &self.consecutive_failures)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ProviderLease {
    provider_id: String,
    url: String,
}

impl ProviderLease {
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for ProviderLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderLease")
            .field("provider_id", &self.provider_id)
            .finish_non_exhaustive()
    }
}

impl Provider {
    pub fn new(name: String, url: String, weight: u32, _now: Instant) -> Self {
        Self {
            name,
            url,
            weight,
            health_score: 100,
            circuit: CircuitState::Closed,
            cooldown_until: None,
            consecutive_failures: 0,
        }
    }

    pub fn available(&mut self, now: Instant) -> bool {
        if !self.refresh_eligibility(now) {
            return false;
        }
        true
    }

    pub fn reserve(&mut self, now: Instant) -> bool {
        self.refresh_eligibility(now)
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

    pub fn record_cooldown(&mut self, now: Instant, duration: Duration) {
        self.cooldown_until = Some(now + duration);
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

    pub fn reserve_best(
        &mut self,
        now: Instant,
        excluded: &HashSet<String>,
    ) -> Option<ProviderLease> {
        let mut best_idx: Option<usize> = None;
        let mut best_weight = 0;
        for (idx, provider) in self.providers.iter_mut().enumerate() {
            if excluded.contains(&provider.name) || !provider.available(now) {
                continue;
            }
            if best_idx.is_none() || provider.weight > best_weight {
                best_weight = provider.weight;
                best_idx = Some(idx);
            }
        }
        let idx = best_idx?;
        if !self.providers[idx].reserve(now) {
            return None;
        }
        Some(ProviderLease {
            provider_id: self.providers[idx].name.clone(),
            url: self.providers[idx].url.clone(),
        })
    }

    pub fn reserve_named(&mut self, now: Instant, provider_id: &str) -> Option<ProviderLease> {
        let provider = self
            .providers
            .iter_mut()
            .find(|provider| provider.name == provider_id)?;
        if !provider.reserve(now) {
            return None;
        }
        Some(ProviderLease {
            provider_id: provider.name.clone(),
            url: provider.url.clone(),
        })
    }

    pub fn record_success(&mut self, provider_id: &str) -> bool {
        if let Some(provider) = self
            .providers
            .iter_mut()
            .find(|provider| provider.name == provider_id)
        {
            provider.record_success();
            true
        } else {
            false
        }
    }

    pub fn record_failure(&mut self, provider_id: &str, now: Instant) -> bool {
        if let Some(provider) = self
            .providers
            .iter_mut()
            .find(|provider| provider.name == provider_id)
        {
            provider.record_failure(now);
            true
        } else {
            false
        }
    }

    pub fn record_cooldown(&mut self, provider_id: &str, now: Instant, duration: Duration) -> bool {
        if let Some(provider) = self
            .providers
            .iter_mut()
            .find(|provider| provider.name == provider_id)
        {
            provider.record_cooldown(now, duration);
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub providers: Vec<ProviderSpec>,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("provider_count", &self.providers.len())
            .finish()
    }
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

#[derive(Clone, PartialEq, Eq)]
pub struct ProviderSpec {
    pub name: String,
    pub url: String,
    pub priority: u32,
}

impl fmt::Debug for ProviderSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderSpec")
            .field("name", &self.name)
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
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
        }
    }
}

impl std::error::Error for ProviderConfigError {}

pub fn parse_provider_config(
    urls: &str,
    priorities: &str,
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

    let mut providers = Vec::with_capacity(urls.len());
    let mut provider_names = HashSet::with_capacity(urls.len());
    for (index, (url, priority)) in urls.into_iter().zip(priorities).enumerate() {
        if url.is_empty() {
            return Err(ProviderConfigError::EmptyProviderUrl { index });
        }
        if !is_http_url(url) {
            return Err(ProviderConfigError::InvalidProviderUrl { index });
        }
        let priority = parse_priority(index, priority)?;
        let mut name = safe_provider_name(index, url);
        if !provider_names.insert(name.clone()) {
            name = format!("{name}_{index}");
            provider_names.insert(name.clone());
        }
        providers.push(ProviderSpec {
            name,
            url: url.to_string(),
            priority,
        });
    }
    if providers.is_empty() {
        return Err(ProviderConfigError::EmptyProviderList);
    }
    Ok(ProviderConfig { providers })
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
