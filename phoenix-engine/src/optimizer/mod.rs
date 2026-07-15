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
}
