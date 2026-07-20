use crate::domain::{Amount, Direction, DomainError, Tick};
use crate::state::PoolState;
use primitive_types::{U256, U512};
use std::sync::OnceLock;

const FEE_DENOMINATOR: u128 = 1_000_000;
const BPS_DENOMINATOR: u128 = 10_000;
const MIN_TICK: i32 = -887_272;
const MAX_TICK: i32 = 887_272;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentRangeSwap {
    pub amount_in: Amount,
    pub amount_in_less_fee: Amount,
    pub fee_amount: Amount,
    pub amount_out: Amount,
    pub spot_amount_out: Amount,
    pub current_range_capacity: Amount,
    pub utilization_bps: u16,
    pub final_sqrt_price_x96: U256,
}

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

pub fn simulate_current_range_exact_input(
    pool: &PoolState,
    amount_in: Amount,
    direction: Direction,
    tick_spacing: i32,
) -> Result<CurrentRangeSwap, DomainError> {
    if amount_in.0 == 0
        || pool.liquidity.0 == 0
        || pool.sqrt_price_x96.0.is_zero()
        || !(1..=MAX_TICK).contains(&tick_spacing)
    {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let amount_in_less_fee = amount_less_fee(amount_in, pool.fee)?.0;
    if amount_in_less_fee == 0 {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let target_tick = current_range_target_tick(pool.tick.0, tick_spacing, direction)?;
    let target_sqrt = sqrt_ratio_at_tick(target_tick)?;
    let current_sqrt = pool.sqrt_price_x96.0;
    let capacity = current_range_input_capacity(pool, direction, tick_spacing)?.0;
    if capacity == 0 || amount_in_less_fee >= capacity {
        return Err(DomainError::StateIncomplete);
    }
    let final_sqrt = next_sqrt_price_from_input(
        current_sqrt,
        pool.liquidity.0,
        amount_in_less_fee,
        direction,
    )?;
    if matches!(direction, Direction::ZeroForOne) && final_sqrt <= target_sqrt
        || matches!(direction, Direction::OneForZero) && final_sqrt >= target_sqrt
    {
        return Err(DomainError::StateIncomplete);
    }
    let amount_out = match direction {
        Direction::ZeroForOne => amount_1_delta(final_sqrt, current_sqrt, pool.liquidity.0, false)?,
        Direction::OneForZero => amount_0_delta(current_sqrt, final_sqrt, pool.liquidity.0, false)?,
    };
    let amount_out = u256_to_u128(amount_out)?;
    let spot_amount_out = quote_spot_exact_input(pool, Amount(amount_in_less_fee), direction)?.0;
    if amount_out == 0 || amount_out > spot_amount_out {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let utilization_bps = ratio_bps_ceil(amount_in_less_fee, capacity)?;
    Ok(CurrentRangeSwap {
        amount_in,
        amount_in_less_fee: Amount(amount_in_less_fee),
        fee_amount: Amount(
            amount_in
                .0
                .checked_sub(amount_in_less_fee)
                .ok_or(DomainError::ArithmeticUnderflow)?,
        ),
        amount_out: Amount(amount_out),
        spot_amount_out: Amount(spot_amount_out),
        current_range_capacity: Amount(capacity),
        utilization_bps,
        final_sqrt_price_x96: final_sqrt,
    })
}

pub fn amount_less_fee(amount: Amount, fee: u32) -> Result<Amount, DomainError> {
    let fee_multiplier = FEE_DENOMINATOR
        .checked_sub(u128::from(fee))
        .ok_or(DomainError::ArithmeticUnderflow)?;
    amount
        .0
        .checked_mul(fee_multiplier)
        .map(|value| Amount(value / FEE_DENOMINATOR))
        .ok_or(DomainError::ArithmeticOverflow)
}

pub fn current_range_input_capacity(
    pool: &PoolState,
    direction: Direction,
    tick_spacing: i32,
) -> Result<Amount, DomainError> {
    let target_tick = current_range_target_tick(pool.tick.0, tick_spacing, direction)?;
    let target_sqrt = sqrt_ratio_at_tick(target_tick)?;
    let current_sqrt = pool.sqrt_price_x96.0;
    let capacity = match direction {
        Direction::ZeroForOne if target_sqrt < current_sqrt => {
            amount_0_delta(target_sqrt, current_sqrt, pool.liquidity.0, true)?
        }
        Direction::OneForZero if target_sqrt > current_sqrt => {
            amount_1_delta(current_sqrt, target_sqrt, pool.liquidity.0, true)?
        }
        _ => return Err(DomainError::StateIncomplete),
    };
    u256_to_u128(capacity).map(Amount)
}

pub fn quote_spot_exact_input(
    pool: &PoolState,
    amount_in: Amount,
    direction: Direction,
) -> Result<Amount, DomainError> {
    spot_output(pool.sqrt_price_x96.0, amount_in.0, direction).map(Amount)
}

fn current_range_target_tick(
    tick: i32,
    tick_spacing: i32,
    direction: Direction,
) -> Result<i32, DomainError> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) || tick_spacing <= 0 {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let mut compressed = tick / tick_spacing;
    if tick < 0 && tick % tick_spacing != 0 {
        compressed = compressed
            .checked_sub(1)
            .ok_or(DomainError::ArithmeticUnderflow)?;
    }
    let lower = compressed
        .checked_mul(tick_spacing)
        .ok_or(DomainError::ArithmeticOverflow)?;
    let target = match direction {
        Direction::ZeroForOne => lower,
        Direction::OneForZero => lower
            .checked_add(tick_spacing)
            .ok_or(DomainError::ArithmeticOverflow)?,
    };
    if !(MIN_TICK..=MAX_TICK).contains(&target) {
        return Err(DomainError::StateIncomplete);
    }
    Ok(target)
}

fn sqrt_ratio_at_tick(tick: i32) -> Result<U256, DomainError> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let absolute = tick.unsigned_abs();
    let factors = tick_ratio_factors();
    let mut ratio = if absolute & 1 != 0 {
        factors[0]
    } else {
        U256::one() << 128
    };
    for (index, factor) in factors.iter().enumerate().skip(1) {
        if absolute & (1_u32 << index) != 0 {
            ratio = u512_to_u256((ratio.full_mul(*factor)) >> 128)?;
        }
    }
    if tick > 0 {
        ratio = U256::MAX / ratio;
    }
    let remainder_mask = (U256::one() << 32) - U256::one();
    let rounded = (ratio >> 32)
        .checked_add(U256::from(u8::from(
            (ratio & remainder_mask) != U256::zero(),
        )))
        .ok_or(DomainError::ArithmeticOverflow)?;
    if rounded.is_zero() || rounded.bits() > 160 {
        return Err(DomainError::ArithmeticOverflow);
    }
    Ok(rounded)
}

fn tick_ratio_factors() -> &'static [U256; 20] {
    static FACTORS: OnceLock<[U256; 20]> = OnceLock::new();
    FACTORS.get_or_init(|| {
        [
            "fffcb933bd6fad37aa2d162d1a594001",
            "fff97272373d413259a46990580e213a",
            "fff2e50f5f656932ef12357cf3c7fdcc",
            "ffe5caca7e10e4e61c3624eaa0941cd0",
            "ffcb9843d60f6159c9db58835c926644",
            "ff973b41fa98c081472e6896dfb254c0",
            "ff2ea16466c96a3843ec78b326b52861",
            "fe5dee046a99a2a811c461f1969c3053",
            "fcbe86c7900a88aedcffc83b479aa3a4",
            "f987a7253ac413176f2b074cf7815e54",
            "f3392b0822b70005940c7a398e4b70f3",
            "e7159475a2c29b7443b29c7fa6e889d9",
            "d097f3bdfd2022b8845ad8f792aa5825",
            "a9f746462d870fdf8a65dc1f90e061e5",
            "70d869a156d2a1b890bb3df62baf32f7",
            "31be135f97d08fd981231505542fcfa6",
            "9aa508b5b7a84e1c677de54f3e99bc9",
            "5d6af8dedb81196699c329225ee604",
            "2216e584f5fa1ea926041bedfe98",
            "48a170391f7dc42444e8fa2",
        ]
        .map(|value| U256::from_str_radix(value, 16).expect("tick ratio constant is valid"))
    })
}

fn next_sqrt_price_from_input(
    sqrt_price: U256,
    liquidity: u128,
    amount_in: u128,
    direction: Direction,
) -> Result<U256, DomainError> {
    match direction {
        Direction::ZeroForOne => {
            let numerator_1 = U512::from(liquidity) << 96;
            let numerator = numerator_1
                .checked_mul(U512::from(sqrt_price))
                .ok_or(DomainError::ArithmeticOverflow)?;
            let denominator = numerator_1
                .checked_add(
                    U512::from(amount_in)
                        .checked_mul(U512::from(sqrt_price))
                        .ok_or(DomainError::ArithmeticOverflow)?,
                )
                .ok_or(DomainError::ArithmeticOverflow)?;
            u512_to_u256(div_rounding_up(numerator, denominator)?)
        }
        Direction::OneForZero => {
            let quotient = (U512::from(amount_in) << 96) / U512::from(liquidity);
            sqrt_price
                .checked_add(u512_to_u256(quotient)?)
                .ok_or(DomainError::ArithmeticOverflow)
        }
    }
}

fn amount_0_delta(
    sqrt_a: U256,
    sqrt_b: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256, DomainError> {
    let (lower, upper) = if sqrt_a <= sqrt_b {
        (sqrt_a, sqrt_b)
    } else {
        (sqrt_b, sqrt_a)
    };
    if lower.is_zero() {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let numerator = (U512::from(liquidity) << 96)
        .checked_mul(U512::from(upper - lower))
        .ok_or(DomainError::ArithmeticOverflow)?;
    let denominator = U512::from(upper)
        .checked_mul(U512::from(lower))
        .ok_or(DomainError::ArithmeticOverflow)?;
    let value = if round_up {
        div_rounding_up(numerator, denominator)?
    } else {
        numerator / denominator
    };
    u512_to_u256(value)
}

fn amount_1_delta(
    sqrt_a: U256,
    sqrt_b: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256, DomainError> {
    let (lower, upper) = if sqrt_a <= sqrt_b {
        (sqrt_a, sqrt_b)
    } else {
        (sqrt_b, sqrt_a)
    };
    let numerator = U512::from(liquidity)
        .checked_mul(U512::from(upper - lower))
        .ok_or(DomainError::ArithmeticOverflow)?;
    let denominator = U512::one() << 96;
    let value = if round_up {
        div_rounding_up(numerator, denominator)?
    } else {
        numerator / denominator
    };
    u512_to_u256(value)
}

fn spot_output(
    sqrt_price: U256,
    amount_in: u128,
    direction: Direction,
) -> Result<u128, DomainError> {
    let price_square = sqrt_price.full_mul(sqrt_price);
    let q192 = U512::one() << 192;
    let output = match direction {
        Direction::ZeroForOne => {
            U512::from(amount_in)
                .checked_mul(price_square)
                .ok_or(DomainError::ArithmeticOverflow)?
                / q192
        }
        Direction::OneForZero => (U512::from(amount_in) << 192) / price_square,
    };
    u512_to_u128(output)
}

fn ratio_bps_ceil(value: u128, denominator: u128) -> Result<u16, DomainError> {
    let ratio = div_rounding_up(
        U512::from(value)
            .checked_mul(U512::from(BPS_DENOMINATOR))
            .ok_or(DomainError::ArithmeticOverflow)?,
        U512::from(denominator),
    )?;
    let ratio = u512_to_u128(ratio)?;
    u16::try_from(ratio).map_err(|_| DomainError::ArithmeticOverflow)
}

fn div_rounding_up(numerator: U512, denominator: U512) -> Result<U512, DomainError> {
    if denominator.is_zero() {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let quotient = numerator / denominator;
    quotient
        .checked_add(U512::from(u8::from(
            numerator % denominator != U512::zero(),
        )))
        .ok_or(DomainError::ArithmeticOverflow)
}

fn u512_to_u128(value: U512) -> Result<u128, DomainError> {
    if value > U512::from(u128::MAX) {
        return Err(DomainError::ArithmeticOverflow);
    }
    Ok(value.low_u128())
}

fn u256_to_u128(value: U256) -> Result<u128, DomainError> {
    if value > U256::from(u128::MAX) {
        return Err(DomainError::ArithmeticOverflow);
    }
    Ok(value.low_u128())
}

fn u512_to_u256(value: U512) -> Result<U256, DomainError> {
    if value >> 256 != U512::zero() {
        return Err(DomainError::ArithmeticOverflow);
    }
    let mut bytes = [0_u8; 64];
    value.to_big_endian(&mut bytes);
    Ok(U256::from_big_endian(&bytes[32..]))
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

    #[test]
    fn tick_math_matches_canonical_boundary_vectors() {
        assert_eq!(
            sqrt_ratio_at_tick(MIN_TICK).unwrap(),
            U256::from(4_295_128_739_u64)
        );
        assert_eq!(sqrt_ratio_at_tick(0).unwrap(), U256::one() << 96);
        assert_eq!(
            sqrt_ratio_at_tick(MAX_TICK).unwrap().to_string(),
            "1461446703485210103287273052203988822378723970342"
        );
    }

    #[test]
    fn current_range_swap_reports_same_token_capacity_and_price_impact() {
        let state = pool();
        let result =
            simulate_current_range_exact_input(&state, Amount(100), Direction::OneForZero, 10)
                .unwrap();
        assert_eq!(result.amount_in, Amount(100));
        assert_eq!(result.amount_in_less_fee, Amount(99));
        assert!(result.current_range_capacity.0 > result.amount_in_less_fee.0);
        assert!(result.utilization_bps > 0);
        assert!(result.spot_amount_out >= result.amount_out);
        assert!(result.final_sqrt_price_x96 > state.sqrt_price_x96.0);
    }

    #[test]
    fn candidate_that_reaches_unverified_tick_range_fails_closed() {
        let state = pool();
        let error = simulate_current_range_exact_input(
            &state,
            Amount(1_000_000),
            Direction::OneForZero,
            10,
        )
        .unwrap_err();
        assert_eq!(error, DomainError::StateIncomplete);
    }
}
