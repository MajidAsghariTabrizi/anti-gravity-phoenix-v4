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
            'phoenix.live-canary-schema.v11',
            'phoenix.live-canary-schema.v12'
        )
    );

INSERT INTO live_canary.schema_contract(version)
VALUES ('phoenix.live-canary-schema.v12')
ON CONFLICT (version) DO NOTHING;

-- Mission §3.2: widen the Atlas shadow evidence-mode contract to accept the
-- REAL solver-callback frame evidence alongside the historical
-- callback_proxy wrapper. The two modes are never interchangeable: a row's
-- evidence_mode records exactly which frame produced its economics, and the
-- live revenue lane continues to require the legacy mode until a separate
-- reviewed owner mission changes that gate. This migration only widens an
-- evidence ledger constraint; it authorizes no execution anywhere.
ALTER TABLE live_canary.atlas_auction_shadow
    DROP CONSTRAINT IF EXISTS atlas_auction_shadow_evidence_mode_check;

ALTER TABLE live_canary.atlas_auction_shadow
    ADD CONSTRAINT atlas_auction_shadow_evidence_mode_check CHECK (
        evidence_mode IS NULL
        OR evidence_mode = 'SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED'
        OR evidence_mode = 'SINGLE_PRIMARY_ATLAS_SOLVER_CALL_FORK_VERIFIED'
    );

COMMIT;
