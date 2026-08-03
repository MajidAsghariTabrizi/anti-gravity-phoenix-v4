import copy
import hashlib
import json
import unittest

from scripts.atlas_borrower_index import (
    EvidenceError,
    RAY,
    WAD,
    bind_hash,
    build_inventory,
    build_inventory_from_checkpoint,
    calculate_account,
    evaluate_auction,
    summarize_current_state,
    validate_transcript,
    verify_inventory,
)


ZERO = "0x" + "00" * 20
BORROWER = "0x" + "b0" * 20
ASSET_COLLATERAL = "0x" + "11" * 20
ASSET_DEBT = "0x" + "22" * 20
FEED_COLLATERAL = "0x" + "31" * 20
FEED_DEBT = "0x" + "32" * 20
BLOCK_HASH = "0x" + "a1" * 32
PARENT_HASH = "0x" + "a0" * 32


def address(byte: str) -> str:
    return "0x" + byte * 40


def transaction_hash(number: int) -> str:
    return "0x" + f"{number:064x}"


def market(*, complete: bool = True):
    value = {
        "schema": "phoenix.atlas.aave-market.v1",
        "chain_id": 42161,
        "evidence_status": "complete" if complete else "incomplete",
        "protocol": {
            "pool": address("4"),
            "pool_addresses_provider": address("5"),
            "oracle": address("6"),
            "data_provider": address("7"),
        },
        "liquidation_logic": {
            "source_commit": "test-only",
            "pool_implementation": address("3") if complete else None,
            "pool_implementation_code_hash": "0x" + "33" * 32 if complete else None,
            "default_close_factor_bps": 5_000 if complete else None,
            "close_factor_hf_threshold_wad": 950_000_000_000_000_000 if complete else None,
            "minimum_reserve_value_base": 2_000 * 10**8 if complete else None,
            "minimum_leftover_base": 1_000 * 10**8 if complete else None,
        },
        "sources": {
            "aave_address_book": {"commit": "test-only"},
            "aave_v3_origin": {"commit": "test-only"},
        },
        "reserves": [
            {
                "asset": ASSET_COLLATERAL,
                "symbol": "TEST-COLLATERAL",
                "decimals": 18,
                "atoken": address("8"),
                "variable_debt_token": address("9"),
                "stable_debt_token": None,
                "price_feed": FEED_COLLATERAL,
                "reserve_id": 1 if complete else None,
                "active": True if complete else None,
                "paused": False if complete else None,
                "liquidation_grace_period_until": 0 if complete else None,
                "liquidation_threshold_bps": 8_000 if complete else None,
                "liquidation_bonus_bps": 10_500 if complete else None,
                "liquidation_protocol_fee_bps": 1_000 if complete else None,
                "liquidity_index_ray": RAY if complete else None,
                "variable_borrow_index_ray": RAY if complete else None,
            },
            {
                "asset": ASSET_DEBT,
                "symbol": "TEST-DEBT",
                "decimals": 18,
                "atoken": address("a"),
                "variable_debt_token": address("c"),
                "stable_debt_token": address("d"),
                "price_feed": FEED_DEBT,
                "reserve_id": 2 if complete else None,
                "active": True if complete else None,
                "paused": False if complete else None,
                "liquidation_grace_period_until": 0 if complete else None,
                "liquidation_threshold_bps": 8_000 if complete else None,
                "liquidation_bonus_bps": 10_500 if complete else None,
                "liquidation_protocol_fee_bps": 1_000 if complete else None,
                "liquidity_index_ray": RAY if complete else None,
                "variable_borrow_index_ray": RAY if complete else None,
            },
        ],
        "emode_categories": [],
    }
    return bind_hash(value)


def event(index, kind, **fields):
    return {
        "transaction_hash": transaction_hash(index + 1),
        "log_index": index,
        "event_kind": kind,
        **fields,
    }


def transcript(market_value, *, complete=True):
    supplied = 100 * 10**18
    borrowed = 80 * 10**18
    logs = [
        event(
            0,
            "supply",
            asset=ASSET_COLLATERAL,
            user=BORROWER,
            on_behalf_of=BORROWER,
            amount=supplied,
        ),
        event(
            1,
            "atoken_mint",
            asset=ASSET_COLLATERAL,
            on_behalf_of=BORROWER,
            scaled_amount=supplied,
            accounting_role="primary",
        ),
        event(
            2,
            "collateral_enabled",
            asset=ASSET_COLLATERAL,
            user=BORROWER,
        ),
        event(
            3,
            "borrow",
            asset=ASSET_DEBT,
            user=BORROWER,
            on_behalf_of=BORROWER,
            amount=borrowed,
        ),
        event(
            4,
            "variable_debt_mint",
            asset=ASSET_DEBT,
            on_behalf_of=BORROWER,
            scaled_amount=borrowed,
            accounting_role="primary",
        ),
        event(
            5,
            "variable_debt_transfer",
            asset=ASSET_DEBT,
            **{"from": ZERO, "to": BORROWER},
            accounting_role="mirror",
        ),
        event(
            6,
            "account_configuration_snapshot",
            user=BORROWER,
            configuration_bitmap=24,
        ),
    ]
    value = {
        "schema": "phoenix.atlas.aave-archive-transcript.v1",
        "market_content_sha256": market_value["content_sha256"],
        "start_block": 100,
        "start_hash": BLOCK_HASH,
        "end_block": 100,
        "end_hash": BLOCK_HASH,
        "archive_complete": complete,
        "canonical_head_confirmed": complete,
        "reviewed_start_state_zero": complete,
        "source_methods": ["eth_getBlockByNumber", "eth_getLogs"] if complete else [],
        "blocks": [
            {
                "number": 100,
                "hash": BLOCK_HASH,
                "parent_hash": PARENT_HASH,
                "logs": logs,
            }
        ],
    }
    return bind_hash(value)


def checkpoint(market_value):
    block = 100
    header = {"number": block, "hash": BLOCK_HASH, "parent_hash": PARENT_HASH}
    providers = ("reviewed-provider-1", "reviewed-provider-2")
    reserves = []
    for reserve in market_value["reserves"]:
        reserves.append(
            {
                **copy.deepcopy(reserve),
                "configuration_bitmap": 0,
                "ltv_bps": 7_500,
                "reserve_factor_bps": 1_000,
                "usage_as_collateral_enabled": True,
                "borrowing_enabled": True,
                "stable_borrowing_enabled": True,
                "frozen": False,
                "price_base_units": 10**8,
                "price_base_decimals": 8,
                "stable_debt_token": reserve["stable_debt_token"] or ZERO,
            }
        )
    source_impl = market_value["liquidation_logic"]["pool_implementation"]
    empty_tail_hash = hashlib.sha256(
        json.dumps([], sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    value = {
        "schema": "phoenix.atlas.aave-checkpoint.v1",
        "chain_id": 42161,
        "checkpoint_block": block,
        "checkpoint_hash": BLOCK_HASH,
        "discovery_content_sha256": "d" * 64,
        "archive_checkpoint_block": 99,
        "finalized_heads": [
            {"provider_id": provider, "number": block, "hash": BLOCK_HASH}
            for provider in providers
        ],
        "tail_discovery": {
            "collection_provider_id": providers[0],
            "independent_log_verification": True,
            "provider_bindings": [
                {
                    "provider_id": provider,
                    "log_count": 0,
                    "logs_content_sha256": empty_tail_hash,
                }
                for provider in providers
            ],
            "start_block": block,
            "end_block": block,
            "log_count": 0,
            "borrower_count": 0,
            "logs_content_sha256": empty_tail_hash,
            "logs": [],
        },
        "protocol": {
            "pool": market_value["protocol"]["pool"],
            "data_provider": market_value["protocol"]["data_provider"],
            "oracle": market_value["protocol"]["oracle"],
            "pool_implementation": source_impl,
        },
        "provider_headers": [
            {"provider_id": provider, "checkpoint": copy.deepcopy(header)}
            for provider in providers
        ],
        "protocol_code_bindings": [
            {
                "provider_id": provider,
                "pool_implementation": source_impl,
                "code_sha256": {
                    "pool": "1" * 64,
                    "data_provider": "2" * 64,
                    "oracle": "3" * 64,
                    "pool_implementation": "4" * 64,
                },
            }
            for provider in providers
        ],
        "protocol_code_independent_agreement": True,
        "state_bindings": [
            {
                "provider_id": provider,
                "context": context,
                "call_count": 2,
                "result_sha256": "5" * 64,
            }
            for context in (
                "reserve_list",
                "reserve_state",
                "borrower_activity_retained",
                "borrower_state",
                "emode_state",
            )
            for provider in providers
        ]
        + [
            {
                "provider_id": providers[0],
                "context": "borrower_activity_primary",
                "call_count": 1,
                "result_sha256": "5" * 64,
            }
        ],
        "source_bindings": {
            "aave_address_book": {
                "commit": market_value["sources"]["aave_address_book"]["commit"],
                "path": "src/AaveV3Arbitrum.sol",
                "pool_implementation": source_impl,
            },
            "aave_v3_origin": {
                "commit": market_value["sources"]["aave_v3_origin"]["commit"],
                "path": "LiquidationLogic.sol",
            },
        },
        "liquidation_logic": {
            "default_close_factor_bps": 5_000,
            "close_factor_hf_threshold_wad": 950_000_000_000_000_000,
            "minimum_reserve_value_base": 2_000 * 10**8,
            "minimum_leftover_base": 1_000 * 10**8,
        },
        "archive_complete": True,
        "independent_state_agreement": True,
        "independent_state_agreement_scope": [
            "checkpoint_block_hash",
            "reserve_state",
            "retained_borrower_configuration",
            "retained_borrower_state",
            "emode_state",
        ],
        "source_methods": [
            "eth_chainId",
            "eth_getBlockByNumber",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_call",
            "eth_getLogs",
        ],
        "reserves": reserves,
        "emode_categories": [],
        "discovered_borrower_count": 1,
        "historical_discovered_borrower_count": 1,
        "tail_discovered_borrower_count": 0,
        "screened_borrower_count": 1,
        "discovery_log_count": 3,
        "active_borrower_count": 1,
        "debt_bearing_borrower_count": 1,
        "borrowers": [
            {
                "address": BORROWER,
                "account_configuration_bitmap": 24,
                "emode_category": 0,
                "positions": [
                    {
                        "asset": ASSET_COLLATERAL,
                        "current_supply": 100 * 10**18,
                        "scaled_supply": 100 * 10**18,
                        "current_stable_debt": 0,
                        "current_variable_debt": 0,
                        "principal_stable_debt": 0,
                        "scaled_variable_debt": 0,
                        "usage_as_collateral_enabled": True,
                    },
                    {
                        "asset": ASSET_DEBT,
                        "current_supply": 0,
                        "scaled_supply": 0,
                        "current_stable_debt": 0,
                        "current_variable_debt": 80 * 10**18,
                        "principal_stable_debt": 0,
                        "scaled_variable_debt": 80 * 10**18,
                        "usage_as_collateral_enabled": False,
                    },
                ],
            }
        ],
        "execution_authority": {
            "signer": False,
            "bond": False,
            "bid": False,
            "solver": False,
            "submission": False,
            "production_write": False,
        },
    }
    return bind_hash(value)


def current_state_checkpoint(market_value):
    value = checkpoint(market_value)
    value.pop("content_sha256")
    value["schema"] = "phoenix.atlas.aave-checkpoint.v2"
    value.pop("archive_complete")
    value["checkpoint_timestamp"] = 1_700_000_000
    for provider_index, provider_header in enumerate(value["provider_headers"]):
        provider_header["provider_reference_sha256"] = str(8 + provider_index) * 64
        provider_header["checkpoint"]["timestamp"] = 1_700_000_000
    value["seed_provenance"] = {
        "role": "discovery_only",
        "grants_candidate_authority": False,
        "grants_execution_authority": False,
        "archive_complete_claimed": True,
        "historical_independent_validation_claimed": False,
    }
    value["candidate_authority"] = {
        "source": "exact_finalized_current_state",
        "requires_two_independent_provider_agreement": True,
        "historical_archive_required": False,
        "execution_authority": False,
    }
    value["tail_discovery"].update(
        {
            "independent_log_verification": False,
            "exact_discovered_log_verification": True,
            "range_completeness_claimed": False,
            "grants_candidate_authority": False,
        }
    )
    for binding in value["tail_discovery"]["provider_bindings"]:
        binding.update(
            {
                "verification_mode": (
                    "primary_discovery_secondary_exact_receipts"
                ),
                "range_completeness_claimed": False,
                "grants_candidate_authority": False,
            }
        )
    value["screen_scope"] = {
        "mode": "bounded_resumable_exact_batch",
        "batch_address_count": 1,
        "seed_scan_complete_after_batch": False,
    }
    value["independent_state_agreement_scope"].extend(
        ["protocol_implementation_and_code", "oracle_round_state"]
    )
    providers = ("reviewed-provider-1", "reviewed-provider-2")
    value["state_bindings"].extend(
        {
            "provider_id": provider,
            "context": "oracle_round_state",
            "call_count": 4,
            "result_sha256": "6" * 64,
        }
        for provider in providers
    )
    feeds = {reserve["price_feed"] for reserve in value["reserves"]}
    for binding in value["protocol_code_bindings"]:
        for feed in feeds:
            binding["code_sha256"][f"price_feed:{feed}"] = "7" * 64
    for reserve in value["reserves"]:
        reserve.update(
            {
                "borrowable_in_isolation": False,
                "siloed_borrowing": False,
                "isolation_mode_debt_ceiling": 0,
                "price_feed_decimals": 8,
                "price_feed_round_id": 10,
                "price_feed_answer": 10**8,
                "price_feed_started_at": 1_699_999_900,
                "price_feed_updated_at": 1_699_999_950,
                "price_feed_answered_in_round": 10,
            }
        )
    value["borrowers"][0]["protocol_account_data"] = {
        "total_collateral_base": 100 * 10**8,
        "total_debt_base": 80 * 10**8,
        "available_borrows_base": 0,
        "current_liquidation_threshold_bps": 8_000,
        "ltv_bps": 7_500,
        "health_factor_wad": WAD,
    }
    return bind_hash(value)


def scenario_costs():
    zero = {
        "dex_fee_base": 0,
        "price_impact_base": 0,
        "gas_base": 0,
        "arbitrum_l1_fee_base": 0,
        "atlas_bid_base": 0,
        "ordering_cost_base": 0,
        "failure_reserve_base": 0,
        "latency_reserve_base": 0,
        "state_drift_reserve_base": 0,
    }
    return {
        "expected": dict(zero),
        "conservative": dict(zero),
        "severe": dict(zero),
    }


def auction(price_before=100 * 10**8, price_after=90 * 10**8):
    value = {
        "schema": "phoenix.atlas.aave-auction-evaluation.v1",
        "affected_asset": ASSET_COLLATERAL,
        "affected_feed": FEED_COLLATERAL,
        "price_before": price_before,
        "price_after": price_after,
        "prices_after": {
            ASSET_COLLATERAL: price_after,
            ASSET_DEBT: 100 * 10**8,
        },
        "block_timestamp": 1_800_000_000,
        "pair_quotes": [],
    }
    return bind_hash(value)


class AtlasBorrowerIndexTests(unittest.TestCase):
    def test_discovery_only_seed_builds_exact_current_state_batch(self):
        market_value = market()
        checkpoint_value = current_state_checkpoint(market_value)
        inventory = build_inventory_from_checkpoint(market_value, checkpoint_value)
        self.assertEqual(
            inventory["bootstrap_mode"],
            "discovery_seed_current_state_exact_batch",
        )
        self.assertFalse(inventory["seed_provenance"]["grants_candidate_authority"])
        self.assertEqual(
            inventory["borrowers"][0]["protocol_account_data"]["health_factor_wad"],
            WAD,
        )
        summary = summarize_current_state(inventory)
        self.assertEqual(summary["bucket_counts"]["hf_1_00_to_1_01"], 1)
        self.assertEqual(summary["liquidatable_pair_count"], 0)
        self.assertFalse(summary["candidate_authority"])

    def test_current_state_protocol_health_factor_disagreement_fails_closed(self):
        market_value = market()
        checkpoint_value = current_state_checkpoint(market_value)
        checkpoint_value["borrowers"][0]["protocol_account_data"][
            "health_factor_wad"
        ] -= 1
        checkpoint_value = bind_hash(checkpoint_value)
        with self.assertRaisesRegex(EvidenceError, "protocol/derived account"):
            build_inventory_from_checkpoint(market_value, checkpoint_value)

    def test_index_is_idempotent_and_hash_bound(self):
        market_value = market()
        transcript_value = transcript(market_value)
        first = build_inventory(market_value, transcript_value)
        second = build_inventory(market_value, transcript_value)
        self.assertEqual(first, second)
        verify_inventory(first)
        tampered = copy.deepcopy(first)
        tampered["unique_borrower_count"] = 99
        with self.assertRaisesRegex(EvidenceError, "snapshot_sha256 mismatch"):
            verify_inventory(tampered)

    def test_reorged_block_chain_fails_closed(self):
        market_value = market()
        value = transcript(market_value)
        second_hash = "0x" + "a2" * 32
        value["blocks"].append(
            {
                "number": 101,
                "hash": second_hash,
                "parent_hash": "0x" + "ff" * 32,
                "logs": [],
            }
        )
        value["end_block"] = 101
        value["end_hash"] = second_hash
        value = bind_hash(value)
        with self.assertRaisesRegex(EvidenceError, "reorged"):
            validate_transcript(value)

    def test_borrower_and_feed_indexes_use_scaled_accounting_once(self):
        market_value = market()
        inventory = build_inventory(market_value, transcript(market_value))
        self.assertEqual(inventory["completeness_status"], "complete")
        self.assertEqual(inventory["unique_borrower_count"], 1)
        self.assertEqual(inventory["feed_index"][FEED_COLLATERAL], [BORROWER])
        self.assertEqual(inventory["feed_index"][FEED_DEBT], [BORROWER])
        positions = {item["asset"]: item for item in inventory["borrowers"][0]["positions"]}
        self.assertEqual(positions[ASSET_COLLATERAL]["supplied"], 100 * 10**18)
        self.assertEqual(positions[ASSET_DEBT]["variable_debt"], 80 * 10**18)
        self.assertEqual(positions[ASSET_DEBT]["debt_type"], "variable")

    def test_health_factor_and_liquidation_pair_are_integer_exact(self):
        market_value = market()
        inventory = build_inventory(market_value, transcript(market_value))
        reserves = {item["asset"]: item for item in inventory["reserves"]}
        borrower = inventory["borrowers"][0]
        before = calculate_account(
            borrower,
            reserves,
            {},
            {ASSET_COLLATERAL: 100 * 10**8, ASSET_DEBT: 100 * 10**8},
        )
        self.assertEqual(before["health_factor_wad"], WAD)
        incomplete_auction = auction()
        incomplete_result = evaluate_auction(inventory, incomplete_auction)
        self.assertEqual(incomplete_result["status"], "INCOMPLETE_PAIR_ECONOMICS")
        required_input = incomplete_result["pairs"][0]["required_unwind_input_collateral"]
        exact_auction = copy.deepcopy(incomplete_auction)
        exact_auction["pair_quotes"] = [
            {
                "debt_asset": ASSET_DEBT,
                "collateral_asset": ASSET_COLLATERAL,
                "block_number": inventory["checkpoint_block"],
                "block_hash": inventory["checkpoint_hash"],
                "flash_provider": address("e"),
                "flash_max_amount": 100 * 10**18,
                "flash_premium_bps": 5,
                "unwind_venue": address("f"),
                "unwind_input_collateral": required_input,
                "unwind_outputs_debt": {
                    "expected": 84 * 10**18,
                    "conservative": 82 * 10**18,
                    "severe": 79 * 10**18,
                },
                "unwind_outputs_are_net_of_dex_fee_and_price_impact": True,
                "scenario_costs_base": scenario_costs(),
                "retained_profit_floor_base": 0,
            }
        ]
        exact_auction = bind_hash(exact_auction)
        result = evaluate_auction(inventory, exact_auction)
        self.assertEqual(result["status"], "COMPLETE")
        self.assertEqual(result["borrowers_evaluated"], 1)
        self.assertEqual(len(result["newly_liquidatable"]), 1)
        self.assertEqual(len(result["pairs"]), 1)
        pair = result["pairs"][0]
        self.assertEqual(pair["repay"], 80 * 10**18)
        self.assertGreater(pair["seized_before_protocol_fee"], 0)
        self.assertGreaterEqual(pair["liquidation_protocol_fee_bps"], 0)
        self.assertEqual(pair["economics_status"], "EXACT_FULL_COST")
        self.assertGreater(pair["pnl_base"]["conservative"], 0)
        costed = copy.deepcopy(exact_auction)
        costed.pop("content_sha256")
        explicit_costs = {
            "dex_fee_base": 11,
            "price_impact_base": 13,
            "gas_base": 17,
            "arbitrum_l1_fee_base": 19,
            "atlas_bid_base": 23,
            "ordering_cost_base": 29,
            "failure_reserve_base": 31,
            "latency_reserve_base": 37,
            "state_drift_reserve_base": 41,
        }
        for scenario in ("expected", "conservative", "severe"):
            costed["pair_quotes"][0]["scenario_costs_base"][scenario] = dict(
                explicit_costs
            )
        costed_result = evaluate_auction(inventory, bind_hash(costed))
        costed_pair = costed_result["pairs"][0]
        self.assertEqual(
            pair["pnl_base"]["conservative"]
            - costed_pair["pnl_base"]["conservative"],
            sum(
                value
                for name, value in explicit_costs.items()
                if name not in {"dex_fee_base", "price_impact_base"}
            ),
        )
        self.assertEqual(costed_pair["atlas_bid_base"], 23)
        self.assertEqual(
            costed_pair["margin_to_gate_base"],
            costed_pair["pnl_base"]["conservative"],
        )

    def test_zero_delta_fast_path_needs_no_borrower_scan(self):
        market_value = market(complete=False)
        inventory = build_inventory(
            market_value, transcript(market_value, complete=False)
        )
        result = evaluate_auction(
            inventory, auction(price_before=100 * 10**8, price_after=100 * 10**8)
        )
        self.assertEqual(result["status"], "ZERO_DELTA_NO_RISK_CHANGE")
        self.assertEqual(result["borrowers_evaluated"], 0)
        self.assertTrue(result["fast_path"])

    def test_nonzero_delta_preserves_incomplete_coverage(self):
        market_value = market(complete=False)
        inventory = build_inventory(
            market_value, transcript(market_value, complete=False)
        )
        self.assertEqual(inventory["completeness_status"], "incomplete")
        result = evaluate_auction(inventory, auction())
        self.assertEqual(result["status"], "INCOMPLETE_INVENTORY")
        self.assertEqual(result["borrowers_evaluated"], 0)
        self.assertEqual(result["pairs"], [])

    def test_missing_scaled_token_amount_is_not_estimated(self):
        market_value = market()
        value = transcript(market_value)
        del value["blocks"][0]["logs"][1]["scaled_amount"]
        value = bind_hash(value)
        with self.assertRaisesRegex(EvidenceError, "scaled_amount"):
            build_inventory(market_value, value)

    def test_stable_debt_mint_burn_and_transfer_are_supported(self):
        market_value = market()
        value = transcript(market_value)
        logs = value["blocks"][0]["logs"]
        stable_amount = 10 * 10**18
        logs.extend(
            [
                event(
                    7,
                    "stable_debt_mint",
                    asset=ASSET_DEBT,
                    on_behalf_of=BORROWER,
                    scaled_amount=stable_amount,
                    balance_increase_adjusted_amount=stable_amount,
                    accounting_role="primary",
                ),
                event(
                    8,
                    "stable_debt_transfer",
                    asset=ASSET_DEBT,
                    **{"from": ZERO, "to": BORROWER},
                    accounting_role="mirror",
                ),
                event(
                    9,
                    "stable_debt_burn",
                    asset=ASSET_DEBT,
                    user=BORROWER,
                    scaled_amount=stable_amount,
                    balance_decrease_adjusted_amount=stable_amount,
                    accounting_role="primary",
                ),
            ]
        )
        value = bind_hash(value)
        inventory = build_inventory(market_value, value)
        position = next(
            item
            for item in inventory["borrowers"][0]["positions"]
            if item["asset"] == ASSET_DEBT
        )
        self.assertEqual(position["stable_debt"], 0)
        self.assertEqual(position["variable_debt"], 80 * 10**18)

    def test_hash_bound_checkpoint_import_is_complete_and_deterministic(self):
        market_value = market()
        checkpoint_value = checkpoint(market_value)
        first = build_inventory_from_checkpoint(market_value, checkpoint_value)
        second = build_inventory_from_checkpoint(market_value, checkpoint_value)
        self.assertEqual(first, second)
        verify_inventory(first)
        self.assertEqual(first["completeness_status"], "complete")
        self.assertEqual(first["bootstrap_mode"], "hash_bound_independently_agreed_checkpoint")
        self.assertEqual(first["unique_borrower_count"], 1)
        self.assertIn(FEED_COLLATERAL, first["feed_index"])
        self.assertIn(FEED_DEBT, first["feed_index"])
        self.assertFalse(any(first["execution_authority"].values()))

    def test_checkpoint_provider_disagreement_fails_closed(self):
        market_value = market()
        checkpoint_value = checkpoint(market_value)
        borrower_state_rows = [
            item
            for item in checkpoint_value["state_bindings"]
            if item["context"] == "borrower_state"
        ]
        borrower_state_rows[-1]["result_sha256"] = "6" * 64
        checkpoint_value = bind_hash(checkpoint_value)
        with self.assertRaisesRegex(EvidenceError, "borrower_state state disagreement"):
            build_inventory_from_checkpoint(market_value, checkpoint_value)


if __name__ == "__main__":
    unittest.main()
