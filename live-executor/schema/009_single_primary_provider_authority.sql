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
            'phoenix.live-canary-schema.v9'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v9')
ON CONFLICT (version) DO NOTHING;

-- V8 required a second provider for every recovery sample. Remove only those
-- generated CHECK constraints; the timestamp/count and canonical-ID checks
-- remain authoritative.
DO $$
DECLARE
    constraint_row RECORD;
BEGIN
    FOR constraint_row IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'live_canary.revenue_provider_authority'::regclass
          AND contype = 'c'
          AND (
              pg_get_constraintdef(oid) LIKE '%sample_1_confirmation_provider IS NOT NULL%'
              OR pg_get_constraintdef(oid) LIKE '%sample_2_confirmation_provider IS NOT NULL%'
              OR pg_get_constraintdef(oid) LIKE '%sample_3_confirmation_provider IS NOT NULL%'
              OR pg_get_constraintdef(oid) LIKE '%sample_1_primary_provider <> sample_1_confirmation_provider%'
              OR pg_get_constraintdef(oid) LIKE '%sample_2_primary_provider <> sample_2_confirmation_provider%'
              OR pg_get_constraintdef(oid) LIKE '%sample_3_primary_provider <> sample_3_confirmation_provider%'
          )
    LOOP
        EXECUTE format(
            'ALTER TABLE live_canary.revenue_provider_authority DROP CONSTRAINT %I',
            constraint_row.conname
        );
    END LOOP;
END $$;

UPDATE live_canary.revenue_provider_authority
SET exact_execution_ready = false,
    gate_reason = 'single_primary_migration',
    gate_updated_at = now(),
    request_evidence_not_before = now(),
    recovery_status = 'collecting',
    sample_count = 0,
    sample_1_at = NULL,
    sample_1_primary_provider = NULL,
    sample_1_confirmation_provider = NULL,
    sample_2_at = NULL,
    sample_2_primary_provider = NULL,
    sample_2_confirmation_provider = NULL,
    sample_3_at = NULL,
    sample_3_primary_provider = NULL,
    sample_3_confirmation_provider = NULL,
    updated_at = now()
WHERE singleton;

ALTER TABLE live_canary.revenue_provider_authority
    DROP CONSTRAINT IF EXISTS revenue_provider_authority_single_primary_samples_check;

ALTER TABLE live_canary.revenue_provider_authority
    ADD CONSTRAINT revenue_provider_authority_single_primary_samples_check CHECK (
        sample_1_confirmation_provider IS NULL
        AND sample_2_confirmation_provider IS NULL
        AND sample_3_confirmation_provider IS NULL
        AND (sample_count < 1 OR (
            sample_1_primary_provider = 'production-nownodes-arbitrum'
        )) IS TRUE
        AND (sample_count < 2 OR (
            sample_2_primary_provider = 'production-nownodes-arbitrum'
        )) IS TRUE
        AND (sample_count < 3 OR (
            sample_3_primary_provider = 'production-nownodes-arbitrum'
        )) IS TRUE
    );

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
                'EIP1186_VERIFIED',
                'DUAL_PROVIDER_FORK_VERIFIED',
                'SINGLE_PRIMARY_FORK_VERIFIED'
            )
        )
    ) NOT VALID;

ALTER TABLE live_canary.execution_requests
    VALIDATE CONSTRAINT execution_requests_revenue_route_check;

ALTER TABLE live_canary.revenue_hunting_signals
    DROP CONSTRAINT IF EXISTS revenue_hunting_signals_evidence_mode_check;

ALTER TABLE live_canary.revenue_hunting_signals
    ADD CONSTRAINT revenue_hunting_signals_evidence_mode_check CHECK (
        evidence_mode IS NULL OR evidence_mode IN (
            'EIP1186_VERIFIED',
            'DUAL_PROVIDER_FORK_VERIFIED',
            'DUAL_PROVIDER_COUNTERFACTUAL_FORK_VERIFIED',
            'SINGLE_PRIMARY_FORK_VERIFIED',
            'SINGLE_PRIMARY_COUNTERFACTUAL_FORK_VERIFIED',
            'SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED'
        )
    ) NOT VALID;

ALTER TABLE live_canary.revenue_hunting_signals
    VALIDATE CONSTRAINT revenue_hunting_signals_evidence_mode_check;

COMMIT;
