import importlib.util
import os
from pathlib import Path


def main() -> None:
    os.environ["POSTGRES_DSN"] = "not=valid=conninfo"
    os.environ["PROMETHEUS_METRICS_URL"] = "invalid-metrics-url"
    path = Path(__file__).with_name("app.py")
    spec = importlib.util.spec_from_file_location("phoenix_dashboard_app", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load dashboard app spec")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    for name in ("query", "metric_value", "kpi"):
        if not hasattr(module, name):
            raise RuntimeError(f"dashboard app missing {name}")
    expected_label = "SHADOW / SIMULATED — NOT REALIZED CAPITAL PNL"
    if module.SHADOW_FINANCIAL_LABEL != expected_label:
        raise RuntimeError("dashboard SHADOW financial label changed")


if __name__ == "__main__":
    main()
