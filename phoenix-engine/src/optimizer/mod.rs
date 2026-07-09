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
    pub gross_profit: Amount,
    pub flash_premium: Amount,
    pub expected_execution_cost: Amount,
    pub expected_ordering_cost: Amount,
    pub uncertainty_reserve: Amount,
    pub expected_net_profit: Amount,
    pub evaluated_amount_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateEvaluation {
    pub amount: Amount,
    pub gross_profit: Amount,
    pub flash_premium: Amount,
    pub expected_execution_cost: Amount,
    pub expected_ordering_cost: Amount,
    pub uncertainty_reserve: Amount,
    pub expected_net_profit: Amount,
}

pub fn optimize<F>(
    cfg: OptimizerConfig,
    mut evaluate: F,
) -> Result<Option<OptimizerResult>, DomainError>
where
    F: FnMut(Amount) -> Result<CandidateEvaluation, DomainError>,
{
    if cfg.min_amount.0 == 0
        || cfg.max_amount.0 < cfg.min_amount.0
        || cfg.max_evaluations == 0
    {
        return Ok(None);
    }

    let mut candidates = coarse_grid(
        cfg.min_amount.0,
        cfg.max_amount.0,
        cfg.max_evaluations.min(16),
    );
    let mut best: Option<CandidateEvaluation> = None;
    let mut evaluated = 0usize;

    for amount in candidates.drain(..) {
        if evaluated >= cfg.max_evaluations {
            break;
        }
        let ev = evaluate(Amount(amount))?;
        evaluated += 1;
        if ev.expected_net_profit >= cfg.min_profit
            && best
                .as_ref()
                .map(|b| ev.expected_net_profit > b.expected_net_profit)
                .unwrap_or(true)
        {
            best = Some(ev);
        }
    }

    if let Some(best_ev) = best.clone() {
        let width = ((cfg.max_amount.0 - cfg.min_amount.0) / 16).max(1);
        let start = best_ev.amount.0.saturating_sub(width);
        let end = (best_ev.amount.0 + width).min(cfg.max_amount.0);
        let step = (width / 4).max(1);
        let mut amount = start;
        while amount <= end && evaluated < cfg.max_evaluations {
            let ev = evaluate(Amount(amount))?;
            evaluated += 1;
            if ev.expected_net_profit >= cfg.min_profit
                && best
                    .as_ref()
                    .map(|b| ev.expected_net_profit > b.expected_net_profit)
                    .unwrap_or(true)
            {
                best = Some(ev);
            }
            amount = amount.saturating_add(step);
            if step == 0 {
                break;
            }
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
        evaluated_amount_count: evaluated,
    }))
}

fn coarse_grid(min: u128, max: u128, count: usize) -> Vec<u128> {
    if count <= 1 || min == max {
        return vec![min];
    }
    let span = max - min;
    (0..count)
        .map(|i| min + (span * i as u128) / (count as u128 - 1))
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
            let net = 1_000u128.saturating_sub(distance * 2);
            Ok(CandidateEvaluation {
                amount,
                gross_profit: Amount(net + 10),
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(0),
                uncertainty_reserve: Amount(0),
                expected_net_profit: Amount(net),
            })
        })
        .unwrap()
        .unwrap();
        assert_ne!(result.best_amount, Amount(100));
        assert_eq!(result.best_amount, Amount(500));
    }
}
