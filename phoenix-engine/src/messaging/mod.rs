use crate::domain::{Address, ChainId, SequenceNumber, TxHash};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedTx {
    pub sequence: SequenceNumber,
    pub tx_hash: TxHash,
    pub tx_type: String,
    pub chain_id: ChainId,
    pub from: Address,
    pub to: Option<Address>,
    pub nonce: u64,
    pub value: String,
    pub calldata: String,
    pub gas_limit: String,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
}
