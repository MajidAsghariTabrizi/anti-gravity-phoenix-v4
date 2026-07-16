use crate::domain::{Amount, Direction, DomainError, Tick};
use crate::state::PoolState;
use primitive_types::{U256, U512};

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
    let after_fee = U256::from(amount_in.0)
        .checked_mul(U256::from(fee_denominator.saturating_sub(fee)))
        .ok_or(DomainError::ArithmeticOverflow)?
        / U256::from(fee_denominator);
    if after_fee > U256::from(u128::MAX) {
        return Err(DomainError::ArithmeticOverflow);
    }
    let after_fee = after_fee.low_u128();

    let crossed = u32::try_from(amount_in.0 / pool.liquidity.0.max(1))
        .unwrap_or(u32::MAX)
        .min(max_tick_crossings.saturating_add(1));
    if crossed > max_tick_crossings {
        return Err(DomainError::StateIncomplete);
    }
    let tick_delta = crossed as i32;
    let final_tick = match direction {
        Direction::ZeroForOne => Tick(
            pool.tick
                .0
                .checked_sub(tick_delta)
                .ok_or(DomainError::ArithmeticUnderflow)?,
        ),
        Direction::OneForZero => Tick(
            pool.tick
                .0
                .checked_add(tick_delta)
                .ok_or(DomainError::ArithmeticOverflow)?,
        ),
    };
    pool.require_tick(final_tick)?;

    if pool.sqrt_price_x96.0.is_zero() {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let price_square = pool.sqrt_price_x96.0.full_mul(pool.sqrt_price_x96.0);
    let q192 = U512::from(1_u8) << 192;
    let (numerator, denominator) = match direction {
        Direction::ZeroForOne => (
            U512::from(after_fee)
                .checked_mul(price_square)
                .ok_or(DomainError::ArithmeticOverflow)?,
            q192,
        ),
        Direction::OneForZero => (U512::from(after_fee) << 192, price_square),
    };
    let out = numerator / denominator;
    if out > U512::from(u128::MAX) {
        return Err(DomainError::ArithmeticOverflow);
    }
    Ok(SwapSimulation {
        amount_in,
        amount_out: Amount(out.low_u128()),
        crossed_ticks: crossed,
        final_tick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Address, Liquidity, PoolId, SqrtPriceX96, TokenAddress};
    use crate::state::StateCompleteness;
    use primitive_types::U256;

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
            sqrt_price_x96: SqrtPriceX96(U256::from(1_u8) << 96),
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
        assert_eq!(result.amount_out, Amount(999_500));
    }

    #[test]
    fn rejects_unknown_tick_region() {
        let err =
            simulate_exact_input(&pool(), Amount(3_000_000), Direction::ZeroForOne, 1).unwrap_err();
        assert_eq!(err, DomainError::StateIncomplete);
    }

    #[test]
    fn oversized_tick_crossing_count_cannot_wrap_to_zero() {
        let mut state = pool();
        state.liquidity = Liquidity(1);
        let err = simulate_exact_input(
            &state,
            Amount(u32::MAX as u128 + 1),
            Direction::ZeroForOne,
            0,
        )
        .unwrap_err();
        assert_eq!(err, DomainError::StateIncomplete);
    }

    #[test]
    fn tick_arithmetic_overflow_fails_closed() {
        let mut state = pool();
        state.tick = Tick(i32::MAX);
        state.completeness = StateCompleteness {
            min_tick: Tick(i32::MIN),
            max_tick: Tick(i32::MAX),
        };
        state.liquidity = Liquidity(1);
        let err = simulate_exact_input(&state, Amount(1), Direction::OneForZero, 1).unwrap_err();
        assert_eq!(err, DomainError::ArithmeticOverflow);
    }
}
