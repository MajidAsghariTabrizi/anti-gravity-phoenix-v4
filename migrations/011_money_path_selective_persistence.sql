CREATE TABLE IF NOT EXISTS money_path_ingress_daily (
    bucket_date DATE NOT NULL,
    classification TEXT NOT NULL,
    detail_class TEXT NOT NULL,
    router_kind TEXT NOT NULL,
    wrapper_kind TEXT NOT NULL,
    selector_kind TEXT NOT NULL,
    event_count BIGINT NOT NULL,
    first_seen_at TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL,
    schema_version TEXT NOT NULL,
    PRIMARY KEY (
        bucket_date,
        classification,
        detail_class,
        router_kind,
        wrapper_kind,
        selector_kind
    ),
    CONSTRAINT money_path_ingress_daily_classification_check
        CHECK (classification IN (
            'irrelevant',
            'unsupported_interesting',
            'relevant_route_input'
        )),
    CONSTRAINT money_path_ingress_daily_detail_check
        CHECK (
            char_length(detail_class) BETWEEN 1 AND 128
            AND char_length(router_kind) BETWEEN 1 AND 64
            AND char_length(wrapper_kind) BETWEEN 1 AND 64
            AND char_length(selector_kind) BETWEEN 1 AND 64
        ),
    CONSTRAINT money_path_ingress_daily_count_check CHECK (event_count > 0),
    CONSTRAINT money_path_ingress_daily_time_check
        CHECK (last_seen_at >= first_seen_at),
    CONSTRAINT money_path_ingress_daily_schema_check
        CHECK (schema_version = 'money_path.ingress.v1')
);

CREATE TABLE IF NOT EXISTS money_path_ingress_samples (
    bucket_date DATE NOT NULL,
    classification TEXT NOT NULL,
    detail_class TEXT NOT NULL,
    router_kind TEXT NOT NULL,
    wrapper_kind TEXT NOT NULL,
    selector_kind TEXT NOT NULL,
    sample_ordinal SMALLINT NOT NULL,
    safe_decoder_summary JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL,
    schema_version TEXT NOT NULL,
    PRIMARY KEY (
        bucket_date,
        classification,
        detail_class,
        router_kind,
        wrapper_kind,
        selector_kind,
        sample_ordinal
    ),
    CONSTRAINT money_path_ingress_samples_classification_check
        CHECK (classification = 'unsupported_interesting'),
    CONSTRAINT money_path_ingress_samples_detail_check
        CHECK (
            char_length(detail_class) BETWEEN 1 AND 128
            AND char_length(router_kind) BETWEEN 1 AND 64
            AND char_length(wrapper_kind) BETWEEN 1 AND 64
            AND char_length(selector_kind) BETWEEN 1 AND 64
        ),
    CONSTRAINT money_path_ingress_samples_ordinal_check
        CHECK (sample_ordinal BETWEEN 1 AND 1000),
    CONSTRAINT money_path_ingress_samples_summary_check
        CHECK (
            jsonb_typeof(safe_decoder_summary) = 'object'
            AND octet_length(safe_decoder_summary::text) <= 4096
            AND safe_decoder_summary ?& ARRAY[
                'router_kind',
                'outer_selector_kind',
                'wrapper_kind',
                'decoded_swap_kind',
                'unsupported_reason',
                'command_count',
                'v3_hop_count',
                'reviewed_pool_matches'
            ]
            AND NOT safe_decoder_summary ?| ARRAY[
                'tx_hash',
                'address',
                'source_event_identity',
                'raw_tx',
                'calldata',
                'url',
                'dsn',
                'environment'
            ]
        ),
    CONSTRAINT money_path_ingress_samples_schema_check
        CHECK (schema_version = 'money_path.ingress.v1')
);

CREATE INDEX IF NOT EXISTS money_path_ingress_daily_observed_idx
    ON money_path_ingress_daily(bucket_date, classification, detail_class);

CREATE INDEX IF NOT EXISTS money_path_ingress_samples_observed_idx
    ON money_path_ingress_samples(bucket_date, detail_class, observed_at);
