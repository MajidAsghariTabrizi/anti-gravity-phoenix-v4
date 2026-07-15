ALTER TABLE shadow_profitability_facts
    ADD COLUMN IF NOT EXISTS route_config_hash TEXT,
    ADD COLUMN IF NOT EXISTS secondary_block_number NUMERIC(78,0),
    ADD COLUMN IF NOT EXISTS secondary_block_hash TEXT,
    ADD COLUMN IF NOT EXISTS secondary_route_config_hash TEXT,
    ADD COLUMN IF NOT EXISTS independent_verification_status TEXT,
    ADD COLUMN IF NOT EXISTS independent_verification_lifecycle JSONB;

ALTER TABLE shadow_profitability_facts
    DROP CONSTRAINT IF EXISTS shadow_profitability_verification_check;

ALTER TABLE shadow_profitability_facts
    ADD CONSTRAINT shadow_profitability_verification_check CHECK (
        CASE verification_status
            WHEN 'primary_only' THEN
                secondary_provider_id IS NULL
                AND secondary_state_hash IS NULL
                AND agreement_state = 'not_checked'
                AND verification_skip_reason IN (
                    'primary_below_minimum',
                    'primary_screen_no_profitable_candidate'
                )
            WHEN 'agreed' THEN
                secondary_provider_id IS NOT NULL
                AND secondary_state_hash IS NOT NULL
                AND secondary_state_hash = primary_state_hash
                AND agreement_state = 'agreed'
                AND verification_skip_reason IS NULL
            WHEN 'disagreed' THEN
                secondary_provider_id IS NOT NULL
                AND secondary_state_hash IS NOT NULL
                AND secondary_state_hash <> primary_state_hash
                AND agreement_state = 'disagreed'
                AND verification_skip_reason IS NULL
            WHEN 'secondary_unavailable' THEN
                secondary_provider_id IS NULL
                AND secondary_state_hash IS NULL
                AND agreement_state = 'unavailable'
                AND verification_skip_reason IS NULL
            WHEN 'historical_evidence' THEN
                secondary_provider_id IS NULL
                AND secondary_state_hash IS NULL
                AND agreement_state = 'not_checked'
                AND verification_skip_reason = 'historical_evidence'
            WHEN 'incomplete' THEN
                secondary_provider_id IS NULL
                AND secondary_state_hash IS NULL
                AND agreement_state = 'not_checked'
                AND verification_skip_reason IS NULL
            ELSE true
        END
    );

ALTER TABLE shadow_profitability_facts
    ADD CONSTRAINT shadow_profitability_independent_verification_check CHECK (
        (
            independent_verification_status IS NULL
            AND independent_verification_lifecycle IS NULL
            AND route_config_hash IS NULL
            AND secondary_block_number IS NULL
            AND secondary_block_hash IS NULL
            AND secondary_route_config_hash IS NULL
        )
        OR (
            route_config_hash IS NOT NULL
            AND route_config_hash ~ '^[0-9a-f]{64}$'
            AND independent_verification_status IS NOT NULL
            AND independent_verification_status IN (
                'not_requested',
                'requested',
                'agreed',
                'disagreed',
                'provider_unavailable',
                'integrity_failure'
            )
            AND independent_verification_lifecycle IS NOT NULL
            AND jsonb_typeof(independent_verification_lifecycle) = 'array'
            AND CASE independent_verification_status
                WHEN 'not_requested' THEN
                    independent_verification_lifecycle = '["not_requested"]'::jsonb
                    AND verification_status = 'primary_only'
                    AND verification_skip_reason = 'primary_screen_no_profitable_candidate'
                    AND secondary_provider_id IS NULL
                    AND secondary_state_hash IS NULL
                    AND secondary_block_number IS NULL
                    AND secondary_block_hash IS NULL
                    AND secondary_route_config_hash IS NULL
                WHEN 'requested' THEN
                    independent_verification_lifecycle = '["requested"]'::jsonb
                    AND verification_status = 'incomplete'
                    AND evidence_completeness_status = 'incomplete'
                    AND secondary_provider_id IS NULL
                    AND secondary_state_hash IS NULL
                    AND secondary_block_number IS NULL
                    AND secondary_block_hash IS NULL
                    AND secondary_route_config_hash IS NULL
                WHEN 'agreed' THEN
                    independent_verification_lifecycle = '["requested", "agreed"]'::jsonb
                    AND verification_status = 'agreed'
                    AND secondary_provider_id IS NOT NULL
                    AND secondary_provider_id <> primary_provider_id
                    AND secondary_state_hash = primary_state_hash
                    AND secondary_block_number IS NOT NULL
                    AND secondary_block_number = pinned_block_number
                    AND secondary_block_hash IS NOT NULL
                    AND secondary_block_hash = pinned_block_hash
                    AND secondary_route_config_hash IS NOT NULL
                    AND secondary_route_config_hash = route_config_hash
                WHEN 'disagreed' THEN
                    independent_verification_lifecycle = '["requested", "disagreed"]'::jsonb
                    AND verification_status = 'disagreed'
                    AND secondary_provider_id IS NOT NULL
                    AND secondary_provider_id <> primary_provider_id
                    AND secondary_state_hash IS NOT NULL
                    AND secondary_state_hash <> primary_state_hash
                    AND secondary_block_number IS NOT NULL
                    AND secondary_block_number = pinned_block_number
                    AND secondary_block_hash IS NOT NULL
                    AND secondary_block_hash = pinned_block_hash
                    AND secondary_route_config_hash IS NOT NULL
                    AND secondary_route_config_hash = route_config_hash
                WHEN 'provider_unavailable' THEN
                    independent_verification_lifecycle = '["requested", "provider_unavailable"]'::jsonb
                    AND verification_status = 'secondary_unavailable'
                    AND secondary_provider_id IS NULL
                    AND secondary_state_hash IS NULL
                    AND secondary_block_number IS NULL
                    AND secondary_block_hash IS NULL
                    AND secondary_route_config_hash IS NULL
                WHEN 'integrity_failure' THEN
                    independent_verification_lifecycle = '["requested", "integrity_failure"]'::jsonb
                    AND verification_status = 'secondary_unavailable'
                    AND secondary_provider_id IS NULL
                    AND secondary_state_hash IS NULL
                    AND secondary_block_number IS NULL
                    AND secondary_block_hash IS NULL
                    AND secondary_route_config_hash IS NULL
                ELSE false
            END
        )
    );

ALTER TABLE shadow_profitability_facts
    ADD CONSTRAINT shadow_profitability_secondary_identity_check CHECK (
        (secondary_block_number IS NULL OR secondary_block_number > 0)
        AND (
            secondary_block_hash IS NULL
            OR secondary_block_hash ~ '^0x[0-9a-f]{64}$'
        )
        AND (
            secondary_route_config_hash IS NULL
            OR secondary_route_config_hash ~ '^[0-9a-f]{64}$'
        )
    );

CREATE INDEX IF NOT EXISTS shadow_profitability_independent_verification_idx
    ON shadow_profitability_facts(independent_verification_status, evaluated_at DESC);

CREATE OR REPLACE VIEW shadow_profitability_report_rows AS
SELECT fact.shadow_decision_id::text AS candidate_key,
       fact.source_event_identity,
       fact.route_fingerprint,
       fact.token_path->>0 AS settlement_asset,
       fact.evaluated_at,
       fact.evidence_completeness_status,
       fact.disposition,
       fact.primary_profitability_status,
       fact.final_rejection_reason,
       fact.secondary_rejection_reasons,
       fact.expected_net_pnl,
       fact.conservative_net_pnl,
       fact.severe_net_pnl,
       fact.minimum_required_net_pnl,
       fact.input_amount,
       fact.expected_output,
       fact.gross_spread,
       fact.gross_profit,
       fact.execution_gas,
       fact.gas_price,
       fact.dex_fees,
       fact.price_impact,
       fact.arbitrum_execution_fee,
       fact.l1_data_fee,
       fact.flash_loan_premium,
       fact.protocol_fees,
       fact.failed_attempt_reserve,
       fact.ordering_reserve,
       fact.slippage_reserve,
       fact.stale_state_reserve,
       fact.state_drift_reserve,
       fact.latency_reserve,
       fact.uncertainty_reserve,
       fact.contract_overhead,
       fact.total_cost,
       fact.model_version,
       fact.verification_status,
       fact.agreement_state,
       fact.shadow_only,
       fact.execution_eligible,
       fact.execution_request_created,
       fact.pinned_block_number,
       fact.pinned_block_hash,
       fact.route_config_hash,
       fact.primary_provider_id,
       fact.primary_state_hash,
       fact.secondary_provider_id,
       fact.secondary_state_hash,
       fact.secondary_block_number,
       fact.secondary_block_hash,
       fact.secondary_route_config_hash,
       fact.independent_verification_status,
       fact.independent_verification_lifecycle,
       fact.verification_skip_reason
FROM shadow_profitability_facts AS fact
UNION ALL
SELECT concat(
           'classification:',
           classification.source_event_identity,
           ':',
           route.position::text
       ) AS candidate_key,
       classification.source_event_identity,
       route.fingerprint AS route_fingerprint,
       NULL::text AS settlement_asset,
       classification.classified_at AS evaluated_at,
       'incomplete'::text AS evidence_completeness_status,
       NULL::text AS disposition,
       'incomplete'::text AS primary_profitability_status,
       classification.detail_class AS final_rejection_reason,
       '[]'::jsonb AS secondary_rejection_reasons,
       NULL::numeric AS expected_net_pnl,
       NULL::numeric AS conservative_net_pnl,
       NULL::numeric AS severe_net_pnl,
       NULL::numeric AS minimum_required_net_pnl,
       NULL::numeric AS input_amount,
       NULL::numeric AS expected_output,
       NULL::numeric AS gross_spread,
       NULL::numeric AS gross_profit,
       NULL::numeric AS execution_gas,
       NULL::numeric AS gas_price,
       NULL::numeric AS dex_fees,
       NULL::numeric AS price_impact,
       NULL::numeric AS arbitrum_execution_fee,
       NULL::numeric AS l1_data_fee,
       NULL::numeric AS flash_loan_premium,
       NULL::numeric AS protocol_fees,
       NULL::numeric AS failed_attempt_reserve,
       NULL::numeric AS ordering_reserve,
       NULL::numeric AS slippage_reserve,
       NULL::numeric AS stale_state_reserve,
       NULL::numeric AS state_drift_reserve,
       NULL::numeric AS latency_reserve,
       NULL::numeric AS uncertainty_reserve,
       NULL::numeric AS contract_overhead,
       NULL::numeric AS total_cost,
       NULL::text AS model_version,
       'incomplete'::text AS verification_status,
       'not_checked'::text AS agreement_state,
       true AS shadow_only,
       false AS execution_eligible,
       false AS execution_request_created,
       NULL::numeric AS pinned_block_number,
       NULL::text AS pinned_block_hash,
       NULL::text AS route_config_hash,
       NULL::text AS primary_provider_id,
       NULL::text AS primary_state_hash,
       NULL::text AS secondary_provider_id,
       NULL::text AS secondary_state_hash,
       NULL::numeric AS secondary_block_number,
       NULL::text AS secondary_block_hash,
       NULL::text AS secondary_route_config_hash,
       NULL::text AS independent_verification_status,
       NULL::jsonb AS independent_verification_lifecycle,
       NULL::text AS verification_skip_reason
FROM shadow_engine_classifications AS classification
CROSS JOIN LATERAL jsonb_array_elements_text(
    CASE
        WHEN jsonb_typeof(classification.evidence->'route_fingerprints') = 'array'
            THEN classification.evidence->'route_fingerprints'
        ELSE '[]'::jsonb
    END
) WITH ORDINALITY AS route(fingerprint, position)
WHERE classification.candidate_count > 0
  AND NOT EXISTS (
      SELECT 1
      FROM shadow_profitability_facts AS fact
      WHERE fact.source_event_identity = classification.source_event_identity
        AND fact.route_fingerprint = route.fingerprint
  );
