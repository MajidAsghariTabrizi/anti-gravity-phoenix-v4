use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinnedBlock {
    pub number: u64,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockReference {
    Latest,
    Number(PinnedBlock),
    Safe(PinnedBlock),
    Finalized(PinnedBlock),
}

impl BlockReference {
    pub fn pinned(&self) -> Option<&PinnedBlock> {
        match self {
            Self::Latest => None,
            Self::Number(block) | Self::Safe(block) | Self::Finalized(block) => Some(block),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RpcMethod {
    EthCall,
    EthGetBalance,
    EthGetBlockByNumber,
    EthGetCode,
    EthGetLogs,
    EthGetStorageAt,
}

impl RpcMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EthCall => "eth_call",
            Self::EthGetBalance => "eth_getBalance",
            Self::EthGetBlockByNumber => "eth_getBlockByNumber",
            Self::EthGetCode => "eth_getCode",
            Self::EthGetLogs => "eth_getLogs",
            Self::EthGetStorageAt => "eth_getStorageAt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EconomicCacheKey {
    pub method: RpcMethod,
    pub normalized_params_hash: String,
    pub block: PinnedBlock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EconomicRpcRequest {
    pub method: RpcMethod,
    pub normalized_params_hash: String,
    pub block: BlockReference,
    pub requires_archive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub provider_id: String,
    pub archive_reads: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderResult {
    pub provider_id: String,
    pub block: PinnedBlock,
    pub normalized_response_hash: String,
    pub latency_ns: u128,
    pub retry_count: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EconomicRpcQuality {
    pub provider_id: String,
    pub method: RpcMethod,
    pub block: PinnedBlock,
    pub latency_ns: u128,
    pub success: bool,
    pub stale_result: bool,
    pub disagreement: bool,
    pub timeout: bool,
    pub retry_count: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EconomicRpcError {
    UnpinnedBlock,
    ArchiveUnavailable,
    BlockMismatch,
    ProviderDisagreement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodTimeouts {
    pub eth_call: Duration,
    pub state_read: Duration,
    pub logs: Duration,
}

impl MethodTimeouts {
    pub fn timeout_for(self, method: RpcMethod) -> Duration {
        match method {
            RpcMethod::EthCall => self.eth_call,
            RpcMethod::EthGetLogs => self.logs,
            RpcMethod::EthGetBalance
            | RpcMethod::EthGetBlockByNumber
            | RpcMethod::EthGetCode
            | RpcMethod::EthGetStorageAt => self.state_read,
        }
    }
}

impl EconomicRpcRequest {
    pub fn cache_key(
        &self,
        provider: &ProviderCapabilities,
    ) -> Result<EconomicCacheKey, EconomicRpcError> {
        let block = self.block.pinned().ok_or(EconomicRpcError::UnpinnedBlock)?;
        if self.requires_archive && !provider.archive_reads {
            return Err(EconomicRpcError::ArchiveUnavailable);
        }
        Ok(EconomicCacheKey {
            method: self.method,
            normalized_params_hash: self.normalized_params_hash.clone(),
            block: block.clone(),
        })
    }
}

pub fn compare_provider_results(
    expected_block: &PinnedBlock,
    first: &ProviderResult,
    second: &ProviderResult,
) -> Result<(), EconomicRpcError> {
    if &first.block != expected_block
        || &second.block != expected_block
        || first.block != second.block
    {
        return Err(EconomicRpcError::BlockMismatch);
    }
    if first.normalized_response_hash != second.normalized_response_hash {
        return Err(EconomicRpcError::ProviderDisagreement);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(number: u64, hash: &str) -> PinnedBlock {
        PinnedBlock {
            number,
            hash: hash.to_string(),
        }
    }

    #[test]
    fn economic_reads_reject_latest_and_archive_mismatch() {
        let provider = ProviderCapabilities {
            provider_id: "primary".to_string(),
            archive_reads: false,
        };
        let request = EconomicRpcRequest {
            method: RpcMethod::EthCall,
            normalized_params_hash: "params".to_string(),
            block: BlockReference::Latest,
            requires_archive: false,
        };
        assert_eq!(
            request.cache_key(&provider),
            Err(EconomicRpcError::UnpinnedBlock)
        );
        let archive = EconomicRpcRequest {
            block: BlockReference::Number(block(100, "hash-100")),
            requires_archive: true,
            ..request
        };
        assert_eq!(
            archive.cache_key(&provider),
            Err(EconomicRpcError::ArchiveUnavailable)
        );
    }

    #[test]
    fn cache_identity_includes_number_and_hash() {
        let provider = ProviderCapabilities {
            provider_id: "primary".to_string(),
            archive_reads: true,
        };
        let request = |hash: &str| EconomicRpcRequest {
            method: RpcMethod::EthCall,
            normalized_params_hash: "params".to_string(),
            block: BlockReference::Number(block(100, hash)),
            requires_archive: false,
        };
        assert_ne!(
            request("hash-a").cache_key(&provider).unwrap(),
            request("hash-b").cache_key(&provider).unwrap()
        );
    }

    #[test]
    fn fallback_from_a_different_block_is_never_equivalent() {
        let expected = block(100, "hash-100");
        let first = ProviderResult {
            provider_id: "primary".to_string(),
            block: expected.clone(),
            normalized_response_hash: "state".to_string(),
            latency_ns: 1,
            retry_count: 0,
        };
        let second = ProviderResult {
            provider_id: "fallback".to_string(),
            block: block(101, "hash-101"),
            ..first.clone()
        };
        assert_eq!(
            compare_provider_results(&expected, &first, &second),
            Err(EconomicRpcError::BlockMismatch)
        );
    }

    #[test]
    fn same_block_material_disagreement_fails_closed() {
        let expected = block(100, "hash-100");
        let first = ProviderResult {
            provider_id: "primary".to_string(),
            block: expected.clone(),
            normalized_response_hash: "state-a".to_string(),
            latency_ns: 1,
            retry_count: 0,
        };
        let second = ProviderResult {
            provider_id: "fallback".to_string(),
            normalized_response_hash: "state-b".to_string(),
            ..first.clone()
        };
        assert_eq!(
            compare_provider_results(&expected, &first, &second),
            Err(EconomicRpcError::ProviderDisagreement)
        );
    }
}
