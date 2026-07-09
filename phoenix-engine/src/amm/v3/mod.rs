use crate::domain::{Amount, Direction, DomainError, Tick};
use crate::state::PoolState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwapSimulation {
    pub amount_in: Amount,
    pub amount_out: Amount,
    pub crossed_ticks: u32,
    pub final_tick: Tick,
}

pub fn simulate_exact_input(
    pool: &PoolState,
    amount_in: Amount,
    direction: Direction,
    max_tick_crossings: u32,
) -> Result<SwapSimulation, DomainError> {
    if amount_in.0 == 0 {
        return Ok(SwapSimulation {
            amount_in,
            amount_out: Amount::ZERO,
            crossed_ticks: 0,
            final_tick: pool.tick,
        });
    }
    let fee_denominator = 1_000_000u128;
    let fee = pool.fee as u128;
    let after_fee = amount_in
        .0
        .checked_mul(fee_denominator.saturating_sub(fee))
        .ok_or(DomainError::ArithmeticOverflow)?
        / fee_denominator;

    let crossed = ((amount_in.0 / pool.liquidity.0.max(1)) as u32).min(max_tick_crossings + 1);
    if crossed > max_tick_crossings {
        return Err(DomainError::StateIncomplete);
    }
    let tick_delta = crossed as i32;
    let final_tick = match direction {
        Direction::ZeroForOne => Tick(pool.tick.0 - tick_delta),
        Direction::OneForZero => Tick(pool.tick.0 + tick_delta),
    };
    pool.require_tick(final_tick)?;

    let (num, den) = match direction {
        Direction::ZeroForOne => (pool.price_numerator, pool.price_denominator),
        Direction::OneForZero => (pool.price_denominator, pool.price_numerator),
    };
    if den == 0 {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let out = after_fee
        .checked_mul(num)
        .ok_or(DomainError::ArithmeticOverflow)?
        / den;
    Ok(SwapSimulation {
        amount_in,
        amount_out: Amount(out),
        crossed_ticks: crossed,
        final_tick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Address, Liquidity, PoolId, TokenAddress};
    use crate::state::StateCompleteness;

    fn pool() -> PoolState {
        PoolState {
            pool_id: PoolId("pool".to_string()),
            token0: TokenAddress(
                Address::parse("0x1111111111111111111111111111111111111111").unwrap(),
            ),
            token1: TokenAddress(
                Address::parse("0x2222222222222222222222222222222222222222").unwrap(),
            ),
            fee: 500,
            tick: Tick(0),
            liquidity: Liquidity(1_000_000),
            price_numerator: 2,
            price_denominator: 1,
            completeness: StateCompleteness {
                min_tick: Tick(-10),
                max_tick: Tick(10),
            },
            last_reconciled_block: 1,
        }
    }

    #[test]
    fn integer_segment_swap_applies_fee() {
        let result =
            simulate_exact_input(&pool(), Amount(1_000_000), Direction::ZeroForOne, 1).unwrap();
        assert_eq!(result.amount_out, Amount(1_999_000));
    }

    #[test]
    fn rejects_unknown_tick_region() {
        let err =
            simulate_exact_input(&pool(), Amount(3_000_000), Direction::ZeroForOne, 1).unwrap_err();
        assert_eq!(err, DomainError::StateIncomplete);
    }
}
