use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use ethabi::{ethereum_types::H256, ParamType, Token};
use phoenix_fork_sandbox::model::{PersistedOpportunity, SimulationStatus};
use phoenix_fork_sandbox::rpc::{
    BlockObservation, ForkMetadata, ForkRpc, HttpForkRpc, PoolObservation, RpcError,
    SimulationCall, TraceLog, TraceObservation, ALLOWED_RPC_METHODS,
};
use phoenix_fork_sandbox::{ForkRunner, PlanPolicy, PlannerError, RunnerError, UnsignedPlanner};
use rpc_gateway::shadow_state::{
    canonical_hash_bytes, EvidenceRequest, PoolStateRequest, PoolStateResponse, ShadowStateRequest,
    SHADOW_STATE_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NOW_MS: u64 = 1_700_000_000_000;
const BLOCK_HASH: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_A: &str = "0x1111111111111111111111111111111111111111";
const TOKEN_B: &str = "0x2222222222222222222222222222222222222222";
const POOL_A: &str = "0x3333333333333333333333333333333333333333";
const POOL_B: &str = "0x4444444444444444444444444444444444444444";
const ROUTER: &str = "0x5555555555555555555555555555555555555555";
const TARGET: &str = "0x6666666666666666666666666666666666666666";
const SIMULATION_FROM: &str = "0x7777777777777777777777777777777777777777";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeMode {
    Passed,
    Reverted,
    StateDrift,
}

#[derive(Clone)]
struct FakeRpc {
    mode: FakeMode,
    plan_route_hash: String,
    observations: Vec<PoolObservation>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl ForkRpc for FakeRpc {
    async fn metadata(&self) -> Result<ForkMetadata, RpcError> {
        self.record("anvil_metadata");
        Ok(ForkMetadata {
            chain_id: 42161,
            fork_block_number: 100,
            fork_block_hash: BLOCK_HASH.to_string(),
            instance_hash: "e".repeat(64),
        })
    }

    async fn latest_block(&self) -> Result<BlockObservation, RpcError> {
        self.record("eth_getBlockByNumber");
        Ok(BlockObservation {
            number: 100,
            hash: BLOCK_HASH.to_string(),
        })
    }

    async fn code(&self, _address: &str) -> Result<String, RpcError> {
        self.record("eth_getCode");
        Ok("0x6000".to_string())
    }

    async fn observe_pool(&self, pool: &PoolStateRequest) -> Result<PoolObservation, RpcError> {
        self.record("eth_call");
        let index = match pool.address.as_str() {
            POOL_A => 0,
            POOL_B => 1,
            _ => return Err(RpcError::Integrity),
        };
        let mut observation = self.observations[index].clone();
        if self.mode == FakeMode::StateDrift && index == 1 {
            observation.liquidity = format!("0x{}", "ff".repeat(32));
        }
        Ok(observation)
    }

    async fn estimate_gas(&self, _call: &SimulationCall) -> Result<u64, RpcError> {
        self.record("eth_estimateGas");
        if self.mode == FakeMode::Reverted {
            let mut data =
                ethabi::short_signature("MinProfit", &[ParamType::Uint(256), ParamType::Uint(256)])
                    .to_vec();
            data.extend(ethabi::encode(&[
                Token::Uint(primitive_types::U256::from(10u64)),
                Token::Uint(primitive_types::U256::from(60u64)),
            ]));
            Err(RpcError::Reverted {
                reason: "execution reverted".to_string(),
                data: Some(format!("0x{}", hex::encode(data))),
            })
        } else {
            Ok(10)
        }
    }

    async fn call(&self, _call: &SimulationCall) -> Result<String, RpcError> {
        self.record("eth_call");
        Ok("0x".to_string())
    }

    async fn trace_call(&self, _call: &SimulationCall) -> Result<TraceObservation, RpcError> {
        self.record("debug_traceCall");
        Ok(TraceObservation {
            gas_used: 5,
            output: "0x".to_string(),
            logs: vec![settlement_log(&self.plan_route_hash, 80)],
            revert_reason: None,
            trace_hash: "f".repeat(64),
        })
    }
}

impl FakeRpc {
    fn record(&self, method: &'static str) {
        self.calls
            .lock()
            .expect("record fake RPC method")
            .push(method);
    }

    fn methods(&self) -> Vec<&'static str> {
        self.calls.lock().expect("read fake RPC methods").clone()
    }
}

#[test]
fn planner_builds_unsigned_bounded_calldata_and_preserves_safety() {
    let (fact, _, _) = fixture();
    let plan = UnsignedPlanner
        .build(&fact, &policy(), NOW_MS)
        .expect("build unsigned plan");
    assert_eq!(plan.chain_id, 42161);
    assert_eq!(plan.route_hash, fact.route_config_hash);
    assert!(plan.calldata.starts_with("0x"));
    assert_eq!(plan.minimum_leg_outputs, vec!["148", "198"]);
    assert_eq!(plan.minimum_output, "198");
    assert_eq!(plan.minimum_profit, "60");
    assert!(plan.unsigned);
    assert!(plan.fork_only);
    assert!(plan.shadow_only);
    assert!(!plan.live_execution);
    assert!(!plan.execution_eligible);
    assert!(!plan.execution_request_created);
    assert!(!plan.public_broadcast);
    assert!(!plan.signer_used);
}

#[test]
fn planner_rejects_every_required_invalid_evidence_class() {
    let (fact, _, _) = fixture();
    let base_policy = policy();

    let mut changed = fact.clone();
    changed.chain_id = 1;
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::WrongChain)
    );

    let mut changed_policy = base_policy.clone();
    changed_policy.allowed_tokens.remove(TOKEN_B);
    assert_eq!(
        UnsignedPlanner.build(&fact, &changed_policy, NOW_MS),
        Err(PlannerError::UnsupportedToken)
    );

    let mut changed_policy = base_policy.clone();
    changed_policy.allowed_pools.remove(POOL_B);
    assert_eq!(
        UnsignedPlanner.build(&fact, &changed_policy, NOW_MS),
        Err(PlannerError::UnsupportedPool)
    );

    let mut changed_policy = base_policy.clone();
    changed_policy.allowed_routers.remove(ROUTER);
    changed_policy
        .allowed_routers
        .insert("0x8888888888888888888888888888888888888888".to_string());
    assert_eq!(
        UnsignedPlanner.build(&fact, &changed_policy, NOW_MS),
        Err(PlannerError::UnsupportedRouter)
    );

    let mut changed = fact.clone();
    changed.opportunity_expires_at = Utc
        .timestamp_millis_opt(NOW_MS as i64)
        .single()
        .expect("stale timestamp");
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::StaleOpportunity)
    );

    let mut changed = fact.clone();
    changed.secondary_provider_id = None;
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::MissingVerification)
    );

    let mut changed = fact.clone();
    changed.independent_verification_status = "disagreed".to_string();
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::ProviderDisagreement)
    );

    let mut changed = fact.clone();
    changed.pool_state_hash_path.clear();
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::MissingStateHash)
    );

    let mut changed = fact.clone();
    changed.route_config_hash = "d".repeat(64);
    changed.secondary_route_config_hash = Some("d".repeat(64));
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::RouteHashMismatch)
    );

    let mut changed = fact.clone();
    changed.expected_net_pnl = "0".to_string();
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::NonPositiveExpectedNetPnl)
    );

    let mut changed = fact.clone();
    changed.expected_net_pnl = "40".to_string();
    assert_eq!(
        UnsignedPlanner.build(&changed, &base_policy, NOW_MS),
        Err(PlannerError::BelowThreshold)
    );

    let mut changed_policy = base_policy;
    changed_policy.maximum_calldata_bytes = 1;
    assert_eq!(
        UnsignedPlanner.build(&fact, &changed_policy, NOW_MS),
        Err(PlannerError::OversizedCalldata)
    );
}

#[tokio::test]
async fn profitable_fixture_records_gas_balance_delta_and_net_pnl() {
    let (fact, observations, _) = fixture();
    let plan = UnsignedPlanner
        .build(&fact, &policy(), NOW_MS)
        .expect("build profitable plan");
    let rpc = FakeRpc {
        mode: FakeMode::Passed,
        plan_route_hash: plan.route_hash.clone(),
        observations,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let result = ForkRunner
        .run(
            &plan,
            &rpc,
            Utc.timestamp_millis_opt(NOW_MS as i64)
                .single()
                .expect("simulation timestamp"),
        )
        .await
        .expect("run profitable fork fixture");
    assert_eq!(result.body.status, SimulationStatus::Passed);
    assert_eq!(result.body.gas_estimate, Some(10));
    assert_eq!(result.body.gas_used, Some(5));
    assert_eq!(result.body.simulated_gross_profit.as_deref(), Some("80"));
    assert_eq!(result.body.simulated_balance_delta.as_deref(), Some("80"));
    assert_eq!(result.body.simulated_gas_cost.as_deref(), Some("5"));
    assert_eq!(result.body.simulated_net_pnl.as_deref(), Some("75"));
    assert_eq!(result.body.prediction_error.as_deref(), Some("-15"));
    assert!(!result.body.public_broadcast);
    assert!(!result.body.signer_used);
    assert!(rpc
        .methods()
        .iter()
        .all(|method| ALLOWED_RPC_METHODS.contains(method)));
}

#[tokio::test]
async fn unprofitable_executor_revert_is_exported_without_success_claim() {
    let (fact, observations, _) = fixture();
    let plan = UnsignedPlanner
        .build(&fact, &policy(), NOW_MS)
        .expect("build reverted plan");
    let rpc = FakeRpc {
        mode: FakeMode::Reverted,
        plan_route_hash: plan.route_hash.clone(),
        observations,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let result = ForkRunner
        .run(
            &plan,
            &rpc,
            Utc.timestamp_millis_opt(NOW_MS as i64)
                .single()
                .expect("simulation timestamp"),
        )
        .await
        .expect("persistable reverted result");
    assert_eq!(result.body.status, SimulationStatus::Reverted);
    assert_eq!(result.body.revert_reason.as_deref(), Some("MinProfit"));
    assert_eq!(result.body.gas_used, None);
    assert_eq!(result.body.simulated_net_pnl, None);
}

#[tokio::test]
async fn state_drift_fails_before_estimate_or_executor_call() {
    let (fact, observations, _) = fixture();
    let plan = UnsignedPlanner
        .build(&fact, &policy(), NOW_MS)
        .expect("build drift plan");
    let rpc = FakeRpc {
        mode: FakeMode::StateDrift,
        plan_route_hash: plan.route_hash.clone(),
        observations,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    assert_eq!(
        ForkRunner
            .run(
                &plan,
                &rpc,
                Utc.timestamp_millis_opt(NOW_MS as i64)
                    .single()
                    .expect("simulation timestamp"),
            )
            .await,
        Err(RunnerError::StateDrift)
    );
    let methods = rpc.methods();
    assert!(!methods.contains(&"eth_estimateGas"));
    assert!(!methods.contains(&"debug_traceCall"));
}

#[tokio::test]
async fn target_bytecode_mismatch_fails_before_pool_or_executor_calls() {
    let (fact, observations, _) = fixture();
    let mut plan = UnsignedPlanner
        .build(&fact, &policy(), NOW_MS)
        .expect("build bytecode-bound plan");
    plan.target_code_hash = "f".repeat(64);
    let rpc = FakeRpc {
        mode: FakeMode::Passed,
        plan_route_hash: plan.route_hash.clone(),
        observations,
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    assert_eq!(
        ForkRunner
            .run(
                &plan,
                &rpc,
                Utc.timestamp_millis_opt(NOW_MS as i64)
                    .single()
                    .expect("simulation timestamp"),
            )
            .await,
        Err(RunnerError::ContractMismatch)
    );
    assert_eq!(
        rpc.methods(),
        vec!["anvil_metadata", "eth_getBlockByNumber", "eth_getCode"]
    );
}

#[test]
fn transport_is_loopback_only_and_has_no_broadcast_method() {
    assert!(HttpForkRpc::new("http://127.0.0.1:8545", Duration::from_secs(5)).is_ok());
    assert!(HttpForkRpc::new("http://[::1]:8545", Duration::from_secs(5)).is_ok());
    assert!(HttpForkRpc::new("http://localhost:8545", Duration::from_secs(5)).is_ok());
    assert!(HttpForkRpc::new("https://127.0.0.1:8545", Duration::from_secs(5)).is_err());
    assert!(HttpForkRpc::new("http://rpc.example:8545", Duration::from_secs(5)).is_err());
    let embedded_credentials = ["http://", "user", ":", "password", "@127.0.0.1:8545"].concat();
    assert!(HttpForkRpc::new(&embedded_credentials, Duration::from_secs(5)).is_err());
    assert!(ALLOWED_RPC_METHODS
        .iter()
        .all(|method| !method.starts_with("eth_send") && !method.contains("impersonate")));
    let source = include_str!("../src/rpc.rs");
    assert!(!source.contains(&["eth_", "sendRawTransaction"].concat()));
    assert!(!source.contains(&["eth_", "sendTransaction"].concat()));
    assert!(!source.contains(&["anvil_", "impersonateAccount"].concat()));
}

#[test]
fn production_engine_and_compose_do_not_reference_the_sandbox() {
    let engine_manifest = include_str!("../../phoenix-engine/Cargo.toml");
    let production_compose = include_str!("../../compose.prod.yml");
    assert!(!engine_manifest.contains("phoenix-fork-sandbox"));
    assert!(!engine_manifest.contains("fork-sandbox"));
    assert!(!production_compose.contains("fork-sandbox"));
    assert!(!production_compose.contains("PHOENIX_FORK_MODE"));
}

fn fixture() -> (PersistedOpportunity, Vec<PoolObservation>, Vec<String>) {
    let requests = pool_requests();
    let observations = vec![
        PoolObservation {
            token0: TOKEN_A.to_string(),
            token1: TOKEN_B.to_string(),
            fee: 500,
            slot0: format!("0x{}", "01".repeat(64)),
            liquidity: format!("0x{}", "02".repeat(32)),
        },
        PoolObservation {
            token0: TOKEN_A.to_string(),
            token1: TOKEN_B.to_string(),
            fee: 3_000,
            slot0: format!("0x{}", "03".repeat(64)),
            liquidity: format!("0x{}", "04".repeat(32)),
        },
    ];
    let (pool_state_hashes, aggregate_state_hash) = state_hashes(&requests, &observations);
    let request = ShadowStateRequest {
        schema_version: SHADOW_STATE_SCHEMA_VERSION.to_string(),
        chain_id: 42161,
        route_fingerprint: "fixture-route-v1".to_string(),
        pools: requests,
        evidence: EvidenceRequest::Primary,
    };
    let route_config_hash = request
        .route_config_hash()
        .expect("fixture route config hash");
    (
        PersistedOpportunity {
            shadow_decision_id: "11111111-1111-8111-8111-111111111111".to_string(),
            source_event_identity: format!("phoenix.engine.input.v1:7:0x{}", "9".repeat(64)),
            chain_id: 42161,
            route_id: "fixture-route".to_string(),
            route_fingerprint: "fixture-route-v1".to_string(),
            origin_router: ROUTER.to_string(),
            token_path: vec![
                TOKEN_A.to_string(),
                TOKEN_B.to_string(),
                TOKEN_A.to_string(),
            ],
            pool_path: vec!["pool-a".to_string(), "pool-b".to_string()],
            pool_address_path: vec![POOL_A.to_string(), POOL_B.to_string()],
            protocol_path: vec!["UniswapV3".to_string(), "SushiSwapV3".to_string()],
            direction_path: vec!["zero_for_one".to_string(), "one_for_zero".to_string()],
            fee_path: vec![500, 3_000],
            expected_leg_outputs: vec!["150".to_string(), "200".to_string()],
            pool_state_hash_path: pool_state_hashes.clone(),
            input_amount: "100".to_string(),
            expected_output: "200".to_string(),
            gross_profit: "100".to_string(),
            total_cost: "10".to_string(),
            expected_net_pnl: "90".to_string(),
            minimum_required_net_pnl: "50".to_string(),
            execution_gas: "10".to_string(),
            gas_price: "1".to_string(),
            detected_at: Utc
                .timestamp_millis_opt(NOW_MS as i64)
                .single()
                .expect("detected timestamp"),
            opportunity_expires_at: Utc
                .timestamp_millis_opt((NOW_MS + 60_000) as i64)
                .single()
                .expect("expiry timestamp"),
            pinned_block_number: 100,
            pinned_block_hash: BLOCK_HASH.to_string(),
            primary_state_hash: aggregate_state_hash.clone(),
            route_config_hash,
            primary_provider_id: "provider_0".to_string(),
            secondary_provider_id: Some("provider_1".to_string()),
            secondary_state_hash: Some(aggregate_state_hash),
            secondary_block_number: Some(100),
            secondary_block_hash: Some(BLOCK_HASH.to_string()),
            secondary_route_config_hash: Some(
                request
                    .route_config_hash()
                    .expect("secondary fixture route hash"),
            ),
            verification_status: "agreed".to_string(),
            independent_verification_status: "agreed".to_string(),
            agreement_state: "agreed".to_string(),
            model_version: "shadow-profitability-v1".to_string(),
            policy_version: "shadow-state-policy-v1".to_string(),
            disposition: "accepted".to_string(),
            primary_profitability_status: "meets_minimum".to_string(),
            evidence_completeness_status: "complete".to_string(),
            fork_evidence_schema_version: "phoenix.fork-evidence.v1".to_string(),
            shadow_only: true,
            execution_eligible: false,
            execution_request_created: false,
        },
        observations,
        pool_state_hashes,
    )
}

fn policy() -> PlanPolicy {
    PlanPolicy {
        allowed_tokens: [TOKEN_A.to_string(), TOKEN_B.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        allowed_pools: [POOL_A.to_string(), POOL_B.to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        allowed_routers: [ROUTER.to_string()].into_iter().collect(),
        allowed_protocols: ["UniswapV3".to_string(), "SushiSwapV3".to_string()]
            .into_iter()
            .collect(),
        target_contract: TARGET.to_string(),
        target_code_hash: hex::encode(Sha256::digest([0x60, 0x00])),
        simulation_from: SIMULATION_FROM.to_string(),
        minimum_net_pnl: 50,
        maximum_input_amount: 1_000,
        slippage_bps: 100,
        maximum_calldata_bytes: 65_536,
    }
}

fn pool_requests() -> Vec<PoolStateRequest> {
    vec![
        PoolStateRequest {
            pool_id: "pool-a".to_string(),
            address: POOL_A.to_string(),
            protocol: "UniswapV3".to_string(),
            token0: TOKEN_A.to_string(),
            token1: TOKEN_B.to_string(),
            fee: 500,
        },
        PoolStateRequest {
            pool_id: "pool-b".to_string(),
            address: POOL_B.to_string(),
            protocol: "SushiSwapV3".to_string(),
            token0: TOKEN_A.to_string(),
            token1: TOKEN_B.to_string(),
            fee: 3_000,
        },
    ]
}

fn state_hashes(
    requests: &[PoolStateRequest],
    observations: &[PoolObservation],
) -> (Vec<String>, String) {
    let responses = requests
        .iter()
        .zip(observations)
        .map(|(request, observation)| {
            let material = serde_json::to_vec(&(
                &request.pool_id,
                &request.address,
                &request.protocol,
                &request.token0,
                &request.token1,
                request.fee,
                &observation.slot0,
                &observation.liquidity,
            ))
            .expect("serialize fixture pool state");
            PoolStateResponse {
                pool_id: request.pool_id.clone(),
                address: request.address.clone(),
                protocol: request.protocol.clone(),
                token0: request.token0.clone(),
                token1: request.token1.clone(),
                fee: request.fee,
                slot0: observation.slot0.clone(),
                liquidity: observation.liquidity.clone(),
                state_hash: canonical_hash_bytes(&material),
            }
        })
        .collect::<Vec<_>>();
    let hashes = responses
        .iter()
        .map(|response| response.state_hash.clone())
        .collect();
    let aggregate = canonical_hash_bytes(
        &serde_json::to_vec(&responses).expect("serialize fixture aggregate state"),
    );
    (hashes, aggregate)
}

fn settlement_log(route_hash: &str, realized_profit: u128) -> TraceLog {
    let event = {
        let contract = ethabi::Contract::load(std::io::Cursor::new(include_bytes!(
            "../abi/PhoenixExecutor.json"
        )))
        .expect("load executor ABI");
        contract
            .event("OpportunitySettled")
            .expect("settlement event")
            .clone()
    };
    let asset_topic = format!("0x{}{}", "0".repeat(24), &TOKEN_A[2..]);
    TraceLog {
        address: TARGET.to_string(),
        topics: vec![
            format!("{:#066x}", event.signature()),
            format!("0x{route_hash}"),
            asset_topic,
        ],
        data: format!(
            "0x{}",
            hex::encode(ethabi::encode(&[
                Token::Uint(primitive_types::U256::from(100u64)),
                Token::Uint(primitive_types::U256::from(1u64)),
                Token::Uint(primitive_types::U256::from(realized_profit)),
            ]))
        ),
    }
}

#[allow(dead_code)]
fn _assert_h256_format(value: H256) -> String {
    format!("{value:#066x}")
}
