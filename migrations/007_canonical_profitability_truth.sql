CREATE TABLE IF NOT EXISTS shadow_profitability_facts (
    shadow_decision_id UUID PRIMARY KEY REFERENCES shadow_decisions(id) ON DELETE CASCADE,
    source_event_identity TEXT,
    source_sequence NUMERIC(78,0),
    transaction_hash TEXT,
    chain_id BIGINT,
    route_id TEXT,
    route_fingerprint TEXT,
    detected_at TIMESTAMPTZ,
    evaluated_at TIMESTAMPTZ NOT NULL,
    pinned_block_number NUMERIC(78,0),
    pinned_block_hash TEXT,
    primary_state_hash TEXT,
    token_path JSONB,
    pool_path JSONB,
    fee_path JSONB,
    input_amount NUMERIC(78,0),
    expected_output NUMERIC(78,0),
    gross_spread NUMERIC(78,0),
    gross_profit NUMERIC(78,0),
    dex_fees NUMERIC(78,0),
    price_impact NUMERIC(78,0),
    execution_gas NUMERIC(78,0),
    gas_price NUMERIC(78,0),
    arbitrum_execution_fee NUMERIC(78,0),
    l1_data_fee NUMERIC(78,0),
    flash_loan_premium NUMERIC(78,0),
    protocol_fees NUMERIC(78,0),
    failed_attempt_reserve NUMERIC(78,0),
    ordering_reserve NUMERIC(78,0),
    slippage_reserve NUMERIC(78,0),
    stale_state_reserve NUMERIC(78,0),
    state_drift_reserve NUMERIC(78,0),
    latency_reserve NUMERIC(78,0),
    uncertainty_reserve NUMERIC(78,0),
    contract_overhead NUMERIC(78,0),
    total_cost NUMERIC(78,0),
    expected_net_pnl NUMERIC(78,0),
    conservative_net_pnl NUMERIC(78,0),
    severe_net_pnl NUMERIC(78,0),
    minimum_required_net_pnl NUMERIC(78,0),
    primary_profitability_status TEXT NOT NULL DEFAULT 'incomplete',
    disposition TEXT,
    final_rejection_reason TEXT,
    secondary_rejection_reasons JSONB NOT NULL DEFAULT '[]'::jsonb,
    model_version TEXT,
    policy_version TEXT,
    detector_version TEXT,
    code_version TEXT,
    primary_provider_id TEXT,
    primary_response_hash TEXT,
    secondary_provider_id TEXT,
    secondary_state_hash TEXT,
    verification_status TEXT NOT NULL DEFAULT 'incomplete',
    agreement_state TEXT NOT NULL DEFAULT 'not_checked',
    verification_skip_reason TEXT,
    shadow_only BOOLEAN NOT NULL DEFAULT true,
    execution_eligible BOOLEAN NOT NULL DEFAULT false,
    execution_request_created BOOLEAN NOT NULL DEFAULT false,
    evidence_completeness_status TEXT NOT NULL DEFAULT 'incomplete',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT shadow_profitability_identity_check CHECK (
        source_event_identity IS NULL
        OR char_length(source_event_identity) BETWEEN 1 AND 200
    ),
    CONSTRAINT shadow_profitability_sequence_check CHECK (
        source_sequence IS NULL OR source_sequence >= 0
    ),
    CONSTRAINT shadow_profitability_transaction_hash_check CHECK (
        transaction_hash IS NULL OR transaction_hash ~ '^0x[0-9a-f]{64}$'
    ),
    CONSTRAINT shadow_profitability_chain_check CHECK (
        chain_id IS NULL OR chain_id = 42161
    ),
    CONSTRAINT shadow_profitability_route_check CHECK (
        (route_id IS NULL OR char_length(route_id) BETWEEN 1 AND 128)
        AND (route_fingerprint IS NULL OR char_length(route_fingerprint) BETWEEN 1 AND 256)
    ),
    CONSTRAINT shadow_profitability_block_check CHECK (
        (pinned_block_number IS NULL OR pinned_block_number > 0)
        AND (
            pinned_block_hash IS NULL
            OR pinned_block_hash ~ '^0x[0-9a-f]{64}$'
        )
    ),
    CONSTRAINT shadow_profitability_hash_check CHECK (
        (primary_state_hash IS NULL OR primary_state_hash ~ '^[0-9a-f]{64}$')
        AND (
            primary_response_hash IS NULL
            OR primary_response_hash ~ '^[0-9a-f]{64}$'
        )
        AND (
            secondary_state_hash IS NULL
            OR secondary_state_hash ~ '^[0-9a-f]{64}$'
        )
    ),
    CONSTRAINT shadow_profitability_paths_check CHECK (
        (token_path IS NULL OR jsonb_typeof(token_path) = 'array')
        AND (pool_path IS NULL OR jsonb_typeof(pool_path) = 'array')
        AND (fee_path IS NULL OR jsonb_typeof(fee_path) = 'array')
    ),
    CONSTRAINT shadow_profitability_unsigned_costs_check CHECK (
        (input_amount IS NULL OR input_amount >= 0)
        AND (expected_output IS NULL OR expected_output >= 0)
        AND (
            minimum_required_net_pnl IS NULL
            OR minimum_required_net_pnl >= 0
        )
        AND (dex_fees IS NULL OR dex_fees >= 0)
        AND (price_impact IS NULL OR price_impact >= 0)
        AND (execution_gas IS NULL OR execution_gas >= 0)
        AND (gas_price IS NULL OR gas_price >= 0)
        AND (arbitrum_execution_fee IS NULL OR arbitrum_execution_fee >= 0)
        AND (l1_data_fee IS NULL OR l1_data_fee >= 0)
        AND (flash_loan_premium IS NULL OR flash_loan_premium >= 0)
        AND (protocol_fees IS NULL OR protocol_fees >= 0)
        AND (failed_attempt_reserve IS NULL OR failed_attempt_reserve >= 0)
        AND (ordering_reserve IS NULL OR ordering_reserve >= 0)
        AND (slippage_reserve IS NULL OR slippage_reserve >= 0)
        AND (stale_state_reserve IS NULL OR stale_state_reserve >= 0)
        AND (state_drift_reserve IS NULL OR state_drift_reserve >= 0)
        AND (latency_reserve IS NULL OR latency_reserve >= 0)
        AND (uncertainty_reserve IS NULL OR uncertainty_reserve >= 0)
        AND (contract_overhead IS NULL OR contract_overhead >= 0)
        AND (total_cost IS NULL OR total_cost >= 0)
    ),
    CONSTRAINT shadow_profitability_status_check CHECK (
        primary_profitability_status IN ('incomplete', 'meets_minimum', 'below_minimum')
        AND evidence_completeness_status IN ('complete', 'incomplete')
        AND (disposition IS NULL OR disposition IN ('accepted', 'rejected'))
        AND verification_status IN (
            'incomplete',
            'primary_only',
            'agreed',
            'disagreed',
            'secondary_unavailable',
            'historical_evidence'
        )
        AND agreement_state IN ('not_checked', 'agreed', 'disagreed', 'unavailable')
    ),
    CONSTRAINT shadow_profitability_reasons_check CHECK (
        jsonb_typeof(secondary_rejection_reasons) = 'array'
        AND (
            evidence_completeness_status <> 'complete'
            OR
            disposition IS DISTINCT FROM 'rejected'
            OR final_rejection_reason IS NOT NULL
        )
    ),
    CONSTRAINT shadow_profitability_provider_check CHECK (
        (
            primary_provider_id IS NULL
            OR (
                char_length(primary_provider_id) BETWEEN 1 AND 128
                AND primary_provider_id NOT LIKE '%://%'
            )
        )
        AND (
            secondary_provider_id IS NULL
            OR (
                char_length(secondary_provider_id) BETWEEN 1 AND 128
                AND secondary_provider_id NOT LIKE '%://%'
            )
        )
    ),
    CONSTRAINT shadow_profitability_verification_check CHECK (
        CASE verification_status
            WHEN 'primary_only' THEN
                secondary_provider_id IS NULL
                AND secondary_state_hash IS NULL
                AND agreement_state = 'not_checked'
                AND verification_skip_reason = 'primary_below_minimum'
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
    ),
    CONSTRAINT shadow_profitability_safety_check CHECK (
        shadow_only = true
        AND execution_eligible = false
        AND execution_request_created = false
    ),
    CONSTRAINT shadow_profitability_arithmetic_check CHECK (
        evidence_completeness_status <> 'complete'
        OR (
            gross_profit = gross_spread - protocol_fees - dex_fees - price_impact
            AND arbitrum_execution_fee = execution_gas * gas_price
            AND total_cost = protocol_fees
                + dex_fees
                + price_impact
                + slippage_reserve
                + flash_loan_premium
                + arbitrum_execution_fee
                + l1_data_fee
                + contract_overhead
                + failed_attempt_reserve
                + stale_state_reserve
                + ordering_reserve
                + state_drift_reserve
                + latency_reserve
                + uncertainty_reserve
            AND expected_net_pnl = gross_spread - total_cost
            AND expected_net_pnl >= conservative_net_pnl
            AND conservative_net_pnl >= severe_net_pnl
            AND (
                (primary_profitability_status = 'meets_minimum'
                    AND expected_net_pnl >= minimum_required_net_pnl)
                OR (primary_profitability_status = 'below_minimum'
                    AND expected_net_pnl < minimum_required_net_pnl)
            )
        )
    ),
    CONSTRAINT shadow_profitability_complete_check CHECK (
        evidence_completeness_status <> 'complete'
        OR (
            source_event_identity IS NOT NULL
            AND source_sequence IS NOT NULL
            AND transaction_hash IS NOT NULL
            AND chain_id = 42161
            AND route_id IS NOT NULL
            AND route_fingerprint IS NOT NULL
            AND detected_at IS NOT NULL
            AND pinned_block_number IS NOT NULL
            AND pinned_block_hash IS NOT NULL
            AND primary_state_hash IS NOT NULL
            AND token_path IS NOT NULL
            AND pool_path IS NOT NULL
            AND fee_path IS NOT NULL
            AND jsonb_array_length(token_path) >= 2
            AND jsonb_array_length(pool_path) >= 1
            AND jsonb_array_length(token_path) = jsonb_array_length(pool_path) + 1
            AND jsonb_array_length(pool_path) = jsonb_array_length(fee_path)
            AND (token_path ->> 0) = (token_path ->> -1)
            AND input_amount IS NOT NULL
            AND expected_output IS NOT NULL
            AND gross_spread IS NOT NULL
            AND gross_profit IS NOT NULL
            AND dex_fees IS NOT NULL
            AND price_impact IS NOT NULL
            AND execution_gas IS NOT NULL
            AND gas_price IS NOT NULL
            AND arbitrum_execution_fee IS NOT NULL
            AND l1_data_fee IS NOT NULL
            AND flash_loan_premium IS NOT NULL
            AND protocol_fees IS NOT NULL
            AND failed_attempt_reserve IS NOT NULL
            AND ordering_reserve IS NOT NULL
            AND slippage_reserve IS NOT NULL
            AND stale_state_reserve IS NOT NULL
            AND state_drift_reserve IS NOT NULL
            AND latency_reserve IS NOT NULL
            AND uncertainty_reserve IS NOT NULL
            AND contract_overhead IS NOT NULL
            AND total_cost IS NOT NULL
            AND expected_net_pnl IS NOT NULL
            AND conservative_net_pnl IS NOT NULL
            AND severe_net_pnl IS NOT NULL
            AND minimum_required_net_pnl IS NOT NULL
            AND primary_profitability_status <> 'incomplete'
            AND disposition IS NOT NULL
            AND model_version IS NOT NULL
            AND policy_version IS NOT NULL
            AND detector_version IS NOT NULL
            AND code_version IS NOT NULL
            AND primary_provider_id IS NOT NULL
            AND primary_response_hash IS NOT NULL
            AND verification_status <> 'incomplete'
        )
    )
);

INSERT INTO shadow_profitability_facts (
    shadow_decision_id,
    source_event_identity,
    source_sequence,
    chain_id,
    route_fingerprint,
    detected_at,
    evaluated_at,
    disposition,
    final_rejection_reason,
    secondary_rejection_reasons,
    policy_version,
    detector_version,
    code_version,
    shadow_only,
    execution_eligible,
    execution_request_created,
    evidence_completeness_status
)
SELECT id,
       source_event_identity,
       source_sequence,
       chain_id,
       route_fingerprint,
       detected_at,
       decided_at,
       disposition,
       primary_rejection_reason,
       secondary_rejection_reasons,
       policy_version,
       detector_version,
       code_version,
       true,
       false,
       false,
       'incomplete'
FROM shadow_decisions
ON CONFLICT (shadow_decision_id) DO NOTHING;

CREATE INDEX IF NOT EXISTS shadow_profitability_evaluated_idx
    ON shadow_profitability_facts(evaluated_at DESC, shadow_decision_id DESC);
CREATE INDEX IF NOT EXISTS shadow_profitability_route_idx
    ON shadow_profitability_facts(route_fingerprint, evaluated_at DESC);
CREATE INDEX IF NOT EXISTS shadow_profitability_rejection_idx
    ON shadow_profitability_facts(final_rejection_reason, evaluated_at DESC)
    WHERE disposition = 'rejected';
CREATE INDEX IF NOT EXISTS shadow_profitability_model_idx
    ON shadow_profitability_facts(model_version, primary_profitability_status);
CREATE INDEX IF NOT EXISTS shadow_profitability_verification_idx
    ON shadow_profitability_facts(verification_status, evaluated_at DESC);
CREATE INDEX IF NOT EXISTS shadow_engine_classification_profitability_idx
    ON shadow_engine_classifications(classified_at DESC, source_event_identity)
    WHERE candidate_count > 0;

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
       fact.execution_request_created
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
       false AS execution_request_created
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
