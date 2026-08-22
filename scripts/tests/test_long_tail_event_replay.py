import copy
import importlib.util
import json
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "long_tail_event_replay.py"
SPEC = importlib.util.spec_from_file_location("long_tail_event_replay", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)

ALLOWLIST_PATH = ROOT / "fixtures" / "long-tail" / "immutable_events_20260801.json"
LINK_PATH = ROOT / "fixtures" / "long-tail" / "link_backfill_20260801.json"
ATLAS_LINK_PATH = ROOT / "fixtures" / "long-tail" / "atlas_link_zero_delta_20260801.json"


class LongTailReplayTests(unittest.TestCase):
    def setUp(self):
        self.allowlist = MODULE.load_allowlist(ALLOWLIST_PATH)
        self.event = copy.deepcopy(self.allowlist["events"][1])

    def proof(self):
        transaction_index = int(self.event["transaction_index"])
        transactions = []
        for index in range(transaction_index + 1):
            tx_hash = f"0x{index + 1:064x}"
            if index == transaction_index:
                tx_hash = self.event["transaction_hash"]
            transactions.append(
                {
                    "transaction_index": str(index),
                    "transaction_hash": tx_hash,
                    "receipt_status": "success",
                    "receipt_hash": f"{index + 1:064x}",
                }
            )
        return {
            "schema_version": MODULE.BOUNDARY_SCHEMA,
            "source_event_identity": MODULE.event_identity(self.event),
            "transaction_hash": self.event["transaction_hash"],
            "block_number": self.event["block_number"],
            "block_hash": self.event["block_hash"],
            "transaction_index": self.event["transaction_index"],
            "parent_block_number": self.event["parent_block_number"],
            "parent_block_hash": self.event["parent_block_hash"],
            "boundary_scope": "post_initiating_transaction_pre_next_canonical_transaction",
            "state_block_tag": self.event["block_hash"],
            "end_of_block_state": False,
            "method": "bounded_parent_block_replay",
            "canonical_transactions": transactions,
            "last_replayed_transaction_index": self.event["transaction_index"],
            "last_replayed_transaction_hash": self.event["transaction_hash"],
            "post_state_hash": "a" * 64,
            "provider_agreement_hash": "b" * 64,
            "provider_bindings": [
                {
                    "provider_id": "reviewed_primary",
                    "chain_id": MODULE.CHAIN_ID,
                    "block_number": self.event["block_number"],
                    "block_hash": self.event["block_hash"],
                    "transaction_hash": self.event["transaction_hash"],
                    "transaction_index": self.event["transaction_index"],
                },
                {
                    "provider_id": "reviewed_secondary",
                    "chain_id": MODULE.CHAIN_ID,
                    "block_number": self.event["block_number"],
                    "block_hash": self.event["block_hash"],
                    "transaction_hash": self.event["transaction_hash"],
                    "transaction_index": self.event["transaction_index"],
                },
            ],
            "state_reads": [
                {
                    "role": "initiating",
                    "pool": self.event["pool_addresses"][0],
                    "block_number": self.event["block_number"],
                    "block_hash": self.event["block_hash"],
                    "transaction_index": self.event["transaction_index"],
                    "boundary_scope": "post_initiating_transaction_pre_next_canonical_transaction",
                    "state_hash": "c" * 64,
                },
                {
                    "role": "alternative",
                    "pool": "0x1111111111111111111111111111111111111111",
                    "block_number": self.event["block_number"],
                    "block_hash": self.event["block_hash"],
                    "transaction_index": self.event["transaction_index"],
                    "boundary_scope": "post_initiating_transaction_pre_next_canonical_transaction",
                    "state_hash": "d" * 64,
                },
            ],
        }

    def test_immutable_allowlist_is_exactly_nine_aave_and_one_uni(self):
        self.assertEqual(len(self.allowlist["events"]), 10)
        self.assertEqual(
            {MODULE.event_identity(event) for event in self.allowlist["events"]},
            MODULE.EXPECTED_EVENT_IDENTITIES,
        )
        self.assertEqual(
            sum(event["token_path"] == [MODULE.AAVE, MODULE.WETH, MODULE.USDC] for event in self.allowlist["events"]),
            9,
        )

    def test_allowlist_addition_and_identity_mutation_fail_closed(self):
        payload = copy.deepcopy(self.allowlist)
        payload["events"].append(copy.deepcopy(payload["events"][0]))
        with self.assertRaisesRegex(MODULE.EvidenceError, "immutable_event_count_changed"):
            self._load_payload(payload)
        payload = copy.deepcopy(self.allowlist)
        payload["events"][0]["source_event_identity"]["source_feed_sequence"] = "467484827"
        payload["events"][0]["source_feed_sequence"] = "467484827"
        with self.assertRaisesRegex(MODULE.EvidenceError, "immutable_event_set_changed"):
            self._load_payload(payload)

    def _load_payload(self, payload):
        path = ROOT / "tmp-long-tail-test.json"
        try:
            path.write_text(json.dumps(payload), encoding="utf-8")
            return MODULE.load_allowlist(path)
        finally:
            path.unlink(missing_ok=True)

    def test_parent_block_replay_accepts_canonical_prefix_through_target(self):
        MODULE.validate_boundary_proof(self.event, self.proof())

    def test_parent_block_replay_rejects_order_gap(self):
        proof = self.proof()
        proof["canonical_transactions"][1]["transaction_index"] = "2"
        with self.assertRaisesRegex(MODULE.EvidenceError, "canonical_transaction_order_gap"):
            MODULE.validate_boundary_proof(self.event, proof)

    def test_parent_block_replay_rejects_next_or_end_of_block_transaction(self):
        proof = self.proof()
        proof["canonical_transactions"].append(
            {
                "transaction_index": str(int(self.event["transaction_index"]) + 1),
                "transaction_hash": "0x" + "9" * 64,
                "receipt_status": "success",
                "receipt_hash": "9" * 64,
            }
        )
        with self.assertRaisesRegex(MODULE.EvidenceError, "parent_replay_did_not_stop_at_boundary"):
            MODULE.validate_boundary_proof(self.event, proof)

    def test_current_and_end_of_block_state_substitution_are_forbidden(self):
        proof = self.proof()
        proof["state_block_tag"] = "latest"
        with self.assertRaisesRegex(MODULE.EvidenceError, "dynamic_state_tag_forbidden"):
            MODULE.validate_boundary_proof(self.event, proof)
        proof = self.proof()
        proof["end_of_block_state"] = True
        with self.assertRaisesRegex(MODULE.EvidenceError, "end_of_block_state_forbidden"):
            MODULE.validate_boundary_proof(self.event, proof)

    def test_missing_trace_and_missing_alternative_pool_fail_closed(self):
        proof = self.proof()
        proof["method"] = "debug_trace_transaction_prestate_diff"
        proof.pop("canonical_transactions")
        with self.assertRaisesRegex(MODULE.EvidenceError, "prestate_hash_missing"):
            MODULE.validate_boundary_proof(self.event, proof)
        proof = self.proof()
        proof["state_reads"] = proof["state_reads"][:1]
        with self.assertRaisesRegex(MODULE.EvidenceError, "initiating_or_alternative_state_missing"):
            MODULE.validate_boundary_proof(self.event, proof)

    def test_provider_disagreement_fails_closed(self):
        proof = self.proof()
        proof["provider_bindings"][1]["block_hash"] = "0x" + "f" * 64
        with self.assertRaisesRegex(MODULE.EvidenceError, "provider_block_hash_disagreement"):
            MODULE.validate_boundary_proof(self.event, proof)

    def test_candidate_requires_complete_integer_costs_and_passing_fork(self):
        proof = self.proof()
        proof["economics"] = {
            "gross_profit_wei": "20000",
            "dex_fee_wei": "100",
            "price_impact_wei": "100",
            "flash_premium_wei": "100",
            "execution_gas_wei": "100",
            "l1_data_fee_wei": "100",
            "ordering_cost_wei": "100",
            "failure_reserve_wei": "100",
            "retained_profit_floor_wei": "1000",
            "expected_pnl_wei": "18000",
            "conservative_pnl_wei": "12000",
            "severe_pnl_wei": "5000",
            "opportunity_lifetime_ms": "5000",
            "end_to_end_latency_ms": "500",
            "prediction_error_bps": "25",
            "prediction_error_limit_bps": "100",
            "severe_loss_within_reviewed_limit": True,
            "security_accounting_defects": [],
            "fork": {
                "passed": True,
                "public_broadcast": False,
                "signer_used": False,
                "result_hash": "e" * 64,
            },
        }
        report = MODULE.evaluate(
            self.allowlist,
            {"proofs": {MODULE.event_identity(self.event): proof}},
        )
        row = next(
            row for row in report["events"]
            if row["source_event_identity"] == MODULE.event_identity(self.event)
        )
        self.assertEqual(row["recommendation"], "CANDIDATE")
        self.assertEqual(report["complete_boundaries"], 1)
        proof["economics"]["opportunity_lifetime_ms"] = "100"
        with self.assertRaisesRegex(MODULE.EvidenceError, "opportunity_lifetime_not_above_latency"):
            MODULE.evaluate(
                self.allowlist,
                {"proofs": {MODULE.event_identity(self.event): proof}},
            )

    def test_link_backfill_matches_only_exact_adjacent_hop_and_pool_identity(self):
        exact = {
            "token_path": [MODULE.USDC, MODULE.WETH, MODULE.LINK],
            "fee_path": [500, 3000],
            "pool_path": [
                f"{MODULE.WETH}:{MODULE.USDC}:500",
                f"{MODULE.LINK}:{MODULE.WETH}:3000",
            ],
            "command_index": "1",
            "source_factory": MODULE.UNISWAP_V3_FACTORY,
            "pool_addresses": [
                "0x2222222222222222222222222222222222222222",
                "0x3333333333333333333333333333333333333333",
            ],
            "recipient": MODULE.LINK,
            "calldata": MODULE.LINK,
        }
        final_token_only = {
            "token_path": [MODULE.USDC, MODULE.AAVE, MODULE.LINK],
            "fee_path": [500, 3000],
            "pool_path": [
                f"{MODULE.AAVE}:{MODULE.USDC}:500",
                f"{MODULE.AAVE}:{MODULE.LINK}:3000",
            ],
            "command_index": "0",
            "source_factory": MODULE.UNISWAP_V3_FACTORY,
            "pool_addresses": [
                "0x4444444444444444444444444444444444444444",
                "0x5555555555555555555555555555555555555555",
            ],
            "recipient": MODULE.WETH,
        }
        wrong_pool_fee = copy.deepcopy(exact)
        wrong_pool_fee["pool_path"][1] = f"{MODULE.LINK}:{MODULE.WETH}:500"
        self.assertEqual(MODULE.exact_link_adjacent_hops([exact, final_token_only, wrong_pool_fee]), [exact])

    def test_reviewed_link_backfill_is_link_d(self):
        self.assertEqual(
            MODULE.validate_link_backfill(LINK_PATH),
            {"records_scanned": 288, "exact_match_count": 0, "link_status": "LINK-D"},
        )

    def test_atlas_link_zero_delta_is_not_liquidation_opportunity(self):
        self.assertEqual(
            MODULE.validate_atlas_link(ATLAS_LINK_PATH),
            {
                "auction_id": "94b5e867-122c-4dca-84b8-f106feaaef67",
                "price_delta": "0",
                "newly_induced_hf_crossings": 0,
                "result": "ZERO_DELTA_NOT_A_LIQUIDATION_OPPORTUNITY",
            },
        )


if __name__ == "__main__":
    unittest.main()
