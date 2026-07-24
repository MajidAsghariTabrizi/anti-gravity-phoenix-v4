BEGIN;

ALTER TABLE live_canary.schema_contract
    DROP CONSTRAINT IF EXISTS schema_contract_version_check;

ALTER TABLE live_canary.schema_contract
    ADD CONSTRAINT schema_contract_version_check CHECK (
        version IN (
            'phoenix.live-canary-schema.v1',
            'phoenix.live-canary-schema.v2',
            'phoenix.live-canary-schema.v3'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v3')
ON CONFLICT (version) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.autonomous_global_control (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    schema_version TEXT NOT NULL
        DEFAULT 'phoenix.autonomous-global-control.v1'
        CHECK (schema_version = 'phoenix.autonomous-global-control.v1'),
    chain_id BIGINT NOT NULL DEFAULT 42161 CHECK (chain_id = 42161),
    armed BOOLEAN NOT NULL DEFAULT false,
    kill_switch BOOLEAN NOT NULL DEFAULT true,
    execution_mode TEXT NOT NULL DEFAULT 'disabled'
        CHECK (
            execution_mode IN (
                'disabled',
                'dry_run',
                'armed_idle',
                'live',
                'disarmed'
            )
        ),
    maximum_input_amount NUMERIC(78,0) NOT NULL
        CHECK (maximum_input_amount > 0),
    daily_loss_limit NUMERIC(78,0) NOT NULL
        CHECK (daily_loss_limit >= 0),
    daily_ordering_budget NUMERIC(78,0) NOT NULL
        CHECK (daily_ordering_budget >= 0),
    maximum_concurrent_candidates INTEGER NOT NULL DEFAULT 1
        CHECK (maximum_concurrent_candidates BETWEEN 1 AND 64),
    control_epoch BIGINT NOT NULL DEFAULT 0 CHECK (control_epoch >= 0),
    disarm_reason TEXT DEFAULT 'not_armed'
        CHECK (
            disarm_reason IS NULL
            OR length(disarm_reason) BETWEEN 1 AND 128
        ),
    control_hash TEXT
        CHECK (control_hash IS NULL OR control_hash ~ '^[0-9a-f]{64}$'),
    control_contract JSONB
        CHECK (
            control_contract IS NULL
            OR (
                jsonb_typeof(control_contract) = 'object'
                AND octet_length(control_contract::text) <= 32768
                AND control_contract ?& ARRAY[
                    'schema_version',
                    'control_hash'
                ]
                AND control_contract->>'schema_version'
                    = 'phoenix.autonomous-global-control.v1'
                AND control_contract->>'control_hash' = control_hash
            )
        ),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        execution_mode <> 'live'
        OR (armed AND NOT kill_switch AND disarm_reason IS NULL)
    ),
    CHECK (
        NOT kill_switch
        OR (execution_mode <> 'live' AND disarm_reason IS NOT NULL)
    )
);

INSERT INTO live_canary.autonomous_global_control(
    singleton,
    armed,
    kill_switch,
    execution_mode,
    maximum_input_amount,
    daily_loss_limit,
    daily_ordering_budget,
    maximum_concurrent_candidates,
    disarm_reason
)
VALUES (
    true,
    false,
    true,
    'disabled',
    1,
    0,
    0,
    1,
    'not_armed'
)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.autonomous_route_controls (
    route_fingerprint TEXT PRIMARY KEY
        CHECK (length(route_fingerprint) BETWEEN 1 AND 256),
    schema_version TEXT NOT NULL
        DEFAULT 'phoenix.autonomous-route-control.v1'
        CHECK (schema_version = 'phoenix.autonomous-route-control.v1'),
    chain_id BIGINT NOT NULL DEFAULT 42161 CHECK (chain_id = 42161),
    route_policy_hash TEXT NOT NULL
        CHECK (route_policy_hash ~ '^[0-9a-f]{64}$'),
    enabled BOOLEAN NOT NULL DEFAULT false,
    kill_switch BOOLEAN NOT NULL DEFAULT true,
    current_size_level TEXT NOT NULL DEFAULT '0.25x'
        CHECK (
            current_size_level IN (
                '0.25x',
                '0.50x',
                '1.00x',
                '1.25x',
                '1.50x',
                '2.00x'
            )
        ),
    maximum_permitted_size NUMERIC(78,0) NOT NULL
        CHECK (maximum_permitted_size > 0),
    cooldown_until TIMESTAMPTZ,
    control_epoch BIGINT NOT NULL DEFAULT 0 CHECK (control_epoch >= 0),
    disarm_reason TEXT DEFAULT 'not_armed'
        CHECK (
            disarm_reason IS NULL
            OR length(disarm_reason) BETWEEN 1 AND 128
        ),
    control_hash TEXT NOT NULL
        CHECK (control_hash ~ '^[0-9a-f]{64}$'),
    control_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(control_contract) = 'object'
            AND octet_length(control_contract::text) <= 32768
            AND control_contract ?& ARRAY[
                'schema_version',
                'route_fingerprint',
                'route_policy_hash',
                'control_hash'
            ]
            AND control_contract->>'schema_version'
                = 'phoenix.autonomous-route-control.v1'
            AND control_contract->>'route_fingerprint' = route_fingerprint
            AND control_contract->>'route_policy_hash' = route_policy_hash
            AND control_contract->>'control_hash' = control_hash
        ),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (kill_switch AND disarm_reason IS NOT NULL)
        OR (NOT kill_switch AND enabled AND disarm_reason IS NULL)
    )
);

CREATE TABLE IF NOT EXISTS live_canary.autonomous_candidates (
    candidate_id UUID PRIMARY KEY,
    opportunity_id UUID NOT NULL UNIQUE,
    origin_event_id TEXT NOT NULL
        CHECK (length(origin_event_id) BETWEEN 1 AND 256),
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.autonomous-candidate.v1'),
    chain_id BIGINT NOT NULL CHECK (chain_id = 42161),
    route_fingerprint TEXT NOT NULL
        CHECK (length(route_fingerprint) BETWEEN 1 AND 256),
    route_universe_hash TEXT NOT NULL
        CHECK (route_universe_hash ~ '^[0-9a-f]{64}$'),
    route_policy_hash TEXT NOT NULL
        CHECK (route_policy_hash ~ '^[0-9a-f]{64}$'),
    risk_policy_hash TEXT NOT NULL
        CHECK (
            risk_policy_hash ~ '^[0-9a-f]{64}$'
            AND risk_policy_hash = route_policy_hash
        ),
    state_block_number NUMERIC(78,0) NOT NULL
        CHECK (state_block_number > 0),
    state_block_hash TEXT NOT NULL
        CHECK (state_block_hash ~ '^0x[0-9a-f]{64}$'),
    state_hash TEXT NOT NULL
        CHECK (state_hash ~ '^[0-9a-f]{64}$'),
    selected_size NUMERIC(78,0) NOT NULL CHECK (selected_size > 0),
    predicted_gross_profit NUMERIC(78,0) NOT NULL
        CHECK (predicted_gross_profit >= 0),
    predicted_total_cost NUMERIC(78,0) NOT NULL
        CHECK (predicted_total_cost >= 0),
    conservative_predicted_net_pnl NUMERIC(79,0) NOT NULL,
    plan_hash TEXT NOT NULL UNIQUE
        CHECK (plan_hash ~ '^[0-9a-f]{64}$'),
    calldata_hash TEXT NOT NULL
        CHECK (calldata_hash ~ '^[0-9a-f]{64}$'),
    executor_address TEXT NOT NULL
        CHECK (executor_address ~ '^0x[0-9a-f]{40}$'),
    executor_code_hash TEXT NOT NULL
        CHECK (executor_code_hash ~ '^[0-9a-f]{64}$'),
    submission_channel TEXT NOT NULL
        CHECK (submission_channel IN ('standard_rpc', 'disabled_ordering')),
    submission_quote_hash TEXT NOT NULL
        CHECK (submission_quote_hash ~ '^[0-9a-f]{64}$'),
    risk_snapshot_hash TEXT NOT NULL
        CHECK (risk_snapshot_hash ~ '^[0-9a-f]{64}$'),
    risk_snapshot_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(risk_snapshot_contract) = 'object'
            AND octet_length(risk_snapshot_contract::text) <= 131072
            AND risk_snapshot_contract ?& ARRAY[
                'schema_version',
                'risk_snapshot_hash'
            ]
            AND risk_snapshot_contract->>'schema_version'
                = 'phoenix.risk-snapshot.v1'
            AND risk_snapshot_contract->>'risk_snapshot_hash'
                = risk_snapshot_hash
        ),
    submission_quote_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(submission_quote_contract) = 'object'
            AND octet_length(submission_quote_contract::text) <= 32768
            AND submission_quote_contract ?& ARRAY[
                'schema_version',
                'quote_evidence_hash'
            ]
            AND submission_quote_contract->>'schema_version'
                = 'phoenix.submission-quote.v1'
            AND submission_quote_contract->>'quote_evidence_hash'
                = submission_quote_hash
        ),
    candidate_hash TEXT NOT NULL UNIQUE
        CHECK (candidate_hash ~ '^[0-9a-f]{64}$'),
    candidate_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(candidate_contract) = 'object'
            AND octet_length(candidate_contract::text) <= 131072
            AND candidate_contract ?& ARRAY[
                'schema_version',
                'candidate_id',
                'candidate_hash'
            ]
            AND candidate_contract->>'schema_version'
                = 'phoenix.autonomous-candidate.v1'
            AND candidate_contract->>'candidate_id' = candidate_id::text
            AND candidate_contract->>'candidate_hash' = candidate_hash
        ),
    status TEXT NOT NULL DEFAULT 'materialized'
        CHECK (
            status IN (
                'materialized',
                'claimed',
                'revalidated',
                'nonce_reserved',
                'signed',
                'submitted',
                'included',
                'settled',
                'expired',
                'policy_rejected',
                'risk_rejected',
                'state_changed',
                'simulation_mismatch',
                'submission_unknown',
                'replaced',
                'reverted',
                'receipt_timed_out',
                'integrity_failure',
                'operator_killed'
            )
        ),
    candidate_created_at TIMESTAMPTZ NOT NULL,
    candidate_expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (candidate_expires_at > candidate_created_at),
    CHECK (
        conservative_predicted_net_pnl
            <= predicted_gross_profit - predicted_total_cost
    )
);

CREATE TABLE IF NOT EXISTS live_canary.autonomous_approvals (
    candidate_id UUID PRIMARY KEY
        REFERENCES live_canary.autonomous_candidates(candidate_id)
        ON DELETE RESTRICT,
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.automatic-approval.v1'),
    candidate_hash TEXT NOT NULL
        CHECK (candidate_hash ~ '^[0-9a-f]{64}$'),
    route_policy_hash TEXT NOT NULL
        CHECK (route_policy_hash ~ '^[0-9a-f]{64}$'),
    route_universe_hash TEXT NOT NULL
        CHECK (route_universe_hash ~ '^[0-9a-f]{64}$'),
    risk_snapshot_hash TEXT NOT NULL
        CHECK (risk_snapshot_hash ~ '^[0-9a-f]{64}$'),
    submission_quote_hash TEXT NOT NULL
        CHECK (submission_quote_hash ~ '^[0-9a-f]{64}$'),
    state_hash TEXT NOT NULL
        CHECK (state_hash ~ '^[0-9a-f]{64}$'),
    plan_hash TEXT NOT NULL UNIQUE
        CHECK (plan_hash ~ '^[0-9a-f]{64}$'),
    simulation_result_hash TEXT NOT NULL UNIQUE
        CHECK (simulation_result_hash ~ '^[0-9a-f]{64}$'),
    calldata_hash TEXT NOT NULL
        CHECK (calldata_hash ~ '^[0-9a-f]{64}$'),
    executor_address TEXT NOT NULL
        CHECK (executor_address ~ '^0x[0-9a-f]{40}$'),
    executor_code_hash TEXT NOT NULL
        CHECK (executor_code_hash ~ '^[0-9a-f]{64}$'),
    approval_source TEXT NOT NULL
        CHECK (approval_source = 'autonomous_policy'),
    approval_created_at TIMESTAMPTZ NOT NULL,
    approval_expires_at TIMESTAMPTZ NOT NULL,
    automatic_approval_digest TEXT NOT NULL UNIQUE
        CHECK (automatic_approval_digest ~ '^[0-9a-f]{64}$'),
    approval_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(approval_contract) = 'object'
            AND octet_length(approval_contract::text) <= 65536
            AND approval_contract ?& ARRAY[
                'schema_version',
                'candidate_id',
                'candidate_hash',
                'automatic_approval_digest'
            ]
            AND approval_contract->>'schema_version'
                = 'phoenix.automatic-approval.v1'
            AND approval_contract->>'candidate_id' = candidate_id::text
            AND approval_contract->>'candidate_hash' = candidate_hash
            AND approval_contract->>'automatic_approval_digest'
                = automatic_approval_digest
        ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (approval_expires_at > approval_created_at)
);

CREATE TABLE IF NOT EXISTS live_canary.autonomous_outcome_attributions (
    candidate_id UUID PRIMARY KEY
        REFERENCES live_canary.autonomous_candidates(candidate_id)
        ON DELETE RESTRICT,
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.outcome.v1'),
    outcome_class TEXT NOT NULL
        CHECK (
            outcome_class IN (
                'confirmed_profitable',
                'confirmed_below_prediction',
                'confirmed_negative',
                'reverted',
                'not_included',
                'transaction_replaced',
                'receipt_timed_out',
                'submission_unknown',
                'submitted_too_late',
                'competitor_or_state_changed',
                'ordering_bid_too_low',
                'rpc_failure',
                'model_mismatch',
                'policy_rejected',
                'risk_rejected',
                'integrity_failure',
                'operator_killed'
            )
        ),
    transaction_hash TEXT UNIQUE
        CHECK (
            transaction_hash IS NULL
            OR transaction_hash ~ '^0x[0-9a-f]{64}$'
        ),
    block_number NUMERIC(78,0) CHECK (block_number IS NULL OR block_number > 0),
    receipt_status SMALLINT CHECK (receipt_status IS NULL OR receipt_status IN (0, 1)),
    predicted_gross_profit NUMERIC(78,0) NOT NULL
        CHECK (predicted_gross_profit >= 0),
    predicted_total_cost NUMERIC(78,0) NOT NULL
        CHECK (predicted_total_cost >= 0),
    conservative_predicted_net_pnl NUMERIC(79,0) NOT NULL,
    realized_gross_profit NUMERIC(79,0) NOT NULL,
    actual_gas_cost NUMERIC(78,0) NOT NULL CHECK (actual_gas_cost >= 0),
    actual_ordering_cost NUMERIC(78,0) NOT NULL
        CHECK (actual_ordering_cost >= 0),
    realized_chain_net_pnl NUMERIC(79,0) NOT NULL,
    allocated_infrastructure_cost NUMERIC(78,0) NOT NULL
        CHECK (allocated_infrastructure_cost >= 0),
    realized_business_net_pnl NUMERIC(79,0) NOT NULL,
    terminal_at TIMESTAMPTZ NOT NULL,
    attributed_at TIMESTAMPTZ NOT NULL,
    outcome_hash TEXT NOT NULL UNIQUE
        CHECK (outcome_hash ~ '^[0-9a-f]{64}$'),
    outcome_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(outcome_contract) = 'object'
            AND octet_length(outcome_contract::text) <= 131072
            AND outcome_contract ?& ARRAY[
                'schema_version',
                'candidate_id',
                'outcome_hash'
            ]
            AND outcome_contract->>'schema_version' = 'phoenix.outcome.v1'
            AND outcome_contract->>'candidate_id' = candidate_id::text
            AND outcome_contract->>'outcome_hash' = outcome_hash
        ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (transaction_hash IS NULL AND block_number IS NULL AND receipt_status IS NULL)
        OR (
            transaction_hash IS NOT NULL
            AND block_number IS NOT NULL
            AND receipt_status IS NOT NULL
        )
    ),
    CHECK (
        realized_chain_net_pnl
            = realized_gross_profit - actual_gas_cost - actual_ordering_cost
    ),
    CHECK (
        realized_business_net_pnl
            = realized_chain_net_pnl - allocated_infrastructure_cost
    ),
    CHECK (attributed_at >= terminal_at)
);

CREATE INDEX IF NOT EXISTS live_canary_autonomous_materialized_candidates
    ON live_canary.autonomous_candidates(candidate_created_at, candidate_id)
    WHERE status = 'materialized';

CREATE INDEX IF NOT EXISTS live_canary_autonomous_route_candidates
    ON live_canary.autonomous_candidates(route_fingerprint, candidate_created_at);

CREATE INDEX IF NOT EXISTS live_canary_autonomous_outcome_time
    ON live_canary.autonomous_outcome_attributions(attributed_at, outcome_class);

COMMIT;
