BEGIN;

CREATE TABLE IF NOT EXISTS origin_transactions (
    id BIGSERIAL PRIMARY KEY,
    tx_hash TEXT NOT NULL UNIQUE,
    sequence_number NUMERIC(78,0) NOT NULL,
    chain_id BIGINT NOT NULL CHECK (chain_id = 42161),
    router TEXT,
    classification TEXT NOT NULL,
    calldata BYTEA,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS opportunities (
    id UUID PRIMARY KEY,
    route_id TEXT NOT NULL,
    origin_tx_hash TEXT NOT NULL REFERENCES origin_transactions(tx_hash),
    origin_sequence NUMERIC(78,0) NOT NULL,
    snapshot_id TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL,
    flash_asset TEXT NOT NULL,
    optimized_amount NUMERIC(78,0) NOT NULL,
    expected_gross_profit NUMERIC(78,0) NOT NULL,
    expected_flash_premium NUMERIC(78,0) NOT NULL,
    expected_execution_cost NUMERIC(78,0) NOT NULL,
    expected_ordering_cost NUMERIC(78,0) NOT NULL DEFAULT 0,
    uncertainty_reserve NUMERIC(78,0) NOT NULL DEFAULT 0,
    expected_net_profit NUMERIC(78,0) NOT NULL,
    min_profit NUMERIC(78,0) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS opportunity_legs (
    id BIGSERIAL PRIMARY KEY,
    opportunity_id UUID NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    leg_index INTEGER NOT NULL,
    protocol TEXT NOT NULL,
    pool TEXT NOT NULL,
    token_in TEXT NOT NULL,
    token_out TEXT NOT NULL,
    fee INTEGER NOT NULL,
    direction TEXT NOT NULL,
    expected_amount_in NUMERIC(78,0) NOT NULL,
    expected_amount_out NUMERIC(78,0) NOT NULL,
    UNIQUE (opportunity_id, leg_index)
);

CREATE TABLE IF NOT EXISTS execution_attempts (
    id BIGSERIAL PRIMARY KEY,
    opportunity_id UUID NOT NULL REFERENCES opportunities(id),
    tx_hash TEXT,
    submitted_at TIMESTAMPTZ,
    submission_latency_ms NUMERIC(20,3),
    submission_endpoint_class TEXT NOT NULL,
    nonce NUMERIC(78,0),
    expected_net_profit NUMERIC(78,0) NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE TABLE IF NOT EXISTS executions (
    id BIGSERIAL PRIMARY KEY,
    opportunity_id UUID NOT NULL REFERENCES opportunities(id),
    execution_attempt_id BIGINT REFERENCES execution_attempts(id),
    tx_hash TEXT NOT NULL UNIQUE,
    receipt_status INTEGER NOT NULL,
    block_number NUMERIC(78,0) NOT NULL,
    gas_used NUMERIC(78,0) NOT NULL,
    effective_gas_price NUMERIC(78,0) NOT NULL,
    actual_tx_fee_wei NUMERIC(78,0) NOT NULL,
    settled_event_found BOOLEAN NOT NULL DEFAULT false,
    reconciled_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS realized_pnl (
    id BIGSERIAL PRIMARY KEY,
    execution_id BIGINT NOT NULL REFERENCES executions(id),
    asset TEXT NOT NULL,
    flash_amount NUMERIC(78,0) NOT NULL,
    premium NUMERIC(78,0) NOT NULL,
    realized_profit_asset_units NUMERIC(78,0) NOT NULL,
    actual_tx_fee_wei NUMERIC(78,0) NOT NULL,
    actual_ordering_cost_wei NUMERIC(78,0) NOT NULL DEFAULT 0,
    source_event_signature TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS miss_reasons (
    id BIGSERIAL PRIMARY KEY,
    origin_tx_hash TEXT REFERENCES origin_transactions(tx_hash),
    opportunity_id UUID REFERENCES opportunities(id),
    reason TEXT NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS gas_profiles (
    execution_shape TEXT PRIMARY KEY,
    gas_used_p50 NUMERIC(78,0),
    gas_used_p90 NUMERIC(78,0),
    gas_used_p99 NUMERIC(78,0),
    sample_count BIGINT NOT NULL DEFAULT 0,
    ewma NUMERIC(78,6),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS pool_state_checkpoints (
    id BIGSERIAL PRIMARY KEY,
    pool TEXT NOT NULL,
    block_number NUMERIC(78,0) NOT NULL,
    sqrt_price_x96 NUMERIC(78,0) NOT NULL,
    tick INTEGER NOT NULL,
    liquidity NUMERIC(78,0) NOT NULL,
    completeness_min_tick INTEGER NOT NULL,
    completeness_max_tick INTEGER NOT NULL,
    checkpoint JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pool, block_number)
);

CREATE TABLE IF NOT EXISTS feed_events (
    id BIGSERIAL PRIMARY KEY,
    sequence_number NUMERIC(78,0) NOT NULL,
    tx_hash TEXT,
    payload JSONB NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (sequence_number, tx_hash)
);

CREATE INDEX IF NOT EXISTS opportunities_lifecycle_idx ON opportunities(lifecycle_state);
CREATE INDEX IF NOT EXISTS miss_reasons_reason_idx ON miss_reasons(reason);
CREATE INDEX IF NOT EXISTS execution_attempts_status_idx ON execution_attempts(status);

COMMIT;

