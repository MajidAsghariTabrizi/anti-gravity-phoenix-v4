\set ON_ERROR_STOP on

BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;

WITH requested_window AS (
    SELECT now() AS generated_at,
           now() - make_interval(hours => :window_hours::integer) AS window_start
),
params AS (
    SELECT generated_at,
           greatest(window_start, :'evidence_start'::timestamptz) AS window_start
    FROM requested_window
),
complete_facts AS (
    SELECT fact.*
    FROM shadow_profitability_facts AS fact, params
    WHERE fact.evaluated_at >= params.window_start
      AND fact.evidence_completeness_status = 'complete'
      AND fact.route_fingerprint IS NOT NULL
),
ranked_routes AS (
    SELECT fact.route_fingerprint,
           count(*) AS sample_count,
           count(*) FILTER (
               WHERE fact.primary_profitability_status = 'meets_minimum'
           ) AS primary_profitable_count,
           count(*) FILTER (
               WHERE fact.independent_verification_status = 'agreed'
           ) AS independently_verified_count,
           count(*) FILTER (
               WHERE fact.independent_verification_status = 'disagreed'
           ) AS verification_disagreed_count,
           count(*) FILTER (
               WHERE fact.independent_verification_status IN (
                   'provider_unavailable',
                   'integrity_failure'
               )
           ) AS verification_unavailable_count,
           coalesce(sum(fact.gross_spread), 0) AS gross_profit,
           coalesce(sum(fact.total_cost), 0) AS total_cost,
           coalesce(sum(fact.arbitrum_execution_fee + fact.l1_data_fee), 0) AS gas_cost,
           coalesce(sum(fact.flash_loan_premium), 0) AS flash_premium,
           coalesce(sum(fact.ordering_reserve), 0) AS ordering_cost,
           coalesce(sum(
               fact.total_cost
               - fact.arbitrum_execution_fee
               - fact.l1_data_fee
               - fact.flash_loan_premium
               - fact.ordering_reserve
           ), 0) AS safety_cost,
           coalesce(sum(fact.expected_net_pnl), 0) AS expected,
           coalesce(sum(fact.conservative_net_pnl), 0) AS conservative,
           coalesce(sum(fact.severe_net_pnl), 0) AS severe,
           min(fact.minimum_required_net_pnl - fact.expected_net_pnl) FILTER (
               WHERE fact.expected_net_pnl > 0
                 AND fact.expected_net_pnl < fact.minimum_required_net_pnl
           ) AS minimum_shortfall,
           min(fact.evaluated_at) AS first_observed_at,
           max(fact.evaluated_at) AS last_observed_at
    FROM complete_facts AS fact
    GROUP BY fact.route_fingerprint
    ORDER BY count(*) DESC,
             coalesce(sum(fact.expected_net_pnl), 0) DESC,
             fact.route_fingerprint
    LIMIT 10
),
selected_facts AS (
    SELECT fact.*
    FROM complete_facts AS fact
    JOIN ranked_routes AS route
      ON route.route_fingerprint = fact.route_fingerprint
),
fork_by_fact AS (
    SELECT result.shadow_decision_id,
           count(*) AS unsigned_plans,
           count(*) AS simulations,
           count(*) FILTER (WHERE result.status = 'passed') AS success,
           count(*) FILTER (WHERE result.status = 'reverted') AS reverted,
           count(*) FILTER (
               WHERE result.status = 'passed' AND result.simulated_net_pnl > 0
           ) AS profitable,
           coalesce(sum(result.gas_used) FILTER (WHERE result.status = 'passed'), 0) AS gas_used,
           coalesce(sum(result.simulated_balance_delta) FILTER (WHERE result.status = 'passed'), 0) AS balance_delta,
           coalesce(sum(result.simulated_net_pnl) FILTER (WHERE result.status = 'passed'), 0) AS simulated_net_pnl,
           coalesce(sum(abs(result.prediction_error)) FILTER (WHERE result.status = 'passed'), 0) AS absolute_prediction_error,
           coalesce(max(result.fork_block_number), 0) AS fork_block,
           count(*) FILTER (
               WHERE NOT result.fork_only
                  OR NOT result.shadow_only
                  OR result.live_execution
                  OR result.execution_eligible
                  OR result.execution_request_created
                  OR result.public_broadcast
                  OR result.signer_used
           ) AS guard_failures
    FROM fork_simulation_results AS result
    JOIN selected_facts AS fact
      ON fact.shadow_decision_id = result.shadow_decision_id
    CROSS JOIN params
    WHERE result.simulated_at >= params.window_start
    GROUP BY result.shadow_decision_id
),
route_fork AS (
    SELECT fact.route_fingerprint,
           coalesce(sum(fork.unsigned_plans), 0) AS unsigned_plans,
           coalesce(sum(fork.simulations), 0) AS simulations,
           coalesce(sum(fork.success), 0) AS success,
           coalesce(sum(fork.reverted), 0) AS reverted,
           coalesce(sum(fork.profitable), 0) AS profitable,
           coalesce(sum(fork.gas_used), 0) AS gas_used,
           coalesce(sum(fork.balance_delta), 0) AS balance_delta,
           coalesce(sum(fork.simulated_net_pnl), 0) AS simulated_net_pnl,
           coalesce(sum(fork.absolute_prediction_error), 0) AS absolute_prediction_error,
           coalesce(max(fork.fork_block), 0) AS fork_block,
           coalesce(sum(fork.guard_failures), 0) AS guard_failures
    FROM selected_facts AS fact
    LEFT JOIN fork_by_fact AS fork
      ON fork.shadow_decision_id = fact.shadow_decision_id
    GROUP BY fact.route_fingerprint
),
route_rpc AS (
    SELECT fact.route_fingerprint,
           count(quality.id) AS requests,
           count(quality.id) FILTER (WHERE NOT quality.success) AS failures
    FROM selected_facts AS fact
    LEFT JOIN rpc_quality_records AS quality
      ON quality.shadow_decision_id = fact.shadow_decision_id
     AND quality.recorded_at >= (SELECT window_start FROM params)
    GROUP BY fact.route_fingerprint
),
route_rows AS (
    SELECT route.route_fingerprint,
           route.sample_count,
           route.primary_profitable_count,
           route.independently_verified_count,
           route.verification_disagreed_count,
           route.verification_unavailable_count,
           route.gross_profit,
           route.total_cost,
           route.gas_cost,
           route.flash_premium,
           route.ordering_cost,
           route.safety_cost,
           route.expected,
           route.conservative,
           route.severe,
           route.minimum_shortfall,
           route.first_observed_at,
           route.last_observed_at,
           rpc.requests AS provider_requests,
           rpc.failures AS provider_failures,
           fork.unsigned_plans,
           fork.simulations,
           fork.success,
           fork.reverted,
           fork.profitable,
           fork.gas_used,
           fork.balance_delta,
           fork.simulated_net_pnl,
           fork.absolute_prediction_error,
           fork.fork_block,
           fork.guard_failures
    FROM ranked_routes AS route
    JOIN route_fork AS fork USING (route_fingerprint)
    JOIN route_rpc AS rpc USING (route_fingerprint)
),
canonical_route_rows AS (
    SELECT route.*,
           CASE WHEN route.gross_profit = trunc(route.gross_profit)
                THEN route.gross_profit::numeric(78,0)::text END AS gross_profit_text,
           CASE WHEN route.total_cost = trunc(route.total_cost)
                THEN route.total_cost::numeric(78,0)::text END AS total_cost_text,
           CASE WHEN route.gas_cost = trunc(route.gas_cost)
                THEN route.gas_cost::numeric(78,0)::text END AS gas_cost_text,
           CASE WHEN route.flash_premium = trunc(route.flash_premium)
                THEN route.flash_premium::numeric(78,0)::text END AS flash_premium_text,
           CASE WHEN route.ordering_cost = trunc(route.ordering_cost)
                THEN route.ordering_cost::numeric(78,0)::text END AS ordering_cost_text,
           CASE WHEN route.safety_cost = trunc(route.safety_cost)
                THEN route.safety_cost::numeric(78,0)::text END AS safety_cost_text,
           CASE WHEN route.expected = trunc(route.expected)
                THEN route.expected::numeric(78,0)::text END AS expected_text,
           CASE WHEN route.conservative = trunc(route.conservative)
                THEN route.conservative::numeric(78,0)::text END AS conservative_text,
           CASE WHEN route.severe = trunc(route.severe)
                THEN route.severe::numeric(78,0)::text END AS severe_text,
           CASE WHEN route.minimum_shortfall = trunc(route.minimum_shortfall)
                THEN route.minimum_shortfall::numeric(78,0)::text END AS minimum_shortfall_text,
           CASE WHEN route.gas_used = trunc(route.gas_used)
                THEN route.gas_used::numeric(78,0)::text END AS gas_used_text,
           CASE WHEN route.balance_delta = trunc(route.balance_delta)
                THEN route.balance_delta::numeric(78,0)::text END AS balance_delta_text,
           CASE WHEN route.simulated_net_pnl = trunc(route.simulated_net_pnl)
                THEN route.simulated_net_pnl::numeric(78,0)::text END AS simulated_net_pnl_text,
           CASE WHEN route.absolute_prediction_error = trunc(route.absolute_prediction_error)
                THEN route.absolute_prediction_error::numeric(78,0)::text END AS absolute_prediction_error_text,
           CASE WHEN route.fork_block = trunc(route.fork_block)
                THEN route.fork_block::numeric(78,0)::text END AS fork_block_text
    FROM route_rows AS route
),
provider_roles AS (
    SELECT fact.shadow_decision_id, fact.primary_provider_id AS provider_key, 'primary'::text AS role
    FROM selected_facts AS fact
    WHERE fact.primary_provider_id IS NOT NULL
    UNION ALL
    SELECT fact.shadow_decision_id, fact.secondary_provider_id, 'secondary'::text
    FROM selected_facts AS fact
    WHERE fact.secondary_provider_id IS NOT NULL
),
provider_rows AS (
    SELECT role.provider_key,
           role.role,
           count(quality.id) AS requests,
           count(quality.id) FILTER (WHERE quality.success) AS success,
           count(quality.id) FILTER (WHERE quality.timeout) AS timeouts,
           count(quality.id) FILTER (
               WHERE NOT quality.success AND NOT quality.timeout
           ) AS unavailable,
           round(percentile_cont(0.50) WITHIN GROUP (ORDER BY quality.latency_ns) / 1000000.0)::bigint AS p50_latency_ms,
           round(percentile_cont(0.95) WITHIN GROUP (ORDER BY quality.latency_ns) / 1000000.0)::bigint AS p95_latency_ms,
           round(percentile_cont(0.99) WITHIN GROUP (ORDER BY quality.latency_ns) / 1000000.0)::bigint AS p99_latency_ms
    FROM provider_roles AS role
    JOIN rpc_quality_records AS quality
      ON quality.shadow_decision_id = role.shadow_decision_id
     AND quality.provider_id = role.provider_key
     AND quality.recorded_at >= (SELECT window_start FROM params)
    GROUP BY role.provider_key, role.role
    ORDER BY role.role, count(quality.id) DESC, role.provider_key
    LIMIT 8
),
selected_enriched AS (
    SELECT fact.*,
           coalesce(fork.simulated_net_pnl, 0) AS fork_simulated_net_pnl,
           coalesce(fork.absolute_prediction_error, 0) AS fork_absolute_prediction_error
    FROM selected_facts AS fact
    LEFT JOIN fork_by_fact AS fork
      ON fork.shadow_decision_id = fact.shadow_decision_id
),
distribution_rows AS (
    SELECT scenario, bucket, count(*) AS count
    FROM (
        SELECT 'expected'::text AS scenario,
               CASE WHEN expected_net_pnl > 0 THEN 'positive'
                    WHEN expected_net_pnl = 0 THEN 'near_zero'
                    ELSE 'negative' END AS bucket
        FROM selected_facts
        UNION ALL
        SELECT 'conservative',
               CASE WHEN conservative_net_pnl > 0 THEN 'positive'
                    WHEN conservative_net_pnl = 0 THEN 'near_zero'
                    ELSE 'negative' END
        FROM selected_facts
        UNION ALL
        SELECT 'severe',
               CASE WHEN severe_net_pnl > 0 THEN 'positive'
                    WHEN severe_net_pnl = 0 THEN 'near_zero'
                    ELSE 'negative' END
        FROM selected_facts
        UNION ALL
        SELECT 'fork_simulated',
               CASE WHEN result.status = 'reverted' THEN 'reverted'
                    WHEN result.simulated_net_pnl > 0 THEN 'positive'
                    ELSE 'negative' END
        FROM fork_simulation_results AS result
        JOIN selected_facts AS fact
          ON fact.shadow_decision_id = result.shadow_decision_id
        WHERE result.simulated_at >= (SELECT window_start FROM params)
    ) AS samples
    GROUP BY scenario, bucket
),
prediction_rows AS (
    SELECT CASE WHEN result.status = 'reverted' THEN 'reverted'
                WHEN abs(result.prediction_error) < 100 THEN 'under_100'
                WHEN abs(result.prediction_error) <= 500 THEN '100_to_500'
                ELSE 'over_500' END AS bucket,
           count(*) AS count
    FROM fork_simulation_results AS result
    JOIN selected_facts AS fact
      ON fact.shadow_decision_id = result.shadow_decision_id
    WHERE result.simulated_at >= (SELECT window_start FROM params)
    GROUP BY bucket
),
daily_rows AS (
    SELECT to_char(fact.evaluated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS period,
           sum(fact.expected_net_pnl) AS expected,
           sum(fact.conservative_net_pnl) AS conservative,
           sum(fact.severe_net_pnl) AS severe,
           sum(fact.fork_simulated_net_pnl) AS fork_simulated,
           count(*) AS sample_count
    FROM selected_enriched AS fact
    GROUP BY period
    ORDER BY period
    LIMIT 31
),
weekly_rows AS (
    SELECT to_char(fact.evaluated_at AT TIME ZONE 'UTC', 'IYYY-"W"IW') AS period,
           sum(fact.expected_net_pnl) AS expected,
           sum(fact.conservative_net_pnl) AS conservative,
           sum(fact.severe_net_pnl) AS severe,
           sum(fact.fork_simulated_net_pnl) AS fork_simulated,
           count(*) AS sample_count
    FROM selected_enriched AS fact
    GROUP BY period
    ORDER BY period
    LIMIT 12
),
model_rows AS (
    SELECT fact.model_version,
           count(*) AS sample_count,
           sum(fact.expected_net_pnl) AS expected,
           sum(fact.conservative_net_pnl) AS conservative,
           sum(fact.fork_absolute_prediction_error) AS absolute_fork_error
    FROM selected_enriched AS fact
    WHERE fact.model_version IS NOT NULL
    GROUP BY fact.model_version
    ORDER BY count(*) DESC, fact.model_version
    LIMIT 10
),
canonical_daily_rows AS (
    SELECT daily.*,
           CASE WHEN daily.expected = trunc(daily.expected)
                THEN daily.expected::numeric(78,0)::text END AS expected_text,
           CASE WHEN daily.conservative = trunc(daily.conservative)
                THEN daily.conservative::numeric(78,0)::text END AS conservative_text,
           CASE WHEN daily.severe = trunc(daily.severe)
                THEN daily.severe::numeric(78,0)::text END AS severe_text,
           CASE WHEN daily.fork_simulated = trunc(daily.fork_simulated)
                THEN daily.fork_simulated::numeric(78,0)::text END AS fork_simulated_text
    FROM daily_rows AS daily
),
canonical_weekly_rows AS (
    SELECT weekly.*,
           CASE WHEN weekly.expected = trunc(weekly.expected)
                THEN weekly.expected::numeric(78,0)::text END AS expected_text,
           CASE WHEN weekly.conservative = trunc(weekly.conservative)
                THEN weekly.conservative::numeric(78,0)::text END AS conservative_text,
           CASE WHEN weekly.severe = trunc(weekly.severe)
                THEN weekly.severe::numeric(78,0)::text END AS severe_text,
           CASE WHEN weekly.fork_simulated = trunc(weekly.fork_simulated)
                THEN weekly.fork_simulated::numeric(78,0)::text END AS fork_simulated_text
    FROM weekly_rows AS weekly
),
canonical_model_rows AS (
    SELECT model.*,
           CASE WHEN model.expected = trunc(model.expected)
                THEN model.expected::numeric(78,0)::text END AS expected_text,
           CASE WHEN model.conservative = trunc(model.conservative)
                THEN model.conservative::numeric(78,0)::text END AS conservative_text,
           CASE WHEN model.absolute_fork_error = trunc(model.absolute_fork_error)
                THEN model.absolute_fork_error::numeric(78,0)::text END AS absolute_fork_error_text
    FROM model_rows AS model
),
database_stats AS (
    SELECT pg_database_size(current_database()) AS size_bytes,
           (SELECT count(*) FROM pg_stat_activity WHERE datname = current_database()) AS active_connections,
           coalesce((to_jsonb(bgwriter)->>'checkpoints_timed')::bigint, 0) AS checkpoints_timed,
           coalesce((to_jsonb(bgwriter)->>'checkpoints_req')::bigint, 0) AS checkpoints_requested,
           coalesce((SELECT wal_bytes::numeric FROM pg_stat_wal), 0) AS wal_bytes,
           (SELECT min(evaluated_at) FROM selected_facts) AS oldest_relevant_event,
           (SELECT max(evaluated_at) FROM selected_facts) AS newest_relevant_event,
           (SELECT version FROM schema_migrations ORDER BY applied_at DESC, version DESC LIMIT 1) AS migration_version,
           (SELECT checksum FROM schema_migrations ORDER BY applied_at DESC, version DESC LIMIT 1) AS migration_checksum
    FROM pg_stat_bgwriter AS bgwriter
),
canonical_database_stats AS (
    SELECT database_stats.*,
           CASE WHEN database_stats.wal_bytes = trunc(database_stats.wal_bytes)
                THEN database_stats.wal_bytes::numeric(78,0)::text END AS wal_bytes_text
    FROM database_stats
),
route_registry_stats AS (
    SELECT count(*) AS fact_count,
           count(*) FILTER (
               WHERE fact.route_config_hash IS NULL
                  OR fact.route_config_hash <> :'route_hash'
                  OR (
                      fact.secondary_route_config_hash IS NOT NULL
                      AND fact.secondary_route_config_hash <> :'route_hash'
                  )
           ) AS mismatch_count,
           count(*) FILTER (
               WHERE fact.primary_provider_id IS NOT NULL
                 AND fact.secondary_provider_id IS NOT NULL
                 AND fact.primary_provider_id = fact.secondary_provider_id
           ) AS self_verification_collisions
    FROM complete_facts AS fact
)
SELECT jsonb_build_object(
           'schema_version', 'phoenix.prelive.dashboard-source.v1',
           'generated_at', to_char(params.generated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
           'database_clock', to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
           'evidence_window_started_at', to_char(params.window_start AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
           'window_hours', :window_hours::text,
           'database', jsonb_build_object(
               'size_bytes', database_stats.size_bytes::text,
               'active_connections', database_stats.active_connections::text,
               'checkpoints_timed', database_stats.checkpoints_timed::text,
               'checkpoints_requested', database_stats.checkpoints_requested::text,
               'wal_bytes', database_stats.wal_bytes_text,
               'oldest_relevant_event', CASE WHEN database_stats.oldest_relevant_event IS NULL THEN NULL ELSE to_char(database_stats.oldest_relevant_event AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') END,
               'newest_relevant_event', CASE WHEN database_stats.newest_relevant_event IS NULL THEN NULL ELSE to_char(database_stats.newest_relevant_event AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') END,
               'migration_version', database_stats.migration_version,
               'migration_checksum', database_stats.migration_checksum,
               'retention_status', 'not_configured'
           ),
           'route_registry', jsonb_build_object(
               'fact_count', route_registry_stats.fact_count::text,
               'mismatch_count', route_registry_stats.mismatch_count::text,
               'self_verification_collisions', route_registry_stats.self_verification_collisions::text
           ),
           'routes', coalesce((
               SELECT jsonb_agg(jsonb_build_object(
                   'route_key', row.route_fingerprint,
                   'sample_count', row.sample_count::text,
                   'primary_profitable_count', row.primary_profitable_count::text,
                   'independently_verified_count', row.independently_verified_count::text,
                   'verification_disagreed_count', row.verification_disagreed_count::text,
                   'verification_unavailable_count', row.verification_unavailable_count::text,
                   'gross_profit', row.gross_profit_text,
                   'total_cost', row.total_cost_text,
                   'gas_cost', row.gas_cost_text,
                   'flash_premium', row.flash_premium_text,
                   'ordering_cost', row.ordering_cost_text,
                   'safety_cost', row.safety_cost_text,
                   'expected', row.expected_text,
                   'conservative', row.conservative_text,
                   'severe', row.severe_text,
                   'minimum_shortfall', row.minimum_shortfall_text,
                   'first_observed_at', to_char(row.first_observed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                   'last_observed_at', to_char(row.last_observed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
                   'liquidity_score_bps', NULL,
                   'provider_requests', row.provider_requests::text,
                   'provider_failures', row.provider_failures::text,
                   'fork_unsigned_plans', row.unsigned_plans::text,
                   'fork_simulations', row.simulations::text,
                   'fork_success', row.success::text,
                   'fork_reverted', row.reverted::text,
                   'fork_profitable', row.profitable::text,
                   'fork_gas_used', row.gas_used_text,
                   'fork_balance_delta', row.balance_delta_text,
                   'fork_simulated_net_pnl', row.simulated_net_pnl_text,
                   'fork_absolute_prediction_error', row.absolute_prediction_error_text,
                   'fork_block', row.fork_block_text,
                   'fork_guard_failures', row.guard_failures::text
               ) ORDER BY row.sample_count DESC, row.expected DESC, row.route_fingerprint)
               FROM canonical_route_rows AS row
           ), '[]'::jsonb),
           'distribution', coalesce((
               SELECT jsonb_agg(jsonb_build_object('scenario', scenario, 'bucket', bucket, 'count', count::text) ORDER BY scenario, bucket)
               FROM distribution_rows
           ), '[]'::jsonb),
           'prediction_error', coalesce((
               SELECT jsonb_agg(jsonb_build_object('bucket', bucket, 'count', count::text) ORDER BY bucket)
               FROM prediction_rows
           ), '[]'::jsonb),
           'daily_trend', coalesce((
               SELECT jsonb_agg(jsonb_build_object('period', period, 'expected', expected_text, 'conservative', conservative_text, 'severe', severe_text, 'fork_simulated', fork_simulated_text, 'sample_count', sample_count::text) ORDER BY period)
               FROM canonical_daily_rows
           ), '[]'::jsonb),
           'weekly_trend', coalesce((
               SELECT jsonb_agg(jsonb_build_object('period', period, 'expected', expected_text, 'conservative', conservative_text, 'severe', severe_text, 'fork_simulated', fork_simulated_text, 'sample_count', sample_count::text) ORDER BY period)
               FROM canonical_weekly_rows
           ), '[]'::jsonb),
           'model_comparison', coalesce((
               SELECT jsonb_agg(jsonb_build_object('model_version', model_version, 'sample_count', sample_count::text, 'expected', expected_text, 'conservative', conservative_text, 'absolute_fork_error', absolute_fork_error_text) ORDER BY sample_count DESC, model_version)
               FROM canonical_model_rows
           ), '[]'::jsonb),
           'providers', coalesce((
               SELECT jsonb_agg(jsonb_build_object(
                   'provider_key', provider_key,
                   'role', role,
                   'requests', requests::text,
                   'success', success::text,
                   'timeouts', timeouts::text,
                   'unavailable', unavailable::text,
                   'p50_latency_ms', p50_latency_ms::text,
                   'p95_latency_ms', p95_latency_ms::text,
                   'p99_latency_ms', p99_latency_ms::text
               ) ORDER BY role, requests DESC, provider_key)
               FROM provider_rows
           ), '[]'::jsonb)
       )::text
FROM params, canonical_database_stats AS database_stats, route_registry_stats;

COMMIT;
