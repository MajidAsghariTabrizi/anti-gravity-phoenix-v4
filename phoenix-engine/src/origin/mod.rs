use crate::domain::{Address, Amount, DomainError, PoolId, SequenceNumber, TokenAddress, TxHash};
use crate::messaging::NormalizedTx;

const EXACT_INPUT_SINGLE_SELECTOR: &str = "414bf389";
const EXACT_INPUT_SELECTOR: &str = "c04b8d59";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginClassification {
    SupportedSwapOrigin(OriginEvent),
    KnownRouterUnsupportedCommand,
    PossibleAggregator,
    Irrelevant,
    Malformed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginEvent {
    pub origin_tx_hash: TxHash,
    pub origin_sequence: SequenceNumber,
    pub router: Address,
    pub decoded_commands: Vec<String>,
    pub swap_path: Vec<TokenAddress>,
    pub exact_in: bool,
    pub amount: Amount,
    pub candidate_touched_pools: Vec<PoolId>,
}

#[derive(Clone, Debug)]
pub struct OriginDetector {
    routers: Vec<Address>,
}

impl OriginDetector {
    pub fn new(routers: Vec<Address>) -> Self {
        Self { routers }
    }

    pub fn classify(&self, tx: &NormalizedTx) -> OriginClassification {
        let Some(to) = &tx.to else {
            return OriginClassification::Irrelevant;
        };
        if !self.routers.iter().any(|r| r == to) {
            return if tx.calldata.len() > 10 {
                OriginClassification::PossibleAggregator
            } else {
                OriginClassification::Irrelevant
            };
        }
        let calldata = tx.calldata.trim_start_matches("0x").to_ascii_lowercase();
        if calldata.len() < 8 {
            return OriginClassification::Malformed;
        }
        let selector = &calldata[0..8];
        match selector {
            EXACT_INPUT_SINGLE_SELECTOR => match decode_exact_input_single(&calldata[8..]) {
                Ok(decoded) => {
                    let touched_pool = PoolId(format!(
                        "{}:{}:{}",
                        decoded.token_in.as_str(),
                        decoded.token_out.as_str(),
                        decoded.fee
                    ));

                    OriginClassification::SupportedSwapOrigin(OriginEvent {
                        origin_tx_hash: tx.tx_hash.clone(),
                        origin_sequence: tx.sequence,
                        router: to.clone(),
                        decoded_commands: vec!["exactInputSingle".to_string()],
                        swap_path: vec![
                            TokenAddress(decoded.token_in),
                            TokenAddress(decoded.token_out),
                        ],
                        exact_in: true,
                        amount: decoded.amount_in,
                        candidate_touched_pools: vec![touched_pool],
                    })
                }
                Err(_) => OriginClassification::Malformed,
            },
            EXACT_INPUT_SELECTOR => OriginClassification::KnownRouterUnsupportedCommand,
            _ => OriginClassification::KnownRouterUnsupportedCommand,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactInputSingle {
    token_in: Address,
    token_out: Address,
    fee: u32,
    amount_in: Amount,
}

fn decode_exact_input_single(data: &str) -> Result<ExactInputSingle, DomainError> {
    let slots = abi_slots(data)?;
    if slots.len() < 8 {
        return Err(DomainError::InvalidCalldata(
            "exactInputSingle too short".to_string(),
        ));
    }
    let token_in = Address::parse(&format!("0x{}", &slots[0][24..64]))?;
    let token_out = Address::parse(&format!("0x{}", &slots[1][24..64]))?;
    let fee = parse_u32_slot(&slots[2])?;
    let amount_in = Amount(parse_u128_slot(&slots[4])?);
    Ok(ExactInputSingle {
        token_in,
        token_out,
        fee,
        amount_in,
    })
}

fn abi_slots(data: &str) -> Result<Vec<String>, DomainError> {
    if data.len() % 64 != 0 || !data.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(DomainError::InvalidCalldata(
            "invalid abi slot data".to_string(),
        ));
    }
    Ok(data
        .as_bytes()
        .chunks(64)
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect())
}

fn parse_u32_slot(slot: &str) -> Result<u32, DomainError> {
    u32::from_str_radix(&slot[56..64], 16)
        .map_err(|_| DomainError::InvalidCalldata("invalid uint24/uint32 slot".to_string()))
}

fn parse_u128_slot(slot: &str) -> Result<u128, DomainError> {
    if &slot[0..32] != "00000000000000000000000000000000" {
        return Err(DomainError::InvalidCalldata(
            "uint256 exceeds local u128 fixture range".to_string(),
        ));
    }
    u128::from_str_radix(&slot[32..64], 16)
        .map_err(|_| DomainError::InvalidCalldata("invalid uint256 slot".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChainId, TxHash};
    use crate::messaging::NormalizedTx;

    fn slot_address(address: &str) -> String {
        format!(
            "000000000000000000000000{}",
            address.trim_start_matches("0x")
        )
    }

    fn slot_u(value: u128) -> String {
        format!("{value:064x}")
    }

    #[test]
    fn decodes_exact_input_single_origin() {
        let router = Address::parse("0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45").unwrap();
        let detector = OriginDetector::new(vec![router.clone()]);
        let token_in = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
        let token_out = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
        let calldata = format!(
            "0x{}{}{}{}{}{}{}{}{}",
            EXACT_INPUT_SINGLE_SELECTOR,
            slot_address(token_in),
            slot_address(token_out),
            slot_u(500),
            slot_address("0x1111111111111111111111111111111111111111"),
            slot_u(12345),
            slot_u(0),
            slot_u(0),
            slot_u(0)
        );
        let tx = NormalizedTx {
            sequence: SequenceNumber(7),
            tx_hash: TxHash(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            ),
            tx_type: "0x2".to_string(),
            chain_id: ChainId(42161),
            from: Address::parse("0x1111111111111111111111111111111111111111").unwrap(),
            to: Some(router),
            nonce: 1,
            value: "0".to_string(),
            calldata,
            gas_limit: "1".to_string(),
            max_fee_per_gas: "1".to_string(),
            max_priority_fee_per_gas: "1".to_string(),
        };
        match detector.classify(&tx) {
            OriginClassification::SupportedSwapOrigin(event) => {
                assert_eq!(event.amount, Amount(12345));
                assert_eq!(event.swap_path.len(), 2);
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }
}
