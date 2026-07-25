BEGIN;

ALTER TABLE live_canary.schema_contract
    DROP CONSTRAINT IF EXISTS schema_contract_version_check;

ALTER TABLE live_canary.schema_contract
    ADD CONSTRAINT schema_contract_version_check CHECK (
        version IN (
            'phoenix.live-canary-schema.v1',
            'phoenix.live-canary-schema.v2',
            'phoenix.live-canary-schema.v3',
            'phoenix.live-canary-schema.v4'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v4')
ON CONFLICT (version) DO NOTHING;

ALTER TABLE live_canary.autonomous_candidates
    ADD COLUMN IF NOT EXISTS plan_contract JSONB,
    ADD COLUMN IF NOT EXISTS calldata_hex TEXT,
    ADD COLUMN IF NOT EXISTS state_contract JSONB,
    ADD COLUMN IF NOT EXISTS approval_deadline TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS execution_request_id UUID;

ALTER TABLE live_canary.autonomous_candidates
    ALTER COLUMN submission_quote_hash DROP NOT NULL,
    ALTER COLUMN risk_snapshot_hash DROP NOT NULL,
    ALTER COLUMN risk_snapshot_contract DROP NOT NULL,
    ALTER COLUMN submission_quote_contract DROP NOT NULL;

ALTER TABLE live_canary.autonomous_route_controls
    ALTER COLUMN control_hash DROP NOT NULL,
    ALTER COLUMN control_contract DROP NOT NULL;

ALTER TABLE live_canary.autonomous_candidates
    DROP CONSTRAINT IF EXISTS autonomous_candidates_plan_contract_check,
    ADD CONSTRAINT autonomous_candidates_plan_contract_check CHECK (
        plan_contract IS NULL
        OR (
            jsonb_typeof(plan_contract) = 'object'
            AND octet_length(plan_contract::text) <= 131072
            AND plan_contract ?& ARRAY[
                'schema_version',
                'route_fingerprint',
                'calldata_hash'
            ]
            AND plan_contract->>'route_fingerprint' = route_fingerprint
            AND plan_contract->>'calldata_hash' = calldata_hash
        )
    ),
    DROP CONSTRAINT IF EXISTS autonomous_candidates_calldata_hex_check,
    ADD CONSTRAINT autonomous_candidates_calldata_hex_check CHECK (
        calldata_hex IS NULL
        OR (
            calldata_hex ~ '^0x[0-9a-f]+$'
            AND mod(length(calldata_hex) - 2, 2) = 0
            AND octet_length(calldata_hex) <= 131074
        )
    ),
    DROP CONSTRAINT IF EXISTS autonomous_candidates_state_contract_check,
    ADD CONSTRAINT autonomous_candidates_state_contract_check CHECK (
        state_contract IS NULL
        OR (
            jsonb_typeof(state_contract) = 'object'
            AND octet_length(state_contract::text) <= 524288
        )
    );

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_autonomous_event_block_route
    ON live_canary.autonomous_candidates(
        origin_event_id,
        state_block_number,
        state_block_hash,
        route_fingerprint
    );

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_autonomous_execution_request
    ON live_canary.autonomous_candidates(execution_request_id)
    WHERE execution_request_id IS NOT NULL;

ALTER TABLE live_canary.autonomous_candidates
    DROP CONSTRAINT IF EXISTS autonomous_candidates_status_check;

ALTER TABLE live_canary.autonomous_candidates
    ADD CONSTRAINT autonomous_candidates_status_check CHECK (
        status IN (
            'materialized',
            'approval_pending',
            'approved',
            'request_materialized',
            'claimed',
            'signed',
            'submitted',
            'confirmed_profitable',
            'confirmed_unprofitable',
            'rejected_policy',
            'rejected_state',
            'rejected_economics',
            'expired',
            'submission_failed_known',
            'submission_unknown',
            'reverted',
            'disarmed',
            'integrity_failure',
            'revalidated',
            'nonce_reserved',
            'included',
            'settled',
            'policy_rejected',
            'risk_rejected',
            'state_changed',
            'simulation_mismatch',
            'replaced',
            'receipt_timed_out',
            'operator_killed'
        )
    );

CREATE OR REPLACE FUNCTION live_canary.enforce_autonomous_candidate_transition()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.status = OLD.status THEN
        RETURN NEW;
    END IF;
    IF (OLD.status, NEW.status) IN (
        ('materialized', 'approval_pending'),
        ('materialized', 'rejected_state'),
        ('materialized', 'rejected_economics'),
        ('materialized', 'expired'),
        ('materialized', 'disarmed'),
        ('materialized', 'integrity_failure'),
        ('approval_pending', 'approved'),
        ('approval_pending', 'rejected_policy'),
        ('approval_pending', 'rejected_state'),
        ('approval_pending', 'rejected_economics'),
        ('approval_pending', 'expired'),
        ('approval_pending', 'disarmed'),
        ('approval_pending', 'integrity_failure'),
        ('approved', 'request_materialized'),
        ('approved', 'expired'),
        ('approved', 'disarmed'),
        ('approved', 'integrity_failure'),
        ('request_materialized', 'claimed'),
        ('request_materialized', 'expired'),
        ('request_materialized', 'disarmed'),
        ('claimed', 'signed'),
        ('claimed', 'submission_failed_known'),
        ('claimed', 'submission_unknown'),
        ('claimed', 'disarmed'),
        ('signed', 'submitted'),
        ('signed', 'submission_failed_known'),
        ('signed', 'submission_unknown'),
        ('signed', 'disarmed'),
        ('submitted', 'confirmed_profitable'),
        ('submitted', 'confirmed_unprofitable'),
        ('submitted', 'reverted'),
        ('submitted', 'submission_unknown'),
        ('submitted', 'disarmed')
    ) THEN
        NEW.updated_at = now();
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'invalid autonomous candidate transition';
END;
$$;

DROP TRIGGER IF EXISTS autonomous_candidate_transition
    ON live_canary.autonomous_candidates;

CREATE TRIGGER autonomous_candidate_transition
BEFORE UPDATE OF status ON live_canary.autonomous_candidates
FOR EACH ROW
EXECUTE FUNCTION live_canary.enforce_autonomous_candidate_transition();

ALTER TABLE live_canary.execution_requests
    ADD COLUMN IF NOT EXISTS candidate_id UUID,
    ADD COLUMN IF NOT EXISTS candidate_hash TEXT,
    ADD COLUMN IF NOT EXISTS automatic_approval_digest TEXT,
    ADD COLUMN IF NOT EXISTS state_hash TEXT,
    ADD COLUMN IF NOT EXISTS submission_quote_contract JSONB;

ALTER TABLE live_canary.execution_requests
    DROP CONSTRAINT IF EXISTS execution_requests_candidate_hash_check,
    ADD CONSTRAINT execution_requests_candidate_hash_check CHECK (
        candidate_hash IS NULL OR candidate_hash ~ '^[0-9a-f]{64}$'
    ),
    DROP CONSTRAINT IF EXISTS execution_requests_automatic_approval_digest_check,
    ADD CONSTRAINT execution_requests_automatic_approval_digest_check CHECK (
        automatic_approval_digest IS NULL
        OR automatic_approval_digest ~ '^[0-9a-f]{64}$'
    ),
    DROP CONSTRAINT IF EXISTS execution_requests_state_hash_check,
    ADD CONSTRAINT execution_requests_state_hash_check CHECK (
        state_hash IS NULL OR state_hash ~ '^[0-9a-f]{64}$'
    );

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_execution_request_candidate
    ON live_canary.execution_requests(candidate_id)
    WHERE candidate_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_execution_request_automatic_approval
    ON live_canary.execution_requests(automatic_approval_digest)
    WHERE automatic_approval_digest IS NOT NULL;

ALTER TABLE live_canary.autonomous_candidates
    DROP CONSTRAINT IF EXISTS autonomous_candidates_execution_request_id_fkey,
    ADD CONSTRAINT autonomous_candidates_execution_request_id_fkey
        FOREIGN KEY (execution_request_id)
        REFERENCES live_canary.execution_requests(id)
        ON DELETE RESTRICT;

ALTER TABLE live_canary.execution_outcomes
    ADD COLUMN IF NOT EXISTS l1_cost_wei NUMERIC(39,0) NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS ordering_cost_wei NUMERIC(39,0) NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS allocated_infrastructure_cost_wei NUMERIC(39,0)
        NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS submitted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS submission_channel TEXT NOT NULL DEFAULT 'standard_rpc',
    ADD COLUMN IF NOT EXISTS failure_reason TEXT;

ALTER TABLE live_canary.autonomous_outcome_attributions
    ADD COLUMN IF NOT EXISTS nonce NUMERIC(20,0),
    ADD COLUMN IF NOT EXISTS submission_channel TEXT,
    ADD COLUMN IF NOT EXISTS submitted_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS gas_used NUMERIC(20,0),
    ADD COLUMN IF NOT EXISTS effective_gas_price NUMERIC(39,0),
    ADD COLUMN IF NOT EXISTS actual_l1_cost NUMERIC(78,0) NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS actual_flash_premium NUMERIC(78,0) NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS prediction_error NUMERIC(79,0),
    ADD COLUMN IF NOT EXISTS failure_reason TEXT;

DO $$
DECLARE
    constraint_name TEXT;
BEGIN
    FOR constraint_name IN
        SELECT con.conname
        FROM pg_constraint con
        JOIN pg_class rel ON rel.oid = con.conrelid
        JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace
        WHERE nsp.nspname = 'live_canary'
          AND rel.relname = 'autonomous_outcome_attributions'
          AND con.contype = 'c'
          AND pg_get_constraintdef(con.oid) LIKE '%realized_chain_net_pnl%'
    LOOP
        EXECUTE format(
            'ALTER TABLE live_canary.autonomous_outcome_attributions DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;
END;
$$;

ALTER TABLE live_canary.autonomous_outcome_attributions
    ADD CONSTRAINT autonomous_outcome_chain_pnl_v4 CHECK (
        realized_chain_net_pnl
            = realized_gross_profit
            - actual_gas_cost
            - actual_l1_cost
            - actual_ordering_cost
    ),
    ADD CONSTRAINT autonomous_outcome_business_pnl_v4 CHECK (
        realized_business_net_pnl
            = realized_chain_net_pnl - allocated_infrastructure_cost
    );

CREATE INDEX IF NOT EXISTS live_canary_autonomous_approval_queue
    ON live_canary.autonomous_candidates(candidate_created_at, candidate_id)
    WHERE status IN ('materialized', 'approval_pending');

COMMIT;
