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
            'phoenix.live-canary-schema.v7'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v7')
ON CONFLICT (version) DO NOTHING;

ALTER TABLE live_canary.revenue_hunting_signals
    ADD COLUMN IF NOT EXISTS exact_diagnostics JSONB;

ALTER TABLE live_canary.revenue_hunting_signals
    DROP CONSTRAINT IF EXISTS revenue_hunting_signals_evidence_mode_check;

ALTER TABLE live_canary.revenue_hunting_signals
    ADD CONSTRAINT revenue_hunting_signals_evidence_mode_check CHECK (
        evidence_mode IS NULL OR evidence_mode IN (
            'EIP1186_VERIFIED',
            'DUAL_PROVIDER_FORK_VERIFIED',
            'DUAL_PROVIDER_COUNTERFACTUAL_FORK_VERIFIED'
        )
    ) NOT VALID;

ALTER TABLE live_canary.revenue_hunting_signals
    VALIDATE CONSTRAINT revenue_hunting_signals_evidence_mode_check;

ALTER TABLE live_canary.revenue_hunting_signals
    DROP CONSTRAINT IF EXISTS revenue_hunting_signals_exact_diagnostics_check;

ALTER TABLE live_canary.revenue_hunting_signals
    ADD CONSTRAINT revenue_hunting_signals_exact_diagnostics_check CHECK (
        exact_diagnostics IS NULL
        OR (
            source_lane = 'aave_liquidation'
            AND jsonb_typeof(exact_diagnostics) = 'object'
            AND exact_diagnostics @> '{"schema":"phoenix.aave-exact-diagnostics.v1"}'::jsonb
            AND exact_diagnostics ? 'rejection_counts'
            AND jsonb_typeof(exact_diagnostics->'rejection_counts') = 'object'
            AND (
                NOT (exact_diagnostics ? 'top_diagnostics')
                OR (
                    jsonb_typeof(exact_diagnostics->'top_diagnostics') = 'array'
                    AND jsonb_array_length(exact_diagnostics->'top_diagnostics') <= 3
                )
            )
            AND octet_length(exact_diagnostics::text) <= 65536
        )
    ) NOT VALID;

ALTER TABLE live_canary.revenue_hunting_signals
    VALIDATE CONSTRAINT revenue_hunting_signals_exact_diagnostics_check;

ALTER TABLE live_canary.atlas_auction_ingress
    ADD COLUMN IF NOT EXISTS rejection_reason TEXT;

ALTER TABLE live_canary.atlas_auction_ingress
    DROP CONSTRAINT IF EXISTS atlas_auction_ingress_rejection_reason_check;

ALTER TABLE live_canary.atlas_auction_ingress
    ADD CONSTRAINT atlas_auction_ingress_rejection_reason_check CHECK (
        rejection_reason IS NULL OR length(rejection_reason) BETWEEN 1 AND 128
    ) NOT VALID;

ALTER TABLE live_canary.atlas_auction_ingress
    VALIDATE CONSTRAINT atlas_auction_ingress_rejection_reason_check;

CREATE INDEX IF NOT EXISTS live_canary_revenue_signal_source_observed
ON live_canary.revenue_hunting_signals(source_lane, observed_at);

CREATE INDEX IF NOT EXISTS live_canary_atlas_ingress_observed
ON live_canary.atlas_auction_ingress(observed_at);

CREATE INDEX IF NOT EXISTS live_canary_atlas_solver_request_created
ON live_canary.atlas_solver_requests(created_at);

CREATE INDEX IF NOT EXISTS live_canary_execution_request_route_created
ON live_canary.execution_requests(route_type, created_at);

COMMIT;
