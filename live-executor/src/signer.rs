use crate::model::{CanonicalAddress, TransactionHash};
use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_eips::eip2930::AccessList;
use alloy_primitives::{Address, Bytes, TxKind, U256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use zeroize::Zeroizing;

pub struct TransactionSigner {
    signer: PrivateKeySigner,
    address: CanonicalAddress,
    chain_id: u64,
}

impl TransactionSigner {
    pub fn from_secret(secret: &str, chain_id: u64) -> Result<Self, SignerError> {
        let normalized = secret.strip_prefix("0x").unwrap_or(secret);
        if normalized.len() != 64 {
            return Err(SignerError::InvalidPrivateKey);
        }
        let signer =
            PrivateKeySigner::from_str(normalized).map_err(|_| SignerError::InvalidPrivateKey)?;
        let address = CanonicalAddress::parse(&signer.address().to_string().to_lowercase())
            .map_err(|_| SignerError::InvalidPrivateKey)?;
        Ok(Self {
            signer,
            address,
            chain_id,
        })
    }

    pub const fn address(&self) -> CanonicalAddress {
        self.address
    }

    pub fn sign(&self, draft: TransactionDraft) -> Result<SignedTransaction, SignerError> {
        if draft.chain_id != self.chain_id {
            return Err(SignerError::WrongChain);
        }
        let transaction = TxEip1559 {
            chain_id: draft.chain_id,
            nonce: draft.nonce,
            gas_limit: draft.gas_limit,
            max_fee_per_gas: draft.max_fee_per_gas,
            max_priority_fee_per_gas: draft.max_priority_fee_per_gas,
            to: TxKind::Call(Address::from_slice(draft.to.as_bytes())),
            value: U256::ZERO,
            access_list: AccessList::default(),
            input: Bytes::from(draft.calldata),
        };
        let signature = self
            .signer
            .sign_hash_sync(&transaction.signature_hash())
            .map_err(|_| SignerError::Signing)?;
        let signed = transaction.into_signed(signature);
        let tx_hash = TransactionHash::from_bytes(*signed.hash().as_ref());
        let mut raw = Vec::with_capacity(signed.eip2718_encoded_length());
        signed.eip2718_encode(&mut raw);
        let protected = Zeroizing::new(raw);
        Ok(SignedTransaction {
            tx_hash,
            raw: protected,
        })
    }
}

impl fmt::Debug for TransactionSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransactionSigner")
            .field("address", &self.address)
            .field("chain_id", &self.chain_id)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionDraft {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub to: CanonicalAddress,
    pub calldata: Vec<u8>,
}

pub struct SignedTransaction {
    tx_hash: TransactionHash,
    raw: Zeroizing<Vec<u8>>,
}

impl SignedTransaction {
    pub const fn tx_hash(&self) -> TransactionHash {
        self.tx_hash
    }

    pub fn raw_bytes(&self) -> &[u8] {
        self.raw.as_slice()
    }
}

impl fmt::Debug for SignedTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignedTransaction")
            .field("tx_hash", &self.tx_hash)
            .field("raw", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SignerError {
    #[error("private key is invalid")]
    InvalidPrivateKey,
    #[error("transaction chain does not match signer")]
    WrongChain,
    #[error("transaction signing failed")]
    Signing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signer_and_signed_payload_debug_are_redacted() {
        let key_material = hex::encode([9_u8; 32]);
        let signer = TransactionSigner::from_secret(&key_material, 42_161).expect("test signer");
        let signed = signer
            .sign(TransactionDraft {
                chain_id: 42_161,
                nonce: 1,
                gas_limit: 100_000,
                max_fee_per_gas: 10,
                max_priority_fee_per_gas: 1,
                to: CanonicalAddress::parse("0x1111111111111111111111111111111111111111")
                    .expect("address"),
                calldata: vec![1, 2, 3, 4],
            })
            .expect("sign");
        let signer_debug = format!("{signer:?}");
        let signed_debug = format!("{signed:?}");
        assert!(!signer_debug.contains(&key_material));
        assert!(!signed_debug.contains(&hex::encode(signed.raw_bytes())));
        assert!(signed_debug.contains("<redacted>"));
    }
}
