\set ON_ERROR_STOP on

SELECT to_regclass('public.phoenix_live_economic_truth') IS NOT NULL
    AS phoenix_has_economic_truth \gset

BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;

WITH params AS (
    SELECT now() AS generated_at
),
windows(label, window_start) AS (
    SELECT '1h', now() - interval '1 hour'
    UNION ALL
    SELECT '24h', now() - interval '24 hours'
    UNION ALL
    SELECT '7d', now() - interval '7 days'
),
window_funnel AS (
    SELECT
        evidence_window.label,
        evidence_window.window_start,
        (SELECT count(*) FROM feed_events event
         WHERE event.recorded_at >= evidence_window.window_start) AS normalized_inputs,
        (SELECT count(*) FROM shadow_engine_classifications classification
         WHERE classification.classified_at >= evidence_window.window_start) AS engine_inputs,
        (SELECT count(*) FROM shadow_engine_classifications classification
         WHERE classification.classified_at >= evidence_window.window_start
           AND classification.classification NOT IN (
               'malformed_internal_event',
               'unsupported_schema',
               'terminal_integrity_failure'
           )) AS valid_inputs,
        (SELECT coalesce(sum(
             CASE
                 WHEN jsonb_typeof(classification.evidence->'route_fingerprints') = 'array'
                 THEN jsonb_array_length(classification.evidence->'route_fingerprints')
                 ELSE classification.candidate_count
             END
         ), 0)
         FROM shadow_engine_classifications classification
         WHERE classification.classified_at >= evidence_window.window_start) AS route_matches,
        (SELECT count(*) FROM shadow_profitability_facts fact
         WHERE fact.evaluated_at >= evidence_window.window_start
           AND fact.evidence_completeness_status = 'complete') AS complete_evaluations,
        (SELECT count(*) FROM shadow_profitability_facts fact
         WHERE fact.evaluated_at >= evidence_window.window_start
           AND fact.evidence_completeness_status = 'complete'
           AND fact.primary_profitability_status = 'meets_minimum') AS primary_profitable,
        (SELECT count(*) FROM shadow_profitability_facts fact
         WHERE fact.evaluated_at >= evidence_window.window_start
           AND fact.evidence_completeness_status = 'complete'
           AND fact.primary_profitability_status = 'below_minimum'
           AND fact.conservative_net_pnl - fact.minimum_required_net_pnl
               >= -greatest(fact.minimum_required_net_pnl, 1)) AS near_profitable,
        (SELECT max(fact.conservative_net_pnl - fact.minimum_required_net_pnl)
         FROM shadow_profitability_facts fact
         WHERE fact.evaluated_at >= evidence_window.window_start
           AND fact.evidence_completeness_status = 'complete') AS closest_margin_to_gate,
        (SELECT count(*) FROM rpc_quality_records quality
         WHERE quality.recorded_at >= evidence_window.window_start) AS rpc_requests,
        (SELECT count(*) FROM rpc_quality_records quality
         WHERE quality.recorded_at >= evidence_window.window_start
           AND quality.disagreement) AS rpc_disagreements,
        (SELECT count(*) FROM shadow_engine_classifications classification
         WHERE classification.classified_at >= evidence_window.window_start
           AND classification.detail_class = 'upstream_call_budget_exhausted')
            AS rpc_budget_exhaustions,
        (SELECT count(*) FROM shadow_engine_classifications classification
         WHERE classification.classified_at >= evidence_window.window_start
           AND classification.detail_class IN (
               'shadow_model_arithmetic_failure',
               'hunter_arithmetic',
               'autonomous_integrity_failure'
           )) AS model_invariant_failures,
        (SELECT count(*) FROM fork_simulation_results result
         WHERE result.simulated_at >= evidence_window.window_start) AS fork_attempts,
        (SELECT count(*) FROM fork_simulation_results result
         WHERE result.simulated_at >= evidence_window.window_start
           AND result.status = 'passed') AS fork_passes,
        (SELECT count(*) FROM live_canary.autonomous_candidates candidate
         WHERE candidate.created_at >= evidence_window.window_start) AS candidates,
        (SELECT count(*) FROM live_canary.execution_requests request
         WHERE request.created_at >= evidence_window.window_start) AS execution_requests,
        (SELECT count(*) FROM live_canary.execution_attempts attempt
         WHERE attempt.claimed_at >= evidence_window.window_start) AS attempts,
        (SELECT count(*) FROM live_canary.execution_attempts attempt
         WHERE attempt.submitted_at >= evidence_window.window_start) AS submissions,
        (SELECT count(*) FROM live_canary.autonomous_outcome_attributions outcome
         WHERE outcome.attributed_at >= evidence_window.window_start) AS realized_outcomes
    FROM windows evidence_window
),
window_rejections AS (
    SELECT evidence_window.label, coalesce(
        jsonb_object_agg(grouped.reason, grouped.count ORDER BY grouped.count DESC)
            FILTER (WHERE grouped.reason IS NOT NULL),
        '{}'::jsonb
    ) AS values
    FROM windows evidence_window
    LEFT JOIN LATERAL (
        SELECT coalesce(
                   fact.final_rejection_reason,
                   classification.detail_class,
                   classification.classification
               ) AS reason,
               count(*)::bigint AS count
        FROM shadow_engine_classifications classification
        LEFT JOIN shadow_profitability_facts fact
          ON fact.source_event_identity = classification.source_event_identity
        WHERE classification.classified_at >= evidence_window.window_start
          AND (
              fact.disposition = 'rejected'
              OR classification.classification IN (
                  'candidate_rejected',
                  'malformed_internal_event',
                  'unsupported_schema',
                  'transient_dependency_failure',
                  'dependency_exhausted',
                  'terminal_integrity_failure'
              )
          )
        GROUP BY coalesce(
            fact.final_rejection_reason,
            classification.detail_class,
            classification.classification
        )
    ) grouped ON true
    GROUP BY evidence_window.label
),
window_documents AS (
    SELECT jsonb_object_agg(
        funnel.label,
        jsonb_build_object(
            'window_start', to_char(funnel.window_start AT TIME ZONE 'UTC',
                'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
            'normalized_inputs', funnel.normalized_inputs::text,
            'engine_inputs', funnel.engine_inputs::text,
            'valid_inputs', funnel.valid_inputs::text,
            'route_matches', funnel.route_matches::text,
            'complete_evaluations', funnel.complete_evaluations::text,
            'near_profitable', funnel.near_profitable::text,
            'closest_margin_to_gate_wei',
                coalesce(funnel.closest_margin_to_gate::text, 'not_available'),
            'primary_profitable', funnel.primary_profitable::text,
            'rpc_requests', funnel.rpc_requests::text,
            'rpc_disagreements', funnel.rpc_disagreements::text,
            'rpc_budget_exhaustions', funnel.rpc_budget_exhaustions::text,
            'model_invariant_failures', funnel.model_invariant_failures::text,
            'fork_attempts', funnel.fork_attempts::text,
            'fork_passes', funnel.fork_passes::text,
            'candidates', funnel.candidates::text,
            'execution_requests', funnel.execution_requests::text,
            'attempts', funnel.attempts::text,
            'submissions', funnel.submissions::text,
            'realized_outcomes', funnel.realized_outcomes::text,
            'top_rejection_reasons', rejection.values
        )
        ORDER BY CASE funnel.label WHEN '1h' THEN 1 WHEN '24h' THEN 2 ELSE 3 END
    ) AS values
    FROM window_funnel funnel
    JOIN window_rejections rejection USING (label)
),
size_points AS (
\if :phoenix_has_economic_truth
    SELECT
        truth.classified_at,
        truth.route_fingerprint,
        truth.input_size_wei AS input_size,
        truth.expected_net_pnl_wei AS expected_net_pnl,
        truth.conservative_net_pnl_wei AS conservative_net_pnl,
        truth.severe_net_pnl_wei AS severe_net_pnl,
        truth.margin_to_profitability_gate_wei AS margin_to_gate,
        truth.gross_spread_bps,
        truth.net_pnl_bps,
        truth.break_even_spread_bps,
        truth.fixed_cost_wei AS fixed_cost,
        truth.variable_cost_wei AS variable_cost,
        truth.price_divergence_direction,
        truth.exact_rejection_reason AS rejection_reason,
        truth.tick_crossings,
        (
            SELECT max((leg->>'utilization_bps')::numeric)
            FROM jsonb_array_elements(
                coalesce(truth.active_liquidity_near_current_tick, '[]'::jsonb)
            ) leg
            WHERE leg->>'utilization_bps' ~ '^[0-9]+$'
        ) AS maximum_liquidity_utilization_bps
    FROM phoenix_live_economic_truth truth
    WHERE truth.classified_at >= now() - interval '7 days'
\else
    SELECT
        NULL::timestamptz AS classified_at,
        NULL::text AS route_fingerprint,
        NULL::text AS input_size,
        NULL::text AS expected_net_pnl,
        NULL::text AS conservative_net_pnl,
        NULL::text AS severe_net_pnl,
        NULL::text AS margin_to_gate,
        NULL::text AS gross_spread_bps,
        NULL::text AS net_pnl_bps,
        NULL::text AS break_even_spread_bps,
        NULL::text AS fixed_cost,
        NULL::text AS variable_cost,
        NULL::text AS price_divergence_direction,
        NULL::text AS rejection_reason,
        NULL::integer AS tick_crossings,
        NULL::numeric AS maximum_liquidity_utilization_bps
    WHERE false
\endif
),
size_summary AS (
    SELECT coalesce(jsonb_agg(row_to_json(ranked)::jsonb ORDER BY ranked.route_rank, ranked.input_size), '[]'::jsonb) AS values
    FROM (
        SELECT
            route_fingerprint,
            input_size,
            count(*)::text AS evaluation_count,
            max(expected_net_pnl::numeric)::text AS best_expected_net_pnl_wei,
            max(conservative_net_pnl::numeric)::text AS best_conservative_net_pnl_wei,
            max(severe_net_pnl::numeric)::text AS best_severe_net_pnl_wei,
            max(margin_to_gate::numeric)::text AS best_margin_to_gate_wei,
            max(gross_spread_bps::numeric)::text AS best_gross_spread_bps,
            max(net_pnl_bps::numeric)::text AS best_net_pnl_bps,
            min(break_even_spread_bps::numeric)::text AS minimum_break_even_spread_bps,
            min(fixed_cost::numeric)::text AS minimum_fixed_cost_wei,
            min(variable_cost::numeric)::text AS minimum_variable_cost_wei,
            max(maximum_liquidity_utilization_bps)::text
                AS maximum_liquidity_utilization_bps,
            max(tick_crossings)::text AS maximum_tick_crossings,
            mode() WITHIN GROUP (ORDER BY price_divergence_direction)
                AS price_divergence_direction,
            mode() WITHIN GROUP (ORDER BY rejection_reason) AS dominant_rejection_reason,
            dense_rank() OVER (
                ORDER BY max(margin_to_gate::numeric) DESC, route_fingerprint
            ) AS route_rank
        FROM size_points
        WHERE expected_net_pnl ~ '^-?[0-9]+$'
          AND conservative_net_pnl ~ '^-?[0-9]+$'
          AND severe_net_pnl ~ '^-?[0-9]+$'
          AND margin_to_gate ~ '^-?[0-9]+$'
          AND gross_spread_bps ~ '^-?[0-9]+$'
          AND net_pnl_bps ~ '^-?[0-9]+$'
          AND break_even_spread_bps ~ '^[0-9]+$'
          AND fixed_cost ~ '^[0-9]+$'
          AND variable_cost ~ '^[0-9]+$'
        GROUP BY route_fingerprint, input_size
    ) ranked
),
route_ranking AS (
    SELECT coalesce(jsonb_agg(row_to_json(ranked)::jsonb ORDER BY ranked.route_rank), '[]'::jsonb) AS values
    FROM (
        SELECT
            fact.route_fingerprint,
            dense_rank() OVER (
                ORDER BY max(fact.conservative_net_pnl - fact.minimum_required_net_pnl) DESC,
                         fact.route_fingerprint
            ) AS route_rank,
            count(*)::text AS complete_evaluations,
            count(*) FILTER (
                WHERE fact.primary_profitability_status = 'meets_minimum'
            )::text AS primary_profitable,
            max(fact.input_amount) FILTER (
                WHERE fact.conservative_net_pnl = route_best.best_conservative_net_pnl
            )::text AS best_input_wei,
            max(fact.expected_net_pnl)::text AS best_expected_net_pnl_wei,
            max(fact.conservative_net_pnl)::text AS best_conservative_net_pnl_wei,
            max(fact.severe_net_pnl)::text AS best_severe_net_pnl_wei,
            max(fact.conservative_net_pnl - fact.minimum_required_net_pnl)::text
                AS best_margin_to_gate_wei,
            max(fact.gross_spread)::text AS best_gross_spread_wei,
            sum(fact.dex_fees)::text AS dex_fees_wei,
            sum(fact.price_impact)::text AS price_impact_wei,
            sum(fact.arbitrum_execution_fee)::text AS execution_gas_cost_wei,
            sum(fact.l1_data_fee)::text AS l1_data_fee_wei,
            sum(fact.flash_loan_premium)::text AS flash_premium_wei
        FROM shadow_profitability_facts fact
        JOIN (
            SELECT route_fingerprint, max(conservative_net_pnl) AS best_conservative_net_pnl
            FROM shadow_profitability_facts
            WHERE evaluated_at >= now() - interval '7 days'
              AND evidence_completeness_status = 'complete'
            GROUP BY route_fingerprint
        ) route_best USING (route_fingerprint)
        WHERE fact.evaluated_at >= now() - interval '7 days'
          AND fact.evidence_completeness_status = 'complete'
        GROUP BY fact.route_fingerprint
    ) ranked
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
            'semantics', jsonb_build_object(
                'route_matches', 'count of exact configured Route fingerprints matched',
                'complete_evaluations', 'canonical complete economic facts',
                'near_profitable', 'margin to conservative gate within one configured minimum',
                'candidates', 'materialized autonomous Candidates only'
            ),
            'windows', window_documents.values
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
            'by_size_level', level_profit.values,
            'route_ranking_7d', route_ranking.values,
            'size_sweep_7d', size_summary.values
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
    CROSS JOIN window_documents
    CROSS JOIN size_summary
    CROSS JOIN route_ranking
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
