CREATE TABLE IF NOT EXISTS source_event_identities (
    source_event_identity TEXT PRIMARY KEY
        CHECK (char_length(source_event_identity) BETWEEN 1 AND 200),
    schema_version TEXT NOT NULL
        CHECK (schema_version = 'phoenix.source-identity.v1'),
    source_chain_id BIGINT NOT NULL CHECK (source_chain_id = 42161),
    source_transaction_hash TEXT NOT NULL
        CHECK (source_transaction_hash ~ '^0x[0-9a-f]{64}$'),
    source_feed_sequence NUMERIC(78,0) NOT NULL CHECK (source_feed_sequence > 0),
    source_feed_order_position NUMERIC(78,0) CHECK (source_feed_order_position >= 0),
    source_block_number NUMERIC(78,0),
    source_block_hash TEXT,
    source_transaction_index NUMERIC(78,0),
    source_command_index INTEGER NOT NULL CHECK (source_command_index BETWEEN 0 AND 65535),
    source_event_index NUMERIC(78,0),
    source_observed_at TIMESTAMPTZ NOT NULL,
    source_router TEXT NOT NULL CHECK (source_router ~ '^0x[0-9a-f]{40}$'),
    source_factory TEXT NOT NULL CHECK (source_factory ~ '^0x[0-9a-f]{40}$'),
    source_pool TEXT NOT NULL CHECK (char_length(source_pool) BETWEEN 1 AND 256),
    source_pool_path JSONB NOT NULL
        CHECK (jsonb_typeof(source_pool_path) = 'array'
            AND jsonb_array_length(source_pool_path) BETWEEN 1 AND 8),
    source_token_path JSONB NOT NULL
        CHECK (jsonb_typeof(source_token_path) = 'array'
            AND jsonb_array_length(source_token_path) BETWEEN 2 AND 9),
    source_encoded_token_path TEXT NOT NULL
        CHECK (source_encoded_token_path ~ '^0x[0-9a-f]+$'
            AND mod(char_length(source_encoded_token_path) - 2, 2) = 0),
    source_fee_path JSONB NOT NULL
        CHECK (jsonb_typeof(source_fee_path) = 'array'
            AND jsonb_array_length(source_fee_path) BETWEEN 1 AND 8),
    source_direction TEXT NOT NULL
        CHECK (source_direction IN ('zero_for_one', 'one_for_zero')),
    source_input_amount NUMERIC(78,0) NOT NULL CHECK (source_input_amount > 0),
    decoded_commands JSONB NOT NULL
        CHECK (jsonb_typeof(decoded_commands) = 'array'
            AND jsonb_array_length(decoded_commands) BETWEEN 1 AND 32),
    unavailable_reason TEXT NOT NULL CHECK (char_length(unavailable_reason) BETWEEN 1 AND 128),
    source_identity_hash TEXT NOT NULL UNIQUE
        CHECK (source_identity_hash ~ '^[0-9a-f]{64}$'),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT source_identity_unresolved_block_check CHECK (
        source_block_number IS NULL
        AND source_block_hash IS NULL
        AND source_transaction_index IS NULL
        AND source_event_index IS NULL
    ),
    CONSTRAINT source_identity_path_cardinality_check CHECK (
        jsonb_array_length(source_token_path) = jsonb_array_length(source_fee_path) + 1
        AND jsonb_array_length(source_pool_path) = jsonb_array_length(source_fee_path)
    ),
    CONSTRAINT source_identity_unavailable_reason_check CHECK (
        (
            source_feed_order_position IS NULL
            AND unavailable_reason = 'legacy_event_missing_order_position'
        )
        OR (
            source_feed_order_position IS NOT NULL
            AND unavailable_reason = 'awaiting_canonical_block_assignment'
        )
    ),
    CONSTRAINT source_identity_event_hash_unique UNIQUE (
        source_event_identity,
        source_identity_hash
    )
);

CREATE INDEX IF NOT EXISTS source_event_identities_pending_idx
    ON source_event_identities(
        recorded_at,
        source_feed_sequence,
        source_feed_order_position
    )
    WHERE unavailable_reason = 'awaiting_canonical_block_assignment';

CREATE TABLE IF NOT EXISTS source_block_enrichments (
    enrichment_hash TEXT PRIMARY KEY CHECK (enrichment_hash ~ '^[0-9a-f]{64}$'),
    source_event_identity TEXT NOT NULL UNIQUE,
    source_identity_hash TEXT NOT NULL,
    source_chain_id BIGINT NOT NULL CHECK (source_chain_id = 42161),
    source_transaction_hash TEXT NOT NULL
        CHECK (source_transaction_hash ~ '^0x[0-9a-f]{64}$'),
    source_block_number NUMERIC(78,0) NOT NULL CHECK (source_block_number > 0),
    source_block_hash TEXT NOT NULL CHECK (source_block_hash ~ '^0x[0-9a-f]{64}$'),
    source_transaction_index NUMERIC(78,0) NOT NULL CHECK (source_transaction_index >= 0),
    source_event_index NUMERIC(78,0),
    source_pool_addresses JSONB NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(source_pool_addresses) = 'array'
            AND jsonb_array_length(source_pool_addresses) <= 8),
    transaction_status TEXT NOT NULL CHECK (transaction_status IN ('success', 'reverted')),
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 128),
    provider_response_hash TEXT NOT NULL CHECK (provider_response_hash ~ '^[0-9a-f]{64}$'),
    enriched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT source_block_identity_fk FOREIGN KEY (
        source_event_identity,
        source_identity_hash
    ) REFERENCES source_event_identities (
        source_event_identity,
        source_identity_hash
    ),
    CONSTRAINT source_block_event_hash_unique UNIQUE (
        enrichment_hash,
        source_event_identity,
        source_identity_hash
    ),
    CONSTRAINT source_block_status_evidence_check CHECK (
        (
            transaction_status = 'success'
            AND source_event_index IS NOT NULL
            AND jsonb_array_length(source_pool_addresses) BETWEEN 1 AND 8
        )
        OR (
            transaction_status = 'reverted'
            AND source_event_index IS NULL
            AND jsonb_array_length(source_pool_addresses) = 0
        )
    )
);

CREATE INDEX IF NOT EXISTS source_block_enrichments_block_idx
    ON source_block_enrichments(source_block_number, source_transaction_index);

CREATE TABLE IF NOT EXISTS source_enrichment_attempts (
    id BIGSERIAL PRIMARY KEY,
    source_event_identity TEXT NOT NULL
        REFERENCES source_event_identities(source_event_identity),
    attempt_number INTEGER NOT NULL CHECK (attempt_number BETWEEN 1 AND 3),
    result TEXT NOT NULL CHECK (result IN ('completed', 'retryable_failure', 'terminal_failure')),
    failure_reason TEXT,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (source_event_identity, attempt_number),
    CONSTRAINT source_enrichment_attempt_result_check CHECK (
        (result = 'completed' AND failure_reason IS NULL)
        OR (
            result <> 'completed'
            AND char_length(failure_reason) BETWEEN 1 AND 128
        )
    )
);

CREATE TABLE IF NOT EXISTS transaction_boundary_state_evidence (
    evidence_hash TEXT PRIMARY KEY CHECK (evidence_hash ~ '^[0-9a-f]{64}$'),
    source_event_identity TEXT NOT NULL UNIQUE,
    source_identity_hash TEXT NOT NULL,
    enrichment_hash TEXT NOT NULL,
    source_block_number NUMERIC(78,0) NOT NULL CHECK (source_block_number > 0),
    source_block_hash TEXT NOT NULL CHECK (source_block_hash ~ '^0x[0-9a-f]{64}$'),
    source_transaction_hash TEXT NOT NULL
        CHECK (source_transaction_hash ~ '^0x[0-9a-f]{64}$'),
    source_transaction_index NUMERIC(78,0) NOT NULL CHECK (source_transaction_index >= 0),
    parent_block_number NUMERIC(78,0) NOT NULL CHECK (parent_block_number >= 0),
    parent_block_hash TEXT NOT NULL CHECK (parent_block_hash ~ '^0x[0-9a-f]{64}$'),
    reconstruction_method TEXT NOT NULL CHECK (
        reconstruction_method IN ('debug_trace_transaction_prestate_diff', 'unavailable')
    ),
    prestate_hash TEXT,
    state_diff_hash TEXT,
    post_initiating_state_hash TEXT,
    completeness_status TEXT NOT NULL CHECK (completeness_status IN ('complete', 'incomplete')),
    failure_reason TEXT,
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 128),
    provider_response_hash TEXT NOT NULL
        CHECK (provider_response_hash ~ '^[0-9a-f]{64}$'),
    evidence JSONB NOT NULL CHECK (jsonb_typeof(evidence) = 'object'),
    reconstructed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT transaction_boundary_identity_fk FOREIGN KEY (
        source_event_identity,
        source_identity_hash
    ) REFERENCES source_event_identities (
        source_event_identity,
        source_identity_hash
    ),
    CONSTRAINT transaction_boundary_enrichment_fk FOREIGN KEY (
        enrichment_hash,
        source_event_identity,
        source_identity_hash
    ) REFERENCES source_block_enrichments (
        enrichment_hash,
        source_event_identity,
        source_identity_hash
    ),
    CONSTRAINT transaction_boundary_completeness_check CHECK (
        (
            completeness_status = 'complete'
            AND reconstruction_method = 'debug_trace_transaction_prestate_diff'
            AND prestate_hash ~ '^[0-9a-f]{64}$'
            AND state_diff_hash ~ '^[0-9a-f]{64}$'
            AND post_initiating_state_hash ~ '^[0-9a-f]{64}$'
            AND failure_reason IS NULL
            AND provider_response_hash ~ '^[0-9a-f]{64}$'
            AND evidence->>'schema_version' = 'phoenix.transaction-boundary-state.v1'
            AND evidence->>'complete' = 'true'
        )
        OR (
            completeness_status = 'incomplete'
            AND reconstruction_method = 'unavailable'
            AND prestate_hash IS NULL
            AND state_diff_hash IS NULL
            AND post_initiating_state_hash IS NULL
            AND char_length(failure_reason) BETWEEN 1 AND 128
            AND evidence->>'schema_version' = 'phoenix.transaction-boundary-state.v1'
            AND evidence->>'complete' = 'false'
            AND evidence->>'failure_reason' = failure_reason
        )
    )
);

CREATE OR REPLACE FUNCTION reject_exact_source_evidence_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'exact source evidence is append-only';
END;
$$;

DROP TRIGGER IF EXISTS source_event_identities_immutable
    ON source_event_identities;
CREATE TRIGGER source_event_identities_immutable
BEFORE UPDATE OR DELETE ON source_event_identities
FOR EACH ROW EXECUTE FUNCTION reject_exact_source_evidence_mutation();

DROP TRIGGER IF EXISTS source_block_enrichments_immutable
    ON source_block_enrichments;
CREATE TRIGGER source_block_enrichments_immutable
BEFORE UPDATE OR DELETE ON source_block_enrichments
FOR EACH ROW EXECUTE FUNCTION reject_exact_source_evidence_mutation();

DROP TRIGGER IF EXISTS source_enrichment_attempts_immutable
    ON source_enrichment_attempts;
CREATE TRIGGER source_enrichment_attempts_immutable
BEFORE UPDATE OR DELETE ON source_enrichment_attempts
FOR EACH ROW EXECUTE FUNCTION reject_exact_source_evidence_mutation();

DROP TRIGGER IF EXISTS transaction_boundary_state_evidence_immutable
    ON transaction_boundary_state_evidence;
CREATE TRIGGER transaction_boundary_state_evidence_immutable
BEFORE UPDATE OR DELETE ON transaction_boundary_state_evidence
FOR EACH ROW EXECUTE FUNCTION reject_exact_source_evidence_mutation();
