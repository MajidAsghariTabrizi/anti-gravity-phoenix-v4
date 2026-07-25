use primitive_types::U256;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use thiserror::Error;

pub const PINNED_V3_STATE_SCHEMA: &str = "phoenix.hunter-pinned-v3-state.v1";
pub const MAX_TICK_BITMAP_WORDS: usize = 32;
pub const MAX_INITIALIZED_TICKS: usize = 512;
pub const MAX_CACHE_ENTRIES: usize = 1_024;
pub const HUNTER_STATE_REQUEST_SCHEMA: &str = "phoenix.rpc.hunter-state-request.v1";
pub const HUNTER_STATE_RESPONSE_SCHEMA: &str = "phoenix.rpc.hunter-state-response.v1";
pub const MAX_HUNTER_POOLS_PER_REQUEST: usize = 16;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HunterPoolRequest {
    pub pool_id: String,
    pub pool_address: String,
    pub factory_address: String,
    pub protocol_id: String,
    pub token0: String,
    pub token1: String,
    pub fee: u32,
    pub tick_spacing: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HunterStateRequest {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub pools: Vec<HunterPoolRequest>,
    pub maximum_tick_words_per_pool: usize,
    pub maximum_initialized_ticks: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HunterStateResponse {
    pub schema_version: String,
    pub chain_id: u64,
    pub request_id: String,
    pub block_number: u64,
    pub block_hash: String,
    pub agreements: Vec<ProviderStateAgreement>,
    pub resolved_at_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TickBitmapWord {
    pub word_position: i16,
    pub bitmap: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InitializedTick {
    pub tick: i32,
    pub liquidity_gross: String,
    pub liquidity_net: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PinnedV3PoolState {
    pub schema_version: String,
    pub chain_id: u64,
    pub block_number: u64,
    pub block_hash: String,
    pub pool_id: String,
    pub pool_address: String,
    pub pool_code_hash: String,
    pub factory_address: String,
    pub protocol_id: String,
    pub token0: String,
    pub token1: String,
    pub fee: u32,
    pub tick_spacing: i32,
    pub sqrt_price_x96: String,
    pub tick: i32,
    pub liquidity: String,
    pub coverage_min_tick: i32,
    pub coverage_max_tick: i32,
    pub tick_bitmap_words: Vec<TickBitmapWord>,
    pub initialized_ticks: Vec<InitializedTick>,
    pub state_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderStateAgreement {
    pub primary_provider_id: String,
    pub secondary_provider_id: String,
    pub primary: PinnedV3PoolState,
    pub secondary: PinnedV3PoolState,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HunterStateError {
    #[error("hunter state contract is invalid")]
    InvalidContract,
    #[error("hunter state evidence exceeds a bounded limit")]
    LimitExceeded,
    #[error("hunter state hash does not match its evidence")]
    HashMismatch,
    #[error("hunter state providers disagree")]
    ProviderDisagreement,
    #[error("hunter state provider evidence is incomplete")]
    StateIncomplete,
}

impl HunterStateRequest {
    pub fn validate(&self) -> Result<(), HunterStateError> {
        if self.schema_version != HUNTER_STATE_REQUEST_SCHEMA
            || self.chain_id != 42_161
            || !bounded_identifier(&self.request_id, 1, 256)
            || self.pools.is_empty()
            || self.pools.len() > MAX_HUNTER_POOLS_PER_REQUEST
            || self.maximum_tick_words_per_pool == 0
            || self.maximum_tick_words_per_pool > MAX_TICK_BITMAP_WORDS
            || self.maximum_initialized_ticks == 0
            || self.maximum_initialized_ticks > MAX_INITIALIZED_TICKS
        {
            return Err(HunterStateError::InvalidContract);
        }
        let mut ids = std::collections::HashSet::new();
        let mut addresses = std::collections::HashSet::new();
        for pool in &self.pools {
            if !bounded_identifier(&pool.pool_id, 1, 256)
                || !ids.insert(pool.pool_id.as_str())
                || !canonical_address(&pool.pool_address)
                || !addresses.insert(pool.pool_address.as_str())
                || !canonical_address(&pool.factory_address)
                || !bounded_identifier(&pool.protocol_id, 1, 64)
                || !canonical_address(&pool.token0)
                || !canonical_address(&pool.token1)
                || pool.token0 >= pool.token1
                || pool.fee == 0
                || pool.fee >= 1_000_000
                || pool.tick_spacing <= 0
                || pool.tick_spacing > 887_272
            {
                return Err(HunterStateError::InvalidContract);
            }
        }
        Ok(())
    }
}

impl HunterStateResponse {
    pub fn validate(&self, request: &HunterStateRequest) -> Result<(), HunterStateError> {
        request.validate()?;
        if self.schema_version != HUNTER_STATE_RESPONSE_SCHEMA
            || self.chain_id != request.chain_id
            || self.request_id != request.request_id
            || self.block_number == 0
            || !canonical_hash(&self.block_hash, true)
            || self.agreements.len() != request.pools.len()
        {
            return Err(HunterStateError::InvalidContract);
        }
        for (agreement, expected) in self.agreements.iter().zip(&request.pools) {
            let state = agreement.agreed()?;
            if state.block_number != self.block_number
                || state.block_hash != self.block_hash
                || state.pool_id != expected.pool_id
                || state.pool_address != expected.pool_address
                || state.factory_address != expected.factory_address
                || state.protocol_id != expected.protocol_id
                || state.token0 != expected.token0
                || state.token1 != expected.token1
                || state.fee != expected.fee
                || state.tick_spacing != expected.tick_spacing
            {
                return Err(HunterStateError::InvalidContract);
            }
        }
        Ok(())
    }
}

impl PinnedV3PoolState {
    pub fn validate(&self) -> Result<(), HunterStateError> {
        if self.schema_version != PINNED_V3_STATE_SCHEMA
            || self.chain_id != 42_161
            || self.block_number == 0
            || !canonical_hash(&self.block_hash, true)
            || !bounded_identifier(&self.pool_id, 1, 256)
            || !canonical_address(&self.pool_address)
            || !canonical_hash(&self.pool_code_hash, false)
            || !canonical_address(&self.factory_address)
            || !bounded_identifier(&self.protocol_id, 1, 64)
            || !canonical_address(&self.token0)
            || !canonical_address(&self.token1)
            || self.token0 >= self.token1
            || self.fee == 0
            || self.fee >= 1_000_000
            || self.tick_spacing <= 0
            || self.tick_spacing > 887_272
            || self.tick < -887_272
            || self.tick > 887_272
            || self.coverage_min_tick < -887_272
            || self.coverage_max_tick > 887_272
            || self.coverage_min_tick > self.tick
            || self.coverage_max_tick < self.tick
            || parse_u256_decimal(&self.sqrt_price_x96)?.is_zero()
            || parse_u128_decimal(&self.liquidity)? == 0
        {
            return Err(HunterStateError::InvalidContract);
        }
        if self.tick_bitmap_words.len() > MAX_TICK_BITMAP_WORDS
            || self.initialized_ticks.len() > MAX_INITIALIZED_TICKS
        {
            return Err(HunterStateError::LimitExceeded);
        }

        let mut words = BTreeMap::new();
        let mut previous_word = None;
        for word in &self.tick_bitmap_words {
            let bitmap = parse_bitmap(&word.bitmap)?;
            if previous_word.is_some_and(|position| word.word_position <= position)
                || words.insert(word.word_position, bitmap).is_some()
            {
                return Err(HunterStateError::InvalidContract);
            }
            previous_word = Some(word.word_position);
        }
        let mut previous = None;
        for initialized in &self.initialized_ticks {
            if initialized.tick < self.coverage_min_tick
                || initialized.tick > self.coverage_max_tick
                || initialized.tick % self.tick_spacing != 0
                || previous.is_some_and(|tick| initialized.tick <= tick)
                || parse_u128_decimal(&initialized.liquidity_gross)? == 0
                || parse_i128_decimal(&initialized.liquidity_net).is_err()
            {
                return Err(HunterStateError::InvalidContract);
            }
            let compressed = initialized.tick / self.tick_spacing;
            let word_position = compressed >> 8;
            let bit_position = (compressed & 255) as usize;
            let word_position =
                i16::try_from(word_position).map_err(|_| HunterStateError::InvalidContract)?;
            if !words
                .get(&word_position)
                .is_some_and(|bitmap| bitmap.bit(bit_position))
            {
                return Err(HunterStateError::InvalidContract);
            }
            previous = Some(initialized.tick);
        }
        if !canonical_hash(&self.state_hash, false) {
            return Err(HunterStateError::InvalidContract);
        }
        let expected = self.canonical_hash()?;
        if self.state_hash != expected {
            return Err(HunterStateError::HashMismatch);
        }
        Ok(())
    }

    pub fn canonical_hash(&self) -> Result<String, HunterStateError> {
        let mut value =
            serde_json::to_value(self).map_err(|_| HunterStateError::InvalidContract)?;
        value
            .as_object_mut()
            .ok_or(HunterStateError::InvalidContract)?
            .remove("state_hash");
        let body = canonical_json(&value)?;
        let prefix = format!(
            "phoenix.canonical-json.v1:hunter-pinned-v3-state:{}\n",
            self.schema_version
        );
        Ok(hex::encode(Sha256::digest(
            [prefix.as_bytes(), body.as_slice()].concat(),
        )))
    }

    pub fn cache_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.pool_address, self.block_number, self.block_hash
        )
    }
}

impl ProviderStateAgreement {
    pub fn agreed(&self) -> Result<&PinnedV3PoolState, HunterStateError> {
        if !bounded_identifier(&self.primary_provider_id, 1, 64)
            || !bounded_identifier(&self.secondary_provider_id, 1, 64)
            || self.primary_provider_id == self.secondary_provider_id
        {
            return Err(HunterStateError::InvalidContract);
        }
        self.primary.validate()?;
        self.secondary.validate()?;
        if self.primary != self.secondary {
            return Err(HunterStateError::ProviderDisagreement);
        }
        Ok(&self.primary)
    }
}

#[derive(Clone, Debug)]
pub struct BlockStateCache {
    capacity: usize,
    order: VecDeque<String>,
    entries: HashMap<String, PinnedV3PoolState>,
}

impl BlockStateCache {
    pub fn new(capacity: usize) -> Result<Self, HunterStateError> {
        if capacity == 0 || capacity > MAX_CACHE_ENTRIES {
            return Err(HunterStateError::LimitExceeded);
        }
        Ok(Self {
            capacity,
            order: VecDeque::new(),
            entries: HashMap::new(),
        })
    }

    pub fn insert(&mut self, state: PinnedV3PoolState) -> Result<(), HunterStateError> {
        state.validate()?;
        let key = state.cache_key();
        if let Some(existing) = self.entries.get_mut(&key) {
            *existing = state;
            return Ok(());
        }
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, state);
        Ok(())
    }

    pub fn get(
        &self,
        pool_address: &str,
        block_number: u64,
        block_hash: &str,
    ) -> Option<&PinnedV3PoolState> {
        self.entries
            .get(&format!("{pool_address}:{block_number}:{block_hash}"))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, HunterStateError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            serde_json::to_vec(value).map_err(|_| HunterStateError::InvalidContract)
        }
        Value::Array(values) => {
            let mut output = vec![b'['];
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(canonical_json(child)?);
            }
            output.push(b']');
            Ok(output)
        }
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let mut output = vec![b'{'];
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(
                    serde_json::to_vec(key).map_err(|_| HunterStateError::InvalidContract)?,
                );
                output.push(b':');
                output.extend(canonical_json(
                    values.get(key).ok_or(HunterStateError::InvalidContract)?,
                )?);
            }
            output.push(b'}');
            Ok(output)
        }
    }
}

fn parse_bitmap(value: &str) -> Result<U256, HunterStateError> {
    if value.len() != 66
        || !value.starts_with("0x")
        || !value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(HunterStateError::InvalidContract);
    }
    U256::from_str_radix(&value[2..], 16).map_err(|_| HunterStateError::InvalidContract)
}

fn parse_u256_decimal(value: &str) -> Result<U256, HunterStateError> {
    if !canonical_unsigned_decimal(value, 78) {
        return Err(HunterStateError::InvalidContract);
    }
    U256::from_dec_str(value).map_err(|_| HunterStateError::InvalidContract)
}

fn parse_u128_decimal(value: &str) -> Result<u128, HunterStateError> {
    if !canonical_unsigned_decimal(value, 39) {
        return Err(HunterStateError::InvalidContract);
    }
    value.parse().map_err(|_| HunterStateError::InvalidContract)
}

fn parse_i128_decimal(value: &str) -> Result<i128, HunterStateError> {
    if value.is_empty()
        || value.len() > 40
        || value == "-0"
        || value.starts_with('+')
        || (!value
            .trim_start_matches('-')
            .bytes()
            .all(|byte| byte.is_ascii_digit()))
        || (value.trim_start_matches('-').len() > 1
            && value.trim_start_matches('-').starts_with('0'))
    {
        return Err(HunterStateError::InvalidContract);
    }
    value.parse().map_err(|_| HunterStateError::InvalidContract)
}

fn canonical_unsigned_decimal(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
}

fn canonical_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_hash(value: &str, prefixed: bool) -> bool {
    let expected = if prefixed { 66 } else { 64 };
    value.len() == expected
        && (!prefixed || value.starts_with("0x"))
        && value[usize::from(prefixed) * 2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_identifier(value: &str, minimum: usize, maximum: usize) -> bool {
    value.len() >= minimum && value.len() <= maximum && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(pool: &str) -> PinnedV3PoolState {
        let mut state = PinnedV3PoolState {
            schema_version: PINNED_V3_STATE_SCHEMA.to_string(),
            chain_id: 42_161,
            block_number: 1,
            block_hash: format!("0x{}", "a".repeat(64)),
            pool_id: "pool-500".to_string(),
            pool_address: pool.to_string(),
            pool_code_hash: "b".repeat(64),
            factory_address: "0x1111111111111111111111111111111111111111".to_string(),
            protocol_id: "uniswap-v3".to_string(),
            token0: "0x2222222222222222222222222222222222222222".to_string(),
            token1: "0x3333333333333333333333333333333333333333".to_string(),
            fee: 500,
            tick_spacing: 10,
            sqrt_price_x96: (U256::one() << 96).to_string(),
            tick: 0,
            liquidity: "1000000000000".to_string(),
            coverage_min_tick: -20,
            coverage_max_tick: 20,
            tick_bitmap_words: vec![TickBitmapWord {
                word_position: -1,
                bitmap: format!("0x{:064x}", U256::one() << 255),
            }],
            initialized_ticks: vec![InitializedTick {
                tick: -10,
                liquidity_gross: "100000000".to_string(),
                liquidity_net: "100000000".to_string(),
            }],
            state_hash: "0".repeat(64),
        };
        state.state_hash = state.canonical_hash().unwrap();
        state
    }

    #[test]
    fn complete_tick_evidence_hashes_and_provider_agreement_are_strict() {
        let primary = state("0x4444444444444444444444444444444444444444");
        primary.validate().unwrap();
        let agreement = ProviderStateAgreement {
            primary_provider_id: "primary".to_string(),
            secondary_provider_id: "secondary".to_string(),
            primary: primary.clone(),
            secondary: primary,
        };
        assert!(agreement.agreed().is_ok());
        let mut changed = agreement.clone();
        changed.secondary.liquidity = "1000000000001".to_string();
        changed.secondary.state_hash = changed.secondary.canonical_hash().unwrap();
        assert_eq!(
            changed.agreed(),
            Err(HunterStateError::ProviderDisagreement)
        );
    }

    #[test]
    fn block_cache_is_bounded_and_block_hash_keyed() {
        let mut cache = BlockStateCache::new(1).unwrap();
        let first = state("0x4444444444444444444444444444444444444444");
        cache.insert(first.clone()).unwrap();
        let mut second = state("0x5555555555555555555555555555555555555555");
        second.state_hash = second.canonical_hash().unwrap();
        cache.insert(second.clone()).unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache
            .get(&first.pool_address, first.block_number, &first.block_hash)
            .is_none());
        assert!(cache
            .get(
                &second.pool_address,
                second.block_number,
                &second.block_hash
            )
            .is_some());
    }
}
