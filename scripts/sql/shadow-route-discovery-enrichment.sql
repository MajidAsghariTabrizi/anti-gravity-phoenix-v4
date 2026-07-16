\set ON_ERROR_STOP on

BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;

WITH settings AS (
    SELECT :'evidence_limit'::bigint AS evidence_limit
),
eligible AS (
    SELECT fact.*
    FROM shadow_profitability_facts AS fact
    WHERE fact.evidence_completeness_status = 'complete'
      AND fact.chain_id = 42161
      AND fact.shadow_only = true
      AND fact.execution_eligible = false
      AND fact.execution_request_created = false
      AND jsonb_typeof(fact.token_path) = 'array'
      AND jsonb_array_length(fact.token_path) = 3
      AND jsonb_typeof(fact.pool_path) = 'array'
      AND jsonb_array_length(fact.pool_path) = 2
      AND jsonb_typeof(fact.fee_path) = 'array'
      AND jsonb_array_length(fact.fee_path) = 2
),
bounded AS (
    SELECT eligible.*
    FROM eligible
    ORDER BY eligible.evaluated_at DESC, eligible.shadow_decision_id DESC
    LIMIT (SELECT evidence_limit + 1 FROM settings)
),
overflow AS (
    SELECT count(*) > (SELECT evidence_limit FROM settings) AS exceeded
    FROM bounded
),
facts AS (
    SELECT bounded.*
    FROM bounded
    WHERE NOT (SELECT exceeded FROM overflow)
),
rpc AS (
    SELECT fact.shadow_decision_id,
           count(quality.id)::bigint AS record_count,
           count(quality.id) FILTER (
               WHERE NOT quality.success
                  OR quality.timeout
                  OR quality.stale_result
                  OR quality.disagreement
           )::bigint AS failure_count,
           coalesce(sum(quality.latency_ns), 0)::numeric AS latency_ns_total
    FROM facts AS fact
    LEFT JOIN rpc_quality_records AS quality
      ON quality.shadow_decision_id = fact.shadow_decision_id
    GROUP BY fact.shadow_decision_id
)
SELECT jsonb_build_object(
           'record_type', 'profitability',
           'candidate_key', fact.shadow_decision_id::text,
           'pool_path', fact.pool_path,
           'token_path', fact.token_path,
           'fee_path', fact.fee_path,
           'pinned_block_number', fact.pinned_block_number::text,
           'detected_at_unix_ms', floor(extract(epoch FROM fact.detected_at) * 1000)::bigint::text,
           'evaluated_at_unix_ms', floor(extract(epoch FROM fact.evaluated_at) * 1000)::bigint::text,
           'expected_net_pnl', fact.expected_net_pnl::text,
           'severe_net_pnl', fact.severe_net_pnl::text,
           'minimum_required_net_pnl', fact.minimum_required_net_pnl::text,
           'primary_profitability_status', fact.primary_profitability_status,
           'primary_provider_present', fact.primary_provider_id IS NOT NULL,
           'verification_status', fact.verification_status,
           'agreement_state', fact.agreement_state,
           'rpc_records', rpc.record_count::text,
           'rpc_failures', rpc.failure_count::text,
           'rpc_latency_ns_total', rpc.latency_ns_total::text,
           'shadow_only', fact.shadow_only,
           'execution_eligible', fact.execution_eligible,
           'execution_request_created', fact.execution_request_created
       )::text
FROM facts AS fact
JOIN rpc ON rpc.shadow_decision_id = fact.shadow_decision_id
ORDER BY fact.evaluated_at DESC, fact.shadow_decision_id DESC;

WITH settings AS (
    SELECT :'evidence_limit'::bigint AS evidence_limit
),
bounded_eligible AS (
    SELECT 1
    FROM shadow_profitability_facts AS fact
    WHERE fact.evidence_completeness_status = 'complete'
      AND fact.chain_id = 42161
      AND fact.shadow_only = true
      AND fact.execution_eligible = false
      AND fact.execution_request_created = false
      AND jsonb_typeof(fact.token_path) = 'array'
      AND jsonb_array_length(fact.token_path) = 3
      AND jsonb_typeof(fact.pool_path) = 'array'
      AND jsonb_array_length(fact.pool_path) = 2
      AND jsonb_typeof(fact.fee_path) = 'array'
      AND jsonb_array_length(fact.fee_path) = 2
    ORDER BY fact.evaluated_at DESC, fact.shadow_decision_id DESC
    LIMIT (SELECT evidence_limit + 1 FROM settings)
),
eligible_count AS (
    SELECT count(*)::bigint AS row_count
    FROM bounded_eligible
)
SELECT jsonb_build_object(
           'record_type', 'overflow',
           'source', 'shadow_profitability_facts',
           'row_count', eligible_count.row_count::text,
           'configured_limit', settings.evidence_limit::text
       )::text
FROM eligible_count
CROSS JOIN settings
WHERE eligible_count.row_count > settings.evidence_limit;

WITH settings AS (
    SELECT :'evidence_limit'::bigint AS evidence_limit
),
latest AS (
    SELECT DISTINCT ON (lower(checkpoint.pool))
           lower(checkpoint.pool) AS pool_address,
           checkpoint.block_number,
           checkpoint.liquidity
    FROM pool_state_checkpoints AS checkpoint
    WHERE lower(checkpoint.pool) ~ '^0x[0-9a-f]{40}$'
    ORDER BY lower(checkpoint.pool), checkpoint.block_number DESC, checkpoint.id DESC
    LIMIT (SELECT evidence_limit + 1 FROM settings)
),
overflow AS (
    SELECT count(*) > (SELECT evidence_limit FROM settings) AS exceeded
    FROM latest
)
SELECT jsonb_build_object(
           'record_type', 'pool_checkpoint',
           'pool_address', latest.pool_address,
           'block_number', latest.block_number::text,
           'liquidity', latest.liquidity::text
       )::text
FROM latest
WHERE NOT (SELECT exceeded FROM overflow)
ORDER BY latest.pool_address;

WITH settings AS (
    SELECT :'evidence_limit'::bigint AS evidence_limit
),
bounded_pools AS (
    SELECT DISTINCT lower(checkpoint.pool) AS pool_address
    FROM pool_state_checkpoints AS checkpoint
    WHERE lower(checkpoint.pool) ~ '^0x[0-9a-f]{40}$'
    ORDER BY pool_address
    LIMIT (SELECT evidence_limit + 1 FROM settings)
),
pool_count AS (
    SELECT count(*)::bigint AS row_count
    FROM bounded_pools
)
SELECT jsonb_build_object(
           'record_type', 'overflow',
           'source', 'pool_state_checkpoints',
           'row_count', pool_count.row_count::text,
           'configured_limit', settings.evidence_limit::text
       )::text
FROM pool_count
CROSS JOIN settings
WHERE pool_count.row_count > settings.evidence_limit;

SELECT jsonb_build_object(
           'record_type', 'data_availability',
           'feed_gap_overlap_status', 'unavailable_not_persisted',
           'feed_gap_overlap_events', NULL,
           'feed_gap_observed_events', NULL
       )::text;

COMMIT;
