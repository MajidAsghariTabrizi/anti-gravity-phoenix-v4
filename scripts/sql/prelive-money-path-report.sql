\set ON_ERROR_STOP on

BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;

WITH params AS (
    SELECT now() AS generated_at,
           now() - make_interval(hours => :window_hours::integer) AS window_start
),
relation_names(name) AS (
    VALUES
        ('engine_outbox'),
        ('fork_simulation_results'),
        ('rpc_quality_records'),
        ('shadow_engine_classifications'),
        ('shadow_engine_processing_attempts'),
        ('shadow_profitability_facts')
),
relation_sizes AS (
    SELECT jsonb_agg(
               jsonb_build_object(
                   'name', name,
                   'size_bytes', pg_total_relation_size(to_regclass(name))::text
               )
               ORDER BY name
           ) AS value
    FROM relation_names
),
engine AS (
    SELECT jsonb_build_object(
               'classifications_total', count(*)::text,
               'candidate_count', coalesce(sum(candidate_count), 0)::text,
               'decisions_total', coalesce(sum(decision_count), 0)::text,
               'redeliveries_total', coalesce(sum(greatest(delivery_attempts - 1, 0)), 0)::text,
               'dependency_exhausted_total', count(*) FILTER (
                   WHERE classification = 'dependency_exhausted'
               )::text,
               'terminal_integrity_total', count(*) FILTER (
                   WHERE classification = 'terminal_integrity_failure'
               )::text,
               'processing_latency_ns_sum', coalesce(sum(processing_latency_ns), 0)::text,
               'processing_latency_ns_max', coalesce(max(processing_latency_ns), 0)::text
           ) AS value
    FROM shadow_engine_classifications, params
    WHERE classified_at >= params.window_start
),
attempts AS (
    SELECT jsonb_build_object(
               'processing_attempts_total', count(*)::text
           ) AS value
    FROM shadow_engine_processing_attempts, params
    WHERE completed_at >= params.window_start
),
outbox_counts AS (
    SELECT count(*) FILTER (WHERE published_at IS NULL) AS pending_rows,
           count(*) FILTER (
               WHERE created_at < (SELECT window_start FROM params)
                 AND (
                     published_at IS NULL
                     OR published_at >= (SELECT window_start FROM params)
                 )
           ) AS pending_at_window_start,
           count(*) FILTER (
               WHERE published_at >= (SELECT window_start FROM params)
           ) AS published_in_window,
           count(*) FILTER (
               WHERE created_at >= (SELECT window_start FROM params)
                 AND publish_attempts > 1
           ) AS retry_rows,
           coalesce(sum(publish_attempts) FILTER (
               WHERE created_at >= (SELECT window_start FROM params)
           ), 0) AS publish_attempts_total,
           coalesce(
               floor(extract(epoch FROM now() - min(created_at)) FILTER (
                   WHERE published_at IS NULL
               )),
               0
           )::bigint AS oldest_pending_age_seconds
    FROM engine_outbox
),
outbox AS (
    SELECT jsonb_build_object(
               'pending_rows', pending_rows::text,
               'pending_at_window_start', pending_at_window_start::text,
               'backlog_growth', greatest(pending_rows - pending_at_window_start, 0)::text,
               'published_in_window', published_in_window::text,
               'retry_rows', retry_rows::text,
               'publish_attempts_total', publish_attempts_total::text,
               'oldest_pending_age_seconds', oldest_pending_age_seconds::text
           ) AS value
    FROM outbox_counts
),
rpc AS (
    SELECT jsonb_build_object(
               'records_total', count(*)::text,
               'success_total', count(*) FILTER (WHERE success)::text,
               'timeouts_total', count(*) FILTER (WHERE timeout)::text,
               'stale_total', count(*) FILTER (WHERE stale_result)::text,
               'disagreements_total', count(*) FILTER (WHERE disagreement)::text,
               'retries_total', coalesce(sum(retry_count), 0)::text,
               'latency_ns_sum', coalesce(sum(latency_ns), 0)::text,
               'latency_ns_max', coalesce(max(latency_ns), 0)::text
           ) AS value
    FROM rpc_quality_records, params
    WHERE recorded_at >= params.window_start
),
profitability_reasons AS (
    SELECT coalesce(
               jsonb_agg(
                   jsonb_build_object('reason', reason, 'count', reason_count::text)
                   ORDER BY reason_count DESC, reason
               ),
               '[]'::jsonb
           ) AS value
    FROM (
        SELECT final_rejection_reason AS reason, count(*) AS reason_count
        FROM shadow_profitability_facts, params
        WHERE evaluated_at >= params.window_start
          AND final_rejection_reason IS NOT NULL
        GROUP BY final_rejection_reason
        ORDER BY reason_count DESC, reason
        LIMIT :reason_limit
    ) AS bounded_reasons
),
profitability AS (
    SELECT jsonb_build_object(
               'facts_total', count(*)::text,
               'complete_total', count(*) FILTER (
                   WHERE evidence_completeness_status = 'complete'
               )::text,
               'profitable_total', count(*) FILTER (
                   WHERE primary_profitability_status = 'meets_minimum'
               )::text,
               'not_profitable_total', count(*) FILTER (
                   WHERE primary_profitability_status = 'below_minimum'
               )::text,
               'incomplete_total', count(*) FILTER (
                   WHERE primary_profitability_status = 'incomplete'
               )::text,
               'near_profitable_total', count(*) FILTER (
                   WHERE expected_net_pnl > 0
                     AND minimum_required_net_pnl > 0
                     AND expected_net_pnl < minimum_required_net_pnl
                     AND expected_net_pnl * 2 >= minimum_required_net_pnl
               )::text,
               'accepted_total', count(*) FILTER (WHERE disposition = 'accepted')::text,
               'rejected_total', count(*) FILTER (WHERE disposition = 'rejected')::text,
               'sum_expected_net_pnl', coalesce(sum(expected_net_pnl), 0)::text,
               'sum_conservative_net_pnl', coalesce(sum(conservative_net_pnl), 0)::text,
               'sum_severe_net_pnl', coalesce(sum(severe_net_pnl), 0)::text,
               'sum_total_cost', coalesce(sum(total_cost), 0)::text,
               'rejection_reasons', (SELECT value FROM profitability_reasons)
           ) AS value
    FROM shadow_profitability_facts, params
    WHERE evaluated_at >= params.window_start
),
fork AS (
    SELECT jsonb_build_object(
               'simulations_total', count(*)::text,
               'passed_total', count(*) FILTER (WHERE status = 'passed')::text,
               'reverted_total', count(*) FILTER (WHERE status = 'reverted')::text,
               'simulated_profitable_total', count(*) FILTER (
                   WHERE status = 'passed' AND simulated_net_pnl > 0
               )::text,
               'simulated_not_profitable_total', count(*) FILTER (
                   WHERE status = 'passed' AND simulated_net_pnl <= 0
               )::text,
               'prediction_error_negative_total', count(*) FILTER (
                   WHERE status = 'passed' AND prediction_error < 0
               )::text,
               'prediction_error_non_negative_total', count(*) FILTER (
                   WHERE status = 'passed' AND prediction_error >= 0
               )::text,
               'gas_utilization_at_most_50_total', count(*) FILTER (
                   WHERE status = 'passed' AND gas_used * 100 <= gas_estimate * 50
               )::text,
               'gas_utilization_at_most_90_total', count(*) FILTER (
                   WHERE status = 'passed'
                     AND gas_used * 100 > gas_estimate * 50
                     AND gas_used * 100 <= gas_estimate * 90
               )::text,
               'gas_utilization_over_90_total', count(*) FILTER (
                   WHERE status = 'passed' AND gas_used * 100 > gas_estimate * 90
               )::text,
               'sum_absolute_prediction_error', coalesce(sum(abs(prediction_error)), 0)::text,
               'sum_gas_used', coalesce(sum(gas_used), 0)::text
           ) AS value
    FROM fork_simulation_results, params
    WHERE simulated_at >= params.window_start
)
SELECT jsonb_build_object(
           'schema_version', 'phoenix.prelive.money-path-source.v1',
           'generated_at', to_char(
               params.generated_at AT TIME ZONE 'UTC',
               'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'
           ),
           'window_hours', :window_hours::text,
           'mode', 'SHADOW',
           'live_execution', false,
           'execution_eligible', false,
           'execution_request_created', false,
           'database', jsonb_build_object(
               'size_bytes', pg_database_size(current_database())::text,
               'relations', relation_sizes.value
           ),
           'engine', engine.value || attempts.value,
           'outbox', outbox.value,
           'rpc', rpc.value,
           'profitability', profitability.value,
           'fork', fork.value
       )::text
FROM params, relation_sizes, engine, attempts, outbox, rpc, profitability, fork;

COMMIT;
