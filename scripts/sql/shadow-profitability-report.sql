\set ON_ERROR_STOP on

BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;

WITH bounded_rows AS (
    SELECT *
    FROM shadow_profitability_report_rows
    ORDER BY evaluated_at DESC, candidate_key DESC
    LIMIT :report_limit
)
SELECT jsonb_build_object(
           'candidate_key', candidate_key,
           'source_event_identity', source_event_identity,
           'route_fingerprint', route_fingerprint,
           'settlement_asset', settlement_asset,
           'evaluated_at', to_char(
               evaluated_at AT TIME ZONE 'UTC',
               'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'
           ),
           'evidence_completeness_status', evidence_completeness_status,
           'disposition', disposition,
           'primary_profitability_status', primary_profitability_status,
           'final_rejection_reason', final_rejection_reason,
           'secondary_rejection_reasons', secondary_rejection_reasons,
           'expected_net_pnl', expected_net_pnl::text,
           'conservative_net_pnl', conservative_net_pnl::text,
           'severe_net_pnl', severe_net_pnl::text,
           'minimum_required_net_pnl', minimum_required_net_pnl::text,
           'input_amount', input_amount::text,
           'expected_output', expected_output::text,
           'gross_spread', gross_spread::text,
           'gross_profit', gross_profit::text,
           'execution_gas', execution_gas::text,
           'gas_price', gas_price::text,
           'dex_fees', dex_fees::text,
           'price_impact', price_impact::text,
           'arbitrum_execution_fee', arbitrum_execution_fee::text,
           'l1_data_fee', l1_data_fee::text,
           'flash_loan_premium', flash_loan_premium::text,
           'protocol_fees', protocol_fees::text,
           'failed_attempt_reserve', failed_attempt_reserve::text,
           'ordering_reserve', ordering_reserve::text,
           'slippage_reserve', slippage_reserve::text,
           'stale_state_reserve', stale_state_reserve::text,
           'state_drift_reserve', state_drift_reserve::text,
           'latency_reserve', latency_reserve::text,
           'uncertainty_reserve', uncertainty_reserve::text,
           'contract_overhead', contract_overhead::text,
           'total_cost', total_cost::text,
           'model_version', model_version,
           'verification_status', verification_status,
           'agreement_state', agreement_state,
           'shadow_only', shadow_only,
           'execution_eligible', execution_eligible,
           'execution_request_created', execution_request_created
       )::text
FROM bounded_rows
ORDER BY evaluated_at DESC, candidate_key DESC;

COMMIT;
