import copy
import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts" / "camelot_curve_economic_gate.py"
PROOF_PATH = ROOT / "fixtures" / "revenue-proof" / "camelot_curve_b_b_489927908.json"
SPEC = importlib.util.spec_from_file_location("camelot_curve_economic_gate", MODULE_PATH)
assert SPEC and SPEC.loader
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)


class EconomicGateTest(unittest.TestCase):
    def setUp(self):
        self.document = json.loads(PROOF_PATH.read_text(encoding="utf-8"))

    def test_authoritative_fixture_is_definitive_b_b(self):
        result = gate.validate(self.document)
        self.assertEqual(result["classification"], "B-B")
        self.assertEqual(result["best_expected_net_upper_bound_wei"], "-670705341118")
        self.assertEqual(result["reviewed_provider_count"], 2)

    def test_rejects_quote_or_cost_arithmetic_tampering(self):
        tampered = copy.deepcopy(self.document)
        tampered["seven_size_ladder"][0]["flash_premium_wei"] = "0"
        with self.assertRaisesRegex(gate.GateError, "flash-premium"):
            gate.validate(tampered)

    def test_rejects_provider_disagreement(self):
        tampered = copy.deepcopy(self.document)
        tampered["provider_agreement"]["same_quotes"] = False
        with self.assertRaisesRegex(gate.GateError, "provider agreement"):
            gate.validate(tampered)

    def test_rejects_non_negative_gross_row(self):
        tampered = copy.deepcopy(self.document)
        row = tampered["seven_size_ladder"][0]
        row["curve_weth_out_wei"] = row["amount_in_wei"]
        row["gross_profit_wei"] = "0"
        row["expected_net_upper_bound_wei"] = "-50000000000"
        row["conservative_net_upper_bound_wei"] = "-62500000000"
        row["severe_net_upper_bound_wei"] = "-100000000000"
        with self.assertRaisesRegex(gate.GateError, "non-negative"):
            gate.validate(tampered)


if __name__ == "__main__":
    unittest.main()
