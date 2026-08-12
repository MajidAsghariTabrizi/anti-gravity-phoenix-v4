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
            'phoenix.live-canary-schema.v8'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v8')
ON CONFLICT (version) DO NOTHING;

-- This singleton is an execution interlock, not a replacement for the lane
-- controls.  Brief provider failures close only exact_execution_ready.  A
-- sustained failure still closes both revenue lanes through the existing
-- atomic control transition, and the remaining fields bind the narrowly
-- allowlisted recovery evidence to that exact transition.
CREATE TABLE IF NOT EXISTS live_canary.revenue_provider_authority (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    schema_version TEXT NOT NULL DEFAULT 'phoenix.revenue-provider-authority.v1'
        CHECK (schema_version = 'phoenix.revenue-provider-authority.v1'),
    exact_execution_ready BOOLEAN NOT NULL DEFAULT false,
    gate_reason TEXT NOT NULL DEFAULT 'not_verified'
        CHECK (gate_reason ~ '^[a-z0-9_]+$' AND length(gate_reason) BETWEEN 1 AND 128),
    gate_updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    request_evidence_not_before TIMESTAMPTZ NOT NULL DEFAULT now(),
    recovery_status TEXT NOT NULL DEFAULT 'collecting'
        CHECK (recovery_status IN ('idle', 'collecting', 'ready', 'recovered', 'blocked')),
    failure_reason TEXT CHECK (failure_reason IS NULL OR failure_reason IN (
        'provider_disagreement',
        'provider_unavailable',
        'provider_timeout',
        'provider_rate_limited'
    )),
    failure_control_epoch BIGINT CHECK (failure_control_epoch IS NULL OR failure_control_epoch >= 0),
    failure_transition_at TIMESTAMPTZ,
    failure_release_sha TEXT CHECK (failure_release_sha IS NULL OR failure_release_sha ~ '^[0-9a-f]{40}$'),
    restore_phase TEXT CHECK (restore_phase IS NULL OR restore_phase = 'DISARMED_EVIDENCE'),
    restore_size_level TEXT CHECK (restore_size_level IS NULL OR restore_size_level = 'MAX_REVIEWED'),
    sample_count SMALLINT NOT NULL DEFAULT 0 CHECK (sample_count BETWEEN 0 AND 3),
    sample_1_at TIMESTAMPTZ,
    sample_1_primary_provider TEXT CHECK (sample_1_primary_provider IS NULL OR sample_1_primary_provider ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    sample_1_confirmation_provider TEXT CHECK (sample_1_confirmation_provider IS NULL OR sample_1_confirmation_provider ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    sample_2_at TIMESTAMPTZ,
    sample_2_primary_provider TEXT CHECK (sample_2_primary_provider IS NULL OR sample_2_primary_provider ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    sample_2_confirmation_provider TEXT CHECK (sample_2_confirmation_provider IS NULL OR sample_2_confirmation_provider ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    sample_3_at TIMESTAMPTZ,
    sample_3_primary_provider TEXT CHECK (sample_3_primary_provider IS NULL OR sample_3_primary_provider ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    sample_3_confirmation_provider TEXT CHECK (sample_3_confirmation_provider IS NULL OR sample_3_confirmation_provider ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    recovery_attempted_total BIGINT NOT NULL DEFAULT 0 CHECK (recovery_attempted_total >= 0),
    recovery_succeeded_total BIGINT NOT NULL DEFAULT 0 CHECK (recovery_succeeded_total >= 0),
    recovery_blocked_total BIGINT NOT NULL DEFAULT 0 CHECK (recovery_blocked_total >= 0),
    last_block_reason TEXT CHECK (last_block_reason IS NULL OR (last_block_reason ~ '^[a-z0-9_]+$' AND length(last_block_reason) BETWEEN 1 AND 128)),
    recovery_evidence_hash TEXT CHECK (recovery_evidence_hash IS NULL OR recovery_evidence_hash ~ '^[0-9a-f]{64}$'),
    last_recovered_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (failure_reason IS NULL AND failure_control_epoch IS NULL AND failure_transition_at IS NULL
         AND failure_release_sha IS NULL AND restore_phase IS NULL AND restore_size_level IS NULL)
        OR
        (failure_reason IS NOT NULL AND failure_control_epoch IS NOT NULL AND failure_transition_at IS NOT NULL
         AND failure_release_sha IS NOT NULL AND restore_phase IS NOT NULL AND restore_size_level IS NOT NULL)
    ),
    CHECK ((sample_count >= 1) = (sample_1_at IS NOT NULL)),
    CHECK ((sample_count >= 1) = (sample_1_primary_provider IS NOT NULL)),
    CHECK ((sample_count >= 1) = (sample_1_confirmation_provider IS NOT NULL)),
    CHECK ((sample_count >= 2) = (sample_2_at IS NOT NULL)),
    CHECK ((sample_count >= 2) = (sample_2_primary_provider IS NOT NULL)),
    CHECK ((sample_count >= 2) = (sample_2_confirmation_provider IS NOT NULL)),
    CHECK ((sample_count >= 3) = (sample_3_at IS NOT NULL)),
    CHECK ((sample_count >= 3) = (sample_3_primary_provider IS NOT NULL)),
    CHECK ((sample_count >= 3) = (sample_3_confirmation_provider IS NOT NULL)),
    CHECK (sample_1_primary_provider IS NULL OR sample_1_primary_provider <> sample_1_confirmation_provider),
    CHECK (sample_2_primary_provider IS NULL OR sample_2_primary_provider <> sample_2_confirmation_provider),
    CHECK (sample_3_primary_provider IS NULL OR sample_3_primary_provider <> sample_3_confirmation_provider),
    CHECK (sample_2_at IS NULL OR sample_2_at > sample_1_at),
    CHECK (sample_3_at IS NULL OR sample_3_at > sample_2_at),
    CHECK (failure_transition_at IS NULL OR sample_1_at IS NULL OR sample_1_at > failure_transition_at),
    CHECK (NOT exact_execution_ready OR sample_count = 3),
    CHECK (recovery_status NOT IN ('ready','recovered') OR sample_count = 3)
);

INSERT INTO live_canary.revenue_provider_authority(singleton)
VALUES (true)
ON CONFLICT (singleton) DO NOTHING;

COMMIT;
