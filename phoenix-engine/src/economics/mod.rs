use crate::domain::Amount;
use crate::opportunity::{
    BasisPoints, CostBreakdown, PrimaryProfitabilityStatus, ScenarioEconomics, SignedAmount,
    PROFITABILITY_MODEL_VERSION,
};

const BPS_DENOMINATOR: u128 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EconomicInput {
    pub principal: Amount,
    pub gross_output: Amount,
    pub protocol_fees: Amount,
    pub pool_fees: Amount,
    pub price_impact: Amount,
    pub minimum_slippage_buffer: Amount,
    pub flash_loan_fee: Amount,
    pub estimated_execution_gas: u64,
    pub gas_price_wei: u128,
    pub l1_data_fee: Amount,
    pub contract_overhead: Amount,
    pub failed_attempt_gas_cost: Amount,
    pub failure_probability_bps: u16,
    pub stale_state_loss: Amount,
    pub stale_quote_probability_bps: u16,
    pub state_drift_reserve: Amount,
    pub latency_reserve: Amount,
    pub uncertainty_reserve: Amount,
    pub replacement_transaction_cost: Amount,
    pub probability_of_success_bps: u16,
    pub minimum_required_net_pnl: SignedAmount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScenarioConfig {
    pub gas_multiplier_bps: u32,
    pub l1_fee_multiplier_bps: u32,
    pub slippage_multiplier_bps: u32,
    pub price_impact_multiplier_bps: u32,
    pub failure_multiplier_bps: u32,
    pub stale_state_multiplier_bps: u32,
    pub state_drift_multiplier_bps: u32,
    pub latency_multiplier_bps: u32,
    pub uncertainty_multiplier_bps: u32,
    pub replacement_cost_multiplier_bps: u32,
}

impl ScenarioConfig {
    pub const BASE: Self = Self {
        gas_multiplier_bps: 10_000,
        l1_fee_multiplier_bps: 10_000,
        slippage_multiplier_bps: 10_000,
        price_impact_multiplier_bps: 10_000,
        failure_multiplier_bps: 10_000,
        stale_state_multiplier_bps: 10_000,
        state_drift_multiplier_bps: 10_000,
        latency_multiplier_bps: 10_000,
        uncertainty_multiplier_bps: 10_000,
        replacement_cost_multiplier_bps: 0,
    };

    pub const CONSERVATIVE: Self = Self {
        gas_multiplier_bps: 12_500,
        l1_fee_multiplier_bps: 12_500,
        slippage_multiplier_bps: 15_000,
        price_impact_multiplier_bps: 12_500,
        failure_multiplier_bps: 15_000,
        stale_state_multiplier_bps: 15_000,
        state_drift_multiplier_bps: 15_000,
        latency_multiplier_bps: 15_000,
        uncertainty_multiplier_bps: 15_000,
        replacement_cost_multiplier_bps: 0,
    };

    pub const SEVERE: Self = Self {
        gas_multiplier_bps: 20_000,
        l1_fee_multiplier_bps: 20_000,
        slippage_multiplier_bps: 30_000,
        price_impact_multiplier_bps: 20_000,
        failure_multiplier_bps: 20_000,
        stale_state_multiplier_bps: 25_000,
        state_drift_multiplier_bps: 25_000,
        latency_multiplier_bps: 25_000,
        uncertainty_multiplier_bps: 25_000,
        replacement_cost_multiplier_bps: 10_000,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EconomicError {
    InvalidProbability,
    InvalidScenario,
    ArithmeticOverflow,
}

pub fn evaluate_scenarios(input: &EconomicInput) -> Result<ScenarioEconomics, EconomicError> {
    let base = evaluate(input, ScenarioConfig::BASE)?;
    Ok(ScenarioEconomics {
        primary_status: if base.expected_net_pnl >= input.minimum_required_net_pnl {
            PrimaryProfitabilityStatus::MeetsMinimum
        } else {
            PrimaryProfitabilityStatus::BelowMinimum
        },
        base,
        conservative: evaluate(input, ScenarioConfig::CONSERVATIVE)?,
        severe: evaluate(input, ScenarioConfig::SEVERE)?,
        minimum_required_net_pnl: input.minimum_required_net_pnl,
        model_version: PROFITABILITY_MODEL_VERSION.to_string(),
    })
}

pub fn evaluate(
    input: &EconomicInput,
    scenario: ScenarioConfig,
) -> Result<CostBreakdown, EconomicError> {
    validate(input, scenario)?;

    let gross_spread = signed_difference(input.gross_output.0, input.principal.0)?;
    let gas_cost = (input.estimated_execution_gas as u128)
        .checked_mul(input.gas_price_wei)
        .ok_or(EconomicError::ArithmeticOverflow)?;
    let arbitrum_execution_fee = scale(gas_cost, scenario.gas_multiplier_bps)?;
    let l1_data_fee = scale(input.l1_data_fee.0, scenario.l1_fee_multiplier_bps)?;
    let slippage = scale(
        input.minimum_slippage_buffer.0,
        scenario.slippage_multiplier_bps,
    )?;
    let price_impact = scale(input.price_impact.0, scenario.price_impact_multiplier_bps)?;
    let failure_probability = scale_probability(
        input.failure_probability_bps,
        scenario.failure_multiplier_bps,
    )?;
    let stale_probability = scale_probability(
        input.stale_quote_probability_bps,
        scenario.stale_state_multiplier_bps,
    )?;
    let failure_cost_reserve =
        probability_cost(input.failed_attempt_gas_cost.0, failure_probability)?;
    let stale_probability_cost = probability_cost(input.stale_state_loss.0, stale_probability)?;
    let state_drift = scale(
        input.state_drift_reserve.0,
        scenario.state_drift_multiplier_bps,
    )?;
    let latency = scale(input.latency_reserve.0, scenario.latency_multiplier_bps)?;
    let stale_state_penalty = stale_probability_cost;
    let uncertainty_reserve = scale(
        input.uncertainty_reserve.0,
        scenario.uncertainty_multiplier_bps,
    )?;
    let replacement_cost = scale(
        input.replacement_transaction_cost.0,
        scenario.replacement_cost_multiplier_bps,
    )?;
    let contract_overhead = input.contract_overhead.0;

    let market_cost = checked_sum(&[input.protocol_fees.0, input.pool_fees.0, price_impact])?;
    let gross_profit = gross_spread
        .0
        .checked_sub(as_i128(market_cost)?)
        .ok_or(EconomicError::ArithmeticOverflow)?;

    let total_cost = checked_sum(&[
        input.protocol_fees.0,
        input.pool_fees.0,
        price_impact,
        slippage,
        input.flash_loan_fee.0,
        arbitrum_execution_fee,
        l1_data_fee,
        contract_overhead,
        failure_cost_reserve,
        stale_state_penalty,
        replacement_cost,
        state_drift,
        latency,
        uncertainty_reserve,
    ])?;
    let expected_net_pnl = gross_spread
        .0
        .checked_sub(as_i128(total_cost)?)
        .ok_or(EconomicError::ArithmeticOverflow)?;
    let roi_bps = if input.principal.0 == 0 {
        0
    } else {
        expected_net_pnl
            .checked_mul(BPS_DENOMINATOR as i128)
            .ok_or(EconomicError::ArithmeticOverflow)?
            / as_i128(input.principal.0)?
    };
    let expected_value = expected_net_pnl
        .checked_mul(input.probability_of_success_bps as i128)
        .ok_or(EconomicError::ArithmeticOverflow)?
        / BPS_DENOMINATOR as i128;

    Ok(CostBreakdown {
        gross_spread,
        gross_profit: SignedAmount(gross_profit),
        protocol_fees: input.protocol_fees,
        pool_fees: input.pool_fees,
        price_impact: Amount(price_impact),
        slippage_allowance: Amount(slippage),
        flash_loan_fee: input.flash_loan_fee,
        estimated_execution_gas: input.estimated_execution_gas,
        gas_price_wei: input.gas_price_wei,
        arbitrum_execution_fee: Amount(arbitrum_execution_fee),
        l1_data_fee: Amount(l1_data_fee),
        contract_overhead: Amount(contract_overhead),
        failure_cost_reserve: Amount(failure_cost_reserve),
        stale_state_penalty: Amount(stale_state_penalty),
        ordering_reserve: Amount(replacement_cost),
        state_drift_reserve: Amount(state_drift),
        latency_reserve: Amount(latency),
        uncertainty_reserve: Amount(uncertainty_reserve),
        total_cost: Amount(total_cost),
        expected_net_pnl: SignedAmount(expected_net_pnl),
        expected_roi_bps: BasisPoints(
            i32::try_from(roi_bps).map_err(|_| EconomicError::ArithmeticOverflow)?,
        ),
        probability_of_success_bps: input.probability_of_success_bps,
        expected_value_after_success_probability: SignedAmount(expected_value),
    })
}

fn validate(input: &EconomicInput, scenario: ScenarioConfig) -> Result<(), EconomicError> {
    if input.failure_probability_bps > 10_000
        || input.stale_quote_probability_bps > 10_000
        || input.probability_of_success_bps > 10_000
        || input.minimum_required_net_pnl.0 < 0
    {
        return Err(EconomicError::InvalidProbability);
    }
    let multipliers = [
        scenario.gas_multiplier_bps,
        scenario.l1_fee_multiplier_bps,
        scenario.slippage_multiplier_bps,
        scenario.price_impact_multiplier_bps,
        scenario.failure_multiplier_bps,
        scenario.stale_state_multiplier_bps,
        scenario.state_drift_multiplier_bps,
        scenario.latency_multiplier_bps,
        scenario.uncertainty_multiplier_bps,
        scenario.replacement_cost_multiplier_bps,
    ];
    if multipliers.iter().any(|value| *value > 100_000) {
        return Err(EconomicError::InvalidScenario);
    }
    Ok(())
}

fn signed_difference(lhs: u128, rhs: u128) -> Result<SignedAmount, EconomicError> {
    if lhs >= rhs {
        Ok(SignedAmount(as_i128(
            lhs.checked_sub(rhs)
                .ok_or(EconomicError::ArithmeticOverflow)?,
        )?))
    } else {
        Ok(SignedAmount(
            as_i128(
                rhs.checked_sub(lhs)
                    .ok_or(EconomicError::ArithmeticOverflow)?,
            )?
            .checked_neg()
            .ok_or(EconomicError::ArithmeticOverflow)?,
        ))
    }
}

fn as_i128(value: u128) -> Result<i128, EconomicError> {
    i128::try_from(value).map_err(|_| EconomicError::ArithmeticOverflow)
}

fn scale(value: u128, multiplier_bps: u32) -> Result<u128, EconomicError> {
    value
        .checked_mul(multiplier_bps as u128)
        .ok_or(EconomicError::ArithmeticOverflow)
        .map(|scaled| scaled / BPS_DENOMINATOR)
}

fn scale_probability(probability_bps: u16, multiplier_bps: u32) -> Result<u16, EconomicError> {
    let scaled = u128::from(probability_bps)
        .checked_mul(u128::from(multiplier_bps))
        .ok_or(EconomicError::ArithmeticOverflow)?
        / BPS_DENOMINATOR;
    u16::try_from(scaled.min(BPS_DENOMINATOR)).map_err(|_| EconomicError::ArithmeticOverflow)
}

fn probability_cost(value: u128, probability_bps: u16) -> Result<u128, EconomicError> {
    scale(value, probability_bps as u32)
}

fn checked_sum(values: &[u128]) -> Result<u128, EconomicError> {
    values.iter().try_fold(0u128, |total, value| {
        total
            .checked_add(*value)
            .ok_or(EconomicError::ArithmeticOverflow)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> EconomicInput {
        EconomicInput {
            principal: Amount(1_000_000),
            gross_output: Amount(1_100_000),
            protocol_fees: Amount(1_000),
            pool_fees: Amount(2_000),
            price_impact: Amount(3_000),
            minimum_slippage_buffer: Amount(4_000),
            flash_loan_fee: Amount(5_000),
            estimated_execution_gas: 10,
            gas_price_wei: 100,
            l1_data_fee: Amount(2_000),
            contract_overhead: Amount(1_000),
            failed_attempt_gas_cost: Amount(10_000),
            failure_probability_bps: 1_000,
            stale_state_loss: Amount(20_000),
            stale_quote_probability_bps: 500,
            state_drift_reserve: Amount(2_000),
            latency_reserve: Amount(1_000),
            uncertainty_reserve: Amount(2_000),
            replacement_transaction_cost: Amount(5_000),
            probability_of_success_bps: 9_000,
            minimum_required_net_pnl: SignedAmount(1),
        }
    }

    #[test]
    fn charges_every_base_cost_once() {
        let result = evaluate(&input(), ScenarioConfig::BASE).unwrap();
        assert_eq!(result.gross_spread, SignedAmount(100_000));
        assert_eq!(result.arbitrum_execution_fee, Amount(1_000));
        assert_eq!(result.failure_cost_reserve, Amount(1_000));
        assert_eq!(result.stale_state_penalty, Amount(1_000));
        assert_eq!(result.state_drift_reserve, Amount(2_000));
        assert_eq!(result.latency_reserve, Amount(1_000));
        assert_eq!(result.gross_profit, SignedAmount(94_000));
        assert_eq!(result.total_cost, Amount(26_000));
        assert_eq!(result.expected_net_pnl, SignedAmount(74_000));
        assert_eq!(
            result.expected_value_after_success_probability,
            SignedAmount(66_600)
        );
    }

    #[test]
    fn scenarios_are_monotonically_more_conservative() {
        let result = evaluate_scenarios(&input()).unwrap();
        assert!(result.base.expected_net_pnl >= result.conservative.expected_net_pnl);
        assert!(result.conservative.expected_net_pnl >= result.severe.expected_net_pnl);
        assert_eq!(
            result.primary_status,
            PrimaryProfitabilityStatus::MeetsMinimum
        );
        assert_eq!(result.model_version, PROFITABILITY_MODEL_VERSION);
    }

    #[test]
    fn costs_exceeding_spread_cannot_produce_positive_pnl() {
        let mut candidate = input();
        candidate.gross_output = Amount(1_001_000);
        assert!(
            evaluate(&candidate, ScenarioConfig::BASE)
                .unwrap()
                .expected_net_pnl
                < SignedAmount(0)
        );
    }

    #[test]
    fn increasing_gas_or_slippage_never_increases_pnl() {
        let baseline = evaluate(&input(), ScenarioConfig::BASE)
            .unwrap()
            .expected_net_pnl;
        let mut stressed = input();
        stressed.gas_price_wei += 1;
        stressed.minimum_slippage_buffer.0 += 1;
        assert!(
            evaluate(&stressed, ScenarioConfig::BASE)
                .unwrap()
                .expected_net_pnl
                <= baseline
        );
    }

    #[test]
    fn rejects_invalid_probabilities() {
        let mut invalid = input();
        invalid.failure_probability_bps = 10_001;
        assert_eq!(
            evaluate(&invalid, ScenarioConfig::BASE),
            Err(EconomicError::InvalidProbability)
        );
    }

    #[test]
    fn rejects_negative_minimum_profitability_threshold() {
        let mut invalid = input();
        invalid.minimum_required_net_pnl = SignedAmount(-1);
        assert_eq!(
            evaluate_scenarios(&invalid),
            Err(EconomicError::InvalidScenario)
        );
    }

    #[test]
    fn below_threshold_status_preserves_signed_economics() {
        let mut candidate = input();
        candidate.minimum_required_net_pnl = SignedAmount(80_000);
        let result = evaluate_scenarios(&candidate).unwrap();
        assert_eq!(
            result.primary_status,
            PrimaryProfitabilityStatus::BelowMinimum
        );
        assert_eq!(result.base.expected_net_pnl, SignedAmount(74_000));
    }

    #[test]
    fn checked_arithmetic_rejects_overflow() {
        let mut candidate = input();
        candidate.estimated_execution_gas = u64::MAX;
        candidate.gas_price_wei = u128::MAX;
        assert_eq!(
            evaluate(&candidate, ScenarioConfig::BASE),
            Err(EconomicError::ArithmeticOverflow)
        );
    }
}
