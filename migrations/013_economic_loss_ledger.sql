CREATE OR REPLACE VIEW phoenix_live_economic_loss_ledger AS
WITH numeric_truth AS NOT MATERIALIZED (
    SELECT
        truth.*,
        CASE WHEN truth.gross_spread_wei ~ '^-?[0-9]+$'
            THEN truth.gross_spread_wei::numeric END AS gross_spread_value,
        CASE WHEN truth.gross_spread_bps ~ '^-?[0-9]+$'
            THEN truth.gross_spread_bps::numeric END AS gross_spread_bps_value,
        CASE WHEN truth.break_even_spread_bps ~ '^[0-9]+$'
            THEN truth.break_even_spread_bps::numeric END AS break_even_spread_bps_value,
        CASE WHEN truth.minimum_required_net_pnl_wei ~ '^[0-9]+$'
            THEN truth.minimum_required_net_pnl_wei::numeric END AS minimum_required_value,
        CASE WHEN truth.margin_to_profitability_gate_wei ~ '^-?[0-9]+$'
            THEN truth.margin_to_profitability_gate_wei::numeric END AS margin_to_gate_value,
        CASE WHEN truth.fixed_cost_wei ~ '^[0-9]+$'
            THEN truth.fixed_cost_wei::numeric END AS fixed_cost_value,
        CASE WHEN truth.variable_cost_wei ~ '^[0-9]+$'
            THEN truth.variable_cost_wei::numeric END AS variable_cost_value,
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
        (
            SELECT max((leg->>'utilization_bps')::numeric)
            FROM jsonb_array_elements(
                coalesce(truth.active_liquidity_near_current_tick, '[]'::jsonb)
            ) leg
            WHERE leg->>'utilization_bps' ~ '^[0-9]+$'
        ) AS maximum_liquidity_utilization_bps
    FROM phoenix_live_economic_truth truth
),
contextual AS NOT MATERIALIZED (
    SELECT
        truth.*,
        classification.classification,
        classification.detail_class,
        reverse_route.route_fingerprint AS reverse_route_fingerprint,
        reverse_route.margin_to_gate_value AS reverse_margin_to_gate_value,
        reverse_route.gross_spread_value AS reverse_gross_spread_value,
        counterfactual.route_fingerprint AS best_counterfactual_route_fingerprint,
        counterfactual.input_size_wei AS best_counterfactual_input_size_wei,
        counterfactual.margin_to_gate_value AS best_counterfactual_margin_to_gate_value
    FROM numeric_truth truth
    JOIN shadow_engine_classifications classification
      ON classification.source_event_identity = truth.source_event_identity
    LEFT JOIN LATERAL (
        SELECT candidate.route_fingerprint,
               candidate.margin_to_gate_value,
               candidate.gross_spread_value
        FROM numeric_truth candidate
        WHERE candidate.source_event_identity = truth.source_event_identity
          AND candidate.input_size_wei = truth.input_size_wei
          AND candidate.route_fingerprint <> truth.route_fingerprint
          AND candidate.pool_address_path = (
              SELECT jsonb_agg(pool.value ORDER BY pool.ordinality DESC)
              FROM jsonb_array_elements(truth.pool_address_path)
                   WITH ORDINALITY AS pool(value, ordinality)
          )
        ORDER BY candidate.margin_to_gate_value DESC NULLS LAST,
                 candidate.route_fingerprint
        LIMIT 1
    ) reverse_route ON true
    LEFT JOIN LATERAL (
        SELECT candidate.route_fingerprint,
               candidate.input_size_wei,
               candidate.margin_to_gate_value
        FROM numeric_truth candidate
        WHERE candidate.source_event_identity = truth.source_event_identity
        ORDER BY candidate.margin_to_gate_value DESC NULLS LAST,
                 candidate.route_fingerprint,
                 candidate.input_size_wei::numeric
        LIMIT 1
    ) counterfactual ON true
),
caused AS NOT MATERIALIZED (
    SELECT
        contextual.*,
        CASE
            WHEN detail_class = 'upstream_call_budget_exhausted'
                THEN 'rpc_budget_exhausted'
            WHEN independent_verification_status = 'disagreed'
                OR independent_verification_lifecycle @> '["disagreed"]'::jsonb
                THEN 'rpc_disagreement'
            WHEN fork_status = 'reverted'
                THEN 'fork_revert'
            WHEN fork_status = 'passed'
             AND fork_simulated_net_pnl_wei <= minimum_required_value
                THEN 'fork_pnl_below_gate'
            WHEN exact_rejection_reason IN ('quote_stale', 'quote_expired')
                THEN 'quote_stale'
            WHEN state_age_blocks > 1
                THEN 'state_stale'
            WHEN exact_rejection_reason IN (
                'liquidity_unknown',
                'quote_incomplete'
            ) THEN 'state_incomplete'
            WHEN exact_rejection_reason = 'liquidity_insufficient'
              OR maximum_liquidity_utilization_bps > 1000
                THEN 'liquidity_utilization_limit'
            WHEN tick_crossings > 64
                THEN 'tick_crossing_limit'
            WHEN exact_rejection_reason IN (
                'price_impact_limit_exceeded',
                'slippage_limit_exceeded'
            ) THEN 'price_impact_dominated'
            WHEN exact_rejection_reason IN (
                'token_not_allowed',
                'protocol_not_allowed',
                'route_not_in_universe'
            ) THEN 'route_not_in_universe'
            WHEN gross_spread_value < 0
             AND reverse_gross_spread_value > 0
                THEN 'wrong_direction'
            WHEN gross_spread_value <= 0
                THEN 'gross_spread_negative'
            WHEN margin_to_gate_value < 0
             AND dex_fees_value >= greatest(
                    execution_fee_value,
                    l1_fee_value,
                    flash_fee_value,
                    price_impact_value
                 )
             AND dex_fees_value > 0
                THEN 'dex_fees_dominated'
            WHEN margin_to_gate_value < 0
             AND execution_fee_value >= greatest(
                    l1_fee_value,
                    flash_fee_value,
                    price_impact_value
                 )
             AND execution_fee_value > 0
                THEN 'fixed_gas_dominated'
            WHEN margin_to_gate_value < 0
             AND l1_fee_value >= greatest(flash_fee_value, price_impact_value)
             AND l1_fee_value > 0
                THEN 'l1_data_fee_dominated'
            WHEN margin_to_gate_value < 0
             AND flash_fee_value >= price_impact_value
             AND flash_fee_value > 0
                THEN 'flash_fee_dominated'
            WHEN margin_to_gate_value < 0
             AND price_impact_value > 0
                THEN 'price_impact_dominated'
            ELSE 'unknown'
        END AS primary_loss_cause
    FROM contextual
)
SELECT
    caused.evaluation_point_id,
    caused.source_event_identity,
    caused.source_sequence,
    caused.classified_at,
    caused.route_fingerprint,
    caused.input_size_wei,
    caused.initiating_swap_direction,
    caused.direction_path AS route_direction_path,
    caused.price_divergence_direction,
    caused.primary_loss_cause,
    secondary.values AS secondary_loss_causes,
    caused.gross_spread_value AS observed_gross_spread_wei,
    caused.gross_spread_bps_value AS observed_gross_spread_bps,
    caused.break_even_spread_bps_value AS break_even_spread_bps,
    greatest(-caused.margin_to_gate_value, 0) AS missing_break_even_amount_wei,
    caused.best_counterfactual_route_fingerprint,
    caused.best_counterfactual_input_size_wei,
    caused.best_counterfactual_margin_to_gate_value
        AS best_counterfactual_margin_to_gate_wei,
    caused.reverse_route_fingerprint,
    caused.fixed_cost_value AS fixed_cost_wei,
    caused.variable_cost_value AS variable_cost_wei,
    caused.dex_fees_value AS dex_fees_wei,
    caused.execution_fee_value AS execution_gas_cost_wei,
    caused.l1_fee_value AS l1_data_fee_wei,
    caused.flash_fee_value AS flash_fee_wei,
    caused.price_impact_value AS price_impact_wei,
    CASE caused.primary_loss_cause
        WHEN 'wrong_direction' THEN greatest(
            caused.reverse_margin_to_gate_value - caused.margin_to_gate_value,
            0
        )
        WHEN 'gross_spread_negative' THEN greatest(-caused.margin_to_gate_value, 0)
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
        WHEN 'l1_data_fee_dominated' THEN 'reduce_calldata_or_wait_for_lower_l1_fee'
        WHEN 'flash_fee_dominated' THEN 'evaluate_lower_cost_capital_source'
        WHEN 'price_impact_dominated' THEN 'prefer_smaller_size_or_deeper_pool'
        WHEN 'liquidity_utilization_limit' THEN 'prefer_smaller_size_or_deeper_pool'
        WHEN 'tick_crossing_limit' THEN 'prefer_smaller_size_or_deeper_pool'
        WHEN 'state_incomplete' THEN 'repair_state_completeness'
        WHEN 'state_stale' THEN 'reduce_state_latency'
        WHEN 'quote_stale' THEN 'reduce_quote_latency'
        WHEN 'candidate_stale' THEN 'reduce_candidate_materialization_latency'
        WHEN 'rpc_budget_exhausted' THEN 'prioritize_promising_primary_routes'
        WHEN 'rpc_disagreement' THEN 'investigate_provider_state_divergence'
        WHEN 'fork_revert' THEN 'inspect_fork_revert_evidence'
        WHEN 'fork_pnl_below_gate' THEN 'calibrate_prediction_against_fork'
        WHEN 'candidate_decay' THEN 'reduce_detection_to_submission_latency'
        WHEN 'contract_guard_rejection' THEN 'retain_guard_and_fix_plan_binding'
        ELSE 'collect_more_bounded_evidence'
    END AS recommended_next_action,
    caused.maximum_liquidity_utilization_bps,
    caused.tick_crossings,
    caused.state_age_blocks,
    caused.quote_age_ms,
    caused.candidate_age_ms,
    caused.event_to_evaluation_latency_ns,
    caused.primary_provider_id,
    caused.primary_response_hash,
    caused.independent_verification_status,
    caused.fork_status,
    caused.fork_simulated_net_pnl_wei,
    caused.classification,
    caused.detail_class,
    caused.exact_rejection_reason
FROM caused
LEFT JOIN LATERAL (
    SELECT coalesce(jsonb_agg(candidate.cause ORDER BY candidate.ordinal), '[]'::jsonb)
        AS values
    FROM (
        VALUES
            (1, CASE WHEN caused.gross_spread_value <= 0
                THEN 'gross_spread_negative' END),
            (2, CASE WHEN caused.dex_fees_value > 0
                THEN 'dex_fees_dominated' END),
            (3, CASE WHEN caused.execution_fee_value > 0
                THEN 'fixed_gas_dominated' END),
            (4, CASE WHEN caused.l1_fee_value > 0
                THEN 'l1_data_fee_dominated' END),
            (5, CASE WHEN caused.flash_fee_value > 0
                THEN 'flash_fee_dominated' END),
            (6, CASE WHEN caused.price_impact_value > 0
                THEN 'price_impact_dominated' END),
            (7, CASE WHEN caused.maximum_liquidity_utilization_bps > 1000
                THEN 'liquidity_utilization_limit' END),
            (8, CASE WHEN caused.tick_crossings > 64
                THEN 'tick_crossing_limit' END),
            (9, CASE WHEN caused.state_age_blocks > 1
                THEN 'state_stale' END),
            (10, CASE WHEN caused.independent_verification_status = 'disagreed'
                THEN 'rpc_disagreement' END)
    ) AS candidate(ordinal, cause)
    WHERE candidate.cause IS NOT NULL
      AND candidate.cause <> caused.primary_loss_cause
) secondary ON true;

CREATE OR REPLACE VIEW phoenix_daily_economic_attack_surface AS
WITH ranked AS NOT MATERIALIZED (
    SELECT
        date_trunc('day', classified_at) AS evaluation_day,
        ledger.*,
        row_number() OVER (
            PARTITION BY date_trunc('day', classified_at)
            ORDER BY best_counterfactual_margin_to_gate_wei DESC NULLS LAST,
                     route_fingerprint,
                     input_size_wei::numeric
        ) AS opportunity_rank
    FROM phoenix_live_economic_loss_ledger ledger
),
cause_totals AS (
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
    FROM ranked
    GROUP BY evaluation_day, primary_loss_cause
),
latency_routes AS (
    SELECT
        evaluation_day,
        route_fingerprint,
        sum(event_to_evaluation_latency_ns) AS latency_ns,
        row_number() OVER (
            PARTITION BY evaluation_day
            ORDER BY sum(event_to_evaluation_latency_ns) DESC, route_fingerprint
        ) AS latency_rank
    FROM ranked
    GROUP BY evaluation_day, route_fingerprint
)
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
        FROM ranked missing
        WHERE missing.evaluation_day = best.evaluation_day
          AND missing.primary_loss_cause = 'route_not_in_universe'
        ORDER BY missing.best_counterfactual_margin_to_gate_wei DESC NULLS LAST,
                 missing.route_fingerprint
        LIMIT 1
    ) AS top_missing_route_fingerprint,
    latency.route_fingerprint AS top_latency_loss_route_fingerprint,
    best.recommended_next_action AS recommended_next_engineering_change
FROM ranked best
JOIN cause_totals recoverable
  ON recoverable.evaluation_day = best.evaluation_day
 AND recoverable.recoverable_rank = 1
JOIN cause_totals dominant
  ON dominant.evaluation_day = best.evaluation_day
 AND dominant.frequency_rank = 1
JOIN latency_routes latency
  ON latency.evaluation_day = best.evaluation_day
 AND latency.latency_rank = 1
WHERE best.opportunity_rank = 1;
