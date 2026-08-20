-- Shadow Atlas validation metrics (read-only observation evidence).
--
-- Executed through psql with -v window_start=... -v window_end=... as ISO-8601
-- UTC timestamps. Emits exactly one JSON object row of integer counts and
-- text-encoded integer sums. This file performs no DDL and no writes; every
-- statement is a SELECT against live_canary.
--
-- Metrics:
--   coverage            relevant ingress auctions vs shadow-evaluated auctions
--   callback_simulation attempted/passed counters for atlas-mode simulations
--   bid_ability         eligible rows and rows with a maximum bid
--   value_proxy         shadow-only expected/conservative net-after-bid sums
--   zero_invariants     counts that must remain zero while financial
--                       execution authority is closed
SELECT json_build_object(
    'schema', 'phoenix.atlas-shadow-validation.v1',
    'window', json_build_object(
        'start', :'window_start',
        'end', :'window_end'
    ),
    'coverage', json_build_object(
        'relevant_ingress', (
            SELECT count(DISTINCT auction_id)
            FROM live_canary.atlas_auction_ingress
            WHERE relevant_aave
              AND observed_at >= :'window_start'::timestamptz
              AND observed_at < :'window_end'::timestamptz
        ),
        'shadow_evaluated', (
            SELECT count(DISTINCT auction_id)
            FROM live_canary.atlas_auction_shadow
            WHERE evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        )
    ),
    'callback_simulation', json_build_object(
        'attempted', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE callback_simulation_attempted
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        ),
        'passed', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE callback_simulation_attempted
              AND callback_simulation_passed
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        )
    ),
    'bid_ability', json_build_object(
        'evaluated_rows', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        ),
        'eligible_rows', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE shadow_bid_eligible
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        ),
        'eligible_with_maximum_bid', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE shadow_bid_eligible
              AND maximum_bid_wei > 0
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        ),
        'rejected_rows', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE NOT shadow_bid_eligible
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        )
    ),
    'value_proxy', json_build_object(
        'expected_net_after_bid_sum', (
            SELECT coalesce(sum(expected_net_after_bid_wei), 0)::text
            FROM live_canary.atlas_auction_shadow
            WHERE shadow_bid_eligible
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        ),
        'conservative_net_after_bid_sum', (
            SELECT coalesce(sum(conservative_net_after_bid_wei), 0)::text
            FROM live_canary.atlas_auction_shadow
            WHERE shadow_bid_eligible
              AND evaluated_at >= :'window_start'::timestamptz
              AND evaluated_at < :'window_end'::timestamptz
        )
    ),
    'zero_invariants', json_build_object(
        'atlas_solver_requests_total', (
            SELECT count(*) FROM live_canary.atlas_solver_requests
        ),
        'execution_requests_total', (
            SELECT count(*) FROM live_canary.execution_requests
        ),
        'active_attempts', (
            SELECT count(*)
            FROM live_canary.execution_attempts
            WHERE status IN (
                'claimed', 'nonce_allocated', 'submission_unknown',
                'pending', 'timed_out'
            )
        ),
        'unresolved_submissions', (
            SELECT count(*)
            FROM live_canary.execution_attempts
            WHERE status IN ('submission_unknown', 'pending', 'timed_out')
        ),
        'eligible_rows_with_rejection_reason', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE shadow_bid_eligible
              AND terminal_rejection_reason IS NOT NULL
        ),
        'eligible_rows_without_maximum_bid', (
            SELECT count(*)
            FROM live_canary.atlas_auction_shadow
            WHERE shadow_bid_eligible
              AND (maximum_bid_wei IS NULL OR maximum_bid_wei <= 0)
        )
    )
)::text;
