\set ON_ERROR_STOP on

BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;

SELECT to_regclass('public.phoenix_live_economic_truth') IS NOT NULL
    AS phoenix_has_economic_truth \gset
SELECT to_regclass('public.phoenix_live_economic_loss_ledger') IS NOT NULL
    AS phoenix_has_economic_loss_ledger \gset

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
         WHERE request.created_at >= evidence_window.window_start
           AND request.route_type = 'PHOENIX_DEX_V1') AS execution_requests,
        (SELECT count(*) FROM live_canary.execution_attempts attempt
         JOIN live_canary.execution_requests request ON request.id = attempt.request_id
         WHERE attempt.claimed_at >= evidence_window.window_start
           AND request.route_type = 'PHOENIX_DEX_V1') AS attempts,
        (SELECT count(*) FROM live_canary.execution_attempts attempt
         JOIN live_canary.execution_requests request ON request.id = attempt.request_id
         WHERE attempt.submitted_at >= evidence_window.window_start
           AND request.route_type = 'PHOENIX_DEX_V1') AS submissions,
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
bounded_aave_signals AS MATERIALIZED (
    SELECT signal_identity, borrower, zero_cost_profit_upper_bound,
           retained_profit_floor, terminal_outcome, rejection_reason,
           exact_diagnostics, observed_at
    FROM live_canary.revenue_hunting_signals
    WHERE source_lane = 'aave_liquidation'
      AND observed_at >= now() - interval '7 days'
),
bounded_aave_exact_signals AS MATERIALIZED (
    SELECT *
    FROM bounded_aave_signals
    WHERE exact_diagnostics IS NOT NULL
),
bounded_aave_requests AS MATERIALIZED (
    SELECT id, created_at
    FROM live_canary.execution_requests
    WHERE route_type = 'AAVE_LIQUIDATION_V1'
      AND created_at >= now() - interval '7 days'
),
bounded_aave_attempts AS MATERIALIZED (
    SELECT attempt.claimed_at, attempt.submitted_at
    FROM live_canary.execution_attempts attempt
    JOIN bounded_aave_requests request ON request.id = attempt.request_id
),
bounded_aave_outcomes AS MATERIALIZED (
    SELECT outcome.recorded_at, outcome.net_pnl_wei
    FROM live_canary.execution_outcomes outcome
    JOIN bounded_aave_requests request ON request.id = outcome.request_id
),
aave_window_funnel AS (
    SELECT
        evidence_window.label,
        evidence_window.window_start,
        count(*) FILTER (
            WHERE signal.zero_cost_profit_upper_bound IS NOT NULL
        ) AS discovered_liquidatable_signals,
        count(DISTINCT signal.borrower) FILTER (
            WHERE signal.zero_cost_profit_upper_bound IS NOT NULL
        ) AS distinct_liquidatable_borrowers,
        count(*) FILTER (
            WHERE signal.zero_cost_profit_upper_bound > signal.retained_profit_floor
        ) AS prefilter_positive,
        count(*) FILTER (WHERE signal.exact_diagnostics IS NOT NULL) AS exact_evaluated,
        count(*) FILTER (
            WHERE signal.exact_diagnostics->>'route_eligibility' = 'eligible'
        ) AS route_eligible,
        count(*) FILTER (
            WHERE signal.exact_diagnostics->>'fork_attempted' = 'true'
        ) AS fork_attempted,
        count(*) FILTER (
            WHERE signal.exact_diagnostics->>'fork_passed' = 'true'
        ) AS fork_passed,
        count(*) FILTER (
            WHERE signal.exact_diagnostics->>'any_counterfactual_positive' = 'true'
        ) AS counterfactual_positive,
        count(*) FILTER (
            WHERE signal.exact_diagnostics->>'any_live_authorized_positive' = 'true'
        ) AS live_authorized_positive,
        count(*) FILTER (WHERE signal.terminal_outcome = 'candidate') AS candidate_signals,
        percentile_cont(0.5) WITHIN GROUP (
            ORDER BY (signal.exact_diagnostics->>'liquidatable_to_exact_latency_ms')::numeric
        ) FILTER (
            WHERE signal.exact_diagnostics->>'liquidatable_to_exact_latency_ms' ~ '^[0-9]+$'
        ) AS liquidatable_to_exact_p50_ms,
        percentile_cont(0.95) WITHIN GROUP (
            ORDER BY (signal.exact_diagnostics->>'liquidatable_to_exact_latency_ms')::numeric
        ) FILTER (
            WHERE signal.exact_diagnostics->>'liquidatable_to_exact_latency_ms' ~ '^[0-9]+$'
        ) AS liquidatable_to_exact_p95_ms,
        percentile_cont(0.5) WITHIN GROUP (
            ORDER BY (signal.exact_diagnostics->>'exact_fork_latency_ms')::numeric
        ) FILTER (
            WHERE signal.exact_diagnostics->>'exact_fork_latency_ms' ~ '^[0-9]+$'
        ) AS exact_fork_p50_ms,
        percentile_cont(0.95) WITHIN GROUP (
            ORDER BY (signal.exact_diagnostics->>'exact_fork_latency_ms')::numeric
        ) FILTER (
            WHERE signal.exact_diagnostics->>'exact_fork_latency_ms' ~ '^[0-9]+$'
        ) AS exact_fork_p95_ms,
        (SELECT count(*) FROM bounded_aave_requests request
         WHERE request.created_at >= evidence_window.window_start) AS execution_requests,
        (SELECT count(*) FROM bounded_aave_attempts attempt
         WHERE attempt.claimed_at >= evidence_window.window_start) AS attempts,
        (SELECT count(*) FROM bounded_aave_attempts attempt
         WHERE attempt.submitted_at >= evidence_window.window_start) AS submissions,
        (SELECT count(*) FROM bounded_aave_outcomes outcome
         WHERE outcome.recorded_at >= evidence_window.window_start) AS reconciled_outcomes,
        (SELECT coalesce(sum(outcome.net_pnl_wei), 0) FROM bounded_aave_outcomes outcome
         WHERE outcome.recorded_at >= evidence_window.window_start) AS realized_net_pnl
    FROM windows evidence_window
    LEFT JOIN bounded_aave_signals signal
      ON signal.observed_at >= evidence_window.window_start
    GROUP BY evidence_window.label, evidence_window.window_start
),
aave_window_documents AS (
    SELECT jsonb_object_agg(
        funnel.label,
        jsonb_build_object(
            'window_start', to_char(funnel.window_start AT TIME ZONE 'UTC',
                'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
            'discovered_liquidatable_signals', funnel.discovered_liquidatable_signals::text,
            'distinct_liquidatable_borrowers', funnel.distinct_liquidatable_borrowers::text,
            'prefilter_positive', funnel.prefilter_positive::text,
            'exact_eligible', funnel.prefilter_positive::text,
            'exact_deferred_by_reason', reasons.deferrals,
            'exact_attempted', 'not_available',
            'exact_completed', funnel.exact_evaluated::text,
            'exact_evaluated', funnel.exact_evaluated::text,
            'route_eligible', funnel.route_eligible::text,
            'fork_attempted', funnel.fork_attempted::text,
            'fork_passed', funnel.fork_passed::text,
            'counterfactual_positive', funnel.counterfactual_positive::text,
            'live_authorized_positive', funnel.live_authorized_positive::text,
            'candidate_materialized', funnel.candidate_signals::text,
            'execution_requests', funnel.execution_requests::text,
            'attempts', funnel.attempts::text,
            'submissions', funnel.submissions::text,
            'reconciled_outcomes', funnel.reconciled_outcomes::text,
            'realized_net_pnl_wei', funnel.realized_net_pnl::text,
            'closest_margin_to_gate_wei',
                coalesce(closest.margin_to_gate::text, 'not_available'),
            'best_reviewed_size_wei', coalesce(best.reviewed_size, 'not_available'),
            'best_route', coalesce(best.route, 'not_available'),
            'liquidatable_to_exact_p50_ms',
                coalesce(funnel.liquidatable_to_exact_p50_ms::text, 'not_available'),
            'liquidatable_to_exact_p95_ms',
                coalesce(funnel.liquidatable_to_exact_p95_ms::text, 'not_available'),
            'exact_fork_p50_ms',
                coalesce(funnel.exact_fork_p50_ms::text, 'not_available'),
            'exact_fork_p95_ms',
                coalesce(funnel.exact_fork_p95_ms::text, 'not_available'),
            'rejection_reason_counts', reasons.rejections,
            'diagnostic_rejection_occurrences', diagnostic_reasons.occurrences,
            'unknown_diagnostic_rejection_key_rows',
                diagnostic_reasons.unknown_key_rows::text,
            'unexpected_generic_reason_rows', reasons.unexpected_generic_reason_rows::text
        )
        ORDER BY CASE funnel.label WHEN '1h' THEN 1 WHEN '24h' THEN 2 ELSE 3 END
    ) AS values
    FROM aave_window_funnel funnel
    LEFT JOIN LATERAL (
        SELECT
            jsonb_build_object(
                'provider_recovery_requires_fresh_screen', count(*)
                    FILTER (WHERE signal.rejection_reason = 'provider_recovery_requires_fresh_screen'),
                'borrower_cooldown', count(*)
                    FILTER (WHERE signal.rejection_reason = 'borrower_cooldown'),
                'scheduler_capacity', count(*)
                    FILTER (WHERE signal.rejection_reason = 'scheduler_capacity'),
                'route_ineligible_until_tail', count(*)
                    FILTER (WHERE signal.rejection_reason = 'route_ineligible_until_tail')
            ) AS deferrals,
            jsonb_build_object(
                'prefilter_upper_bound_below_floor', count(*) FILTER
                    (WHERE signal.rejection_reason = 'prefilter_upper_bound_below_floor'),
                'no_weth_debt', count(*) FILTER
                    (WHERE signal.rejection_reason = 'no_weth_debt'),
                'no_supported_collateral', count(*) FILTER
                    (WHERE signal.rejection_reason = 'no_supported_collateral'),
                'supported_collateral_disabled', count(*) FILTER
                    (WHERE signal.rejection_reason = 'supported_collateral_disabled'),
                'unsupported_stable_weth_debt', count(*) FILTER
                    (WHERE signal.rejection_reason = 'unsupported_stable_weth_debt'),
                'no_reviewed_liquidation_variant', count(*) FILTER
                    (WHERE signal.rejection_reason = 'no_reviewed_liquidation_variant'),
                'gross_edge_below_retained_profit_gate', count(*) FILTER
                    (WHERE signal.rejection_reason = 'gross_edge_below_retained_profit_gate'),
                'fork_simulation_failed', count(*) FILTER
                    (WHERE signal.rejection_reason = 'fork_simulation_failed'),
                'fork_economics_invalid', count(*) FILTER
                    (WHERE signal.rejection_reason = 'fork_economics_invalid'),
                'bound_convergence_failed', count(*) FILTER
                    (WHERE signal.rejection_reason = 'bound_convergence_failed'),
                'conservative_net_pnl_below_threshold', count(*) FILTER
                    (WHERE signal.rejection_reason = 'conservative_net_pnl_below_threshold'),
                'live_size_authorization_required', count(*) FILTER
                    (WHERE signal.rejection_reason = 'live_size_authorization_required'),
                'fresh_state_mismatch', count(*) FILTER
                    (WHERE signal.rejection_reason = 'fresh_state_mismatch'),
                'fresh_exact_unavailable', count(*) FILTER
                    (WHERE signal.rejection_reason = 'fresh_exact_unavailable'),
                'economic_bound_incomplete', count(*) FILTER
                    (WHERE signal.rejection_reason = 'economic_bound_incomplete')
            ) AS rejections,
            count(*) FILTER (
                WHERE signal.rejection_reason = 'simulation_evidence_insufficient'
                   OR signal.exact_diagnostics->>'failure_class' =
                      'simulation_evidence_insufficient'
                   OR signal.exact_diagnostics->'rejection_counts' ?
                      'simulation_evidence_insufficient'
            ) AS unexpected_generic_reason_rows
        FROM bounded_aave_signals signal
        WHERE signal.observed_at >= funnel.window_start
    ) reasons ON true
    LEFT JOIN LATERAL (
        SELECT (
            signal.exact_diagnostics->>'closest_margin_to_retained_profit_gate_wei'
        )::numeric AS margin_to_gate
        FROM bounded_aave_exact_signals signal
        WHERE signal.observed_at >= funnel.window_start
          AND signal.exact_diagnostics->>'closest_margin_to_retained_profit_gate_wei'
              ~ '^-?[0-9]+$'
        ORDER BY abs((
            signal.exact_diagnostics->>'closest_margin_to_retained_profit_gate_wei'
        )::numeric), signal.observed_at DESC
        LIMIT 1
    ) closest ON true
    LEFT JOIN LATERAL (
        SELECT
            jsonb_build_object(
                'gross_edge_below_retained_profit_gate', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'gross_edge_below_retained_profit_gate' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'gross_edge_below_retained_profit_gate')::bigint ELSE 0 END), 0),
                'fork_simulation_failed', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'fork_simulation_failed' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'fork_simulation_failed')::bigint ELSE 0 END), 0),
                'fork_economics_invalid', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'fork_economics_invalid' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'fork_economics_invalid')::bigint ELSE 0 END), 0),
                'bound_convergence_failed', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'bound_convergence_failed' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'bound_convergence_failed')::bigint ELSE 0 END), 0),
                'conservative_net_pnl_below_threshold', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'conservative_net_pnl_below_threshold' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'conservative_net_pnl_below_threshold')::bigint ELSE 0 END), 0),
                'smallest_positive_reviewed_size_not_selected', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'smallest_positive_reviewed_size_not_selected' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'smallest_positive_reviewed_size_not_selected')::bigint ELSE 0 END), 0),
                'live_size_authorization_required', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'live_size_authorization_required' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'live_size_authorization_required')::bigint ELSE 0 END), 0),
                'fresh_state_mismatch', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'fresh_state_mismatch' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'fresh_state_mismatch')::bigint ELSE 0 END), 0),
                'fresh_exact_unavailable', coalesce(sum(
                    CASE WHEN signal.exact_diagnostics->'rejection_counts'
                                   ->>'fresh_exact_unavailable' ~ '^[0-9]+$'
                         THEN (signal.exact_diagnostics->'rejection_counts'
                                   ->>'fresh_exact_unavailable')::bigint ELSE 0 END), 0)
            ) AS occurrences,
            (
                SELECT count(*)
                FROM bounded_aave_exact_signals exact_signal
                CROSS JOIN LATERAL jsonb_object_keys(
                    exact_signal.exact_diagnostics->'rejection_counts'
                ) AS rejection_key(value)
                WHERE exact_signal.observed_at >= funnel.window_start
                  AND rejection_key.value NOT IN (
                      'gross_edge_below_retained_profit_gate',
                      'fork_simulation_failed',
                      'fork_economics_invalid',
                      'bound_convergence_failed',
                      'conservative_net_pnl_below_threshold',
                      'smallest_positive_reviewed_size_not_selected',
                      'live_size_authorization_required',
                      'fresh_state_mismatch',
                      'fresh_exact_unavailable'
                  )
            ) AS unknown_key_rows
        FROM bounded_aave_exact_signals signal
        WHERE signal.observed_at >= funnel.window_start
    ) diagnostic_reasons ON true
    LEFT JOIN LATERAL (
        SELECT signal.exact_diagnostics->'best_diagnostic'->>'reviewed_size' AS reviewed_size,
               signal.exact_diagnostics->'best_diagnostic'->>'route' AS route
        FROM bounded_aave_exact_signals signal
        WHERE signal.observed_at >= funnel.window_start
          AND signal.exact_diagnostics->'best_diagnostic'->>'margin_to_retained_profit_gate_wei'
              ~ '^-?[0-9]+$'
        ORDER BY (
            signal.exact_diagnostics->'best_diagnostic'->>'margin_to_retained_profit_gate_wei'
        )::numeric DESC,
        signal.observed_at DESC
        LIMIT 1
    ) best ON true
),
bounded_atlas_ingress AS MATERIALIZED (
    SELECT auction_id, relevant_aave, parallel_eligible, terminal_outcome,
           rejection_reason, observed_at, updated_at
    FROM live_canary.atlas_auction_ingress
    WHERE observed_at >= now() - interval '7 days'
),
bounded_atlas_signals AS MATERIALIZED (
    SELECT signal_identity, evidence_mode, observed_at
    FROM live_canary.revenue_hunting_signals
    WHERE source_lane = 'atlas_solver'
      AND observed_at >= now() - interval '7 days'
),
bounded_atlas_requests AS MATERIALIZED (
    SELECT auction_id, status, submission_response_hash,
           inclusion_transaction_hash, realized_net_pnl, created_at, updated_at
    FROM live_canary.atlas_solver_requests
    WHERE created_at >= now() - interval '7 days'
),
atlas_window_documents AS (
    SELECT jsonb_object_agg(
        evidence_window.label,
        jsonb_build_object(
            'window_start', to_char(evidence_window.window_start AT TIME ZONE 'UTC',
                'YYYY-MM-DD"T"HH24:MI:SS"Z"'),
            'ingress', (SELECT count(*) FROM bounded_atlas_ingress ingress
                WHERE ingress.observed_at >= evidence_window.window_start)::text,
            'relevant_aave', (SELECT count(*) FROM bounded_atlas_ingress ingress
                WHERE ingress.observed_at >= evidence_window.window_start
                  AND ingress.relevant_aave)::text,
            'parallel_eligible', (SELECT count(*) FROM bounded_atlas_ingress ingress
                WHERE ingress.observed_at >= evidence_window.window_start
                  AND ingress.relevant_aave AND ingress.parallel_eligible)::text,
            'candidate_signals', (SELECT count(*) FROM bounded_atlas_signals signal
                WHERE signal.observed_at >= evidence_window.window_start)::text,
            'actual_path_verified_candidate_signals', (SELECT count(*) FROM bounded_atlas_signals signal
                WHERE signal.observed_at >= evidence_window.window_start
                  AND signal.evidence_mode = 'DUAL_PROVIDER_ATLAS_CALLBACK_FORK_VERIFIED')::text,
            'request_materialized', (SELECT count(*) FROM bounded_atlas_requests request
                WHERE request.created_at >= evidence_window.window_start)::text,
            'created_cohort_with_submission_evidence', (SELECT count(*) FROM bounded_atlas_requests request
                WHERE request.created_at >= evidence_window.window_start
                  AND request.submission_response_hash IS NOT NULL)::text,
            'created_cohort_with_inclusion_evidence', (SELECT count(*) FROM bounded_atlas_requests request
                WHERE request.created_at >= evidence_window.window_start
                  AND request.inclusion_transaction_hash IS NOT NULL)::text,
            'created_cohort_terminal_outcomes', (SELECT count(*) FROM bounded_atlas_requests request
                WHERE request.created_at >= evidence_window.window_start
                  AND request.status IN ('lost','expired','reconciled'))::text,
            'created_cohort_realized_net_pnl_wei', (SELECT coalesce(sum(request.realized_net_pnl), 0)
                FROM bounded_atlas_requests request
                WHERE request.created_at >= evidence_window.window_start
                  AND request.status = 'reconciled')::text,
            'current_status_counts', jsonb_build_object(
                'ready', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'ready'),
                'claimed', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'claimed'),
                'signed', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'signed'),
                'submitted', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'submitted'),
                'included', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'included'),
                'lost', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'lost'),
                'expired', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'expired'),
                'submission_unknown', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'submission_unknown'),
                'reconciled', (SELECT count(*) FROM bounded_atlas_requests request
                    WHERE request.created_at >= evidence_window.window_start
                      AND request.status = 'reconciled')
            ),
            'rejection_reason_counts', jsonb_build_object(
                'atlas_callback_evidence_unavailable', (
                    SELECT count(*) FROM bounded_atlas_ingress ingress
                    WHERE ingress.observed_at >= evidence_window.window_start
                      AND ingress.rejection_reason = 'atlas_callback_evidence_unavailable'
                ),
                'other_economic_rejection', (
                    SELECT count(*) FROM bounded_atlas_ingress ingress
                    WHERE ingress.observed_at >= evidence_window.window_start
                      AND ingress.terminal_outcome = 'economic_rejection'
                      AND ingress.rejection_reason IS DISTINCT FROM
                          'atlas_callback_evidence_unavailable'
                      AND ingress.rejection_reason IS NOT NULL
                ),
                'missing_rejection_reason', (
                    SELECT count(*) FROM bounded_atlas_ingress ingress
                    WHERE ingress.observed_at >= evidence_window.window_start
                      AND ingress.terminal_outcome = 'economic_rejection'
                      AND ingress.rejection_reason IS NULL
                )
            )
        )
        ORDER BY CASE evidence_window.label WHEN '1h' THEN 1 WHEN '24h' THEN 2 ELSE 3 END
    ) AS values
    FROM windows evidence_window
),
aave_exact_economics AS (
    SELECT jsonb_build_object(
        'exact_evaluated_signals', (SELECT count(*) FROM bounded_aave_exact_signals)::text,
        'reviewed_combination_count', (SELECT coalesce(sum(
            (signal.exact_diagnostics->>'reviewed_combination_count')::numeric
        ), 0) FROM bounded_aave_exact_signals signal
            WHERE signal.exact_diagnostics->>'reviewed_combination_count' ~ '^[0-9]{1,20}$')::text,
        'selected_diagnostic_count', (SELECT count(*) FROM bounded_aave_exact_signals signal
            WHERE signal.exact_diagnostics ? 'selected_diagnostic')::text,
        'best_diagnostic_count', (SELECT count(*) FROM bounded_aave_exact_signals signal
            WHERE signal.exact_diagnostics ? 'best_diagnostic')::text,
        'best_observed_diagnostic', coalesce((
            SELECT signal.exact_diagnostics->'best_diagnostic'
            FROM bounded_aave_exact_signals signal
            WHERE signal.exact_diagnostics->'best_diagnostic'
                  ->>'margin_to_retained_profit_gate_wei' ~ '^-?[0-9]+$'
            ORDER BY (signal.exact_diagnostics->'best_diagnostic'
                      ->>'margin_to_retained_profit_gate_wei')::numeric DESC,
                     signal.observed_at DESC
            LIMIT 1
        ), '{}'::jsonb),
        'minimum_selected_margin_to_gate_wei', coalesce((SELECT min(
            (signal.exact_diagnostics->'selected_diagnostic'
             ->>'margin_to_retained_profit_gate_wei')::numeric
        ) FROM bounded_aave_exact_signals signal
          WHERE signal.exact_diagnostics->'selected_diagnostic'
                ->>'margin_to_retained_profit_gate_wei' ~ '^-?[0-9]+$')::text,
          'not_available'),
        'maximum_selected_margin_to_gate_wei', coalesce((SELECT max(
            (signal.exact_diagnostics->'selected_diagnostic'
             ->>'margin_to_retained_profit_gate_wei')::numeric
        ) FROM bounded_aave_exact_signals signal
          WHERE signal.exact_diagnostics->'selected_diagnostic'
                ->>'margin_to_retained_profit_gate_wei' ~ '^-?[0-9]+$')::text,
          'not_available'),
        'average_liquidatable_to_exact_latency_ms', coalesce((SELECT avg(
            (signal.exact_diagnostics->>'liquidatable_to_exact_latency_ms')::numeric
        ) FROM bounded_aave_exact_signals signal
          WHERE signal.exact_diagnostics->>'liquidatable_to_exact_latency_ms'
                ~ '^[0-9]+$')::text, 'not_available'),
        'average_exact_fork_latency_ms', coalesce((SELECT avg(
            (signal.exact_diagnostics->>'exact_fork_latency_ms')::numeric
        ) FROM bounded_aave_exact_signals signal
          WHERE signal.exact_diagnostics->>'exact_fork_latency_ms'
                ~ '^[0-9]+$')::text, 'not_available')
    ) AS values
),
revenue_lane_authority AS (
    SELECT
        bool_or(armed) FILTER (WHERE lane = 'aave_liquidation') AS aave_armed,
        bool_or(kill_switch) FILTER (WHERE lane = 'aave_liquidation') AS aave_kill_switch,
        bool_or(armed) FILTER (WHERE lane = 'atlas_solver') AS atlas_armed,
        bool_or(kill_switch) FILTER (WHERE lane = 'atlas_solver') AS atlas_kill_switch
    FROM live_canary.revenue_lane_controls
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
loss_points AS (
\if :phoenix_has_economic_truth
    WITH bounded_loss_truth AS MATERIALIZED (
        SELECT
            truth.evaluation_point_id,
            truth.source_event_identity,
            truth.classified_at,
            truth.route_fingerprint,
            truth.input_size_wei,
            truth.direction_path AS route_direction_path,
            truth.event_to_evaluation_latency_ns,
            truth.pool_address_path,
            CASE WHEN truth.gross_spread_wei ~ '^-?[0-9]+$'
                THEN truth.gross_spread_wei::numeric END AS gross_spread_value,
            CASE WHEN truth.minimum_required_net_pnl_wei ~ '^[0-9]+$'
                THEN truth.minimum_required_net_pnl_wei::numeric END
                AS minimum_required_value,
            CASE WHEN truth.margin_to_profitability_gate_wei ~ '^-?[0-9]+$'
                THEN truth.margin_to_profitability_gate_wei::numeric END
                AS margin_to_gate_value,
            CASE WHEN truth.dex_fees_wei ~ '^[0-9]+$'
                THEN truth.dex_fees_wei::numeric ELSE 0 END AS dex_fees_value,
            CASE WHEN truth.arbitrum_execution_fee_wei ~ '^[0-9]+$'
                THEN truth.arbitrum_execution_fee_wei::numeric ELSE 0 END
                AS execution_fee_value,
            CASE WHEN truth.l1_data_fee_wei ~ '^[0-9]+$'
                THEN truth.l1_data_fee_wei::numeric ELSE 0 END AS l1_fee_value,
            CASE WHEN truth.flash_premium_wei ~ '^[0-9]+$'
                THEN truth.flash_premium_wei::numeric ELSE 0 END AS flash_fee_value,
            CASE WHEN truth.price_impact_wei ~ '^[0-9]+$'
                THEN truth.price_impact_wei::numeric ELSE 0 END AS price_impact_value,
            truth.exact_rejection_reason,
            truth.independent_verification_status,
            truth.independent_verification_lifecycle,
            truth.fork_status,
            truth.fork_simulated_net_pnl_wei,
            truth.state_age_blocks,
            truth.tick_crossings,
            classification.detail_class,
            (
                SELECT max((leg->>'utilization_bps')::numeric)
                FROM jsonb_array_elements(
                    coalesce(truth.active_liquidity_near_current_tick, '[]'::jsonb)
                ) leg
                WHERE leg->>'utilization_bps' ~ '^[0-9]+$'
            ) AS maximum_liquidity_utilization_bps,
            (
                SELECT jsonb_agg(pool.value ORDER BY pool.ordinality DESC)
                FROM jsonb_array_elements(truth.pool_address_path)
                     WITH ORDINALITY AS pool(value, ordinality)
            ) AS reverse_pool_address_path
        FROM phoenix_live_economic_truth truth
        JOIN shadow_engine_classifications classification
          ON classification.source_event_identity = truth.source_event_identity
        WHERE truth.classified_at >= now() - interval '7 days'
    ),
    bounded_counterfactuals AS (
        SELECT DISTINCT ON (source_event_identity)
            source_event_identity,
            route_fingerprint,
            input_size_wei,
            margin_to_gate_value
        FROM bounded_loss_truth
        ORDER BY source_event_identity,
                 margin_to_gate_value DESC NULLS LAST,
                 route_fingerprint,
                 input_size_wei::numeric
    ),
    bounded_reverse_routes AS (
        SELECT DISTINCT ON (truth.evaluation_point_id)
            truth.evaluation_point_id,
            candidate.route_fingerprint,
            candidate.margin_to_gate_value,
            candidate.gross_spread_value
        FROM bounded_loss_truth truth
        JOIN bounded_loss_truth candidate
          ON candidate.source_event_identity = truth.source_event_identity
         AND candidate.input_size_wei = truth.input_size_wei
         AND candidate.route_fingerprint <> truth.route_fingerprint
         AND candidate.pool_address_path = truth.reverse_pool_address_path
        ORDER BY truth.evaluation_point_id,
                 candidate.margin_to_gate_value DESC NULLS LAST,
                 candidate.route_fingerprint
    ),
    bounded_causes AS (
        SELECT
            truth.*,
            counterfactual.route_fingerprint
                AS best_counterfactual_route_fingerprint,
            counterfactual.input_size_wei
                AS best_counterfactual_input_size_wei,
            counterfactual.margin_to_gate_value
                AS best_counterfactual_margin_to_gate_wei,
            reverse_route.margin_to_gate_value AS reverse_margin_to_gate_value,
            CASE
                WHEN truth.detail_class = 'upstream_call_budget_exhausted'
                    THEN 'rpc_budget_exhausted'
                WHEN truth.independent_verification_status = 'disagreed'
                    OR truth.independent_verification_lifecycle
                       @> '["disagreed"]'::jsonb
                    THEN 'rpc_disagreement'
                WHEN truth.fork_status = 'reverted'
                    THEN 'fork_revert'
                WHEN truth.fork_status = 'passed'
                 AND truth.fork_simulated_net_pnl_wei <= truth.minimum_required_value
                    THEN 'fork_pnl_below_gate'
                WHEN truth.exact_rejection_reason IN ('quote_stale', 'quote_expired')
                    THEN 'quote_stale'
                WHEN truth.state_age_blocks > 1
                    THEN 'state_stale'
                WHEN truth.exact_rejection_reason IN (
                    'liquidity_unknown',
                    'quote_incomplete'
                ) THEN 'state_incomplete'
                WHEN truth.exact_rejection_reason = 'liquidity_insufficient'
                  OR truth.maximum_liquidity_utilization_bps > 1000
                    THEN 'liquidity_utilization_limit'
                WHEN truth.tick_crossings > 64
                    THEN 'tick_crossing_limit'
                WHEN truth.exact_rejection_reason IN (
                    'price_impact_limit_exceeded',
                    'slippage_limit_exceeded'
                ) THEN 'price_impact_dominated'
                WHEN truth.exact_rejection_reason IN (
                    'token_not_allowed',
                    'protocol_not_allowed',
                    'route_not_in_universe'
                ) THEN 'route_not_in_universe'
                WHEN truth.gross_spread_value < 0
                 AND reverse_route.gross_spread_value > 0
                    THEN 'wrong_direction'
                WHEN truth.gross_spread_value <= 0
                    THEN 'gross_spread_negative'
                WHEN truth.margin_to_gate_value < 0
                 AND truth.dex_fees_value >= greatest(
                        truth.execution_fee_value,
                        truth.l1_fee_value,
                        truth.flash_fee_value,
                        truth.price_impact_value
                     )
                 AND truth.dex_fees_value > 0
                    THEN 'dex_fees_dominated'
                WHEN truth.margin_to_gate_value < 0
                 AND truth.execution_fee_value >= greatest(
                        truth.l1_fee_value,
                        truth.flash_fee_value,
                        truth.price_impact_value
                     )
                 AND truth.execution_fee_value > 0
                    THEN 'fixed_gas_dominated'
                WHEN truth.margin_to_gate_value < 0
                 AND truth.l1_fee_value >= greatest(
                        truth.flash_fee_value,
                        truth.price_impact_value
                     )
                 AND truth.l1_fee_value > 0
                    THEN 'l1_data_fee_dominated'
                WHEN truth.margin_to_gate_value < 0
                 AND truth.flash_fee_value >= truth.price_impact_value
                 AND truth.flash_fee_value > 0
                    THEN 'flash_fee_dominated'
                WHEN truth.margin_to_gate_value < 0
                 AND truth.price_impact_value > 0
                    THEN 'price_impact_dominated'
                ELSE 'unknown'
            END AS primary_loss_cause
        FROM bounded_loss_truth truth
        LEFT JOIN bounded_counterfactuals counterfactual
          ON counterfactual.source_event_identity = truth.source_event_identity
        LEFT JOIN bounded_reverse_routes reverse_route
          ON reverse_route.evaluation_point_id = truth.evaluation_point_id
    ),
    bounded_loss_rows AS (
        SELECT
            caused.*,
            greatest(-caused.margin_to_gate_value, 0)
                AS missing_break_even_amount_wei,
            CASE caused.primary_loss_cause
                WHEN 'wrong_direction' THEN greatest(
                    caused.reverse_margin_to_gate_value - caused.margin_to_gate_value,
                    0
                )
                WHEN 'gross_spread_negative' THEN greatest(
                    -caused.margin_to_gate_value,
                    0
                )
                WHEN 'dex_fees_dominated' THEN caused.dex_fees_value
                WHEN 'fixed_gas_dominated' THEN caused.execution_fee_value
                WHEN 'l1_data_fee_dominated' THEN caused.l1_fee_value
                WHEN 'flash_fee_dominated' THEN caused.flash_fee_value
                WHEN 'price_impact_dominated' THEN caused.price_impact_value
                WHEN 'fork_pnl_below_gate' THEN greatest(
                    caused.minimum_required_value - caused.fork_simulated_net_pnl_wei,
                    0
                )
                ELSE NULL
            END AS recoverable_pnl_if_bottleneck_removed_wei,
            CASE caused.primary_loss_cause
                WHEN 'wrong_direction' THEN 'prioritize_reverse_direction'
                WHEN 'route_not_in_universe' THEN 'verify_and_rank_missing_route'
                WHEN 'gross_spread_negative' THEN 'expand_verified_route_universe'
                WHEN 'dex_fees_dominated' THEN 'prefer_lower_fee_pool_pair'
                WHEN 'fixed_gas_dominated' THEN 'reduce_fixed_execution_gas'
                WHEN 'l1_data_fee_dominated'
                    THEN 'reduce_calldata_or_wait_for_lower_l1_fee'
                WHEN 'flash_fee_dominated' THEN 'evaluate_lower_cost_capital_source'
                WHEN 'price_impact_dominated' THEN 'prefer_smaller_size_or_deeper_pool'
                WHEN 'liquidity_utilization_limit'
                    THEN 'prefer_smaller_size_or_deeper_pool'
                WHEN 'tick_crossing_limit' THEN 'prefer_smaller_size_or_deeper_pool'
                WHEN 'state_incomplete' THEN 'repair_state_completeness'
                WHEN 'state_stale' THEN 'reduce_state_latency'
                WHEN 'quote_stale' THEN 'reduce_quote_latency'
                WHEN 'candidate_stale'
                    THEN 'reduce_candidate_materialization_latency'
                WHEN 'rpc_budget_exhausted'
                    THEN 'prioritize_promising_primary_routes'
                WHEN 'rpc_disagreement'
                    THEN 'investigate_provider_state_divergence'
                WHEN 'fork_revert' THEN 'inspect_fork_revert_evidence'
                WHEN 'fork_pnl_below_gate' THEN 'calibrate_prediction_against_fork'
                WHEN 'candidate_decay'
                    THEN 'reduce_detection_to_submission_latency'
                WHEN 'contract_guard_rejection'
                    THEN 'retain_guard_and_fix_plan_binding'
                ELSE 'collect_more_bounded_evidence'
            END AS recommended_next_action
        FROM bounded_causes caused
    )
    SELECT
        loss.classified_at,
        loss.route_fingerprint,
        loss.input_size_wei,
        CASE
            WHEN outcome.outcome_class IN (
                'submitted_too_late',
                'competitor_or_state_changed',
                'ordering_bid_too_low'
            ) THEN 'candidate_decay'
            WHEN outcome.outcome_class IN (
                'policy_rejected',
                'risk_rejected',
                'integrity_failure',
                'operator_killed'
            ) THEN 'contract_guard_rejection'
            WHEN candidate.status = 'expired' THEN 'candidate_stale'
            WHEN candidate.status IN (
                'rejected_policy',
                'risk_rejected',
                'integrity_failure',
                'policy_rejected',
                'operator_killed'
            ) THEN 'contract_guard_rejection'
            ELSE loss.primary_loss_cause
        END AS primary_loss_cause,
        loss.missing_break_even_amount_wei,
        loss.best_counterfactual_route_fingerprint,
        loss.best_counterfactual_input_size_wei,
        loss.best_counterfactual_margin_to_gate_wei,
        loss.recoverable_pnl_if_bottleneck_removed_wei,
        loss.recommended_next_action,
        loss.route_direction_path,
        loss.event_to_evaluation_latency_ns
    FROM bounded_loss_rows loss
    LEFT JOIN LATERAL (
        SELECT current_candidate.candidate_id,
               current_candidate.status,
               current_candidate.rejection_reason
        FROM live_canary.autonomous_candidates current_candidate
        WHERE current_candidate.origin_event_id = loss.source_event_identity
          AND current_candidate.route_fingerprint = loss.route_fingerprint
          AND current_candidate.selected_size::text = loss.input_size_wei
        ORDER BY current_candidate.candidate_created_at DESC,
                 current_candidate.candidate_id
        LIMIT 1
    ) candidate ON true
    LEFT JOIN live_canary.autonomous_outcome_attributions outcome
      ON outcome.candidate_id = candidate.candidate_id
    WHERE (
          outcome.candidate_id IS NULL
          OR outcome.realized_business_net_pnl <= 0
      )
\else
    SELECT
        NULL::timestamptz AS classified_at,
        NULL::text AS route_fingerprint,
        NULL::text AS input_size_wei,
        NULL::text AS primary_loss_cause,
        NULL::numeric AS missing_break_even_amount_wei,
        NULL::text AS best_counterfactual_route_fingerprint,
        NULL::text AS best_counterfactual_input_size_wei,
        NULL::numeric AS best_counterfactual_margin_to_gate_wei,
        NULL::numeric AS recoverable_pnl_if_bottleneck_removed_wei,
        NULL::text AS recommended_next_action,
        NULL::jsonb AS route_direction_path,
        NULL::bigint AS event_to_evaluation_latency_ns
    WHERE false
\endif
),
loss_ledger AS (
    SELECT coalesce(
        jsonb_agg(
            row_to_json(bucket)::jsonb
            ORDER BY bucket.loss_count::bigint DESC, bucket.primary_loss_cause
        ),
        '[]'::jsonb
    ) AS values
    FROM (
        SELECT
            primary_loss_cause,
            count(*)::text AS loss_count,
            sum(coalesce(recoverable_pnl_if_bottleneck_removed_wei, 0))::text
                AS recoverable_pnl_wei,
            max(best_counterfactual_margin_to_gate_wei)::text
                AS closest_margin_to_gate_wei,
            mode() WITHIN GROUP (ORDER BY recommended_next_action)
                AS recommended_next_action
        FROM loss_points
        GROUP BY primary_loss_cause
    ) bucket
),
daily_ranked AS (
    SELECT
        date_trunc('day', classified_at) AS evaluation_day,
        loss_points.*,
        row_number() OVER (
            PARTITION BY date_trunc('day', classified_at)
            ORDER BY best_counterfactual_margin_to_gate_wei DESC NULLS LAST,
                     route_fingerprint,
                     input_size_wei::numeric
        ) AS opportunity_rank
    FROM loss_points
),
daily_cause_totals AS (
    SELECT
        evaluation_day,
        primary_loss_cause,
        count(*) AS cause_count,
        sum(coalesce(recoverable_pnl_if_bottleneck_removed_wei, 0))
            AS recoverable_pnl_wei,
        row_number() OVER (
            PARTITION BY evaluation_day
            ORDER BY sum(coalesce(recoverable_pnl_if_bottleneck_removed_wei, 0)) DESC,
                     count(*) DESC,
                     primary_loss_cause
        ) AS recoverable_rank,
        row_number() OVER (
            PARTITION BY evaluation_day
            ORDER BY count(*) DESC, primary_loss_cause
        ) AS frequency_rank
    FROM daily_ranked
    GROUP BY evaluation_day, primary_loss_cause
),
daily_latency_routes AS (
    SELECT
        evaluation_day,
        route_fingerprint,
        sum(event_to_evaluation_latency_ns) AS latency_ns,
        row_number() OVER (
            PARTITION BY evaluation_day
            ORDER BY sum(event_to_evaluation_latency_ns) DESC, route_fingerprint
        ) AS latency_rank
    FROM daily_ranked
    GROUP BY evaluation_day, route_fingerprint
),
daily_attack_rows AS (
    SELECT
        best.evaluation_day,
        best.best_counterfactual_route_fingerprint AS best_route_fingerprint,
        best.route_direction_path AS best_direction,
        best.best_counterfactual_input_size_wei AS best_input_size_wei,
        best.best_counterfactual_margin_to_gate_wei AS closest_margin_to_gate_wei,
        recoverable.primary_loss_cause AS largest_recoverable_bucket,
        recoverable.recoverable_pnl_wei AS largest_recoverable_pnl_wei,
        dominant.primary_loss_cause AS dominant_loss_cause,
        dominant.cause_count AS dominant_loss_count,
        (
            SELECT missing.route_fingerprint
            FROM daily_ranked missing
            WHERE missing.evaluation_day = best.evaluation_day
              AND missing.primary_loss_cause = 'route_not_in_universe'
            ORDER BY missing.best_counterfactual_margin_to_gate_wei DESC NULLS LAST,
                     missing.route_fingerprint
            LIMIT 1
        ) AS top_missing_route_fingerprint,
        latency.route_fingerprint AS top_latency_loss_route_fingerprint,
        best.recommended_next_action AS recommended_next_engineering_change
    FROM daily_ranked best
    JOIN daily_cause_totals recoverable
      ON recoverable.evaluation_day = best.evaluation_day
     AND recoverable.recoverable_rank = 1
    JOIN daily_cause_totals dominant
      ON dominant.evaluation_day = best.evaluation_day
     AND dominant.frequency_rank = 1
    JOIN daily_latency_routes latency
      ON latency.evaluation_day = best.evaluation_day
     AND latency.latency_rank = 1
    WHERE best.opportunity_rank = 1
),
daily_attack_surface AS (
    SELECT coalesce(
        jsonb_agg(row_to_json(report)::jsonb ORDER BY report.evaluation_day DESC),
        '[]'::jsonb
    ) AS values
    FROM daily_attack_rows report
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
            'lane_authority', jsonb_build_object(
                'generic_dex', jsonb_build_object(
                    'effective_armed', execution_control.armed
                        AND global_control.armed
                        AND NOT global_control.kill_switch
                        AND NOT execution_control.kill_switch
                        AND global_control.execution_mode = 'live'
                        AND control.phase LIKE 'LIVE_%',
                    'effective_kill_switch', execution_control.kill_switch
                        OR global_control.kill_switch
                        OR global_control.execution_mode <> 'live'
                        OR control.phase NOT LIKE 'LIVE_%',
                    'execution_armed', execution_control.armed,
                    'execution_kill_switch', execution_control.kill_switch,
                    'global_armed', global_control.armed,
                    'global_kill_switch', global_control.kill_switch,
                    'execution_mode', global_control.execution_mode,
                    'economic_phase', control.phase
                ),
                'aave_liquidation', jsonb_build_object(
                    'armed', coalesce(revenue_lane_authority.aave_armed, false),
                    'kill_switch', coalesce(revenue_lane_authority.aave_kill_switch, true)
                ),
                'atlas_solver', jsonb_build_object(
                    'armed', coalesce(revenue_lane_authority.atlas_armed, false),
                    'kill_switch', coalesce(revenue_lane_authority.atlas_kill_switch, true)
                )
            ),
            'next_promotion_gate', '20 reconciled outcomes; positive net PnL; all safety gates',
            'last_transition_reason', control.last_transition_reason
        ),
        'funnel', jsonb_build_object(
            'semantics', jsonb_build_object(
                'scope', 'Generic Phoenix DEX Engine only',
                'route_matches', 'count of exact configured Route fingerprints matched',
                'complete_evaluations', 'canonical complete economic facts',
                'near_profitable', 'margin to conservative gate within one configured minimum',
                'candidates', 'materialized Generic DEX autonomous Candidates only',
                'simulation_evidence_insufficient',
                    'Generic Engine state simulation was stale or non-runnable; never an Aave fork or Atlas callback classification',
                'aave_direct_stages',
                    'screen and Exact values are signal-event volumes; direct request, attempt, submission, and outcome values are independent lane stage-event volumes, not per-signal conversions',
                'aave_exact_attempted',
                    'not durably observable; exact_completed/exact_evaluated count only persisted completed Exact summaries',
                'atlas_request_stages',
                    'submission, inclusion, terminal outcome, and PnL values describe current evidence for requests created in each window, not event timestamps',
                'atlas_rejections',
                    'rejection reasons are observed-auction cohort counts for each window'
            ),
            'windows', window_documents.values,
            'revenue_lane_windows', jsonb_build_object(
                'aave_liquidation', aave_window_documents.values,
                'atlas_solver', atlas_window_documents.values
            )
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
            'size_sweep_7d', size_summary.values,
            'aave_exact_7d', aave_exact_economics.values,
            'atlas_solver_7d', atlas_window_documents.values->'7d',
            'loss_ledger_7d', loss_ledger.values,
            'daily_attack_surface_7d', daily_attack_surface.values,
            'loss_cause_contract', jsonb_build_array(
                'wrong_direction',
                'route_not_in_universe',
                'gross_spread_negative',
                'dex_fees_dominated',
                'fixed_gas_dominated',
                'l1_data_fee_dominated',
                'flash_fee_dominated',
                'price_impact_dominated',
                'liquidity_utilization_limit',
                'tick_crossing_limit',
                'state_incomplete',
                'state_stale',
                'quote_stale',
                'candidate_stale',
                'rpc_budget_exhausted',
                'rpc_disagreement',
                'fork_revert',
                'fork_pnl_below_gate',
                'candidate_decay',
                'contract_guard_rejection',
                'unknown'
            )
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
    CROSS JOIN aave_window_documents
    CROSS JOIN atlas_window_documents
    CROSS JOIN aave_exact_economics
    CROSS JOIN revenue_lane_authority
    CROSS JOIN size_summary
    CROSS JOIN route_ranking
    CROSS JOIN loss_ledger
    CROSS JOIN daily_attack_surface
    CROSS JOIN safety
    CROSS JOIN level_profit
    CROSS JOIN live_canary.realized_profit_windows profit
    CROSS JOIN live_canary.economic_control control
    CROSS JOIN live_canary.control execution_control
    CROSS JOIN live_canary.autonomous_global_control global_control
    LEFT JOIN live_canary.autonomous_route_controls route_control
      ON route_control.route_fingerprint = control.route_fingerprint
    WHERE control.singleton AND execution_control.singleton AND global_control.singleton
)
SELECT document::text FROM snapshot;

COMMIT;
