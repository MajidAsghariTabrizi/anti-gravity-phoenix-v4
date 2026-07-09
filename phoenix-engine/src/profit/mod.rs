use crate::domain::{Amount, DomainError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfitInput {
    pub final_route_output: Amount,
    pub principal: Amount,
    pub flash_premium: Amount,
    pub expected_execution_cost: Amount,
    pub expected_ordering_cost: Amount,
    pub uncertainty_reserve: Amount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfitBreakdown {
    pub gross_profit: Amount,
    pub expected_net_profit: Amount,
}

#[derive(Clone, Debug, Default)]
pub struct ProfitModel;

impl ProfitModel {
    pub fn evaluate(&self, input: ProfitInput) -> Result<ProfitBreakdown, DomainError> {
        let gross = input.final_route_output.checked_sub(input.principal)?;
        let costs = input
            .flash_premium
            .checked_add(input.expected_execution_cost)?
            .checked_add(input.expected_ordering_cost)?
            .checked_add(input.uncertainty_reserve)?;
        let net = gross.checked_sub(costs)?;
        Ok(ProfitBreakdown {
            gross_profit: gross,
            expected_net_profit: net,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtracts_principal_premium_costs_and_reserve() {
        let model = ProfitModel;
        let result = model
            .evaluate(ProfitInput {
                final_route_output: Amount(120),
                principal: Amount(100),
                flash_premium: Amount(1),
                expected_execution_cost: Amount(2),
                expected_ordering_cost: Amount(3),
                uncertainty_reserve: Amount(4),
            })
            .unwrap();
        assert_eq!(result.gross_profit, Amount(20));
        assert_eq!(result.expected_net_profit, Amount(10));
    }
}
