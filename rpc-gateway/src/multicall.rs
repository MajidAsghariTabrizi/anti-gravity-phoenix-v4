use ethabi::{decode, encode, Address, ParamType, Token};
use thiserror::Error;

pub const MULTICALL3_ADDRESS: &str = "0xcA11bde05977b3631167028862bE2a173976CA11";
pub const AGGREGATE3_SELECTOR: [u8; 4] = [0x82, 0xad, 0x56, 0xcb];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EthCall {
    pub target: String,
    pub calldata: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MulticallError {
    #[error("Multicall3 input was malformed")]
    InvalidInput,
    #[error("Multicall3 output was malformed")]
    InvalidOutput,
    #[error("Multicall3 inner call failed")]
    InnerCallFailed,
}

pub fn encode_aggregate3(calls: &[EthCall]) -> Result<String, MulticallError> {
    if calls.is_empty() {
        return Err(MulticallError::InvalidInput);
    }
    let calls = calls
        .iter()
        .map(|call| {
            let address = decode_fixed_hex(&call.target, 20)?;
            let calldata = decode_data(&call.calldata)?;
            Ok(Token::Tuple(vec![
                Token::Address(Address::from_slice(&address)),
                Token::Bool(false),
                Token::Bytes(calldata),
            ]))
        })
        .collect::<Result<Vec<_>, MulticallError>>()?;
    let mut payload = AGGREGATE3_SELECTOR.to_vec();
    payload.extend(encode(&[Token::Array(calls)]));
    Ok(format!("0x{}", hex::encode(payload)))
}

pub fn decode_aggregate3(value: &str, expected: usize) -> Result<Vec<Vec<u8>>, MulticallError> {
    let data = decode_data(value)?;
    let decoded = decode(
        &[ParamType::Array(Box::new(ParamType::Tuple(vec![
            ParamType::Bool,
            ParamType::Bytes,
        ])))],
        &data,
    )
    .map_err(|_| MulticallError::InvalidOutput)?;
    let Some(Token::Array(results)) = decoded.into_iter().next() else {
        return Err(MulticallError::InvalidOutput);
    };
    if results.len() != expected {
        return Err(MulticallError::InvalidOutput);
    }
    results
        .into_iter()
        .map(|result| {
            let Token::Tuple(mut values) = result else {
                return Err(MulticallError::InvalidOutput);
            };
            if values.len() != 2 {
                return Err(MulticallError::InvalidOutput);
            }
            let data = values.pop().ok_or(MulticallError::InvalidOutput)?;
            let success = values.pop().ok_or(MulticallError::InvalidOutput)?;
            match (success, data) {
                (Token::Bool(true), Token::Bytes(data)) => Ok(data),
                (Token::Bool(false), Token::Bytes(_)) => Err(MulticallError::InnerCallFailed),
                _ => Err(MulticallError::InvalidOutput),
            }
        })
        .collect()
}

fn decode_fixed_hex(value: &str, expected_bytes: usize) -> Result<Vec<u8>, MulticallError> {
    let decoded = decode_data(value)?;
    if decoded.len() != expected_bytes {
        return Err(MulticallError::InvalidInput);
    }
    Ok(decoded)
}

fn decode_data(value: &str) -> Result<Vec<u8>, MulticallError> {
    let body = value
        .strip_prefix("0x")
        .ok_or(MulticallError::InvalidInput)?;
    if body.len() % 2 != 0 {
        return Err(MulticallError::InvalidInput);
    }
    hex::decode(body).map_err(|_| MulticallError::InvalidInput)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate3_encoder_uses_canonical_selector_and_typed_calls() {
        let encoded = encode_aggregate3(&[EthCall {
            target: "0x1111111111111111111111111111111111111111".to_string(),
            calldata: "0x3850c7bd".to_string(),
        }])
        .unwrap();
        assert!(encoded.starts_with("0x82ad56cb"));
        assert!(encoded.len() > 10);
    }

    #[test]
    fn aggregate3_decoder_requires_every_inner_call_to_succeed() {
        let encoded = encode(&[Token::Array(vec![Token::Tuple(vec![
            Token::Bool(true),
            Token::Bytes(vec![1, 2, 3]),
        ])])]);
        assert_eq!(
            decode_aggregate3(&format!("0x{}", hex::encode(encoded)), 1),
            Ok(vec![vec![1, 2, 3]])
        );

        let failed = encode(&[Token::Array(vec![Token::Tuple(vec![
            Token::Bool(false),
            Token::Bytes(Vec::new()),
        ])])]);
        assert_eq!(
            decode_aggregate3(&format!("0x{}", hex::encode(failed)), 1),
            Err(MulticallError::InnerCallFailed)
        );
    }
}
