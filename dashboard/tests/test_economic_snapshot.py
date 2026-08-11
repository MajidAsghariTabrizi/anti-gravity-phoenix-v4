from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path

from streamlit.testing.v1 import AppTest


ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "dashboard" / "app.py"
SNAPSHOT_PATH_VARIABLE = "PHOENIX_DASHBOARD_SNAPSHOT_PATH"


def base_snapshot() -> dict[str, object]:
    return {
        "schema": "phoenix.economic-dashboard.v1",
        "executive": {
            "current_release": "1" * 40,
            "phase": "DISARMED_EVIDENCE",
            "armed": False,
            "kill_switch": True,
            "current_size_level": "MAX_REVIEWED",
            "current_input_wei": "10000000000000000",
            "realized_net_pnl_today_wei": "0",
            "realized_net_pnl_7d_wei": "0",
            "realized_net_pnl_30d_wei": "0",
            "active_route": "route-v1",
        },
        "funnel": {"windows": {}, "semantics": {}},
        "economics": {},
        "safety": {},
        "growth": {},
    }


class EconomicSnapshotRenderTests(unittest.TestCase):
    def run_snapshot(self, snapshot: dict[str, object]):
        previous = os.environ.get(SNAPSHOT_PATH_VARIABLE)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "snapshot.json"
            path.write_text(json.dumps(snapshot), encoding="utf-8")
            os.environ[SNAPSHOT_PATH_VARIABLE] = str(path)
            try:
                app = AppTest.from_file(str(APP), default_timeout=15).run()
            finally:
                if previous is None:
                    os.environ.pop(SNAPSHOT_PATH_VARIABLE, None)
                else:
                    os.environ[SNAPSHOT_PATH_VARIABLE] = previous
        self.assertFalse(app.exception)
        return app

    def test_older_additive_v1_snapshot_still_renders(self) -> None:
        app = self.run_snapshot(base_snapshot())
        self.assertIn("Phoenix LIVE Economic Control", [str(item.value) for item in app.title])

    def test_lane_specific_sections_render_without_generic_mislabeling(self) -> None:
        snapshot = base_snapshot()
        snapshot["executive"]["armed"] = True  # type: ignore[index]
        snapshot["executive"]["lane_authority"] = {  # type: ignore[index]
            "generic_dex": {"effective_armed": False, "effective_kill_switch": True},
            "aave_liquidation": {"armed": True, "kill_switch": False},
            "atlas_solver": {"armed": True, "kill_switch": False},
        }
        empty_window = {
            "exact_completed": "0",
            "fork_passed": "0",
            "rejection_reason_counts": {},
            "exact_deferred_by_reason": {},
        }
        snapshot["funnel"] = {
            "windows": {},
            "semantics": {
                "scope": "Generic Phoenix DEX Engine only",
                "simulation_evidence_insufficient": (
                    "Generic Engine only; never an Aave fork or Atlas callback classification"
                ),
            },
            "revenue_lane_windows": {
                "aave_liquidation": {"1h": empty_window, "24h": empty_window, "7d": empty_window},
                "atlas_solver": {
                    "1h": {"ingress": "0", "current_status_counts": {}},
                    "24h": {"ingress": "0", "current_status_counts": {}},
                    "7d": {"ingress": "1", "current_status_counts": {}},
                },
            },
        }
        snapshot["economics"] = {  # type: ignore[assignment]
            "aave_exact_7d": {"exact_evaluated_signals": "0"},
            "atlas_solver_7d": {"request_materialized": "0"},
        }

        app = self.run_snapshot(snapshot)
        subheaders = [str(item.value) for item in app.subheader]
        for required in (
            "Execution authority by lane",
            "Generic DEX funnel",
            "Aave liquidation funnel",
            "Atlas solver funnel",
            "Aave Exact — 7 days",
            "Atlas Solver — 7 days",
        ):
            self.assertIn(required, subheaders)
        captions = [str(item.value) for item in app.caption]
        self.assertTrue(
            any("simulation_evidence_insufficient is not an Aave" in value for value in captions)
        )
        warnings = [str(item.value) for item in app.warning]
        self.assertTrue(any("Generic DEX is closed" in value for value in warnings))
        self.assertFalse(any("Generic DEX execution authority is open" in value for value in warnings))


if __name__ == "__main__":
    unittest.main()
