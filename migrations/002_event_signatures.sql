BEGIN;

CREATE TABLE IF NOT EXISTS contract_events (
    id BIGSERIAL PRIMARY KEY,
    tx_hash TEXT NOT NULL,
    log_index INTEGER NOT NULL,
    contract_address TEXT NOT NULL,
    event_name TEXT NOT NULL,
    event_signature TEXT NOT NULL,
    decoded JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tx_hash, log_index)
);

INSERT INTO gas_profiles (execution_shape, gas_used_p50, gas_used_p90, gas_used_p99, sample_count, ewma)
VALUES ('FLASH_V3_V3_TWO_LEG', NULL, NULL, NULL, 0, NULL)
ON CONFLICT (execution_shape) DO NOTHING;

COMMIT;

