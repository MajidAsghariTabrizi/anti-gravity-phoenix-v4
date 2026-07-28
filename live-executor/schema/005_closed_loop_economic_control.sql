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
            'phoenix.live-canary-schema.v5'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v5')
ON CONFLICT (version) DO NOTHING;

ALTER TABLE live_canary.autonomous_candidates
    ADD COLUMN IF NOT EXISTS rejection_reason TEXT;

ALTER TABLE live_canary.autonomous_candidates
    DROP CONSTRAINT IF EXISTS autonomous_candidates_rejection_reason_check,
    ADD CONSTRAINT autonomous_candidates_rejection_reason_check CHECK (
        rejection_reason IS NULL
        OR (
            length(rejection_reason) BETWEEN 1 AND 128
            AND rejection_reason ~ '^[a-z0-9_]+$'
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
        ('approval_pending', 'materialized'),
        ('approval_pending', 'approved'),
        ('approval_pending', 'rejected_policy'),
        ('approval_pending', 'rejected_state'),
        ('approval_pending', 'rejected_economics'),
        ('approval_pending', 'simulation_mismatch'),
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
          AND rel.relname = 'autonomous_route_controls'
          AND con.contype = 'c'
          AND pg_get_constraintdef(con.oid) LIKE '%current_size_level%'
    LOOP
        EXECUTE format(
            'ALTER TABLE live_canary.autonomous_route_controls DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;
END;
$$;

UPDATE live_canary.autonomous_route_controls
SET current_size_level = CASE current_size_level
    WHEN '0.25x' THEN 'MIN'
    WHEN '0.50x' THEN 'L1'
    WHEN '1.00x' THEN 'L2'
    WHEN '1.25x' THEN 'L3'
    WHEN '1.50x' THEN 'L4'
    WHEN '2.00x' THEN 'L5'
    ELSE current_size_level
END;

ALTER TABLE live_canary.autonomous_route_controls
    ALTER COLUMN current_size_level SET DEFAULT 'MIN',
    ADD CONSTRAINT autonomous_route_controls_size_level_v5 CHECK (
        current_size_level IN (
            'MIN',
            'L1',
            'L2',
            'L3',
            'L4',
            'L5',
            'MAX_REVIEWED'
        )
    );

CREATE TABLE IF NOT EXISTS live_canary.economic_control (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    schema_version TEXT NOT NULL
        DEFAULT 'phoenix.economic-control.v1'
        CHECK (schema_version = 'phoenix.economic-control.v1'),
    phase TEXT NOT NULL DEFAULT 'DISARMED_DEPLOY'
        CHECK (
            phase IN (
                'DISARMED_DEPLOY',
                'DISARMED_EVIDENCE',
                'CANARY_READY',
                'LIVE_CANARY_MIN',
                'LIVE_SCALE_L1',
                'LIVE_SCALE_L2',
                'LIVE_SCALE_L3',
                'LIVE_SCALE_L4',
                'LIVE_SCALE_L5',
                'LIVE_MAX_REVIEWED',
                'COOLDOWN',
                'DISARMED_FAILURE'
            )
        ),
    route_fingerprint TEXT
        CHECK (
            route_fingerprint IS NULL
            OR length(route_fingerprint) BETWEEN 1 AND 256
        ),
    current_size_level TEXT NOT NULL DEFAULT 'MIN'
        CHECK (
            current_size_level IN (
                'MIN',
                'L1',
                'L2',
                'L3',
                'L4',
                'L5',
                'MAX_REVIEWED'
            )
        ),
    current_input_wei NUMERIC(78,0) NOT NULL DEFAULT 100000000000000
        CHECK (
            (current_size_level = 'MIN' AND current_input_wei = 100000000000000)
            OR (current_size_level = 'L1' AND current_input_wei = 250000000000000)
            OR (current_size_level = 'L2' AND current_input_wei = 500000000000000)
            OR (current_size_level = 'L3' AND current_input_wei = 1000000000000000)
            OR (current_size_level = 'L4' AND current_input_wei = 2500000000000000)
            OR (current_size_level = 'L5' AND current_input_wei = 5000000000000000)
            OR (
                current_size_level = 'MAX_REVIEWED'
                AND current_input_wei = 10000000000000000
            )
        ),
    maximum_reviewed_input_wei NUMERIC(78,0) NOT NULL
        DEFAULT 10000000000000000
        CHECK (maximum_reviewed_input_wei = 10000000000000000),
    release_sha TEXT CHECK (release_sha IS NULL OR release_sha ~ '^[0-9a-f]{40}$'),
    engine_image_digest TEXT
        CHECK (
            engine_image_digest IS NULL
            OR engine_image_digest ~ '^sha256:[0-9a-f]{64}$'
        ),
    route_universe_hash TEXT
        CHECK (
            route_universe_hash IS NULL
            OR route_universe_hash ~ '^[0-9a-f]{64}$'
        ),
    route_policy_hash TEXT
        CHECK (
            route_policy_hash IS NULL
            OR route_policy_hash ~ '^[0-9a-f]{64}$'
        ),
    risk_policy_hash TEXT
        CHECK (
            risk_policy_hash IS NULL
            OR risk_policy_hash ~ '^[0-9a-f]{64}$'
        ),
    executor_code_hash TEXT
        CHECK (
            executor_code_hash IS NULL
            OR executor_code_hash ~ '^[0-9a-f]{64}$'
        ),
    readiness_id UUID,
    authorization_id UUID,
    cooldown_until TIMESTAMPTZ,
    gas_reserve_wei NUMERIC(78,0) NOT NULL DEFAULT 0 CHECK (gas_reserve_wei >= 0),
    gas_reserve_floor_wei NUMERIC(78,0) NOT NULL DEFAULT 0
        CHECK (gas_reserve_floor_wei >= 0),
    control_epoch BIGINT NOT NULL DEFAULT 0 CHECK (control_epoch >= 0),
    last_transition_reason TEXT NOT NULL DEFAULT 'initial_migration'
        CHECK (length(last_transition_reason) BETWEEN 1 AND 128),
    state_hash TEXT CHECK (state_hash IS NULL OR state_hash ~ '^[0-9a-f]{64}$'),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        phase NOT LIKE 'LIVE_%'
        OR (
            release_sha IS NOT NULL
            AND engine_image_digest IS NOT NULL
            AND route_fingerprint IS NOT NULL
            AND route_policy_hash IS NOT NULL
            AND executor_code_hash IS NOT NULL
            AND readiness_id IS NOT NULL
            AND authorization_id IS NOT NULL
            AND cooldown_until IS NULL
            AND gas_reserve_wei > gas_reserve_floor_wei
        )
    ),
    CHECK (
        phase <> 'COOLDOWN'
        OR cooldown_until IS NOT NULL
    )
);

INSERT INTO live_canary.economic_control(singleton)
VALUES (true)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE IF NOT EXISTS live_canary.canary_readiness_records (
    readiness_id UUID PRIMARY KEY,
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.canary-readiness.v1'),
    release_sha TEXT NOT NULL CHECK (release_sha ~ '^[0-9a-f]{40}$'),
    engine_image_digest TEXT NOT NULL
        CHECK (engine_image_digest ~ '^sha256:[0-9a-f]{64}$'),
    route_fingerprint TEXT NOT NULL
        CHECK (length(route_fingerprint) BETWEEN 1 AND 256),
    route_universe_hash TEXT NOT NULL
        CHECK (route_universe_hash ~ '^[0-9a-f]{64}$'),
    route_policy_hash TEXT NOT NULL
        CHECK (route_policy_hash ~ '^[0-9a-f]{64}$'),
    risk_policy_hash TEXT NOT NULL
        CHECK (risk_policy_hash ~ '^[0-9a-f]{64}$'),
    global_control_epoch BIGINT NOT NULL CHECK (global_control_epoch >= 0),
    route_control_epoch BIGINT NOT NULL CHECK (route_control_epoch >= 0),
    executor_code_hash TEXT NOT NULL
        CHECK (executor_code_hash ~ '^[0-9a-f]{64}$'),
    contract_identity_hash TEXT NOT NULL
        CHECK (contract_identity_hash ~ '^[0-9a-f]{64}$'),
    wallet_gas_reserve_wei NUMERIC(78,0) NOT NULL
        CHECK (wallet_gas_reserve_wei > 0),
    gas_reserve_floor_wei NUMERIC(78,0) NOT NULL
        CHECK (
            gas_reserve_floor_wei >= 0
            AND wallet_gas_reserve_wei > gas_reserve_floor_wei
        ),
    current_daily_loss_wei NUMERIC(78,0) NOT NULL
        CHECK (current_daily_loss_wei >= 0),
    daily_loss_limit_wei NUMERIC(78,0) NOT NULL
        CHECK (
            daily_loss_limit_wei > 0
            AND current_daily_loss_wei < daily_loss_limit_wei
        ),
    observed_from TIMESTAMPTZ NOT NULL,
    observed_until TIMESTAMPTZ NOT NULL,
    candidate_evidence_hashes JSONB NOT NULL
        CHECK (
            jsonb_typeof(candidate_evidence_hashes) = 'array'
            AND jsonb_array_length(candidate_evidence_hashes) > 0
            AND jsonb_array_length(candidate_evidence_hashes) <= 100
        ),
    evidence_metrics JSONB NOT NULL
        CHECK (
            jsonb_typeof(evidence_metrics) = 'object'
            AND octet_length(evidence_metrics::text) <= 131072
        ),
    readiness_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(readiness_contract) = 'object'
            AND octet_length(readiness_contract::text) <= 262144
        ),
    readiness_hash TEXT NOT NULL UNIQUE
        CHECK (readiness_hash ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK (observed_from < observed_until),
    CHECK (observed_until <= created_at),
    CHECK (expires_at > created_at),
    CHECK (expires_at <= created_at + interval '10 minutes')
);

CREATE TABLE IF NOT EXISTS live_canary.automation_authorizations (
    authorization_id UUID PRIMARY KEY,
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.automation-authorization.v1'),
    route_fingerprint TEXT NOT NULL
        CHECK (length(route_fingerprint) BETWEEN 1 AND 256),
    route_policy_hash TEXT NOT NULL
        CHECK (route_policy_hash ~ '^[0-9a-f]{64}$'),
    maximum_reviewed_input_wei NUMERIC(78,0) NOT NULL
        CHECK (maximum_reviewed_input_wei = 10000000000000000),
    executor_code_hash TEXT NOT NULL
        CHECK (executor_code_hash ~ '^[0-9a-f]{64}$'),
    release_family TEXT NOT NULL CHECK (length(release_family) BETWEEN 1 AND 64),
    one_transaction_at_a_time BOOLEAN NOT NULL CHECK (one_transaction_at_a_time),
    reviewed_ladder_only BOOLEAN NOT NULL CHECK (reviewed_ladder_only),
    automatic_disarm_required BOOLEAN NOT NULL CHECK (automatic_disarm_required),
    authorization_contract JSONB NOT NULL
        CHECK (
            jsonb_typeof(authorization_contract) = 'object'
            AND octet_length(authorization_contract::text) <= 131072
        ),
    authorization_hash TEXT NOT NULL UNIQUE
        CHECK (authorization_hash ~ '^[0-9a-f]{64}$'),
    authorized_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    CHECK (expires_at > authorized_at),
    CHECK (consumed_at IS NULL OR consumed_at >= authorized_at)
);

ALTER TABLE live_canary.economic_control
    DROP CONSTRAINT IF EXISTS economic_control_readiness_id_fkey,
    ADD CONSTRAINT economic_control_readiness_id_fkey
        FOREIGN KEY (readiness_id)
        REFERENCES live_canary.canary_readiness_records(readiness_id)
        ON DELETE RESTRICT,
    DROP CONSTRAINT IF EXISTS economic_control_authorization_id_fkey,
    ADD CONSTRAINT economic_control_authorization_id_fkey
        FOREIGN KEY (authorization_id)
        REFERENCES live_canary.automation_authorizations(authorization_id)
        ON DELETE RESTRICT;

CREATE TABLE IF NOT EXISTS live_canary.economic_transitions (
    transition_id UUID PRIMARY KEY,
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.economic-transition.v1'),
    from_phase TEXT NOT NULL,
    to_phase TEXT NOT NULL,
    from_size_level TEXT NOT NULL,
    to_size_level TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 128),
    evidence_hash TEXT CHECK (evidence_hash IS NULL OR evidence_hash ~ '^[0-9a-f]{64}$'),
    release_sha TEXT CHECK (release_sha IS NULL OR release_sha ~ '^[0-9a-f]{40}$'),
    control_epoch BIGINT NOT NULL CHECK (control_epoch >= 0),
    transition_hash TEXT NOT NULL UNIQUE
        CHECK (transition_hash ~ '^[0-9a-f]{64}$'),
    transitioned_at TIMESTAMPTZ NOT NULL
);

CREATE OR REPLACE FUNCTION live_canary.reject_economic_transition_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'economic transitions are immutable';
END;
$$;

DROP TRIGGER IF EXISTS economic_transition_immutable
    ON live_canary.economic_transitions;

CREATE TRIGGER economic_transition_immutable
BEFORE UPDATE OR DELETE ON live_canary.economic_transitions
FOR EACH ROW
EXECUTE FUNCTION live_canary.reject_economic_transition_mutation();

ALTER TABLE live_canary.autonomous_outcome_attributions
    ADD COLUMN IF NOT EXISTS input_size_level TEXT,
    ADD COLUMN IF NOT EXISTS actual_output NUMERIC(78,0),
    ADD COLUMN IF NOT EXISTS actual_balance_delta NUMERIC(79,0),
    ADD COLUMN IF NOT EXISTS fork_simulated_net_pnl NUMERIC(79,0),
    ADD COLUMN IF NOT EXISTS predicted_net_pnl NUMERIC(79,0),
    ADD COLUMN IF NOT EXISTS detection_to_submission_latency_ms BIGINT,
    ADD COLUMN IF NOT EXISTS receipt_latency_ms BIGINT;

ALTER TABLE live_canary.autonomous_outcome_attributions
    DROP CONSTRAINT IF EXISTS autonomous_outcome_input_size_level_v5,
    ADD CONSTRAINT autonomous_outcome_input_size_level_v5 CHECK (
        input_size_level IS NULL
        OR input_size_level IN (
            'MIN',
            'L1',
            'L2',
            'L3',
            'L4',
            'L5',
            'MAX_REVIEWED'
        )
    ),
    DROP CONSTRAINT IF EXISTS autonomous_outcome_latency_v5,
    ADD CONSTRAINT autonomous_outcome_latency_v5 CHECK (
        (detection_to_submission_latency_ms IS NULL OR detection_to_submission_latency_ms >= 0)
        AND (receipt_latency_ms IS NULL OR receipt_latency_ms >= 0)
    );

CREATE OR REPLACE VIEW live_canary.realized_profit_by_route_level AS
SELECT
    candidate.route_fingerprint,
    outcome.input_size_level,
    count(*) AS reconciled_outcomes,
    count(*) FILTER (
        WHERE outcome.outcome_class = 'confirmed_profitable'
    ) AS successful_outcomes,
    coalesce(sum(outcome.realized_gross_profit), 0)::numeric(79,0)
        AS realized_gross_profit,
    coalesce(sum(outcome.actual_gas_cost + outcome.actual_l1_cost), 0)::numeric(78,0)
        AS gas_cost,
    coalesce(sum(outcome.actual_flash_premium), 0)::numeric(78,0)
        AS flash_fees,
    coalesce(sum(outcome.realized_business_net_pnl), 0)::numeric(79,0)
        AS realized_net_pnl,
    coalesce(sum(
        CASE
            WHEN outcome.realized_business_net_pnl < 0
            THEN -outcome.realized_business_net_pnl
            ELSE 0
        END
    ), 0)::numeric(79,0) AS realized_losses,
    min(outcome.attributed_at) AS first_outcome_at,
    max(outcome.attributed_at) AS last_outcome_at
FROM live_canary.autonomous_outcome_attributions outcome
JOIN live_canary.autonomous_candidates candidate
  ON candidate.candidate_id = outcome.candidate_id
GROUP BY candidate.route_fingerprint, outcome.input_size_level;

CREATE OR REPLACE VIEW live_canary.realized_profit_windows AS
SELECT
    coalesce(sum(realized_business_net_pnl) FILTER (
        WHERE attributed_at >= date_trunc('day', now() AT TIME ZONE 'UTC')
            AT TIME ZONE 'UTC'
    ), 0)::numeric(79,0) AS realized_net_pnl_today,
    coalesce(sum(realized_business_net_pnl) FILTER (
        WHERE attributed_at >= now() - interval '7 days'
    ), 0)::numeric(79,0) AS realized_net_pnl_7d,
    coalesce(sum(realized_business_net_pnl) FILTER (
        WHERE attributed_at >= now() - interval '30 days'
    ), 0)::numeric(79,0) AS realized_net_pnl_30d,
    coalesce(sum(realized_gross_profit), 0)::numeric(79,0) AS gross_profit,
    coalesce(sum(actual_gas_cost + actual_l1_cost), 0)::numeric(78,0) AS gas_cost,
    coalesce(sum(actual_flash_premium), 0)::numeric(78,0) AS flash_fees,
    count(*) AS reconciled_outcomes
FROM live_canary.autonomous_outcome_attributions;

COMMIT;
