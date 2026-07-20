pub mod abi;
pub mod approval;
pub mod config;
pub mod engine;
pub mod model;
pub mod rpc;
pub mod signer;
pub mod store;

pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42_161;
pub const ARBITRUM_WETH_ADDRESS: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
pub const ARBITRUM_NATIVE_USDC_ADDRESS: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
pub const CURRENT_ROUTE_FINGERPRINT: &str = "arbitrum-weth-usdc-uniswap-v3-500-3000-v1";
pub const CURRENT_ROUTE_POOL_500_ADDRESS: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
pub const CURRENT_ROUTE_POOL_3000_ADDRESS: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
pub const REQUEST_SCHEMA_VERSION: &str = "phoenix.live-execution-request.v2";
pub const APPROVAL_POLICY_VERSION: &str = "phoenix.live-canary-approval.v1";
