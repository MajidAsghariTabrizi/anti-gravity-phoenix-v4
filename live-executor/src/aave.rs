use crate::abi::RpcLog;
use crate::model::{CanonicalAddress, ExecutionRequest, ExecutionRouteType, ValidatedLeg};
use chrono::{DateTime, Utc};
use ethabi::{Contract, RawLog, Token};
use primitive_types::{H256, U256};
use std::io::Cursor;
use thiserror::Error;

const MAX_CALLDATA_BYTES: usize = 128 * 1024;
const MAX_LOG_DATA_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveLiquidationRequest {
    pub route_id: [u8; 32],
    pub borrower: CanonicalAddress,
    pub debt_asset: CanonicalAddress,
    pub collateral_asset: CanonicalAddress,
    pub repay_amount: u128,
    pub maximum_input_amount: u128,
    pub minimum_collateral_received: u128,
    pub minimum_unwind_output: u128,
    pub minimum_profit: u128,
    pub maximum_atlas_bid: u128,
    pub deadline: DateTime<Utc>,
    pub unwind_legs: Vec<ValidatedLeg>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveLiquidationSettlement {
    pub borrower: CanonicalAddress,
    pub debt_asset: CanonicalAddress,
    pub repay_amount: u128,
    pub premium: u128,
    pub atlas_bid: u128,
    pub realized_profit: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveLiquidationIdentity {
    pub route_id: [u8; 32],
    pub borrower: CanonicalAddress,
    pub debt_asset: CanonicalAddress,
    pub collateral_asset: CanonicalAddress,
    pub repay_amount: u128,
    pub maximum_input_amount: u128,
    pub minimum_collateral_received: u128,
    pub minimum_unwind_output: u128,
    pub minimum_profit: u128,
    pub maximum_atlas_bid: u128,
    pub deadline: u64,
    pub unwind_legs: Vec<AaveLiquidationLegIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaveLiquidationLegIdentity {
    pub pool: CanonicalAddress,
    pub token_in: CanonicalAddress,
    pub token_out: CanonicalAddress,
    pub fee: u32,
    pub zero_for_one: bool,
    pub minimum_amount_out: u128,
}

impl AaveLiquidationRequest {
    pub fn decode_encoded_identity(
        encoded: &[u8],
    ) -> Result<AaveLiquidationIdentity, AaveAbiError> {
        if encoded.is_empty() || encoded.len() > MAX_CALLDATA_BYTES {
            return Err(AaveAbiError::InvalidRequest);
        }
        let function = contract()
            .and_then(|abi| abi.function("executeAaveLiquidation").cloned())
            .map_err(|_| AaveAbiError::Contract)?;
        let input = function
            .inputs
            .first()
            .ok_or(AaveAbiError::Contract)?
            .kind
            .clone();
        let mut tokens =
            ethabi::decode(&[input], encoded).map_err(|_| AaveAbiError::InvalidRequest)?;
        let Token::Tuple(fields) = tokens.pop().ok_or(AaveAbiError::InvalidRequest)? else {
            return Err(AaveAbiError::InvalidRequest);
        };
        if fields.len() != 13 {
            return Err(AaveAbiError::InvalidRequest);
        }
        let route_id = token_fixed_bytes(&fields[0])?;
        let borrower = token_address(&fields[1])?;
        let debt_asset = token_address(&fields[2])?;
        let collateral_asset = token_address(&fields[3])?;
        let repay_amount = token_uint(&fields[4])?;
        if fields[5] != Token::Bool(false) {
            return Err(AaveAbiError::InvalidRequest);
        }
        let maximum_input_amount = token_uint(&fields[6])?;
        let minimum_collateral_received = token_uint(&fields[7])?;
        let minimum_unwind_output = token_uint(&fields[8])?;
        let minimum_profit = token_uint(&fields[9])?;
        let maximum_atlas_bid = token_uint(&fields[10])?;
        let deadline =
            u64::try_from(token_uint(&fields[11])?).map_err(|_| AaveAbiError::InvalidRequest)?;
        let Token::Array(legs) = &fields[12] else {
            return Err(AaveAbiError::InvalidRequest);
        };
        let unwind_legs = legs
            .iter()
            .map(decode_leg_identity)
            .collect::<Result<Vec<_>, _>>()?;
        if repay_amount == 0
            || maximum_input_amount < repay_amount
            || minimum_collateral_received == 0
            || minimum_unwind_output == 0
            || minimum_profit == 0
            || deadline == 0
            || unwind_legs.len() > crate::model::MAX_ROUTE_LEGS
        {
            return Err(AaveAbiError::InvalidRequest);
        }
        Ok(AaveLiquidationIdentity {
            route_id,
            borrower,
            debt_asset,
            collateral_asset,
            repay_amount,
            maximum_input_amount,
            minimum_collateral_received,
            minimum_unwind_output,
            minimum_profit,
            maximum_atlas_bid,
            deadline,
            unwind_legs,
        })
    }

    pub fn from_execution(request: &ExecutionRequest) -> Result<Self, AaveAbiError> {
        if request.route_type != ExecutionRouteType::AaveLiquidationV1 {
            return Err(AaveAbiError::InvalidRequest);
        }
        let route = request
            .aave_liquidation
            .as_ref()
            .ok_or(AaveAbiError::InvalidRequest)?;
        let result = Self {
            route_id: request.route_id,
            borrower: route.borrower,
            debt_asset: route.debt_asset,
            collateral_asset: route.collateral_asset,
            repay_amount: request.flash_amount,
            maximum_input_amount: request.maximum_input_amount,
            minimum_collateral_received: route.minimum_collateral_received,
            minimum_unwind_output: route.minimum_unwind_output,
            minimum_profit: request.minimum_profit,
            maximum_atlas_bid: route.maximum_atlas_bid,
            deadline: request.deadline,
            unwind_legs: request.legs.clone(),
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), AaveAbiError> {
        if self.repay_amount == 0
            || self.maximum_input_amount < self.repay_amount
            || self.minimum_collateral_received == 0
            || self.minimum_unwind_output == 0
            || self.minimum_profit == 0
            || self.deadline <= Utc::now()
            || self.unwind_legs.len() > crate::model::MAX_ROUTE_LEGS
        {
            return Err(AaveAbiError::InvalidRequest);
        }
        if self.collateral_asset == self.debt_asset {
            if !self.unwind_legs.is_empty() {
                return Err(AaveAbiError::InvalidRequest);
            }
        } else {
            let mut expected = self.collateral_asset;
            for leg in &self.unwind_legs {
                if leg.token_in != expected
                    || leg.token_in == leg.token_out
                    || leg.min_amount_out == 0
                    || leg.factory.is_none()
                {
                    return Err(AaveAbiError::InvalidRequest);
                }
                expected = leg.token_out;
            }
            if self.unwind_legs.is_empty() || expected != self.debt_asset {
                return Err(AaveAbiError::InvalidRequest);
            }
        }
        Ok(())
    }

    pub fn encoded_request(&self) -> Result<Vec<u8>, AaveAbiError> {
        self.validate()?;
        let encoded = ethabi::encode(&[request_token(self)?]);
        if encoded.len() > MAX_CALLDATA_BYTES {
            return Err(AaveAbiError::OversizedCalldata);
        }
        Ok(encoded)
    }

    pub fn encode_direct_call(&self) -> Result<Vec<u8>, AaveAbiError> {
        self.validate()?;
        let request = request_token(self)?;
        let encoded = contract()
            .and_then(|abi| abi.function("executeAaveLiquidation").cloned())
            .and_then(|function| function.encode_input(&[request]))
            .map_err(|_| AaveAbiError::Contract)?;
        if encoded.len() > MAX_CALLDATA_BYTES {
            return Err(AaveAbiError::OversizedCalldata);
        }
        Ok(encoded)
    }

    pub fn decode_settlement(
        &self,
        executor: CanonicalAddress,
        logs: &[RpcLog],
    ) -> Result<AaveLiquidationSettlement, AaveAbiError> {
        self.identity().decode_settlement(executor, logs)
    }

    fn identity(&self) -> AaveLiquidationIdentity {
        AaveLiquidationIdentity {
            route_id: self.route_id,
            borrower: self.borrower,
            debt_asset: self.debt_asset,
            collateral_asset: self.collateral_asset,
            repay_amount: self.repay_amount,
            maximum_input_amount: self.maximum_input_amount,
            minimum_collateral_received: self.minimum_collateral_received,
            minimum_unwind_output: self.minimum_unwind_output,
            minimum_profit: self.minimum_profit,
            maximum_atlas_bid: self.maximum_atlas_bid,
            deadline: u64::try_from(self.deadline.timestamp())
                .expect("validated Aave deadline is a positive u64"),
            unwind_legs: self
                .unwind_legs
                .iter()
                .map(|leg| AaveLiquidationLegIdentity {
                    pool: leg.pool,
                    token_in: leg.token_in,
                    token_out: leg.token_out,
                    fee: leg.fee,
                    zero_for_one: leg.zero_for_one,
                    minimum_amount_out: leg.min_amount_out,
                })
                .collect(),
        }
    }
}

impl AaveLiquidationIdentity {
    pub fn decode_settlement(
        &self,
        executor: CanonicalAddress,
        logs: &[RpcLog],
    ) -> Result<AaveLiquidationSettlement, AaveAbiError> {
        let event = contract()
            .and_then(|abi| abi.event("AaveLiquidationSettled").cloned())
            .map_err(|_| AaveAbiError::Contract)?;
        let mut matches = Vec::new();
        for log in logs {
            if log.address != executor {
                continue;
            }
            if log.data.len() > MAX_LOG_DATA_BYTES {
                return Err(AaveAbiError::InvalidSettlement);
            }
            let topics = log
                .topics
                .iter()
                .map(|topic| H256::from_slice(topic))
                .collect::<Vec<_>>();
            if topics.first() != Some(&event.signature()) {
                continue;
            }
            let parsed = event
                .parse_log(RawLog {
                    topics,
                    data: log.data.clone(),
                })
                .map_err(|_| AaveAbiError::InvalidSettlement)?;
            let route_id = fixed_bytes(&parsed.params, "routeId")?;
            let borrower = address(&parsed.params, "borrower")?;
            let debt_asset = address(&parsed.params, "debtAsset")?;
            let repay_amount = uint(&parsed.params, "repayAmount")?;
            if route_id != self.route_id
                || borrower != self.borrower
                || debt_asset != self.debt_asset
                || repay_amount != self.repay_amount
            {
                return Err(AaveAbiError::InvalidSettlement);
            }
            matches.push(AaveLiquidationSettlement {
                borrower,
                debt_asset,
                repay_amount,
                premium: uint(&parsed.params, "premium")?,
                atlas_bid: uint(&parsed.params, "atlasBid")?,
                realized_profit: uint(&parsed.params, "realizedProfit")?,
            });
        }
        if matches.len() != 1 {
            return Err(AaveAbiError::InvalidSettlement);
        }
        matches.pop().ok_or(AaveAbiError::InvalidSettlement)
    }
}

fn request_token(request: &AaveLiquidationRequest) -> Result<Token, AaveAbiError> {
    let deadline =
        u64::try_from(request.deadline.timestamp()).map_err(|_| AaveAbiError::InvalidRequest)?;
    let legs = request
        .unwind_legs
        .iter()
        .map(|leg| {
            Token::Tuple(vec![
                Token::Address(primitive_address(leg.pool)),
                Token::Address(primitive_address(leg.token_in)),
                Token::Address(primitive_address(leg.token_out)),
                Token::Uint(U256::from(leg.fee)),
                Token::Bool(leg.zero_for_one),
                Token::Uint(U256::from(leg.min_amount_out)),
            ])
        })
        .collect();
    Ok(Token::Tuple(vec![
        Token::FixedBytes(request.route_id.to_vec()),
        Token::Address(primitive_address(request.borrower)),
        Token::Address(primitive_address(request.debt_asset)),
        Token::Address(primitive_address(request.collateral_asset)),
        Token::Uint(U256::from(request.repay_amount)),
        Token::Bool(false),
        Token::Uint(U256::from(request.maximum_input_amount)),
        Token::Uint(U256::from(request.minimum_collateral_received)),
        Token::Uint(U256::from(request.minimum_unwind_output)),
        Token::Uint(U256::from(request.minimum_profit)),
        Token::Uint(U256::from(request.maximum_atlas_bid)),
        Token::Uint(U256::from(deadline)),
        Token::Array(legs),
    ]))
}

fn contract() -> Result<Contract, ethabi::Error> {
    Contract::load(Cursor::new(include_bytes!(
        "../../fork-sandbox/abi/PhoenixExecutor.json"
    )))
}

fn primitive_address(value: CanonicalAddress) -> ethabi::Address {
    ethabi::Address::from_slice(value.as_bytes())
}

fn parameter<'a>(params: &'a [ethabi::LogParam], name: &str) -> Result<&'a Token, AaveAbiError> {
    params
        .iter()
        .find(|parameter| parameter.name == name)
        .map(|parameter| &parameter.value)
        .ok_or(AaveAbiError::InvalidSettlement)
}

fn fixed_bytes(params: &[ethabi::LogParam], name: &str) -> Result<[u8; 32], AaveAbiError> {
    let Token::FixedBytes(bytes) = parameter(params, name)? else {
        return Err(AaveAbiError::InvalidSettlement);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| AaveAbiError::InvalidSettlement)
}

fn address(params: &[ethabi::LogParam], name: &str) -> Result<CanonicalAddress, AaveAbiError> {
    let Token::Address(value) = parameter(params, name)? else {
        return Err(AaveAbiError::InvalidSettlement);
    };
    CanonicalAddress::parse(&format!("0x{}", hex::encode(value)))
        .map_err(|_| AaveAbiError::InvalidSettlement)
}

fn uint(params: &[ethabi::LogParam], name: &str) -> Result<u128, AaveAbiError> {
    let Token::Uint(value) = parameter(params, name)? else {
        return Err(AaveAbiError::InvalidSettlement);
    };
    if *value > U256::from(u128::MAX) {
        return Err(AaveAbiError::InvalidSettlement);
    }
    Ok(value.low_u128())
}

fn token_fixed_bytes(token: &Token) -> Result<[u8; 32], AaveAbiError> {
    let Token::FixedBytes(bytes) = token else {
        return Err(AaveAbiError::InvalidRequest);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| AaveAbiError::InvalidRequest)
}

fn token_address(token: &Token) -> Result<CanonicalAddress, AaveAbiError> {
    let Token::Address(value) = token else {
        return Err(AaveAbiError::InvalidRequest);
    };
    CanonicalAddress::parse(&format!("0x{}", hex::encode(value)))
        .map_err(|_| AaveAbiError::InvalidRequest)
}

fn token_uint(token: &Token) -> Result<u128, AaveAbiError> {
    let Token::Uint(value) = token else {
        return Err(AaveAbiError::InvalidRequest);
    };
    if *value > U256::from(u128::MAX) {
        return Err(AaveAbiError::InvalidRequest);
    }
    Ok(value.low_u128())
}

fn decode_leg_identity(token: &Token) -> Result<AaveLiquidationLegIdentity, AaveAbiError> {
    let Token::Tuple(fields) = token else {
        return Err(AaveAbiError::InvalidRequest);
    };
    if fields.len() != 6 {
        return Err(AaveAbiError::InvalidRequest);
    }
    let fee = u32::try_from(token_uint(&fields[3])?).map_err(|_| AaveAbiError::InvalidRequest)?;
    let Token::Bool(zero_for_one) = &fields[4] else {
        return Err(AaveAbiError::InvalidRequest);
    };
    Ok(AaveLiquidationLegIdentity {
        pool: token_address(&fields[0])?,
        token_in: token_address(&fields[1])?,
        token_out: token_address(&fields[2])?,
        fee,
        zero_for_one: *zero_for_one,
        minimum_amount_out: token_uint(&fields[5])?,
    })
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AaveAbiError {
    #[error("PhoenixExecutor Aave ABI is invalid")]
    Contract,
    #[error("Aave liquidation request is invalid")]
    InvalidRequest,
    #[error("Aave liquidation calldata exceeds the bounded limit")]
    OversizedCalldata,
    #[error("Aave liquidation settlement evidence is invalid")]
    InvalidSettlement,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn address(value: &str) -> CanonicalAddress {
        CanonicalAddress::parse(value).expect("address")
    }

    fn request() -> AaveLiquidationRequest {
        AaveLiquidationRequest {
            route_id: [7; 32],
            borrower: address("0x1111111111111111111111111111111111111111"),
            debt_asset: address("0x2222222222222222222222222222222222222222"),
            collateral_asset: address("0x3333333333333333333333333333333333333333"),
            repay_amount: 100,
            maximum_input_amount: 100,
            minimum_collateral_received: 101,
            minimum_unwind_output: 102,
            minimum_profit: 2,
            maximum_atlas_bid: 1,
            deadline: Utc::now() + Duration::minutes(1),
            unwind_legs: vec![ValidatedLeg {
                pool: address("0x4444444444444444444444444444444444444444"),
                factory: Some(address("0x5555555555555555555555555555555555555555")),
                token_in: address("0x3333333333333333333333333333333333333333"),
                token_out: address("0x2222222222222222222222222222222222222222"),
                fee: 500,
                zero_for_one: false,
                min_amount_out: 102,
            }],
        }
    }

    #[test]
    fn exact_liquidation_identity_encodes_direct_and_atlas_payloads() {
        let request = request();
        let direct = request.encode_direct_call().expect("direct calldata");
        let atlas = request.encoded_request().expect("Atlas solver data");
        assert_eq!(&direct[..4], &hex::decode("f95a8cb8").expect("selector"));
        assert_eq!(&direct[4..], atlas.as_slice());
        let identity = AaveLiquidationRequest::decode_encoded_identity(&atlas).expect("identity");
        assert_eq!(identity.route_id, request.route_id);
        assert_eq!(identity.borrower, request.borrower);
        assert_eq!(identity.debt_asset, request.debt_asset);
        assert_eq!(identity.repay_amount, request.repay_amount);
        assert_eq!(identity.maximum_input_amount, request.maximum_input_amount);
        assert_eq!(
            identity.minimum_collateral_received,
            request.minimum_collateral_received
        );
        assert_eq!(
            identity.minimum_unwind_output,
            request.minimum_unwind_output
        );
        assert_eq!(identity.minimum_profit, request.minimum_profit);
        assert_eq!(identity.maximum_atlas_bid, request.maximum_atlas_bid);
        assert_eq!(identity.unwind_legs.len(), 1);
        assert_eq!(
            identity.unwind_legs[0].minimum_amount_out,
            request.unwind_legs[0].min_amount_out
        );
    }

    #[test]
    fn disconnected_or_expired_routes_fail_closed() {
        let mut invalid = request();
        invalid.unwind_legs[0].token_out = invalid.collateral_asset;
        assert_eq!(invalid.validate(), Err(AaveAbiError::InvalidRequest));
        invalid = request();
        invalid.deadline = Utc::now() - Duration::seconds(1);
        assert_eq!(invalid.validate(), Err(AaveAbiError::InvalidRequest));
    }

    #[test]
    fn encoded_zero_borrower_fails_closed() {
        let mut token = request_token(&request()).expect("request token");
        let Token::Tuple(fields) = &mut token else {
            panic!("request tuple");
        };
        fields[1] = Token::Address(ethabi::Address::zero());
        let encoded = ethabi::encode(&[token]);
        assert_eq!(
            AaveLiquidationRequest::decode_encoded_identity(&encoded),
            Err(AaveAbiError::InvalidRequest)
        );
    }
}
