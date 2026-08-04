use crate::model::CanonicalAddress;
use crate::signer::{SignerError, TransactionSigner};
use crate::{
    ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS, ARBITRUM_ATLAS_V1_6_4_ADDRESS,
    ARBITRUM_ATLAS_VERIFICATION_V1_6_4_ADDRESS, ARBITRUM_ONE_CHAIN_ID,
};
use alloy_primitives::keccak256;
use ethabi::Token;
use futures_util::{SinkExt, StreamExt};
use primitive_types::U256;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

const DOMAIN_NAME: &str = "AtlasVerification";
const DOMAIN_VERSION: &str = "1.6.4";
const MAX_SOLVER_DATA_BYTES: usize = 128 * 1024;
const MAX_AUCTION_ID_BYTES: usize = 128;
const MAX_GATEWAY_RESPONSE_BYTES: usize = 1024 * 1024;
pub const OFFICIAL_ATLAS_SEARCHER_GATEWAY: &str = "wss://svr-bid-endpoint.chain.link/ws/solver";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasSolverOperation {
    pub from: CanonicalAddress,
    pub to: CanonicalAddress,
    pub value: U256,
    pub gas: u64,
    pub max_fee_per_gas: u128,
    pub deadline: u64,
    pub solver: CanonicalAddress,
    pub control: CanonicalAddress,
    pub user_op_hash: [u8; 32],
    pub bid_token: Option<CanonicalAddress>,
    pub bid_amount: u128,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SignedAtlasSolverOperation {
    pub from: String,
    pub to: String,
    pub value: String,
    pub gas: String,
    pub max_fee_per_gas: String,
    pub deadline: String,
    pub solver: String,
    pub control: String,
    pub user_op_hash: String,
    pub bid_token: String,
    pub bid_amount: String,
    pub data: String,
    #[serde(serialize_with = "serialize_signature")]
    signature: Zeroizing<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AtlasSolution {
    pub auction_id: String,
    pub auction_solution: SignedAtlasSolverOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtlasSubmitReceipt {
    pub auction_id: String,
    pub response_hash: String,
}

#[derive(Serialize)]
struct SubmitRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: &'a AtlasSolution,
}

#[derive(Deserialize)]
struct SubmitResponse {
    jsonrpc: String,
    id: u64,
    result: Option<serde_json::Value>,
    error: Option<SubmitError>,
}

#[derive(Deserialize)]
struct SubmitError {
    code: i64,
    message: String,
}

#[derive(Clone, Debug, Default)]
pub struct AtlasGateway;

impl AtlasGateway {
    pub async fn submit(&self, solution: &AtlasSolution) -> Result<AtlasSubmitReceipt, AtlasError> {
        let request = serde_json::to_string(&SubmitRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "solver_submitSolverOperation",
            params: solution,
        })
        .map_err(|_| AtlasError::InvalidOperation)?;
        if request.len() > MAX_SOLVER_DATA_BYTES {
            return Err(AtlasError::InvalidOperation);
        }
        let (mut socket, _) = tokio_tungstenite::connect_async(OFFICIAL_ATLAS_SEARCHER_GATEWAY)
            .await
            .map_err(|_| AtlasError::GatewayUnavailable)?;
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(request))
            .await
            .map_err(|_| AtlasError::GatewayUnavailable)?;
        let message = tokio::time::timeout(std::time::Duration::from_secs(15), socket.next())
            .await
            .map_err(|_| AtlasError::GatewayUnavailable)?
            .ok_or(AtlasError::GatewayUnavailable)?
            .map_err(|_| AtlasError::GatewayUnavailable)?;
        let bytes = message.into_data();
        if bytes.is_empty() || bytes.len() > MAX_GATEWAY_RESPONSE_BYTES {
            return Err(AtlasError::GatewayIntegrity);
        }
        validate_submit_response(&bytes)?;
        Ok(AtlasSubmitReceipt {
            auction_id: solution.auction_id.clone(),
            response_hash: hex::encode(Sha256::digest(&bytes)),
        })
    }
}

fn validate_submit_response(bytes: &[u8]) -> Result<(), AtlasError> {
    let raw: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| AtlasError::GatewayIntegrity)?;
    let result_present = raw
        .as_object()
        .is_some_and(|object| object.contains_key("result"));
    let response: SubmitResponse =
        serde_json::from_value(raw).map_err(|_| AtlasError::GatewayIntegrity)?;
    let accepted_result = response.result.is_none()
        || response.result.as_ref().and_then(serde_json::Value::as_str) == Some("null");
    if response.jsonrpc != "2.0"
        || response.id != 1
        || !result_present
        || !accepted_result
        || response.error.is_some()
    {
        let _sanitized_error_class = response
            .error
            .map(|error| (error.code, !error.message.is_empty()));
        return Err(AtlasError::SubmissionRejected);
    }
    Ok(())
}

impl AtlasSolverOperation {
    pub fn validate(
        &self,
        signer: CanonicalAddress,
        solver_contract: CanonicalAddress,
        solver_gas_limit: u64,
        oracle_gas_price: u128,
        auction_deadline: u64,
        maximum_bid: u128,
    ) -> Result<(), AtlasError> {
        let atlas = CanonicalAddress::parse(ARBITRUM_ATLAS_V1_6_4_ADDRESS)
            .map_err(|_| AtlasError::InvalidOperation)?;
        let control = CanonicalAddress::parse(ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS)
            .map_err(|_| AtlasError::InvalidOperation)?;
        if self.from != signer
            || self.to != atlas
            || self.control != control
            || self.solver != solver_contract
            || self.value != U256::zero()
            || self.gas == 0
            || self.gas > solver_gas_limit
            || self.max_fee_per_gas != oracle_gas_price
            || self.deadline != auction_deadline
            || self.bid_amount == 0
            || self.bid_amount > maximum_bid
            || self.data.is_empty()
            || self.data.len() > MAX_SOLVER_DATA_BYTES
            || self.user_op_hash == [0; 32]
        {
            return Err(AtlasError::InvalidOperation);
        }
        Ok(())
    }

    pub fn eip712_digest(&self) -> Result<[u8; 32], AtlasError> {
        let verification = CanonicalAddress::parse(ARBITRUM_ATLAS_VERIFICATION_V1_6_4_ADDRESS)
            .map_err(|_| AtlasError::InvalidOperation)?;
        let domain_type_hash = keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        );
        let domain = ethabi::encode(&[
            Token::FixedBytes(domain_type_hash.to_vec()),
            Token::FixedBytes(keccak256(DOMAIN_NAME.as_bytes()).to_vec()),
            Token::FixedBytes(keccak256(DOMAIN_VERSION.as_bytes()).to_vec()),
            Token::Uint(U256::from(ARBITRUM_ONE_CHAIN_ID)),
            Token::Address(ethabi::Address::from_slice(verification.as_bytes())),
        ]);
        let domain_separator = keccak256(domain);
        let solver_type_hash = keccak256(
            b"SolverOperation(address from,address to,uint256 value,uint256 gas,uint256 maxFeePerGas,uint256 deadline,address solver,address control,bytes32 userOpHash,address bidToken,uint256 bidAmount,bytes data)",
        );
        let bid_token = self
            .bid_token
            .map(|address| ethabi::Address::from_slice(address.as_bytes()))
            .unwrap_or_default();
        let operation = ethabi::encode(&[
            Token::FixedBytes(solver_type_hash.to_vec()),
            Token::Address(ethabi::Address::from_slice(self.from.as_bytes())),
            Token::Address(ethabi::Address::from_slice(self.to.as_bytes())),
            Token::Uint(self.value),
            Token::Uint(U256::from(self.gas)),
            Token::Uint(U256::from(self.max_fee_per_gas)),
            Token::Uint(U256::from(self.deadline)),
            Token::Address(ethabi::Address::from_slice(self.solver.as_bytes())),
            Token::Address(ethabi::Address::from_slice(self.control.as_bytes())),
            Token::FixedBytes(self.user_op_hash.to_vec()),
            Token::Address(bid_token),
            Token::Uint(U256::from(self.bid_amount)),
            Token::FixedBytes(keccak256(&self.data).to_vec()),
        ]);
        let struct_hash = keccak256(operation);
        let digest = keccak256(
            [
                b"\x19\x01".as_slice(),
                domain_separator.as_slice(),
                struct_hash.as_slice(),
            ]
            .concat(),
        );
        Ok(digest.into())
    }

    pub fn sign(
        &self,
        signer: &TransactionSigner,
    ) -> Result<SignedAtlasSolverOperation, AtlasError> {
        if self.from != signer.address() {
            return Err(AtlasError::SignerMismatch);
        }
        let signature = signer.sign_digest(self.eip712_digest()?)?;
        Ok(SignedAtlasSolverOperation {
            from: self.from.to_string(),
            to: self.to.to_string(),
            value: quantity(self.value),
            gas: quantity(U256::from(self.gas)),
            max_fee_per_gas: quantity(U256::from(self.max_fee_per_gas)),
            deadline: quantity(U256::from(self.deadline)),
            solver: self.solver.to_string(),
            control: self.control.to_string(),
            user_op_hash: format!("0x{}", hex::encode(self.user_op_hash)),
            bid_token: self
                .bid_token
                .map(|address| address.to_string())
                .unwrap_or_else(|| format!("0x{}", "0".repeat(40))),
            bid_amount: quantity(U256::from(self.bid_amount)),
            data: format!("0x{}", hex::encode(&self.data)),
            signature: Zeroizing::new(format!("0x{}", hex::encode(signature.as_slice()))),
        })
    }
}

impl AtlasSolution {
    pub fn new(
        auction_id: String,
        operation: &AtlasSolverOperation,
        signer: &TransactionSigner,
    ) -> Result<Self, AtlasError> {
        if auction_id.is_empty()
            || auction_id.len() > MAX_AUCTION_ID_BYTES
            || auction_id.chars().any(char::is_control)
        {
            return Err(AtlasError::InvalidAuction);
        }
        Ok(Self {
            auction_id,
            auction_solution: operation.sign(signer)?,
        })
    }
}

pub fn query_authorization_digest(
    auction_id: &str,
    user_op_hash: [u8; 32],
    solver_from: CanonicalAddress,
) -> Result<[u8; 32], AtlasError> {
    if auction_id.is_empty()
        || auction_id.len() > MAX_AUCTION_ID_BYTES
        || auction_id.chars().any(char::is_control)
    {
        return Err(AtlasError::InvalidAuction);
    }
    let message = format!(
        "{}:0x{}:{}",
        auction_id,
        hex::encode(user_op_hash),
        solver_from
    );
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    Ok(keccak256([prefix.as_bytes(), message.as_bytes()].concat()).into())
}

fn quantity(value: U256) -> String {
    if value.is_zero() {
        "0x0".to_string()
    } else {
        format!("0x{value:x}")
    }
}

fn serialize_signature<S>(value: &Zeroizing<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(value.as_str())
}

#[derive(Debug, Error)]
pub enum AtlasError {
    #[error("Atlas solver operation is invalid")]
    InvalidOperation,
    #[error("Atlas auction identity is invalid")]
    InvalidAuction,
    #[error("Atlas solver signer does not match the operation")]
    SignerMismatch,
    #[error(transparent)]
    Signer(#[from] SignerError),
    #[error("Atlas Searcher Gateway is unavailable")]
    GatewayUnavailable,
    #[error("Atlas Searcher Gateway response is invalid")]
    GatewayIntegrity,
    #[error("Atlas solver operation was rejected")]
    SubmissionRejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(value: &str) -> CanonicalAddress {
        CanonicalAddress::parse(value).expect("address")
    }

    fn operation(from: CanonicalAddress) -> AtlasSolverOperation {
        AtlasSolverOperation {
            from,
            to: address(ARBITRUM_ATLAS_V1_6_4_ADDRESS),
            value: U256::zero(),
            gas: 500_000,
            max_fee_per_gas: 5_000_000,
            deadline: 49_000_000,
            solver: address("0x1111111111111111111111111111111111111111"),
            control: address(ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS),
            user_op_hash: [7; 32],
            bid_token: Some(address("0xaf88d065e77c8cc2239327c5edb3a432268e5831")),
            bid_amount: 200_000,
            data: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn official_null_submission_acknowledgement_is_accepted() {
        validate_submit_response(br#"{"jsonrpc":"2.0","id":1,"result":null}"#)
            .expect("official acknowledgement");
        assert!(matches!(
            validate_submit_response(br#"{"jsonrpc":"2.0","id":1}"#),
            Err(AtlasError::SubmissionRejected)
        ));
        assert!(matches!(
            validate_submit_response(br#"{"jsonrpc":"2.0","id":1,"result":null,"error":{"code":-1,"message":"rejected"}}"#),
            Err(AtlasError::SubmissionRejected)
        ));
    }

    #[test]
    fn operation_is_strictly_bound_and_signed_under_v1_6_4_domain() {
        let signer =
            TransactionSigner::from_secret(&hex::encode([9_u8; 32]), 42_161).expect("signer");
        let operation = operation(signer.address());
        operation
            .validate(
                signer.address(),
                operation.solver,
                500_000,
                5_000_000,
                49_000_000,
                200_000,
            )
            .expect("valid operation");
        let signed = operation.sign(&signer).expect("signed operation");
        assert_eq!(signed.signature.len(), 132);
        assert!(signed.signature.ends_with("1b") || signed.signature.ends_with("1c"));
        assert_eq!(signed.max_fee_per_gas, "0x4c4b40");
        assert_eq!(signed.bid_amount, "0x30d40");
    }

    #[test]
    fn bid_and_oracle_gas_bounds_fail_closed() {
        let signer =
            TransactionSigner::from_secret(&hex::encode([9_u8; 32]), 42_161).expect("signer");
        let mut operation = operation(signer.address());
        operation.bid_amount = 200_001;
        assert!(operation
            .validate(
                signer.address(),
                operation.solver,
                500_000,
                5_000_000,
                49_000_000,
                200_000,
            )
            .is_err());
        operation.bid_amount = 200_000;
        operation.max_fee_per_gas += 1;
        assert!(operation
            .validate(
                signer.address(),
                operation.solver,
                500_000,
                5_000_000,
                49_000_000,
                200_000,
            )
            .is_err());
    }

    #[test]
    fn query_authorization_is_eip_191_bound_to_exact_auction() {
        let from = address("0x5003676390dfe662af408eb0bf13e182adcace0a");
        let first = query_authorization_digest("auction-a", [3; 32], from).expect("digest");
        let second = query_authorization_digest("auction-b", [3; 32], from).expect("digest");
        assert_ne!(first, second);
    }
}
