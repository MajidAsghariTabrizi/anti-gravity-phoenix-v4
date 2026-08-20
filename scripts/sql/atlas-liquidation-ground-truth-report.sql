-- Read-only Atlas liquidation ground-truth report: joins the decoded public
-- LiquidationCall evidence against the auction ingress and shadow tables.
-- Never writes, never mutates; block-windowed by psql variables.
SELECT (
  json_build_object(
    'schema', 'phoenix.atlas-liquidation-ground-truth-report.v1',
    'generated_at', now()::text,
    'window', json_build_object(
      'from_block', :'window_start_block'::bigint,
      'to_block', :'window_end_block'::bigint
    ),
    'ground_truth', json_build_object(
      'liquidations_total', (
        SELECT count(*) FROM live_canary.atlas_liquidation_ground_truth gt
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
      ),
      'weth_debt_total', (
        SELECT count(*) FROM live_canary.atlas_liquidation_ground_truth gt
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
          AND gt.debt_asset = '0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2'
      ),
      'non_weth_debt_total', (
        SELECT count(*) FROM live_canary.atlas_liquidation_ground_truth gt
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
          AND gt.debt_asset <> '0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2'
      ),
      'non_weth_debt_by_asset', (
        SELECT COALESCE(json_object_agg(debt_asset, cnt), '{}'::json) FROM (
          SELECT gt.debt_asset, count(*) AS cnt
          FROM live_canary.atlas_liquidation_ground_truth gt
          WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
            AND gt.debt_asset <> '0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2'
          GROUP BY gt.debt_asset
        ) non_weth
      )
    ),
    'auction_evidence', json_build_object(
      'distinct_user_operations', (
        SELECT count(DISTINCT gt.user_operation_hash)
        FROM live_canary.atlas_liquidation_ground_truth gt
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
      ),
      'with_ingress', (
        SELECT count(DISTINCT gt.user_operation_hash)
        FROM live_canary.atlas_liquidation_ground_truth gt
        JOIN live_canary.atlas_auction_ingress i ON i.user_operation_hash = gt.user_operation_hash
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
      ),
      'with_shadow_evaluation', (
        SELECT count(DISTINCT gt.user_operation_hash)
        FROM live_canary.atlas_liquidation_ground_truth gt
        JOIN live_canary.atlas_auction_shadow sh ON sh.user_operation_hash = gt.user_operation_hash
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
      ),
      'shadow_eligible', (
        SELECT count(DISTINCT gt.user_operation_hash)
        FROM live_canary.atlas_liquidation_ground_truth gt
        JOIN live_canary.atlas_auction_shadow sh
          ON sh.user_operation_hash = gt.user_operation_hash AND sh.shadow_bid_eligible
        WHERE gt.block_number BETWEEN :'window_start_block'::bigint AND :'window_end_block'::bigint
      )
    )
  )
)::text;
