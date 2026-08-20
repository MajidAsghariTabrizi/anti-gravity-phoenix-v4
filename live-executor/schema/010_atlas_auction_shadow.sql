BEGIN;

SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';

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
            'phoenix.live-canary-schema.v6',
            'phoenix.live-canary-schema.v7',
            'phoenix.live-canary-schema.v8',
            'phoenix.live-canary-schema.v9',
            'phoenix.live-canary-schema.v10'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v10')
ON CONFLICT (version) DO NOTHING;

-- Independent Atlas SHADOW evaluation ledger. Every relevant SVR auction gets
-- exactly one row: either an eligible shadow bid (economics filled, reason
-- NULL) or a terminal rejection (reason set, economics NULL). This table is
-- evidence only. It never authorizes, gates, or materializes any live
-- execution request or Atlas solver request.
CREATE TABLE IF NOT EXISTS live_canary.atlas_auction_shadow (
    auction_id TEXT PRIMARY KEY CHECK (length(auction_id) BETWEEN 1 AND 128),
    user_operation_hash TEXT CHECK (user_operation_hash IS NULL OR user_operation_hash ~ '^0x[0-9a-f]{64}$'),
    dapp TEXT CHECK (dapp IS NULL OR dapp ~ '^0x[0-9a-f]{40}$'),
    asset TEXT CHECK (asset IS NULL OR length(asset) BETWEEN 1 AND 32),
    evidence_hash TEXT NOT NULL CHECK (evidence_hash ~ '^[0-9a-f]{64}$'),
    observed_at TIMESTAMPTZ,
    evaluated_at TIMESTAMPTZ NOT NULL,
    ingress_latency_ms BIGINT NOT NULL DEFAULT 0 CHECK (ingress_latency_ms >= 0),
    identity_valid BOOLEAN NOT NULL DEFAULT false,
    bounds_valid BOOLEAN NOT NULL DEFAULT false,
    borrower TEXT CHECK (borrower IS NULL OR borrower ~ '^0x[0-9a-f]{40}$'),
    block_number NUMERIC(78,0),
    block_hash TEXT CHECK (block_hash IS NULL OR block_hash ~ '^0x[0-9a-f]{64}$'),
    exact_completed BOOLEAN NOT NULL DEFAULT false,
    callback_simulation_attempted BOOLEAN NOT NULL DEFAULT false,
    callback_simulation_passed BOOLEAN NOT NULL DEFAULT false,
    evidence_mode TEXT CHECK (
        evidence_mode IS NULL
        OR evidence_mode = 'SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED'
    ),
    simulated_gas_limit BIGINT CHECK (simulated_gas_limit IS NULL OR simulated_gas_limit > 0),
    solver_gas_settlement_wei NUMERIC(78,0) CHECK (solver_gas_settlement_wei IS NULL OR solver_gas_settlement_wei >= 0),
    gross_value_wei NUMERIC(78,0) CHECK (gross_value_wei IS NULL OR gross_value_wei >= 0),
    direct_cost_wei NUMERIC(78,0) CHECK (direct_cost_wei IS NULL OR direct_cost_wei >= 0),
    zero_bid_conservative_wei NUMERIC(78,0) CHECK (zero_bid_conservative_wei IS NULL OR zero_bid_conservative_wei >= 0),
    maximum_bid_wei NUMERIC(78,0) CHECK (maximum_bid_wei IS NULL OR maximum_bid_wei >= 0),
    selected_bid_wei NUMERIC(78,0) CHECK (selected_bid_wei IS NULL OR selected_bid_wei >= 0),
    competitive_reserve_wei NUMERIC(78,0) CHECK (competitive_reserve_wei IS NULL OR competitive_reserve_wei >= 0),
    expected_net_after_bid_wei NUMERIC(78,0),
    conservative_net_after_bid_wei NUMERIC(78,0),
    shadow_bid_eligible BOOLEAN NOT NULL DEFAULT false,
    terminal_rejection_reason TEXT CHECK (
        terminal_rejection_reason IS NULL
        OR (
            length(terminal_rejection_reason) BETWEEN 1 AND 128
            AND terminal_rejection_reason ~ '^[a-z0-9_]+$'
        )
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (shadow_bid_eligible = (terminal_rejection_reason IS NULL))
);

CREATE INDEX IF NOT EXISTS live_canary_atlas_shadow_rejection
ON live_canary.atlas_auction_shadow(terminal_rejection_reason, evaluated_at);

CREATE INDEX IF NOT EXISTS live_canary_atlas_shadow_validation
ON live_canary.atlas_auction_shadow(shadow_bid_eligible, evaluated_at);

COMMIT;
