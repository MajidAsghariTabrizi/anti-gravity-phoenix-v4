use crate::domain::{Amount, DomainError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizerConfig {
    pub min_amount: Amount,
    pub max_amount: Amount,
    pub max_evaluations: usize,
    pub min_profit: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizerResult {
    pub best_amount: Amount,
    pub gross_profit: i128,
    pub flash_premium: Amount,
    pub expected_execution_cost: Amount,
    pub expected_ordering_cost: Amount,
    pub uncertainty_reserve: Amount,
    pub expected_net_profit: i128,
    pub meets_minimum: bool,
    pub evaluated_amount_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateEvaluation {
    pub amount: Amount,
    pub gross_profit: i128,
    pub flash_premium: Amount,
    pub expected_execution_cost: Amount,
    pub expected_ordering_cost: Amount,
    pub uncertainty_reserve: Amount,
    pub expected_net_profit: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizeLadderConfig {
    pub min_amount: Amount,
    pub max_amount: Amount,
    pub max_evaluations: usize,
    pub explicit_sizes: Option<Vec<Amount>>,
    pub geometric_step_bps: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfitThresholdConfig {
    pub absolute_minimum: Amount,
    pub input_relative_minimum_bps: u16,
    pub conservative_cost_multiplier_bps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfitThreshold {
    pub absolute_minimum: Amount,
    pub input_relative_minimum: Amount,
    pub conservative_cost_safety_buffer: Amount,
    pub required: Amount,
}

pub fn generate_candidate_sizes(config: &SizeLadderConfig) -> Result<Vec<Amount>, DomainError> {
    if config.min_amount.0 == 0
        || config.max_amount < config.min_amount
        || config.max_evaluations == 0
        || config.explicit_sizes.is_some() && config.geometric_step_bps.is_some()
    {
        return Err(DomainError::ArithmeticUnderflow);
    }
    if let Some(explicit) = config.explicit_sizes.as_ref() {
        if explicit.is_empty()
            || explicit.len() > config.max_evaluations
            || explicit.windows(2).any(|window| window[0] >= window[1])
            || explicit
                .iter()
                .any(|amount| *amount < config.min_amount || *amount > config.max_amount)
        {
            return Err(DomainError::ArithmeticUnderflow);
        }
        return Ok(explicit.clone());
    }
    if let Some(step_bps) = config.geometric_step_bps {
        if step_bps <= 10_000 {
            return Err(DomainError::ArithmeticUnderflow);
        }
        let mut sizes = vec![config.min_amount];
        while sizes.len() < config.max_evaluations {
            let previous = sizes
                .last()
                .copied()
                .ok_or(DomainError::ArithmeticUnderflow)?;
            if previous >= config.max_amount {
                break;
            }
            let next =
                mul_div_ceil(previous.0, u128::from(step_bps), 10_000)?.min(config.max_amount.0);
            if next <= previous.0 {
                return Err(DomainError::ArithmeticUnderflow);
            }
            sizes.push(Amount(next));
        }
        return Ok(sizes);
    }
    bounded_coarse_grid(
        config.min_amount.0,
        config.max_amount.0,
        config.max_evaluations,
    )
    .map(|sizes| sizes.into_iter().map(Amount).collect())
}

pub fn calculate_profit_threshold(
    input_amount: Amount,
    conservative_total_cost: Amount,
    config: ProfitThresholdConfig,
) -> Result<ProfitThreshold, DomainError> {
    if input_amount.0 == 0
        || config.absolute_minimum.0 == 0
        || config.input_relative_minimum_bps == 0
        || config.input_relative_minimum_bps > 10_000
        || config.conservative_cost_multiplier_bps < 10_000
    {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let input_relative_minimum = Amount(mul_div_ceil(
        input_amount.0,
        u128::from(config.input_relative_minimum_bps),
        10_000,
    )?);
    let buffer_bps = config
        .conservative_cost_multiplier_bps
        .checked_sub(10_000)
        .ok_or(DomainError::ArithmeticUnderflow)?;
    let conservative_cost_safety_buffer = Amount(mul_div_ceil(
        conservative_total_cost.0,
        u128::from(buffer_bps),
        10_000,
    )?);
    let required = config
        .absolute_minimum
        .max(input_relative_minimum)
        .max(conservative_cost_safety_buffer);
    Ok(ProfitThreshold {
        absolute_minimum: config.absolute_minimum,
        input_relative_minimum,
        conservative_cost_safety_buffer,
        required,
    })
}

pub fn optimize<F>(
    cfg: OptimizerConfig,
    mut evaluate: F,
) -> Result<Option<OptimizerResult>, DomainError>
where
    F: FnMut(Amount) -> Result<CandidateEvaluation, DomainError>,
{
    if cfg.min_amount.0 == 0 || cfg.max_amount.0 < cfg.min_amount.0 || cfg.max_evaluations == 0 {
        return Ok(None);
    }
    let minimum_profit =
        i128::try_from(cfg.min_profit.0).map_err(|_| DomainError::ArithmeticOverflow)?;

    let mut candidates = coarse_grid(
        cfg.min_amount.0,
        cfg.max_amount.0,
        cfg.max_evaluations.min(16),
    )?;
    let mut best: Option<CandidateEvaluation> = None;
    let mut evaluated = 0usize;

    for amount in candidates.drain(..) {
        if evaluated >= cfg.max_evaluations {
            break;
        }
        let ev = match evaluate(Amount(amount)) {
            Ok(ev) => ev,
            Err(DomainError::ArithmeticUnderflow) => {
                evaluated += 1;
                continue;
            }
            Err(err) => return Err(err),
        };
        evaluated += 1;
        if best
            .as_ref()
            .map(|b| ev.expected_net_profit > b.expected_net_profit)
            .unwrap_or(true)
        {
            best = Some(ev);
        }
    }

    if let Some(best_ev) = best.clone() {
        let width = (cfg
            .max_amount
            .0
            .checked_sub(cfg.min_amount.0)
            .ok_or(DomainError::ArithmeticOverflow)?
            / 16)
            .max(1);
        let start = best_ev
            .amount
            .0
            .checked_sub(width)
            .unwrap_or(cfg.min_amount.0)
            .max(cfg.min_amount.0);
        let end = best_ev
            .amount
            .0
            .checked_add(width)
            .unwrap_or(cfg.max_amount.0)
            .min(cfg.max_amount.0);
        let step = (width / 4).max(1);
        let mut amount = start;
        while amount <= end && evaluated < cfg.max_evaluations {
            let ev = match evaluate(Amount(amount)) {
                Ok(ev) => ev,
                Err(DomainError::ArithmeticUnderflow) => {
                    evaluated += 1;
                    let Some(next) = amount.checked_add(step) else {
                        break;
                    };
                    amount = next;
                    continue;
                }
                Err(err) => return Err(err),
            };
            evaluated += 1;
            if best
                .as_ref()
                .map(|b| ev.expected_net_profit > b.expected_net_profit)
                .unwrap_or(true)
            {
                best = Some(ev);
            }
            let Some(next) = amount.checked_add(step) else {
                break;
            };
            amount = next;
        }
    }

    Ok(best.map(|b| OptimizerResult {
        best_amount: b.amount,
        gross_profit: b.gross_profit,
        flash_premium: b.flash_premium,
        expected_execution_cost: b.expected_execution_cost,
        expected_ordering_cost: b.expected_ordering_cost,
        uncertainty_reserve: b.uncertainty_reserve,
        expected_net_profit: b.expected_net_profit,
        meets_minimum: b.expected_net_profit >= minimum_profit,
        evaluated_amount_count: evaluated,
    }))
}

fn coarse_grid(min: u128, max: u128, count: usize) -> Result<Vec<u128>, DomainError> {
    if count <= 1 || min == max {
        return Ok(vec![min]);
    }
    let span = max
        .checked_sub(min)
        .ok_or(DomainError::ArithmeticOverflow)?;
    let denominator = u128::try_from(
        count
            .checked_sub(1)
            .ok_or(DomainError::ArithmeticOverflow)?,
    )
    .map_err(|_| DomainError::ArithmeticOverflow)?;
    (0..count)
        .map(|i| {
            let index = u128::try_from(i).map_err(|_| DomainError::ArithmeticOverflow)?;
            span.checked_mul(index)
                .map(|scaled| scaled / denominator)
                .and_then(|offset| min.checked_add(offset))
                .ok_or(DomainError::ArithmeticOverflow)
        })
        .collect()
}

fn bounded_coarse_grid(min: u128, max: u128, count: usize) -> Result<Vec<u128>, DomainError> {
    if count <= 1 || min == max {
        return Ok(vec![min]);
    }
    let span = max
        .checked_sub(min)
        .ok_or(DomainError::ArithmeticOverflow)?;
    let requested_intervals = count
        .checked_sub(1)
        .ok_or(DomainError::ArithmeticOverflow)?;
    let effective_count = if span
        < u128::try_from(requested_intervals).map_err(|_| DomainError::ArithmeticOverflow)?
    {
        usize::try_from(span.checked_add(1).ok_or(DomainError::ArithmeticOverflow)?)
            .map_err(|_| DomainError::ArithmeticOverflow)?
    } else {
        count
    };
    let denominator = u128::try_from(
        effective_count
            .checked_sub(1)
            .ok_or(DomainError::ArithmeticOverflow)?,
    )
    .map_err(|_| DomainError::ArithmeticOverflow)?;
    let quotient = span / denominator;
    let remainder = span % denominator;
    (0..effective_count)
        .map(|index| {
            let index = u128::try_from(index).map_err(|_| DomainError::ArithmeticOverflow)?;
            let whole = quotient
                .checked_mul(index)
                .ok_or(DomainError::ArithmeticOverflow)?;
            let fraction = remainder
                .checked_mul(index)
                .ok_or(DomainError::ArithmeticOverflow)?
                / denominator;
            min.checked_add(whole)
                .and_then(|value| value.checked_add(fraction))
                .ok_or(DomainError::ArithmeticOverflow)
        })
        .collect()
}

fn mul_div_ceil(value: u128, multiplier: u128, denominator: u128) -> Result<u128, DomainError> {
    if denominator == 0 {
        return Err(DomainError::ArithmeticUnderflow);
    }
    let product = value
        .checked_mul(multiplier)
        .ok_or(DomainError::ArithmeticOverflow)?;
    let quotient = product / denominator;
    let remainder = product % denominator;
    quotient
        .checked_add(u128::from(remainder != 0))
        .ok_or(DomainError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_dynamic_amount_not_first_candidate() {
        let cfg = OptimizerConfig {
            min_amount: Amount(100),
            max_amount: Amount(900),
            max_evaluations: 25,
            min_profit: Amount(1),
        };
        let result = optimize(cfg, |amount| {
            let distance = amount.0.abs_diff(500);
            let net = 1_000u128.saturating_sub(distance * 2) as i128;
            Ok(CandidateEvaluation {
                amount,
                gross_profit: net + 10,
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(0),
                expected_net_profit: net,
            })
        })
        .unwrap()
        .unwrap();
        assert_ne!(result.best_amount, Amount(100));
        assert_eq!(result.best_amount, Amount(500));
        assert!(result.meets_minimum);
    }

    #[test]
    fn skips_underflow_candidates_and_keeps_searching() {
        let cfg = OptimizerConfig {
            min_amount: Amount(100),
            max_amount: Amount(300),
            max_evaluations: 3,
            min_profit: Amount(1),
        };
        let result = optimize(cfg, |amount| {
            if amount == Amount(100) {
                return Err(DomainError::ArithmeticUnderflow);
            }
            let net = if amount == Amount(200) { 50 } else { 0 };
            Ok(CandidateEvaluation {
                amount,
                gross_profit: net + 10,
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(0),
                expected_net_profit: net,
            })
        })
        .unwrap()
        .unwrap();
        assert_eq!(result.best_amount, Amount(200));
        assert!(result.meets_minimum);
    }

    #[test]
    fn propagates_non_underflow_errors() {
        let cfg = OptimizerConfig {
            min_amount: Amount(100),
            max_amount: Amount(300),
            max_evaluations: 3,
            min_profit: Amount(1),
        };
        let err = optimize(cfg, |_| Err(DomainError::ArithmeticOverflow)).unwrap_err();
        assert_eq!(err, DomainError::ArithmeticOverflow);
    }

    #[test]
    fn returns_best_observed_candidate_below_minimum() {
        let cfg = OptimizerConfig {
            min_amount: Amount(100),
            max_amount: Amount(300),
            max_evaluations: 3,
            min_profit: Amount(100),
        };
        let result = optimize(cfg, |amount| {
            let net = i128::try_from(amount.0).unwrap() - 350;
            Ok(CandidateEvaluation {
                amount,
                gross_profit: net + 10,
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(0),
                expected_net_profit: net,
            })
        })
        .unwrap()
        .unwrap();
        assert_eq!(result.best_amount, Amount(300));
        assert_eq!(result.expected_net_profit, -50);
        assert!(!result.meets_minimum);
    }

    #[test]
    fn rejects_grid_arithmetic_overflow() {
        let cfg = OptimizerConfig {
            min_amount: Amount(1),
            max_amount: Amount(u128::MAX),
            max_evaluations: 16,
            min_profit: Amount(1),
        };
        assert_eq!(
            optimize(cfg, |_| unreachable!()),
            Err(DomainError::ArithmeticOverflow)
        );
    }

    #[test]
    fn explicit_and_geometric_size_ladders_are_deterministic() {
        let explicit = generate_candidate_sizes(&SizeLadderConfig {
            min_amount: Amount(100),
            max_amount: Amount(800),
            max_evaluations: 4,
            explicit_sizes: Some(vec![Amount(100), Amount(200), Amount(400), Amount(800)]),
            geometric_step_bps: None,
        })
        .unwrap();
        assert_eq!(
            explicit,
            [Amount(100), Amount(200), Amount(400), Amount(800)]
        );
        let geometric = generate_candidate_sizes(&SizeLadderConfig {
            min_amount: Amount(100),
            max_amount: Amount(800),
            max_evaluations: 4,
            explicit_sizes: None,
            geometric_step_bps: Some(20_000),
        })
        .unwrap();
        assert_eq!(
            geometric,
            [Amount(100), Amount(200), Amount(400), Amount(800)]
        );
    }

    #[test]
    fn duplicate_or_unordered_explicit_sizes_fail_closed() {
        for sizes in [
            vec![Amount(100), Amount(100)],
            vec![Amount(200), Amount(100)],
        ] {
            assert_eq!(
                generate_candidate_sizes(&SizeLadderConfig {
                    min_amount: Amount(100),
                    max_amount: Amount(200),
                    max_evaluations: 2,
                    explicit_sizes: Some(sizes),
                    geometric_step_bps: None,
                }),
                Err(DomainError::ArithmeticUnderflow)
            );
        }
    }

    #[test]
    fn coarse_ladder_never_repeats_small_integer_sizes() {
        assert_eq!(
            generate_candidate_sizes(&SizeLadderConfig {
                min_amount: Amount(1),
                max_amount: Amount(2),
                max_evaluations: 32,
                explicit_sizes: None,
                geometric_step_bps: None,
            })
            .unwrap(),
            [Amount(1), Amount(2)]
        );
    }

    #[test]
    fn small_input_does_not_inherit_one_token_threshold() {
        let threshold = calculate_profit_threshold(
            Amount(1_000_000_000_000_000),
            Amount(4_000_000_000_000),
            ProfitThresholdConfig {
                absolute_minimum: Amount(1_000_000_000_000),
                input_relative_minimum_bps: 10,
                conservative_cost_multiplier_bps: 12_500,
            },
        )
        .unwrap();
        assert_eq!(threshold.required, Amount(1_000_000_000_000));
        assert_ne!(threshold.required, Amount(1_000_000_000_000_000_000));

        let explicitly_configured = calculate_profit_threshold(
            Amount(1_000_000_000_000_000),
            Amount(4_000_000_000_000),
            ProfitThresholdConfig {
                absolute_minimum: Amount(1_000_000_000_000_000_000),
                input_relative_minimum_bps: 10,
                conservative_cost_multiplier_bps: 12_500,
            },
        )
        .unwrap();
        assert_eq!(
            explicitly_configured.required,
            Amount(1_000_000_000_000_000_000)
        );
    }
}
