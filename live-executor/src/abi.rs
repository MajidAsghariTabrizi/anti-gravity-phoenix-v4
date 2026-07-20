use crate::model::{CanonicalAddress, ExecutionRequest, Settlement};
use ethabi::{Contract, RawLog, Token};
use primitive_types::{H256, U256};
use std::io::Cursor;
use thiserror::Error;

const MAX_CALLDATA_BYTES: usize = 128 * 1024;
const MAX_LOG_DATA_BYTES: usize = 4 * 1024;

pub fn encode_execute_opportunity(
    request: &ExecutionRequest,
    executor_address: CanonicalAddress,
) -> Result<Vec<u8>, AbiError> {
    let legs = request
        .legs
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
        .collect::<Vec<_>>();
    let opportunity = Token::Tuple(vec![
        Token::FixedBytes(request.route_id.to_vec()),
        Token::Address(primitive_address(request.origin_router)),
        Token::Address(primitive_address(executor_address)),
        Token::Address(primitive_address(request.flash_asset)),
        Token::Uint(U256::from(request.flash_amount)),
        Token::Uint(U256::from(request.maximum_input_amount)),
        Token::Uint(U256::from(request.minimum_profit)),
        Token::Uint(U256::from(
            u64::try_from(request.deadline.timestamp()).map_err(|_| AbiError::InvalidRequest)?,
        )),
        Token::Array(legs),
    ]);
    let encoded = executor_contract()
        .and_then(|contract| contract.function("executeOpportunity").cloned())
        .and_then(|function| function.encode_input(&[opportunity]))
        .map_err(|_| AbiError::Contract)?;
    if encoded.len() > MAX_CALLDATA_BYTES {
        return Err(AbiError::OversizedCalldata);
    }
    Ok(encoded)
}

pub fn decode_settlement(
    request: &ExecutionRequest,
    executor_address: CanonicalAddress,
    logs: &[RpcLog],
) -> Result<Settlement, AbiError> {
    let event = executor_contract()
        .and_then(|contract| contract.event("OpportunitySettled").cloned())
        .map_err(|_| AbiError::Contract)?;
    let mut matches = Vec::new();
    for log in logs {
        if log.address != executor_address {
            continue;
        }
        if log.data.len() > MAX_LOG_DATA_BYTES {
            return Err(AbiError::InvalidSettlement);
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
            .map_err(|_| AbiError::InvalidSettlement)?;
        let route_id = fixed_bytes(&parsed.params, "routeId")?;
        let asset = address(&parsed.params, "asset")?;
        let flash_amount = uint(&parsed.params, "flashAmount")?;
        let premium = uint(&parsed.params, "premium")?;
        let realized_profit = uint(&parsed.params, "realizedProfit")?;
        if route_id != request.route_id
            || asset != request.flash_asset
            || flash_amount != request.flash_amount
        {
            return Err(AbiError::InvalidSettlement);
        }
        matches.push(Settlement {
            asset,
            flash_amount,
            premium,
            realized_profit,
        });
    }
    if matches.len() != 1 {
        return Err(AbiError::InvalidSettlement);
    }
    matches.pop().ok_or(AbiError::InvalidSettlement)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcLog {
    pub address: CanonicalAddress,
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
}

fn executor_contract() -> Result<Contract, ethabi::Error> {
    Contract::load(Cursor::new(include_bytes!(
        "../../fork-sandbox/abi/PhoenixExecutor.json"
    )))
}

fn primitive_address(value: CanonicalAddress) -> ethabi::Address {
    ethabi::Address::from_slice(value.as_bytes())
}

fn fixed_bytes(params: &[ethabi::LogParam], name: &str) -> Result<[u8; 32], AbiError> {
    let value = parameter(params, name)?;
    let Token::FixedBytes(bytes) = value else {
        return Err(AbiError::InvalidSettlement);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| AbiError::InvalidSettlement)
}

fn address(params: &[ethabi::LogParam], name: &str) -> Result<CanonicalAddress, AbiError> {
    let value = parameter(params, name)?;
    let Token::Address(address) = value else {
        return Err(AbiError::InvalidSettlement);
    };
    CanonicalAddress::parse(&format!("0x{}", hex::encode(address)))
        .map_err(|_| AbiError::InvalidSettlement)
}

fn uint(params: &[ethabi::LogParam], name: &str) -> Result<u128, AbiError> {
    let value = parameter(params, name)?;
    let Token::Uint(number) = value else {
        return Err(AbiError::InvalidSettlement);
    };
    if *number > U256::from(u128::MAX) {
        return Err(AbiError::InvalidSettlement);
    }
    Ok(number.low_u128())
}

fn parameter<'a>(params: &'a [ethabi::LogParam], name: &str) -> Result<&'a Token, AbiError> {
    params
        .iter()
        .find(|param| param.name == name)
        .map(|param| &param.value)
        .ok_or(AbiError::InvalidSettlement)
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AbiError {
    #[error("PhoenixExecutor ABI is invalid")]
    Contract,
    #[error("execution request cannot be encoded")]
    InvalidRequest,
    #[error("execution calldata exceeds the bounded limit")]
    OversizedCalldata,
    #[error("PhoenixExecutor settlement evidence is invalid")]
    InvalidSettlement,
}
