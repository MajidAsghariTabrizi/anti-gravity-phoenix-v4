import importlib.util
import os
from pathlib import Path


def main() -> None:
    os.environ.setdefault("POSTGRES_DSN", "postgres://phoenix:placeholder@localhost:5432/phoenix")
    os.environ.setdefault("PROMETHEUS_METRICS_URL", "http://127.0.0.1:9090")
    path = Path(__file__).with_name("app.py")
    spec = importlib.util.spec_from_file_location("phoenix_dashboard_app", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load dashboard app spec")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    for name in ("query", "metric_value", "kpi"):
        if not hasattr(module, name):
            raise RuntimeError(f"dashboard app missing {name}")


if __name__ == "__main__":
    main()
