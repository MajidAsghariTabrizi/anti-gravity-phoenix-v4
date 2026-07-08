use crate::domain::{DomainError, Liquidity, PoolId, Tick, TokenAddress};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateCompleteness {
    pub min_tick: Tick,
    pub max_tick: Tick,
}

impl StateCompleteness {
    pub fn covers(&self, tick: Tick) -> bool {
        tick.0 >= self.min_tick.0 && tick.0 <= self.max_tick.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub pool_id: PoolId,
    pub token0: TokenAddress,
    pub token1: TokenAddress,
    pub fee: u32,
    pub tick: Tick,
    pub liquidity: Liquidity,
    pub price_numerator: u128,
    pub price_denominator: u128,
    pub completeness: StateCompleteness,
    pub last_reconciled_block: u64,
}

impl PoolState {
    pub fn require_tick(&self, tick: Tick) -> Result<(), DomainError> {
        if self.completeness.covers(tick) {
            Ok(())
        } else {
            Err(DomainError::StateIncomplete)
        }
    }
}

