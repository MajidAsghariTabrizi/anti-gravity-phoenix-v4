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
            'phoenix.live-canary-schema.v9',
            'phoenix.live-canary-schema.v10',
            'phoenix.live-canary-schema.v11'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v11')
ON CONFLICT (version) DO NOTHING;

-- Public-chain ground truth for Atlas SVR settlements that liquidated an
-- Aave V3 borrower. Rows are decoded by the reviewed atlas-reconciler from
-- the credential-free bounded transcript (SolversTxResult/MetacallResult
-- logs plus the Aave V3 pool LiquidationCall events in the settlement
-- receipt). Evidence only: this table never authorizes, gates, or
-- materializes any execution request or solver request.
CREATE TABLE IF NOT EXISTS live_canary.atlas_liquidation_ground_truth (
    transaction_hash TEXT NOT NULL CHECK (transaction_hash ~ '^0x[0-9a-f]{64}$'),
    log_index BIGINT NOT NULL CHECK (log_index >= 0),
    user_operation_hash TEXT NOT NULL CHECK (user_operation_hash ~ '^0x[0-9a-f]{64}$'),
    borrower TEXT NOT NULL CHECK (borrower ~ '^0x[0-9a-f]{40}$'),
    debt_asset TEXT NOT NULL CHECK (debt_asset ~ '^0x[0-9a-f]{40}$'),
    collateral_asset TEXT NOT NULL CHECK (collateral_asset ~ '^0x[0-9a-f]{40}$'),
    debt_to_cover_wei TEXT NOT NULL CHECK (debt_to_cover_wei ~ '^[0-9]+$'),
    liquidated_collateral_wei TEXT NOT NULL CHECK (liquidated_collateral_wei ~ '^[0-9]+$'),
    liquidator TEXT NOT NULL CHECK (liquidator ~ '^0x[0-9a-f]{40}$'),
    receive_a_token BOOLEAN NOT NULL DEFAULT false,
    block_number BIGINT NOT NULL CHECK (block_number > 0),
    reconciled_at TIMESTAMPTZ NOT NULL,
    transcript_sha256 TEXT NOT NULL CHECK (transcript_sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (transaction_hash, log_index)
);

CREATE INDEX IF NOT EXISTS live_canary_liquidation_gt_borrower
ON live_canary.atlas_liquidation_ground_truth(borrower, block_number);

CREATE INDEX IF NOT EXISTS live_canary_liquidation_gt_userop
ON live_canary.atlas_liquidation_ground_truth(user_operation_hash);

COMMIT;
