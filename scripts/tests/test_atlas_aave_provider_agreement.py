import copy
import unittest

from scripts.atlas_aave_provider_agreement import EvidenceError, build_agreement
from scripts.atlas_borrower_index import (
    bind_hash,
    build_inventory_from_checkpoint,
)
from scripts.tests.test_atlas_borrower_index import (
    checkpoint,
    current_state_checkpoint,
    market,
)


def exact_checkpoint_inputs():
    market_value = market()
    checkpoint_value = checkpoint(market_value)
    inventory = build_inventory_from_checkpoint(market_value, checkpoint_value)
    return inventory, checkpoint_value


class AtlasAaveProviderAgreementTests(unittest.TestCase):
    def test_complete_checkpoint_derives_equal_independent_state_hashes(self):
        agreement = build_agreement(*exact_checkpoint_inputs())
        self.assertEqual(agreement["status"], "agreed")
        self.assertEqual(len(agreement["provider_ids"]), 2)
        self.assertEqual(len(set(agreement["state_hashes"])), 1)
        self.assertFalse(any(agreement["execution_authority"].values()))

    def test_inventory_checkpoint_binding_is_required(self):
        inventory, checkpoint_value = exact_checkpoint_inputs()
        changed = copy.deepcopy(checkpoint_value)
        changed["tail_discovery"]["end_block"] += 1
        changed = bind_hash(
            {
                key: value
                for key, value in changed.items()
                if key != "content_sha256"
            }
        )
        with self.assertRaisesRegex(EvidenceError, "inventory checkpoint binding"):
            build_agreement(inventory, changed)

    def test_current_state_checkpoint_binds_distinct_provider_references(self):
        market_value = market()
        checkpoint_value = current_state_checkpoint(market_value)
        inventory = build_inventory_from_checkpoint(market_value, checkpoint_value)
        agreement = build_agreement(inventory, checkpoint_value)
        self.assertIn("oracle_round_state", agreement["agreement_scope"])
        self.assertEqual(len(set(agreement["state_hashes"])), 1)

        changed = copy.deepcopy(checkpoint_value)
        changed["provider_headers"][1]["provider_reference_sha256"] = changed[
            "provider_headers"
        ][0]["provider_reference_sha256"]
        changed = bind_hash(
            {key: value for key, value in changed.items() if key != "content_sha256"}
        )
        inventory = copy.deepcopy(inventory)
        inventory["checkpoint_content_sha256"] = changed["content_sha256"]
        inventory = bind_hash(
            {
                key: value
                for key, value in inventory.items()
                if key != "snapshot_sha256"
            },
            "snapshot_sha256",
        )
        with self.assertRaisesRegex(EvidenceError, "provider references are duplicated"):
            build_agreement(inventory, changed)

    def test_protocol_code_disagreement_fails_closed(self):
        inventory, checkpoint_value = exact_checkpoint_inputs()
        changed = copy.deepcopy(checkpoint_value)
        changed["protocol_code_bindings"][1]["code_sha256"]["pool"] = "8" * 64
        changed = bind_hash(
            {
                key: value
                for key, value in changed.items()
                if key != "content_sha256"
            }
        )
        inventory = copy.deepcopy(inventory)
        inventory["checkpoint_content_sha256"] = changed["content_sha256"]
        inventory = bind_hash(
            {
                key: value
                for key, value in inventory.items()
                if key != "snapshot_sha256"
            },
            "snapshot_sha256",
        )
        with self.assertRaisesRegex(EvidenceError, "state hashes disagree"):
            build_agreement(inventory, changed)


if __name__ == "__main__":
    unittest.main()
