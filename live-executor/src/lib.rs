pub mod abi;
pub mod config;
pub mod engine;
pub mod model;
pub mod rpc;
pub mod signer;
pub mod store;

pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42_161;
pub const ARBITRUM_WETH_ADDRESS: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
pub const REQUEST_SCHEMA_VERSION: &str = "phoenix.live-execution-request.v1";
