CREATE INDEX IF NOT EXISTS rpc_quality_records_shadow_decision_idx
    ON rpc_quality_records(shadow_decision_id, recorded_at DESC, id DESC)
    WHERE shadow_decision_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS pool_state_checkpoints_latest_pool_idx
    ON pool_state_checkpoints(lower(pool), block_number DESC, id DESC);
