BEGIN;

ALTER TABLE live_canary.schema_contract
    DROP CONSTRAINT IF EXISTS schema_contract_version_check;

ALTER TABLE live_canary.schema_contract
    ADD CONSTRAINT schema_contract_version_check CHECK (
        version IN (
            'phoenix.live-canary-schema.v1',
            'phoenix.live-canary-schema.v2'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v2')
ON CONFLICT (version) DO NOTHING;

ALTER TABLE live_canary.execution_requests
    DROP CONSTRAINT IF EXISTS execution_requests_schema_version_check;

ALTER TABLE live_canary.execution_requests
    ADD CONSTRAINT execution_requests_schema_version_check CHECK (
        schema_version IN (
            'phoenix.live-execution-request.v1',
            'phoenix.live-execution-request.v2'
        )
    );

ALTER TABLE live_canary.execution_requests
    ADD COLUMN IF NOT EXISTS route_fingerprint TEXT,
    ADD COLUMN IF NOT EXISTS selected_size NUMERIC(39,0),
    ADD COLUMN IF NOT EXISTS token_path JSONB,
    ADD COLUMN IF NOT EXISTS executor_address TEXT,
    ADD COLUMN IF NOT EXISTS executor_code_hash TEXT,
    ADD COLUMN IF NOT EXISTS calldata_hash TEXT,
    ADD COLUMN IF NOT EXISTS simulation_result_hash TEXT,
    ADD COLUMN IF NOT EXISTS plan_hash TEXT,
    ADD COLUMN IF NOT EXISTS pinned_block_number NUMERIC(78,0),
    ADD COLUMN IF NOT EXISTS pinned_block_hash TEXT,
    ADD COLUMN IF NOT EXISTS approval_deadline TIMESTAMPTZ;

ALTER TABLE live_canary.execution_requests
    DROP CONSTRAINT IF EXISTS execution_requests_approval_evidence_check;

ALTER TABLE live_canary.execution_requests
    ADD CONSTRAINT execution_requests_approval_evidence_check CHECK (
        schema_version <> 'phoenix.live-execution-request.v2'
        OR (
            route_fingerprint IS NOT NULL
            AND length(route_fingerprint) BETWEEN 1 AND 256
            AND selected_size IS NOT NULL
            AND selected_size > 0
            AND selected_size = flash_amount
            AND selected_size <= maximum_input_amount
            AND token_path IS NOT NULL
            AND jsonb_typeof(token_path) = 'array'
            AND jsonb_array_length(token_path) = jsonb_array_length(legs) + 1
            AND jsonb_array_length(token_path) BETWEEN 2 AND 5
            AND executor_address IS NOT NULL
            AND executor_address ~ '^0x[0-9a-f]{40}$'
            AND executor_code_hash IS NOT NULL
            AND executor_code_hash ~ '^[0-9a-f]{64}$'
            AND calldata_hash IS NOT NULL
            AND calldata_hash ~ '^[0-9a-f]{64}$'
            AND simulation_result_hash IS NOT NULL
            AND simulation_result_hash ~ '^[0-9a-f]{64}$'
            AND plan_hash IS NOT NULL
            AND plan_hash ~ '^[0-9a-f]{64}$'
            AND pinned_block_number IS NOT NULL
            AND pinned_block_number > 0
            AND pinned_block_hash IS NOT NULL
            AND pinned_block_hash ~ '^0x[0-9a-f]{64}$'
            AND approval_deadline IS NOT NULL
            AND approved_at IS NOT NULL
            AND approval_deadline > approved_at
            AND approval_deadline <= deadline
        )
    );

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_execution_request_simulation_result
    ON live_canary.execution_requests(simulation_result_hash)
    WHERE simulation_result_hash IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_execution_request_plan
    ON live_canary.execution_requests(plan_hash)
    WHERE plan_hash IS NOT NULL;

COMMIT;
