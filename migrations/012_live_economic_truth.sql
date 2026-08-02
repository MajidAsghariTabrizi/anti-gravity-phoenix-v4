CREATE OR REPLACE VIEW phoenix_live_economic_truth AS
WITH size_points AS NOT MATERIALIZED (
    SELECT
        classification.source_event_identity,
        classification.source_sequence,
        classification.tx_hash AS initiating_transaction_hash,
        classification.classified_at,
        classification.processing_latency_ns AS event_to_evaluation_latency_ns,
        classification.evidence->'economic_origin' AS economic_origin,
        scale,
        point,
        point_ordinal
    FROM shadow_engine_classifications classification
    CROSS JOIN LATERAL jsonb_array_elements(
        CASE
            WHEN jsonb_typeof(classification.evidence->'evaluations') = 'array'
            THEN classification.evidence->'evaluations'
            ELSE '[]'::jsonb
        END
    ) evaluation
    CROSS JOIN LATERAL (
        SELECT evaluation->'profitability_scale'->'primary' AS scale
    ) scale_evidence
    CROSS JOIN LATERAL jsonb_array_elements(
        CASE
            WHEN jsonb_typeof(scale_evidence.scale->'candidate_results') = 'array'
            THEN scale_evidence.scale->'candidate_results'
            ELSE '[]'::jsonb
        END
    ) WITH ORDINALITY AS candidate(point, point_ordinal)
),
facts AS NOT MATERIALIZED (
    SELECT
        size_points.*,
        fact.shadow_decision_id,
        fact.route_id,
        fact.route_fingerprint,
        fact.origin_router,
        fact.pinned_block_number,
        fact.pinned_block_hash,
        fact.primary_state_hash,
        fact.route_config_hash,
        fact.pool_path,
        fact.pool_address_path,
        fact.fee_path,
        fact.protocol_path,
        fact.direction_path,
        fact.pool_state_hash_path,
        fact.detected_at,
        fact.evaluated_at,
        fact.opportunity_expires_at,
        fact.primary_provider_id,
        fact.primary_response_hash,
        fact.secondary_provider_id,
        fact.secondary_state_hash,
        fact.secondary_block_number,
        fact.secondary_block_hash,
        fact.secondary_route_config_hash,
        fact.independent_verification_status,
        fact.independent_verification_lifecycle,
        decision.observed_block,
        decision.market_evidence,
        decision.decision_evidence
    FROM size_points
    JOIN shadow_profitability_facts fact
      ON fact.source_event_identity = size_points.source_event_identity
     AND fact.route_fingerprint = size_points.scale->>'route_fingerprint'
     AND fact.evidence_completeness_status = 'complete'
    JOIN shadow_decisions decision
      ON decision.id = fact.shadow_decision_id
),
daily_route_rank AS (
    SELECT
        date_trunc('day', evaluated_at) AS evaluation_day,
        route_fingerprint,
        dense_rank() OVER (
            PARTITION BY date_trunc('day', evaluated_at)
            ORDER BY max((point->>'margin_to_profitability_gate')::numeric) DESC,
                     route_fingerprint
        ) AS route_rank
    FROM facts
    WHERE point->>'margin_to_profitability_gate' ~ '^-?[0-9]+$'
    GROUP BY date_trunc('day', evaluated_at), route_fingerprint
)
SELECT
    concat(
        facts.source_event_identity,
        ':',
        facts.route_fingerprint,
        ':',
        facts.point_ordinal::text
    ) AS evaluation_point_id,
    facts.source_event_identity,
    facts.source_sequence,
    facts.initiating_transaction_hash,
    facts.origin_router AS initiating_router,
    facts.economic_origin->'initiating_pool_ids' AS initiating_pool_ids,
    facts.economic_origin->>'initiating_swap_direction' AS initiating_swap_direction,
    facts.economic_origin->>'initiating_token_in' AS initiating_token_in,
    facts.economic_origin->>'initiating_token_out' AS initiating_token_out,
    facts.economic_origin->>'initiating_input_amount' AS initiating_input_amount,
    facts.route_id,
    facts.route_fingerprint,
    facts.pool_path,
    facts.pool_address_path,
    facts.fee_path,
    facts.protocol_path,
    facts.direction_path,
    facts.pinned_block_number,
    facts.pinned_block_hash,
    facts.primary_state_hash,
    facts.pool_state_hash_path,
    greatest(facts.pinned_block_number - facts.observed_block, 0) AS state_age_blocks,
    coalesce((facts.market_evidence->>'quote_age_ms')::numeric, 0) AS quote_age_ms,
    greatest(
        extract(epoch FROM (facts.opportunity_expires_at - facts.detected_at)) * 1000,
        0
    )::numeric AS candidate_age_ms,
    facts.point->'liquidity' AS active_liquidity_near_current_tick,
    coalesce((facts.point->>'tick_crossings')::integer, 0) AS tick_crossings,
    facts.point->>'candidate_size' AS input_size_wei,
    facts.point->>'spot_output' AS pre_fee_output_wei,
    facts.point->>'expected_output' AS expected_output_wei,
    facts.point->>'gross_spread' AS gross_spread_wei,
    facts.point->>'gross_profit' AS gross_profit_wei,
    facts.point->>'gross_spread_bps' AS gross_spread_bps,
    facts.point->>'price_impact' AS price_impact_wei,
    facts.point->>'price_impact_bps' AS price_impact_bps,
    facts.point->'fee_components'->>'pool_fees' AS dex_fees_wei,
    facts.point->'fee_components'->>'flash_loan_premium' AS flash_premium_wei,
    facts.point->>'execution_gas' AS execution_gas,
    facts.point->>'gas_price' AS gas_price_wei,
    facts.point->'fee_components'->>'arbitrum_execution_fee'
        AS arbitrum_execution_fee_wei,
    facts.point->'fee_components'->>'l1_data_fee' AS l1_data_fee_wei,
    facts.point->'fee_components'->>'contract_overhead' AS contract_overhead_wei,
    facts.point->'fee_components'->>'failed_attempt_reserve'
        AS failed_attempt_reserve_wei,
    facts.point->'fee_components'->>'ordering_reserve' AS ordering_reserve_wei,
    facts.point->'fee_components'->>'slippage_allowance' AS slippage_reserve_wei,
    facts.point->'fee_components'->>'stale_state_reserve' AS stale_state_reserve_wei,
    facts.point->'fee_components'->>'state_drift_reserve' AS state_drift_reserve_wei,
    facts.point->'fee_components'->>'latency_reserve' AS latency_reserve_wei,
    facts.point->'fee_components'->>'uncertainty_reserve' AS uncertainty_reserve_wei,
    facts.point->>'fixed_cost_wei' AS fixed_cost_wei,
    facts.point->>'variable_cost_wei' AS variable_cost_wei,
    facts.point->'fee_components'->>'base_total_cost' AS total_cost_wei,
    facts.point->>'expected_net_pnl' AS expected_net_pnl_wei,
    facts.point->>'conservative_net_pnl' AS conservative_net_pnl_wei,
    facts.point->>'severe_net_pnl' AS severe_net_pnl_wei,
    facts.point->>'net_pnl_bps' AS net_pnl_bps,
    facts.point->>'break_even_spread_bps' AS break_even_spread_bps,
    facts.point->>'minimum_required_net_pnl' AS minimum_required_net_pnl_wei,
    facts.point->>'margin_to_profitability_gate' AS margin_to_profitability_gate_wei,
    facts.point->>'rejection_reason' AS exact_rejection_reason,
    facts.point->>'price_divergence_direction' AS price_divergence_direction,
    facts.scale->>'best_input_wei' AS best_input_wei,
    facts.scale->>'best_expected_net_pnl_wei' AS best_expected_net_pnl_wei,
    facts.scale->>'best_margin_to_gate_wei' AS best_margin_to_gate_wei,
    facts.scale->>'break_even_size_wei' AS break_even_size_wei,
    facts.scale->>'size_elasticity' AS size_elasticity,
    rank.route_rank,
    facts.primary_provider_id,
    facts.primary_response_hash,
    facts.secondary_provider_id,
    facts.secondary_state_hash,
    facts.secondary_block_number,
    facts.secondary_block_hash,
    facts.secondary_route_config_hash,
    facts.independent_verification_status,
    facts.independent_verification_lifecycle,
    fork.status AS fork_status,
    fork.simulated_net_pnl AS fork_simulated_net_pnl_wei,
    fork.gas_used AS fork_gas_used,
    fork.prediction_error AS fork_prediction_error_wei,
    facts.classified_at,
    facts.detected_at,
    facts.evaluated_at,
    facts.event_to_evaluation_latency_ns
FROM facts
JOIN daily_route_rank rank
  ON rank.evaluation_day = date_trunc('day', facts.evaluated_at)
 AND rank.route_fingerprint = facts.route_fingerprint
LEFT JOIN LATERAL (
    SELECT result.status,
           result.simulated_net_pnl,
           result.gas_used,
           result.prediction_error
    FROM fork_simulation_results result
    WHERE result.shadow_decision_id = facts.shadow_decision_id
    ORDER BY result.simulated_at DESC, result.result_hash
    LIMIT 1
) fork ON true;
