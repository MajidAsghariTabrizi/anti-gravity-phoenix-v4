BEGIN;

CREATE SCHEMA IF NOT EXISTS live_canary;

CREATE TABLE IF NOT EXISTS live_canary.schema_contract (
    version TEXT PRIMARY KEY,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (version = 'phoenix.live-canary-schema.v1')
);

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v1')
ON CONFLICT (version) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.control (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    armed BOOLEAN NOT NULL DEFAULT false,
    kill_switch BOOLEAN NOT NULL DEFAULT true,
    disarm_reason TEXT NOT NULL DEFAULT 'not_armed'
        CHECK (length(disarm_reason) BETWEEN 1 AND 128),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO live_canary.control(singleton, armed, kill_switch, disarm_reason)
VALUES (true, false, true, 'not_armed')
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.execution_requests (
    id UUID PRIMARY KEY,
    opportunity_id UUID NOT NULL UNIQUE,
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.live-execution-request.v1'),
    chain_id BIGINT NOT NULL CHECK (chain_id = 42161),
    route_id TEXT NOT NULL CHECK (route_id ~ '^0x[0-9a-f]{64}$'),
    origin_router TEXT NOT NULL CHECK (origin_router ~ '^0x[0-9a-f]{40}$'),
    flash_asset TEXT NOT NULL CHECK (flash_asset ~ '^0x[0-9a-f]{40}$'),
    flash_amount NUMERIC(39,0) NOT NULL
        CHECK (flash_amount > 0 AND flash_amount <= 340282366920938463463374607431768211455),
    maximum_input_amount NUMERIC(39,0) NOT NULL
        CHECK (maximum_input_amount > 0 AND maximum_input_amount <= 340282366920938463463374607431768211455),
    minimum_profit NUMERIC(39,0) NOT NULL
        CHECK (minimum_profit > 0 AND minimum_profit <= 340282366920938463463374607431768211455),
    expected_profit NUMERIC(39,0) NOT NULL
        CHECK (expected_profit > 0 AND expected_profit <= 340282366920938463463374607431768211455),
    deadline TIMESTAMPTZ NOT NULL,
    legs JSONB NOT NULL CHECK (jsonb_typeof(legs) = 'array'),
    gas_limit BIGINT NOT NULL CHECK (gas_limit > 0),
    max_fee_per_gas NUMERIC(39,0) NOT NULL
        CHECK (max_fee_per_gas > 0 AND max_fee_per_gas <= 340282366920938463463374607431768211455),
    max_priority_fee_per_gas NUMERIC(39,0) NOT NULL
        CHECK (
            max_priority_fee_per_gas > 0
            AND max_priority_fee_per_gas <= max_fee_per_gas
        ),
    approved_by TEXT,
    approved_at TIMESTAMPTZ,
    policy_version TEXT,
    approval_digest TEXT UNIQUE,
    status TEXT NOT NULL DEFAULT 'draft'
        CHECK (
            status IN (
                'draft',
                'approved',
                'claimed',
                'nonce_allocated',
                'submission_unknown',
                'pending',
                'confirmed',
                'reverted',
                'replaced',
                'timed_out',
                'failed'
            )
        ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        status = 'draft'
        OR (
            approved_by IS NOT NULL
            AND length(btrim(approved_by)) BETWEEN 1 AND 128
            AND approved_at IS NOT NULL
            AND policy_version IS NOT NULL
            AND length(btrim(policy_version)) BETWEEN 1 AND 128
            AND approval_digest ~ '^[0-9a-f]{64}$'
        )
    )
);

CREATE TABLE IF NOT EXISTS live_canary.execution_attempts (
    id BIGSERIAL PRIMARY KEY,
    request_id UUID NOT NULL UNIQUE
        REFERENCES live_canary.execution_requests(id) ON DELETE RESTRICT,
    chain_id BIGINT NOT NULL CHECK (chain_id = 42161),
    wallet_address TEXT NOT NULL CHECK (wallet_address ~ '^0x[0-9a-f]{40}$'),
    executor_address TEXT NOT NULL CHECK (executor_address ~ '^0x[0-9a-f]{40}$'),
    nonce NUMERIC(20,0),
    tx_hash TEXT UNIQUE CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$'),
    status TEXT NOT NULL
        CHECK (
            status IN (
                'claimed',
                'nonce_allocated',
                'submission_unknown',
                'pending',
                'confirmed',
                'reverted',
                'replaced',
                'timed_out',
                'failed'
            )
        ),
    error_code TEXT CHECK (error_code IS NULL OR length(error_code) BETWEEN 1 AND 128),
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    submitted_at TIMESTAMPTZ,
    terminal_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (status IN ('claimed') AND nonce IS NULL AND tx_hash IS NULL)
        OR (status = 'nonce_allocated' AND nonce IS NOT NULL AND tx_hash IS NULL)
        OR (
            status = 'submission_unknown'
            AND nonce IS NOT NULL
            AND tx_hash IS NULL
            AND error_code IS NOT NULL
        )
        OR (status = 'pending' AND nonce IS NOT NULL AND tx_hash IS NOT NULL AND submitted_at IS NOT NULL)
        OR (
            status IN ('confirmed', 'reverted', 'replaced', 'timed_out')
            AND nonce IS NOT NULL
            AND tx_hash IS NOT NULL
            AND submitted_at IS NOT NULL
            AND terminal_at IS NOT NULL
        )
        OR (status = 'failed' AND terminal_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_one_active_attempt
ON live_canary.execution_attempts ((true))
WHERE status IN (
    'claimed',
    'nonce_allocated',
    'submission_unknown',
    'pending',
    'timed_out'
);

CREATE TABLE IF NOT EXISTS live_canary.nonce_state (
    chain_id BIGINT NOT NULL CHECK (chain_id = 42161),
    wallet_address TEXT NOT NULL CHECK (wallet_address ~ '^0x[0-9a-f]{40}$'),
    next_nonce NUMERIC(20,0) NOT NULL CHECK (next_nonce >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, wallet_address)
);

CREATE TABLE IF NOT EXISTS live_canary.execution_outcomes (
    request_id UUID PRIMARY KEY
        REFERENCES live_canary.execution_requests(id) ON DELETE RESTRICT,
    tx_hash TEXT NOT NULL UNIQUE CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    outcome_status TEXT NOT NULL CHECK (outcome_status IN ('confirmed', 'reverted')),
    receipt_status SMALLINT NOT NULL CHECK (receipt_status IN (0, 1)),
    settled_event_found BOOLEAN NOT NULL,
    block_number NUMERIC(20,0) NOT NULL CHECK (block_number >= 0),
    gas_used NUMERIC(20,0) NOT NULL CHECK (gas_used > 0),
    effective_gas_price NUMERIC(39,0) NOT NULL CHECK (effective_gas_price > 0),
    actual_fee_wei NUMERIC(39,0) NOT NULL CHECK (actual_fee_wei > 0),
    asset TEXT NOT NULL CHECK (asset ~ '^0x[0-9a-f]{40}$'),
    flash_amount NUMERIC(39,0) NOT NULL CHECK (flash_amount > 0),
    premium NUMERIC(39,0) NOT NULL CHECK (premium >= 0),
    realized_profit NUMERIC(39,0) NOT NULL CHECK (realized_profit >= 0),
    net_pnl_wei NUMERIC(40,0) NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (
            outcome_status = 'confirmed'
            AND receipt_status = 1
            AND settled_event_found
            AND net_pnl_wei = realized_profit - actual_fee_wei
        )
        OR (
            outcome_status = 'reverted'
            AND receipt_status = 0
            AND NOT settled_event_found
            AND premium = 0
            AND realized_profit = 0
            AND net_pnl_wei = -actual_fee_wei
        )
    )
);

CREATE INDEX IF NOT EXISTS live_canary_approved_requests
ON live_canary.execution_requests (approved_at, id)
WHERE status = 'approved';

CREATE INDEX IF NOT EXISTS live_canary_daily_outcomes
ON live_canary.execution_outcomes (recorded_at);

COMMIT;
