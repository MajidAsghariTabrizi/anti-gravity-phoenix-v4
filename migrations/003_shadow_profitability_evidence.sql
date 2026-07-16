CREATE TABLE IF NOT EXISTS shadow_decisions (
    id UUID PRIMARY KEY,
    opportunity_id UUID REFERENCES opportunities(id),
    strategy TEXT NOT NULL,
    strategy_version TEXT NOT NULL,
    detector_version TEXT NOT NULL,
    code_version TEXT NOT NULL,
    config_version TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    chain_id BIGINT NOT NULL CHECK (chain_id = 42161),
    source_sequence NUMERIC(78,0) NOT NULL,
    observed_block NUMERIC(78,0) NOT NULL,
    state_block NUMERIC(78,0) NOT NULL,
    quote_block NUMERIC(78,0) NOT NULL,
    route_fingerprint TEXT NOT NULL,
    disposition TEXT NOT NULL CHECK (disposition IN ('accepted', 'rejected')),
    primary_rejection_reason TEXT,
    confidence_bps INTEGER NOT NULL CHECK (confidence_bps BETWEEN 0 AND 10000),
    execution_eligible BOOLEAN NOT NULL DEFAULT false CHECK (execution_eligible = false),
    base_net_pnl NUMERIC(78,0) NOT NULL,
    conservative_net_pnl NUMERIC(78,0) NOT NULL,
    severe_net_pnl NUMERIC(78,0) NOT NULL,
    identity_evidence JSONB NOT NULL,
    route_evidence JSONB NOT NULL,
    market_evidence JSONB NOT NULL,
    economics_evidence JSONB NOT NULL,
    simulation_evidence JSONB NOT NULL,
    decision_evidence JSONB NOT NULL,
    outcome_evidence JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    detected_at TIMESTAMPTZ NOT NULL,
    decided_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (strategy_version, route_fingerprint, source_sequence, observed_block)
);

CREATE TABLE IF NOT EXISTS rpc_quality_records (
    id BIGSERIAL PRIMARY KEY,
    shadow_decision_id UUID REFERENCES shadow_decisions(id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL,
    method TEXT NOT NULL,
    block_number NUMERIC(78,0) NOT NULL,
    block_hash TEXT NOT NULL,
    response_hash TEXT,
    latency_ns NUMERIC(78,0) NOT NULL,
    success BOOLEAN NOT NULL,
    stale_result BOOLEAN NOT NULL,
    disagreement BOOLEAN NOT NULL,
    timeout BOOLEAN NOT NULL,
    retry_count INTEGER NOT NULL CHECK (retry_count >= 0),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS shadow_replay_runs (
    id UUID PRIMARY KEY,
    replay_schema_version TEXT NOT NULL,
    code_version TEXT NOT NULL,
    config_version TEXT NOT NULL,
    strategy_version TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    input_evidence_hash TEXT NOT NULL,
    output_report_hash TEXT NOT NULL,
    candidate_count BIGINT NOT NULL CHECK (candidate_count >= 0),
    accepted_count BIGINT NOT NULL CHECK (accepted_count >= 0),
    rejected_count BIGINT NOT NULL CHECK (rejected_count >= 0),
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (code_version, config_version, input_evidence_hash)
);

CREATE INDEX IF NOT EXISTS shadow_decisions_observed_block_idx
    ON shadow_decisions(observed_block);
CREATE INDEX IF NOT EXISTS shadow_decisions_disposition_idx
    ON shadow_decisions(disposition, primary_rejection_reason);
CREATE INDEX IF NOT EXISTS shadow_decisions_strategy_created_idx
    ON shadow_decisions(strategy, created_at);
CREATE INDEX IF NOT EXISTS rpc_quality_records_block_idx
    ON rpc_quality_records(block_number, provider_id);
