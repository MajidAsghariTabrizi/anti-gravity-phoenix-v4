use std::collections::HashMap;

use crate::domain::{Direction, PoolId, RouteId, TokenAddress};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolEdge {
    pub pool_id: PoolId,
    pub protocol: String,
    pub fee: u32,
    pub token_in: TokenAddress,
    pub token_out: TokenAddress,
    pub direction: Direction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub route_id: RouteId,
    pub legs: Vec<PoolEdge>,
}

#[derive(Clone, Debug, Default)]
pub struct PoolGraph {
    affected: HashMap<PoolId, Vec<Route>>,
}

impl PoolGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_two_pool_cycle(&mut self, route: Route) {
        for leg in &route.legs {
            self.affected
                .entry(leg.pool_id.clone())
                .or_default()
                .push(route.clone());
        }
    }

    pub fn affected_routes(&self, touched_pool: &PoolId) -> Vec<Route> {
        self.affected.get(touched_pool).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Address;

    #[test]
    fn returns_only_routes_affected_by_pool() {
        let token =
            TokenAddress(Address::parse("0x1111111111111111111111111111111111111111").unwrap());
        let mut graph = PoolGraph::new();
        let route = Route {
            route_id: RouteId("r1".to_string()),
            legs: vec![PoolEdge {
                pool_id: PoolId("p1".to_string()),
                protocol: "UniswapV3".to_string(),
                fee: 500,
                token_in: token.clone(),
                token_out: token,
                direction: Direction::ZeroForOne,
            }],
        };
        graph.add_two_pool_cycle(route);
        assert_eq!(graph.affected_routes(&PoolId("p1".to_string())).len(), 1);
        assert!(graph.affected_routes(&PoolId("p2".to_string())).is_empty());
    }
}
