import copy
import hashlib
import unittest

from scripts.atlas_aave_fork_package import (
    PLAN_SCHEMA,
    EvidenceError,
    build_package,
)
from scripts.atlas_aave_provider_agreement import build_agreement
from scripts.atlas_borrower_index import (
    bind_hash,
    build_inventory_from_checkpoint,
    evaluate_auction,
)
from scripts.tests.test_atlas_borrower_index import (
    ASSET_COLLATERAL,
    ASSET_DEBT,
    BORROWER,
    address,
    auction,
    checkpoint,
    market,
    scenario_costs,
)


def exact_inputs():
    market_value = market()
    checkpoint_value = checkpoint(market_value)
    inventory = build_inventory_from_checkpoint(market_value, checkpoint_value)
    auction_value = auction()
    incomplete = evaluate_auction(inventory, auction_value)
    required = incomplete["pairs"][0]["required_unwind_input_collateral"]
    auction_value = copy.deepcopy(auction_value)
    auction_value["pair_quotes"] = [
        {
            "debt_asset": ASSET_DEBT,
            "collateral_asset": ASSET_COLLATERAL,
            "block_number": inventory["checkpoint_block"],
            "block_hash": inventory["checkpoint_hash"],
            "flash_provider": address("e"),
            "flash_max_amount": 100 * 10**18,
            "flash_premium_bps": 5,
            "unwind_venue": address("f"),
            "unwind_input_collateral": required,
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
    auction_value = bind_hash(auction_value)
    result = evaluate_auction(inventory, auction_value)
    pair = result["pairs"][0]
    agreement = build_agreement(inventory, checkpoint_value)
    calldata = "0x1234"
    plan = bind_hash(
        {
            "schema": PLAN_SCHEMA,
            "chain_id": 42161,
            "block_number": inventory["checkpoint_block"],
            "block_hash": inventory["checkpoint_hash"],
            "borrower": BORROWER,
            "debt_asset": ASSET_DEBT,
            "collateral_asset": ASSET_COLLATERAL,
            "repay": pair["repay"],
            "max_repay": pair["repay"],
            "seized_collateral": pair["liquidator_collateral"],
            "atlas_bid_base": pair["atlas_bid_base"],
            "max_atlas_bid_base": pair["max_rational_atlas_bid_base"],
            "minimum_final_realized_profit": 1,
            "minimum_final_realized_profit_base": 1,
            "atomic_bounds_enforced": True,
            "calldata": calldata,
            "calldata_sha256": hashlib.sha256(calldata.encode()).hexdigest(),
            "flash_amount": pair["repay"],
            "flash_premium": pair["flash_premium_amount"],
            "unwind_min_out": 82 * 10**18,
            "gas_limit": 1,
            "max_fee_per_gas_wei": 1,
            "l1_data_fee_wei": 1,
            "deadline": 1_800_000_100,
            "nonce_assumption": "read-only fork fixture",
            "balance_reconciliation": {"all_costs_included": True},
            "prediction_error_bps": 10,
            "maximum_prediction_error_bps": 25,
            "detected_at_ms": 1_800_000_000_000,
            "expires_at_ms": 1_800_000_010_000,
            "end_to_end_latency_p95_ms": 1_000,
            "execution_authority": {
                "signer": False,
                "bond": False,
                "bid": False,
                "submission": False,
                "capital": False,
            },
        }
    )
    return inventory, auction_value, result, agreement, plan


class AtlasAaveForkPackageTests(unittest.TestCase):
    def test_exact_complete_positive_builds_non_authoritative_package(self):
        package = build_package(*exact_inputs())
        self.assertEqual(package["status"], "READY_FOR_EXTERNAL_FORK")
        self.assertEqual(package["fork_status"], "not_run")
        self.assertFalse(package["fork_request_created"])
        self.assertFalse(any(package["execution_authority"].values()))

    def test_provider_disagreement_fails_closed(self):
        inventory, auction_value, result, agreement, plan = exact_inputs()
        agreement["state_hashes"][1] = "8" * 64
        agreement = bind_hash(
            {key: value for key, value in agreement.items() if key != "content_sha256"}
        )
        with self.assertRaisesRegex(EvidenceError, "state hashes disagree"):
            build_package(inventory, auction_value, result, agreement, plan)

    def test_lifetime_must_exceed_observed_latency(self):
        inventory, auction_value, result, agreement, plan = exact_inputs()
        plan["expires_at_ms"] = plan["detected_at_ms"] + plan["end_to_end_latency_p95_ms"]
        plan = bind_hash(
            {key: value for key, value in plan.items() if key != "content_sha256"}
        )
        with self.assertRaisesRegex(EvidenceError, "lifetime"):
            build_package(inventory, auction_value, result, agreement, plan)

    def test_execution_authority_is_rejected(self):
        inventory, auction_value, result, agreement, plan = exact_inputs()
        plan["execution_authority"]["submission"] = True
        plan = bind_hash(
            {key: value for key, value in plan.items() if key != "content_sha256"}
        )
        with self.assertRaisesRegex(EvidenceError, "must not grant"):
            build_package(inventory, auction_value, result, agreement, plan)

    def test_atomic_candidate_bounds_are_required(self):
        inventory, auction_value, result, agreement, plan = exact_inputs()
        plan["atomic_bounds_enforced"] = False
        plan = bind_hash(
            {key: value for key, value in plan.items() if key != "content_sha256"}
        )
        with self.assertRaisesRegex(EvidenceError, "atomic bounds"):
            build_package(inventory, auction_value, result, agreement, plan)


if __name__ == "__main__":
    unittest.main()
