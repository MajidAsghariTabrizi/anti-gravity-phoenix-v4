ALTER TABLE shadow_profitability_facts
    ADD COLUMN IF NOT EXISTS origin_router TEXT,
    ADD COLUMN IF NOT EXISTS pool_address_path JSONB,
    ADD COLUMN IF NOT EXISTS protocol_path JSONB,
    ADD COLUMN IF NOT EXISTS direction_path JSONB,
    ADD COLUMN IF NOT EXISTS expected_leg_outputs JSONB,
    ADD COLUMN IF NOT EXISTS pool_state_hash_path JSONB,
    ADD COLUMN IF NOT EXISTS opportunity_expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS fork_evidence_schema_version TEXT;

ALTER TABLE shadow_profitability_facts
    ADD CONSTRAINT shadow_profitability_fork_evidence_check CHECK (
        (
            origin_router IS NULL
            AND pool_address_path IS NULL
            AND protocol_path IS NULL
            AND direction_path IS NULL
            AND expected_leg_outputs IS NULL
            AND pool_state_hash_path IS NULL
            AND opportunity_expires_at IS NULL
            AND fork_evidence_schema_version IS NULL
        )
        OR (
            origin_router ~ '^0x[0-9a-f]{40}$'
            AND jsonb_typeof(pool_address_path) = 'array'
            AND jsonb_typeof(protocol_path) = 'array'
            AND jsonb_typeof(direction_path) = 'array'
            AND jsonb_typeof(expected_leg_outputs) = 'array'
            AND jsonb_typeof(pool_state_hash_path) = 'array'
            AND jsonb_array_length(pool_address_path) = jsonb_array_length(pool_path)
            AND jsonb_array_length(protocol_path) = jsonb_array_length(pool_path)
            AND jsonb_array_length(direction_path) = jsonb_array_length(pool_path)
            AND jsonb_array_length(expected_leg_outputs) = jsonb_array_length(pool_path)
            AND jsonb_array_length(pool_state_hash_path) = jsonb_array_length(pool_path)
            AND opportunity_expires_at > detected_at
            AND fork_evidence_schema_version = 'phoenix.fork-evidence.v1'
        )
    );

CREATE TABLE IF NOT EXISTS fork_simulation_results (
    result_hash TEXT PRIMARY KEY,
    plan_hash TEXT NOT NULL UNIQUE,
    shadow_decision_id UUID NOT NULL
        REFERENCES shadow_profitability_facts(shadow_decision_id) ON DELETE RESTRICT,
    plan_schema_version TEXT NOT NULL,
    result_schema_version TEXT NOT NULL,
    plan JSONB NOT NULL,
    evidence JSONB NOT NULL,
    status TEXT NOT NULL,
    predicted_gross_profit NUMERIC(78,0) NOT NULL,
    predicted_total_cost NUMERIC(78,0) NOT NULL,
    predicted_net_pnl NUMERIC(78,0) NOT NULL,
    simulated_gross_profit NUMERIC(78,0),
    simulated_gas_cost NUMERIC(78,0),
    simulated_balance_delta NUMERIC(78,0),
    simulated_net_pnl NUMERIC(78,0),
    prediction_error NUMERIC(78,0),
    gas_estimate NUMERIC(78,0),
    gas_used NUMERIC(78,0),
    model_version TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    fork_chain_id BIGINT NOT NULL,
    fork_block_number NUMERIC(78,0) NOT NULL,
    fork_block_hash TEXT NOT NULL,
    fork_instance_hash TEXT NOT NULL,
    local_block_number NUMERIC(78,0) NOT NULL,
    local_block_hash TEXT NOT NULL,
    simulated_at TIMESTAMPTZ NOT NULL,
    revert_reason TEXT,
    fork_only BOOLEAN NOT NULL DEFAULT true,
    shadow_only BOOLEAN NOT NULL DEFAULT true,
    live_execution BOOLEAN NOT NULL DEFAULT false,
    execution_eligible BOOLEAN NOT NULL DEFAULT false,
    execution_request_created BOOLEAN NOT NULL DEFAULT false,
    public_broadcast BOOLEAN NOT NULL DEFAULT false,
    signer_used BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fork_simulation_result_identity_check CHECK (
        result_hash ~ '^[0-9a-f]{64}$'
        AND plan_hash ~ '^[0-9a-f]{64}$'
        AND fork_instance_hash ~ '^[0-9a-f]{64}$'
        AND fork_block_hash ~ '^0x[0-9a-f]{64}$'
        AND local_block_hash ~ '^0x[0-9a-f]{64}$'
        AND fork_chain_id = 42161
        AND fork_block_number > 0
        AND local_block_number >= fork_block_number
    ),
    CONSTRAINT fork_simulation_result_schema_check CHECK (
        plan_schema_version = 'phoenix.unsigned-fork-plan.v1'
        AND result_schema_version = 'phoenix.fork-result.v1'
        AND jsonb_typeof(plan) = 'object'
        AND jsonb_typeof(evidence) = 'object'
        AND octet_length(plan::text) <= 1048576
        AND octet_length(evidence::text) <= 1048576
        AND char_length(model_version) BETWEEN 1 AND 128
        AND char_length(policy_version) BETWEEN 1 AND 128
    ),
    CONSTRAINT fork_simulation_result_financial_check CHECK (
        predicted_gross_profit > 0
        AND predicted_total_cost >= 0
        AND predicted_net_pnl > 0
        AND CASE status
            WHEN 'passed' THEN
                simulated_gross_profit IS NOT NULL
                AND simulated_gross_profit >= 0
                AND simulated_gas_cost IS NOT NULL
                AND simulated_gas_cost >= 0
                AND simulated_balance_delta IS NOT NULL
                AND simulated_balance_delta = simulated_gross_profit
                AND simulated_net_pnl IS NOT NULL
                AND simulated_net_pnl = simulated_balance_delta - simulated_gas_cost
                AND prediction_error IS NOT NULL
                AND prediction_error = simulated_net_pnl - predicted_net_pnl
                AND gas_estimate IS NOT NULL
                AND gas_estimate > 0
                AND gas_used IS NOT NULL
                AND gas_used > 0
                AND gas_used <= gas_estimate
                AND revert_reason IS NULL
            WHEN 'reverted' THEN
                simulated_gross_profit IS NULL
                AND simulated_gas_cost IS NULL
                AND simulated_balance_delta IS NULL
                AND simulated_net_pnl IS NULL
                AND prediction_error IS NULL
                AND gas_used IS NULL
                AND revert_reason IS NOT NULL
                AND char_length(revert_reason) BETWEEN 1 AND 1024
            ELSE false
        END
    ),
    CONSTRAINT fork_simulation_result_safety_check CHECK (
        fork_only = true
        AND shadow_only = true
        AND live_execution = false
        AND execution_eligible = false
        AND execution_request_created = false
        AND public_broadcast = false
        AND signer_used = false
    )
);

CREATE INDEX IF NOT EXISTS fork_simulation_results_decision_idx
    ON fork_simulation_results(shadow_decision_id, simulated_at DESC);
CREATE INDEX IF NOT EXISTS fork_simulation_results_fork_block_idx
    ON fork_simulation_results(fork_block_number, simulated_at DESC);
