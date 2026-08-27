pub mod aave;
pub mod abi;
pub mod activation_request;
pub mod approval;
pub mod atlas;
pub mod autonomous;
pub mod config;
pub mod control_environment;
pub mod economic_control;
pub mod engine;
pub mod events;
pub mod executor_rotation;
pub mod model;
pub mod owner_bootstrap;
pub mod revenue;
pub mod rpc;
pub mod signer;
pub mod store;

pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42_161;
pub const ARBITRUM_WETH_ADDRESS: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1";
pub const ARBITRUM_NATIVE_USDC_ADDRESS: &str = "0xaf88d065e77c8cc2239327c5edb3a432268e5831";
pub const ARBITRUM_USDC_E_ADDRESS: &str = "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8";
pub const REVIEWED_WETH_USDC_E_POOL_500_ADDRESS: &str =
    "0xc31e54c7a869b9fcbecc14363cf510d1c41fa443";
pub const ARBITRUM_UNISWAP_V3_FACTORY_ADDRESS: &str = "0x1f98431c8ad98523631ae4a59f267346ea31f984";
pub const CURRENT_ROUTE_FINGERPRINT: &str = "arbitrum-weth-usdc-uniswap-v3-500-3000-v1";
pub const REVERSE_ROUTE_FINGERPRINT: &str = "arbitrum-weth-usdc-uniswap-v3-3000-500-v1";
pub const CURRENT_ROUTE_POOL_500_ADDRESS: &str = "0xc6962004f452be9203591991d15f6b388e09e8d0";
pub const CURRENT_ROUTE_POOL_3000_ADDRESS: &str = "0xc473e2aee3441bf9240be85eb122abb059a3b57c";
pub const REQUEST_SCHEMA_VERSION: &str = "phoenix.live-execution-request.v2";
pub const APPROVAL_POLICY_VERSION: &str = "phoenix.live-canary-approval.v1";
pub const ARBITRUM_ATLAS_V1_6_4_ADDRESS: &str = "0x8ad1ae9d97c79aa68a0a151e83ff3942f68f86c1";
pub const ARBITRUM_ATLAS_VERIFICATION_V1_6_4_ADDRESS: &str =
    "0xac116abb948e26b023c9c4815ab001845fbf54ff";
pub const ARBITRUM_ATLAS_DAPP_CONTROL_ADDRESS: &str = "0xe15bba987c002ecc3586e81244517877d294d291";
pub const ARBITRUM_AAVE_V3_POOL_ADDRESS: &str = "0x794a61358d6845594f94dc1db02a252b5b4814ad";
pub const AAVE_LIQUIDATION_ROUTE_FINGERPRINT: &str = "AAVE_LIQUIDATION_V1";
pub const ATLAS_AAVE_SOLVER_ROUTE_FINGERPRINT: &str = "ATLAS_AAVE_SOLVER_V1";

pub fn reviewed_route_policy(fingerprint: &str) -> Option<&'static str> {
    match fingerprint {
        CURRENT_ROUTE_FINGERPRINT => {
            Some(include_str!("../../config/phoenix-route-policy-v1.json"))
        }
        REVERSE_ROUTE_FINGERPRINT => Some(include_str!(
            "../../config/phoenix-route-policy-3000-500-v1.json"
        )),
        _ => None,
    }
}

pub fn reviewed_route_policies() -> [&'static str; 2] {
    [
        include_str!("../../config/phoenix-route-policy-v1.json"),
        include_str!("../../config/phoenix-route-policy-3000-500-v1.json"),
    ]
}
