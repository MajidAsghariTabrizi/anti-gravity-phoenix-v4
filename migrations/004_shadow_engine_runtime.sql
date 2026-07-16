CREATE TABLE IF NOT EXISTS engine_outbox (
    outbox_id TEXT PRIMARY KEY,
    schema_version TEXT NOT NULL,
    source_event_identity TEXT NOT NULL UNIQUE,
    source_sequence NUMERIC(78,0) NOT NULL,
    tx_hash TEXT NOT NULL,
    chain_id BIGINT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    publish_attempts INTEGER NOT NULL DEFAULT 0,
    published_at TIMESTAMPTZ,
    jetstream_ack_sequence NUMERIC(78,0),
    last_error_class TEXT,
    last_error_at TIMESTAMPTZ,
    claim_owner TEXT,
    claimed_at TIMESTAMPTZ,
    claim_expires_at TIMESTAMPTZ,
    CONSTRAINT engine_outbox_feed_event_fk
        FOREIGN KEY (source_sequence, tx_hash)
        REFERENCES feed_events(sequence_number, tx_hash),
    CONSTRAINT engine_outbox_schema_check
        CHECK (schema_version = 'phoenix.engine.input.v1'),
    CONSTRAINT engine_outbox_identity_check
        CHECK (
            char_length(outbox_id) BETWEEN 1 AND 200
            AND char_length(source_event_identity) BETWEEN 1 AND 200
            AND outbox_id = source_event_identity
        ),
    CONSTRAINT engine_outbox_sequence_check CHECK (source_sequence >= 0),
    CONSTRAINT engine_outbox_tx_hash_check
        CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT engine_outbox_chain_check CHECK (chain_id = 42161),
    CONSTRAINT engine_outbox_payload_check
        CHECK (
            jsonb_typeof(payload) = 'object'
            AND octet_length(payload::text) <= 1048576
        ),
    CONSTRAINT engine_outbox_attempts_check CHECK (publish_attempts >= 0),
    CONSTRAINT engine_outbox_ack_sequence_check
        CHECK (jetstream_ack_sequence IS NULL OR jetstream_ack_sequence >= 0),
    CONSTRAINT engine_outbox_error_class_check
        CHECK (last_error_class IS NULL OR char_length(last_error_class) BETWEEN 1 AND 64),
    CONSTRAINT engine_outbox_claim_owner_check
        CHECK (claim_owner IS NULL OR char_length(claim_owner) BETWEEN 1 AND 128),
    CONSTRAINT engine_outbox_claim_coherence_check
        CHECK (
            (claim_owner IS NULL AND claimed_at IS NULL AND claim_expires_at IS NULL)
            OR
            (claim_owner IS NOT NULL AND claimed_at IS NOT NULL AND claim_expires_at IS NOT NULL)
        ),
    CONSTRAINT engine_outbox_published_claim_check
        CHECK (published_at IS NULL OR claim_owner IS NULL)
);

CREATE INDEX IF NOT EXISTS engine_outbox_pending_idx
    ON engine_outbox(available_at, created_at, outbox_id)
    WHERE published_at IS NULL;

CREATE INDEX IF NOT EXISTS engine_outbox_retry_idx
    ON engine_outbox(claim_expires_at, available_at)
    WHERE published_at IS NULL;

CREATE TABLE IF NOT EXISTS shadow_engine_classifications (
    source_event_identity TEXT PRIMARY KEY,
    schema_version TEXT NOT NULL,
    source_sequence NUMERIC(78,0) NOT NULL,
    tx_hash TEXT NOT NULL,
    chain_id BIGINT NOT NULL,
    classification TEXT NOT NULL,
    detail_class TEXT,
    candidate_count INTEGER NOT NULL DEFAULT 0,
    decision_count INTEGER NOT NULL DEFAULT 0,
    delivery_attempts INTEGER NOT NULL DEFAULT 1,
    evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
    first_received_at TIMESTAMPTZ NOT NULL,
    classified_at TIMESTAMPTZ NOT NULL,
    processing_latency_ns NUMERIC(78,0) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT shadow_engine_classification_schema_check
        CHECK (schema_version = 'phoenix.engine.input.v1'),
    CONSTRAINT shadow_engine_classification_identity_check
        CHECK (char_length(source_event_identity) BETWEEN 1 AND 200),
    CONSTRAINT shadow_engine_classification_sequence_check CHECK (source_sequence >= 0),
    CONSTRAINT shadow_engine_classification_tx_hash_check
        CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT shadow_engine_classification_chain_check CHECK (chain_id = 42161),
    CONSTRAINT shadow_engine_classification_value_check
        CHECK (classification IN (
            'no_relevant_route',
            'candidate_generated',
            'candidate_rejected',
            'shadow_accepted',
            'malformed_internal_event',
            'unsupported_schema',
            'transient_dependency_failure',
            'terminal_integrity_failure'
        )),
    CONSTRAINT shadow_engine_classification_detail_check
        CHECK (detail_class IS NULL OR char_length(detail_class) BETWEEN 1 AND 128),
    CONSTRAINT shadow_engine_classification_counts_check
        CHECK (candidate_count >= 0 AND decision_count >= 0 AND delivery_attempts >= 1),
    CONSTRAINT shadow_engine_classification_evidence_check
        CHECK (
            jsonb_typeof(evidence) = 'object'
            AND octet_length(evidence::text) <= 1048576
        ),
    CONSTRAINT shadow_engine_classification_latency_check CHECK (processing_latency_ns >= 0),
    UNIQUE (source_sequence, tx_hash)
);

CREATE INDEX IF NOT EXISTS shadow_engine_classification_created_idx
    ON shadow_engine_classifications(classified_at, classification);

CREATE TABLE IF NOT EXISTS shadow_engine_processing_attempts (
    id BIGSERIAL PRIMARY KEY,
    source_event_identity TEXT NOT NULL,
    delivery_attempt BIGINT NOT NULL,
    classification TEXT NOT NULL,
    error_class TEXT,
    evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ NOT NULL,
    processing_latency_ns NUMERIC(78,0) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT shadow_engine_attempt_identity_check
        CHECK (char_length(source_event_identity) BETWEEN 1 AND 200),
    CONSTRAINT shadow_engine_attempt_delivery_check CHECK (delivery_attempt >= 1),
    CONSTRAINT shadow_engine_attempt_classification_check
        CHECK (classification IN (
            'no_relevant_route',
            'candidate_generated',
            'candidate_rejected',
            'shadow_accepted',
            'malformed_internal_event',
            'unsupported_schema',
            'transient_dependency_failure',
            'terminal_integrity_failure'
        )),
    CONSTRAINT shadow_engine_attempt_error_check
        CHECK (error_class IS NULL OR char_length(error_class) BETWEEN 1 AND 128),
    CONSTRAINT shadow_engine_attempt_evidence_check
        CHECK (
            jsonb_typeof(evidence) = 'object'
            AND octet_length(evidence::text) <= 1048576
        ),
    CONSTRAINT shadow_engine_attempt_latency_check CHECK (processing_latency_ns >= 0),
    UNIQUE (source_event_identity, delivery_attempt)
);

CREATE INDEX IF NOT EXISTS shadow_engine_attempt_identity_idx
    ON shadow_engine_processing_attempts(source_event_identity, created_at);

ALTER TABLE shadow_decisions
    ADD COLUMN IF NOT EXISTS source_event_identity TEXT,
    ADD COLUMN IF NOT EXISTS secondary_rejection_reasons JSONB NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS risk_flags JSONB NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS processing_latency_ns NUMERIC(78,0);

ALTER TABLE shadow_decisions
    ADD CONSTRAINT shadow_decisions_source_event_identity_check
        CHECK (
            source_event_identity IS NULL
            OR char_length(source_event_identity) BETWEEN 1 AND 200
        );

ALTER TABLE shadow_decisions
    ADD CONSTRAINT shadow_decisions_secondary_reasons_check
        CHECK (jsonb_typeof(secondary_rejection_reasons) = 'array');

ALTER TABLE shadow_decisions
    ADD CONSTRAINT shadow_decisions_risk_flags_check
        CHECK (jsonb_typeof(risk_flags) = 'array');

ALTER TABLE shadow_decisions
    ADD CONSTRAINT shadow_decisions_processing_latency_check
        CHECK (processing_latency_ns IS NULL OR processing_latency_ns >= 0);

CREATE UNIQUE INDEX IF NOT EXISTS shadow_decisions_source_event_route_idx
    ON shadow_decisions(source_event_identity, strategy_version, route_fingerprint)
    WHERE source_event_identity IS NOT NULL;
