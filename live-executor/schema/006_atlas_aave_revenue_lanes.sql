BEGIN;

ALTER TABLE live_canary.schema_contract
    DROP CONSTRAINT IF EXISTS schema_contract_version_check;

ALTER TABLE live_canary.schema_contract
    ADD CONSTRAINT schema_contract_version_check CHECK (
        version IN (
            'phoenix.live-canary-schema.v1',
            'phoenix.live-canary-schema.v2',
            'phoenix.live-canary-schema.v3',
            'phoenix.live-canary-schema.v4',
            'phoenix.live-canary-schema.v5',
            'phoenix.live-canary-schema.v6'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v6')
ON CONFLICT (version) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.revenue_lane_controls (
    lane TEXT PRIMARY KEY CHECK (lane IN ('phoenix_dex', 'atlas_solver', 'aave_liquidation')),
    schema_version TEXT NOT NULL DEFAULT 'phoenix.revenue-lane-control.v1'
        CHECK (schema_version = 'phoenix.revenue-lane-control.v1'),
    armed BOOLEAN NOT NULL DEFAULT false,
    kill_switch BOOLEAN NOT NULL DEFAULT true,
    maximum_input_amount NUMERIC(78,0) NOT NULL DEFAULT 1 CHECK (maximum_input_amount > 0),
    maximum_gas_limit BIGINT NOT NULL DEFAULT 1 CHECK (maximum_gas_limit > 0),
    maximum_fee_per_gas NUMERIC(78,0) NOT NULL DEFAULT 1 CHECK (maximum_fee_per_gas > 0),
    maximum_atlas_bid NUMERIC(78,0) NOT NULL DEFAULT 0 CHECK (maximum_atlas_bid >= 0),
    daily_loss_limit NUMERIC(78,0) NOT NULL DEFAULT 0 CHECK (daily_loss_limit >= 0),
    retained_profit_floor NUMERIC(78,0) NOT NULL DEFAULT 1 CHECK (retained_profit_floor > 0),
    disarm_reason TEXT NOT NULL DEFAULT 'not_armed' CHECK (length(disarm_reason) BETWEEN 1 AND 128),
    control_epoch BIGINT NOT NULL DEFAULT 0 CHECK (control_epoch >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((armed AND NOT kill_switch) OR (NOT armed AND kill_switch))
);

INSERT INTO live_canary.revenue_lane_controls(lane)
VALUES ('phoenix_dex'), ('atlas_solver'), ('aave_liquidation')
ON CONFLICT (lane) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.atlas_auction_ingress (
    auction_id TEXT PRIMARY KEY CHECK (length(auction_id) BETWEEN 1 AND 128),
    user_operation_hash TEXT NOT NULL CHECK (user_operation_hash ~ '^0x[0-9a-f]{64}$'),
    parallel_auction_identity TEXT NOT NULL CHECK (length(parallel_auction_identity) BETWEEN 1 AND 256),
    auction_deadline_block NUMERIC(78,0) NOT NULL CHECK (auction_deadline_block > 0),
    oracle_gas_price_wei NUMERIC(78,0) NOT NULL CHECK (oracle_gas_price_wei > 0),
    solver_gas_limit BIGINT NOT NULL CHECK (solver_gas_limit > 0),
    dapp TEXT NOT NULL CHECK (dapp ~ '^0x[0-9a-f]{40}$'),
    oracle_aggregator TEXT CHECK (oracle_aggregator IS NULL OR oracle_aggregator ~ '^0x[0-9a-f]{40}$'),
    oracle_asset TEXT CHECK (oracle_asset IS NULL OR length(oracle_asset) BETWEEN 1 AND 32),
    relevant_aave BOOLEAN NOT NULL,
    parallel_eligible BOOLEAN NOT NULL,
    evidence_hash TEXT NOT NULL CHECK (evidence_hash ~ '^[0-9a-f]{64}$'),
    terminal_outcome TEXT NOT NULL DEFAULT 'observed'
        CHECK (terminal_outcome IN ('observed','economic_rejection','exact_pending','candidate','submitted','settled','expired','incomplete')),
    observed_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_atlas_parallel_auction_identity
ON live_canary.atlas_auction_ingress(parallel_auction_identity, auction_id);

ALTER TABLE live_canary.execution_requests
    ADD COLUMN IF NOT EXISTS route_type TEXT NOT NULL DEFAULT 'PHOENIX_DEX_V1',
    ADD COLUMN IF NOT EXISTS route_payload JSONB;

ALTER TABLE live_canary.execution_requests
    DROP CONSTRAINT IF EXISTS execution_requests_revenue_route_check;

ALTER TABLE live_canary.execution_requests
    ADD CONSTRAINT execution_requests_revenue_route_check CHECK (
        (route_type = 'PHOENIX_DEX_V1' AND route_payload IS NULL)
        OR (
            route_type = 'AAVE_LIQUIDATION_V1'
            AND route_fingerprint = 'AAVE_LIQUIDATION_V1'
            AND route_payload IS NOT NULL
            AND jsonb_typeof(route_payload) = 'object'
            AND route_payload->>'receive_a_token' = 'false'
            AND route_payload->>'evidence_mode' IN (
                'EIP1186_VERIFIED', 'DUAL_PROVIDER_FORK_VERIFIED'
            )
        )
    );

CREATE TABLE IF NOT EXISTS live_canary.revenue_hunting_signals (
    signal_id UUID PRIMARY KEY,
    signal_identity TEXT NOT NULL UNIQUE CHECK (length(signal_identity) BETWEEN 1 AND 256),
    source_lane TEXT NOT NULL CHECK (source_lane IN ('atlas_solver', 'aave_liquidation')),
    source_cursor NUMERIC(78,0),
    auction_id TEXT,
    borrower TEXT CHECK (borrower IS NULL OR borrower ~ '^0x[0-9a-f]{40}$'),
    block_number NUMERIC(78,0) NOT NULL CHECK (block_number > 0),
    block_hash TEXT NOT NULL CHECK (block_hash ~ '^0x[0-9a-f]{64}$'),
    state_root TEXT CHECK (state_root IS NULL OR state_root ~ '^0x[0-9a-f]{64}$'),
    zero_cost_profit_upper_bound NUMERIC(79,0),
    expected_net_pnl NUMERIC(79,0),
    conservative_net_pnl NUMERIC(79,0),
    retained_profit_floor NUMERIC(78,0) NOT NULL CHECK (retained_profit_floor > 0),
    evidence_mode TEXT CHECK (evidence_mode IS NULL OR evidence_mode IN ('EIP1186_VERIFIED', 'DUAL_PROVIDER_FORK_VERIFIED')),
    candidate_id UUID REFERENCES live_canary.autonomous_candidates(candidate_id) ON DELETE RESTRICT,
    terminal_outcome TEXT NOT NULL CHECK (terminal_outcome IN ('prefiltered', 'economic_rejection', 'exact_pending', 'fork_pending', 'fork_rejection', 'candidate', 'submitted', 'settled', 'incomplete')),
    rejection_reason TEXT CHECK (rejection_reason IS NULL OR length(rejection_reason) BETWEEN 1 AND 128),
    evidence_hash TEXT NOT NULL CHECK (evidence_hash ~ '^[0-9a-f]{64}$'),
    observed_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        candidate_id IS NULL
        OR (
            expected_net_pnl IS NOT NULL
            AND conservative_net_pnl IS NOT NULL
            AND expected_net_pnl > retained_profit_floor
            AND conservative_net_pnl > retained_profit_floor
            AND evidence_mode IS NOT NULL
        )
    )
);

CREATE TABLE IF NOT EXISTS live_canary.global_revenue_submission_lock (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    active_lane TEXT CHECK (active_lane IS NULL OR active_lane IN ('phoenix_dex', 'atlas_solver', 'aave_liquidation')),
    active_identity TEXT CHECK (active_identity IS NULL OR length(active_identity) BETWEEN 1 AND 256),
    acquired_at TIMESTAMPTZ,
    control_epoch BIGINT NOT NULL DEFAULT 0 CHECK (control_epoch >= 0),
    CHECK ((active_lane IS NULL) = (active_identity IS NULL)),
    CHECK ((active_lane IS NULL) = (acquired_at IS NULL))
);

INSERT INTO live_canary.global_revenue_submission_lock(singleton)
VALUES (true)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.atlas_solver_requests (
    auction_id TEXT PRIMARY KEY CHECK (length(auction_id) BETWEEN 1 AND 128),
    signal_id UUID NOT NULL REFERENCES live_canary.revenue_hunting_signals(signal_id) ON DELETE RESTRICT,
    user_operation_hash TEXT NOT NULL CHECK (user_operation_hash ~ '^0x[0-9a-f]{64}$'),
    solver_operation_hash TEXT NOT NULL UNIQUE CHECK (solver_operation_hash ~ '^[0-9a-f]{64}$'),
    solver_operation JSONB NOT NULL CHECK (jsonb_typeof(solver_operation) = 'object' AND octet_length(solver_operation::text) <= 262144),
    maximum_bid NUMERIC(78,0) NOT NULL CHECK (maximum_bid > 0),
    selected_bid NUMERIC(78,0) NOT NULL CHECK (selected_bid > 0 AND selected_bid <= maximum_bid),
    status TEXT NOT NULL DEFAULT 'ready' CHECK (status IN ('ready', 'claimed', 'signed', 'submitted', 'included', 'lost', 'expired', 'submission_unknown', 'reconciled')),
    submission_response_hash TEXT CHECK (submission_response_hash IS NULL OR submission_response_hash ~ '^[0-9a-f]{64}$'),
    inclusion_transaction_hash TEXT CHECK (inclusion_transaction_hash IS NULL OR inclusion_transaction_hash ~ '^0x[0-9a-f]{64}$'),
    inclusion_block_number NUMERIC(78,0) CHECK (inclusion_block_number IS NULL OR inclusion_block_number > 0),
    settled_bid NUMERIC(78,0) CHECK (settled_bid IS NULL OR settled_bid > 0),
    executor_balance_before NUMERIC(78,0) CHECK (executor_balance_before IS NULL OR executor_balance_before >= 0),
    executor_balance_after NUMERIC(78,0) CHECK (executor_balance_after IS NULL OR executor_balance_after >= 0),
    solver_bond_before NUMERIC(78,0) CHECK (solver_bond_before IS NULL OR solver_bond_before >= 0),
    solver_bond_after NUMERIC(78,0) CHECK (solver_bond_after IS NULL OR solver_bond_after >= 0),
    realized_net_pnl NUMERIC(79,0),
    outcome_evidence_hash TEXT CHECK (outcome_evidence_hash IS NULL OR outcome_evidence_hash ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_one_global_revenue_submission
ON live_canary.atlas_solver_requests ((true))
WHERE status IN ('claimed', 'signed', 'submitted', 'submission_unknown');

CREATE INDEX IF NOT EXISTS live_canary_revenue_signal_queue
ON live_canary.revenue_hunting_signals(source_lane, terminal_outcome, observed_at);

COMMIT;
