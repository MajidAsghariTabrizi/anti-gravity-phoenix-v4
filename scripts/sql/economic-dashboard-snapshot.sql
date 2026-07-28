\set ON_ERROR_STOP on

BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;

WITH params AS (
    SELECT now() AS generated_at, now() - interval '1 hour' AS window_start
),
funnel AS (
    SELECT
        (SELECT count(*) FROM feed_events event, params
         WHERE event.recorded_at >= params.window_start) AS normalized_inputs,
        (SELECT count(*) FROM shadow_engine_classifications classification, params
         WHERE classification.classified_at >= params.window_start) AS engine_inputs,
        (SELECT coalesce(sum(classification.candidate_count), 0)
         FROM shadow_engine_classifications classification, params
         WHERE classification.classified_at >= params.window_start) AS relevant_inputs,
        (SELECT count(*) FROM shadow_engine_classifications classification, params
         WHERE classification.classified_at >= params.window_start
           AND classification.classification NOT IN (
               'malformed_internal_event',
               'unsupported_schema',
               'terminal_integrity_failure'
           )) AS valid_inputs,
        (SELECT count(*) FROM shadow_decisions decision, params
         WHERE decision.created_at >= params.window_start) AS route_evaluations,
        (SELECT count(*) FROM shadow_decisions decision, params
         WHERE decision.created_at >= params.window_start
           AND decision.disposition = 'accepted') AS primary_profitable,
        (SELECT count(*) FROM rpc_quality_records quality, params
         WHERE quality.recorded_at >= params.window_start) AS rpc_requests,
        (SELECT count(*) FROM rpc_quality_records quality, params
         WHERE quality.recorded_at >= params.window_start
           AND quality.disagreement) AS rpc_disagreements,
        (SELECT count(*) FROM fork_simulation_results result, params
         WHERE result.simulated_at >= params.window_start) AS fork_attempts,
        (SELECT count(*) FROM fork_simulation_results result, params
         WHERE result.simulated_at >= params.window_start
           AND result.status = 'passed') AS fork_passes,
        (SELECT count(*) FROM live_canary.execution_requests request, params
         WHERE request.created_at >= params.window_start) AS execution_requests,
        (SELECT count(*) FROM live_canary.execution_attempts attempt, params
         WHERE attempt.created_at >= params.window_start) AS attempts,
        (SELECT count(*) FROM live_canary.execution_attempts attempt, params
         WHERE attempt.submitted_at >= params.window_start) AS submissions,
        (SELECT count(*) FROM live_canary.autonomous_outcome_attributions outcome, params
         WHERE outcome.attributed_at >= params.window_start) AS realized_outcomes
),
rejections AS (
    SELECT coalesce(
        jsonb_object_agg(reason, count ORDER BY reason),
        '{}'::jsonb
    ) AS values
    FROM (
        SELECT coalesce(classification.detail_class, classification.classification) AS reason,
               count(*)::bigint AS count
        FROM shadow_engine_classifications classification, params
        WHERE classification.classified_at >= params.window_start
          AND classification.classification IN (
              'candidate_rejected',
              'malformed_internal_event',
              'unsupported_schema',
              'transient_dependency_failure',
              'terminal_integrity_failure'
          )
        GROUP BY coalesce(classification.detail_class, classification.classification)
    ) grouped
),
safety AS (
    SELECT
        (SELECT count(*) FROM live_canary.execution_attempts
         WHERE status IN (
             'claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'
         )) AS active_attempts,
        (SELECT count(*) FROM live_canary.execution_attempts
         WHERE status = 'submission_unknown') AS unknown_submissions,
        (SELECT count(*) FROM live_canary.execution_outcomes
         WHERE outcome_status = 'reverted') AS reverted_outcomes,
        (SELECT count(*) FROM live_canary.autonomous_candidates
         WHERE status = 'submission_unknown') AS candidate_unknown_submissions,
        (SELECT count(*) FROM live_canary.autonomous_candidates
         WHERE status = 'integrity_failure') AS integrity_failures
),
level_profit AS (
    SELECT coalesce(
        jsonb_object_agg(
            coalesce(input_size_level, 'HISTORICAL'),
            jsonb_build_object(
                'reconciled_outcomes', reconciled_outcomes::text,
                'successful_outcomes', successful_outcomes::text,
                'realized_net_pnl_wei', realized_net_pnl::text,
                'gas_cost_wei', gas_cost::text,
                'flash_fees_wei', flash_fees::text
            )
            ORDER BY coalesce(input_size_level, 'HISTORICAL')
        ),
        '{}'::jsonb
    ) AS values
    FROM live_canary.realized_profit_by_route_level
),
snapshot AS (
    SELECT jsonb_build_object(
        'schema', 'phoenix.economic-dashboard.v1',
        'generated_at', to_char(params.generated_at AT TIME ZONE 'UTC',
            'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
        'refresh_interval_seconds', 45,
        'executive', jsonb_build_object(
            'current_release', control.release_sha,
            'phase', control.phase,
            'armed', global_control.armed,
            'kill_switch', global_control.kill_switch,
            'current_size_level', control.current_size_level,
            'current_input_wei', control.current_input_wei::text,
            'realized_net_pnl_today_wei', profit.realized_net_pnl_today::text,
            'realized_net_pnl_7d_wei', profit.realized_net_pnl_7d::text,
            'realized_net_pnl_30d_wei', profit.realized_net_pnl_30d::text,
            'active_route', control.route_fingerprint,
            'next_promotion_gate', '20 reconciled outcomes; positive net PnL; all safety gates',
            'last_transition_reason', control.last_transition_reason
        ),
        'funnel', jsonb_build_object(
            'window_start', to_char(params.window_start AT TIME ZONE 'UTC',
                'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
            'normalized_inputs', funnel.normalized_inputs::text,
            'relevant_inputs', funnel.relevant_inputs::text,
            'engine_inputs', funnel.engine_inputs::text,
            'valid_inputs', funnel.valid_inputs::text,
            'route_evaluations', funnel.route_evaluations::text,
            'primary_profitable', funnel.primary_profitable::text,
            'rpc_requests', funnel.rpc_requests::text,
            'rpc_disagreements', funnel.rpc_disagreements::text,
            'fork_attempts', funnel.fork_attempts::text,
            'fork_passes', funnel.fork_passes::text,
            'execution_requests', funnel.execution_requests::text,
            'attempts', funnel.attempts::text,
            'submissions', funnel.submissions::text,
            'realized_outcomes', funnel.realized_outcomes::text,
            'rejection_reasons', rejections.values
        ),
        'economics', jsonb_build_object(
            'north_star', 'REALIZED NET PNL AFTER ALL COSTS',
            'realized_net_pnl_today_wei', profit.realized_net_pnl_today::text,
            'realized_net_pnl_7d_wei', profit.realized_net_pnl_7d::text,
            'realized_net_pnl_30d_wei', profit.realized_net_pnl_30d::text,
            'gross_profit_wei', profit.gross_profit::text,
            'gas_cost_wei', profit.gas_cost::text,
            'flash_fees_wei', profit.flash_fees::text,
            'reconciled_outcomes', profit.reconciled_outcomes::text,
            'by_size_level', level_profit.values
        ),
        'safety', jsonb_build_object(
            'global_armed', global_control.armed,
            'global_kill_switch', global_control.kill_switch,
            'route_enabled', route_control.enabled,
            'route_kill_switch', route_control.kill_switch,
            'active_attempts', safety.active_attempts::text,
            'unknown_submissions', safety.unknown_submissions::text,
            'candidate_unknown_submissions', safety.candidate_unknown_submissions::text,
            'reverted_outcomes', safety.reverted_outcomes::text,
            'integrity_failures', safety.integrity_failures::text,
            'cooldown_until', control.cooldown_until
        ),
        'growth', jsonb_build_object(
            'maximum_reviewed_input_wei', control.maximum_reviewed_input_wei::text,
            'current_operating_input_wei', control.current_input_wei::text,
            'current_size_level', control.current_size_level,
            'promotion_minimum_outcomes', 20,
            'promotion_minimum_success_rate_bps', 9500,
            'promotion_minimum_fork_pass_rate_bps', 9500,
            'promotion_maximum_prediction_error_bps', 1000,
            'route_expansion_authorized', false
        )
    ) AS document
    FROM params
    CROSS JOIN funnel
    CROSS JOIN rejections
    CROSS JOIN safety
    CROSS JOIN level_profit
    CROSS JOIN live_canary.realized_profit_windows profit
    CROSS JOIN live_canary.economic_control control
    CROSS JOIN live_canary.autonomous_global_control global_control
    LEFT JOIN live_canary.autonomous_route_controls route_control
      ON route_control.route_fingerprint = control.route_fingerprint
    WHERE control.singleton AND global_control.singleton
)
SELECT document::text FROM snapshot;

COMMIT;
