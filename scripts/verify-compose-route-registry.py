#!/usr/bin/env python3
import argparse
import json
from pathlib import Path


def fail(reason: str) -> None:
    raise SystemExit(f"ROUTE_REGISTRY_INVALID: {reason}")


def env_value(paths: list[str]) -> str:
    value = None
    for path in paths:
        for raw_line in Path(path).read_text(encoding="utf-8").splitlines():
            line = raw_line.lstrip("\ufeff")
            if not line or line.lstrip().startswith("#") or "=" not in line:
                continue
            name, candidate = line.split("=", 1)
            if name.strip() != "ENGINE_ROUTE_REGISTRY_JSON":
                continue
            candidate = candidate.strip()
            if len(candidate) >= 2 and candidate[0] == candidate[-1] == "'":
                candidate = candidate[1:-1]
            elif len(candidate) >= 2 and candidate[0] == candidate[-1] == '"':
                try:
                    candidate = json.loads(candidate)
                except json.JSONDecodeError:
                    fail("operator env value has invalid quoting")
            value = candidate
    return "[]" if value is None else value


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--compose-config", required=True)
    expected = parser.add_mutually_exclusive_group(required=True)
    expected.add_argument("--expected-json")
    expected.add_argument("--expected-env-file", action="append")
    parser.add_argument("--allow-empty", action="store_true")
    args = parser.parse_args()

    if args.expected_json:
        expected_raw = Path(args.expected_json).read_text(encoding="utf-8")
    else:
        expected_raw = env_value(args.expected_env_file)
    try:
        expected = json.loads(expected_raw)
    except json.JSONDecodeError:
        fail("operator value is not valid JSON")
    if not isinstance(expected, list):
        fail("operator value is not an array")
    if not args.allow_empty and not expected:
        fail("operator route array is empty")

    try:
        config = json.loads(Path(args.compose_config).read_text(encoding="utf-8"))
        rendered_raw = config["services"]["phoenix-engine"]["environment"][
            "ENGINE_ROUTE_REGISTRY_JSON"
        ]
    except (json.JSONDecodeError, KeyError, TypeError):
        fail("rendered Compose value is unavailable")
    if not isinstance(rendered_raw, str):
        fail("rendered Compose value is not a string")
    if rendered_raw != expected_raw:
        fail("rendered Compose value differs from operator input")

    try:
        rendered = json.loads(rendered_raw)
    except json.JSONDecodeError:
        fail("rendered Compose value is not valid JSON")
    if not isinstance(rendered, list):
        fail("rendered Compose value is not an array")
    if rendered != expected:
        fail("rendered Compose structure differs from operator input")


if __name__ == "__main__":
    main()
