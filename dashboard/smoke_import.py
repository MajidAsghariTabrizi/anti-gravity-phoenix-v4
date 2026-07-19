from __future__ import annotations

import os
import tempfile
from pathlib import Path

from streamlit.testing.v1 import AppTest


ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "dashboard" / "app.py"
FIXTURE = ROOT / "fixtures" / "dashboard" / "latest-dashboard.json"
SNAPSHOT_PATH_VARIABLE = "PHOENIX_DASHBOARD_SNAPSHOT_PATH"


def values(elements) -> list[str]:
    return [str(element.value) for element in elements]


def run_app(snapshot_path: Path):
    os.environ[SNAPSHOT_PATH_VARIABLE] = str(snapshot_path)
    app = AppTest.from_file(str(APP), default_timeout=15).run()
    if app.exception:
        raise RuntimeError(f"dashboard app raised {len(app.exception)} exception(s)")
    return app


def main() -> None:
    previous = os.environ.get(SNAPSHOT_PATH_VARIABLE)
    try:
        app = run_app(FIXTURE)
        if "Phoenix PRE-LIVE SHADOW" not in values(app.title):
            raise RuntimeError("dashboard title missing")
        if len(app.tabs) != 14:
            raise RuntimeError("dashboard section count changed")
        if not any(
            "NOT REALIZED CAPITAL PNL" in value for value in values(app.warning)
        ):
            raise RuntimeError("dashboard SHADOW financial label missing")
        metric_labels = [str(metric.label) for metric in app.metric]
        for required in (
            "Mode",
            "LIVE execution",
            "Execution eligible",
            "Execution request created",
            "Feed inputs",
            "Persistence ratio",
            "Pending rows estimate",
        ):
            if required not in metric_labels:
                raise RuntimeError(f"dashboard safety metric missing: {required}")

        missing = Path(tempfile.gettempdir()) / "phoenix-dashboard-missing-smoke.json"
        missing.unlink(missing_ok=True)
        unavailable = run_app(missing)
        if not any("snapshot_missing" in value for value in values(unavailable.error)):
            raise RuntimeError("dashboard missing-evidence state is not fail closed")
        if "UNAVAILABLE" not in [str(metric.value) for metric in unavailable.metric]:
            raise RuntimeError("dashboard missing-evidence mode is not unavailable")
    finally:
        if previous is None:
            os.environ.pop(SNAPSHOT_PATH_VARIABLE, None)
        else:
            os.environ[SNAPSHOT_PATH_VARIABLE] = previous


if __name__ == "__main__":
    main()
