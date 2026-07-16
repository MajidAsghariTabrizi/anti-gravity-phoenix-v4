#!/usr/bin/env python3
"""Verify the rendered production Dashboard isolation contract."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any


MAX_CONFIG_BYTES = 2 * 1024 * 1024
SENSITIVE_ENV_KEYS = {
    "POSTGRES_DSN",
    "POSTGRES_PASSWORD",
    "RPC_PROVIDER_URLS",
    "ARBITRUM_RPC_URL",
    "PARENT_CHAIN_RPC_URL",
    "SIGNER_PRIVATE_KEY",
    "WALLET_ADDRESS",
    "EXECUTOR_ADDRESS",
}
ENDPOINT_RE = re.compile(r"(?i)(?:https?|wss?|postgres(?:ql)?|nats)://")


class ContractError(ValueError):
    pass


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError("compose_duplicate_key")
        result[key] = value
    return result


def validate_compose(config: Any) -> list[str]:
    errors: set[str] = set()
    if not isinstance(config, dict) or not isinstance(config.get("services"), dict):
        return ["compose_shape_invalid"]
    dashboard = config["services"].get("dashboard")
    if not isinstance(dashboard, dict):
        return ["dashboard_service_missing"]

    if "env_file" in dashboard:
        errors.add("dashboard_env_file_forbidden")
    environment = dashboard.get("environment") or {}
    if not isinstance(environment, dict):
        errors.add("dashboard_environment_invalid")
    else:
        if SENSITIVE_ENV_KEYS.intersection(environment):
            errors.add("dashboard_sensitive_environment")
        if any(
            isinstance(value, str) and ENDPOINT_RE.search(value)
            for value in environment.values()
        ):
            errors.add("dashboard_endpoint_environment")

    serialized = json.dumps(dashboard, sort_keys=True).lower()
    if "docker.sock" in serialized:
        errors.add("dashboard_docker_socket_forbidden")

    volumes = dashboard.get("volumes")
    if not isinstance(volumes, list) or len(volumes) != 1:
        errors.add("dashboard_evidence_mount_invalid")
    else:
        volume = volumes[0]
        if not isinstance(volume, dict) or not (
            volume.get("type") == "bind"
            and volume.get("source") == "/opt/phoenix/evidence/dashboard"
            and volume.get("target") == "/evidence"
            and volume.get("read_only") is True
        ):
            errors.add("dashboard_evidence_mount_invalid")

    ports = dashboard.get("ports")
    if not isinstance(ports, list) or len(ports) != 1:
        errors.add("dashboard_loopback_port_invalid")
    else:
        port = ports[0]
        if not isinstance(port, dict) or not (
            port.get("host_ip") == "127.0.0.1"
            and str(port.get("published")) == "8501"
            and str(port.get("target")) == "8501"
        ):
            errors.add("dashboard_loopback_port_invalid")

    if dashboard.get("read_only") is not True:
        errors.add("dashboard_root_filesystem_writable")
    cap_drop = dashboard.get("cap_drop")
    if not isinstance(cap_drop, list) or "ALL" not in cap_drop:
        errors.add("dashboard_capabilities_not_dropped")
    security_opt = dashboard.get("security_opt")
    normalized_security = (
        {str(value).replace("=", ":") for value in security_opt}
        if isinstance(security_opt, list)
        else set()
    )
    if "no-new-privileges:true" not in normalized_security:
        errors.add("dashboard_privilege_escalation_not_blocked")
    tmpfs = dashboard.get("tmpfs")
    if not isinstance(tmpfs, list) or not any(
        isinstance(value, str)
        and value.startswith("/tmp:")
        and all(option in value for option in ("noexec", "nosuid", "nodev", "size=64m"))
        for value in tmpfs
    ):
        errors.add("dashboard_tmpfs_invalid")
    networks = dashboard.get("networks")
    if isinstance(networks, dict):
        network_names = set(networks)
    elif isinstance(networks, list):
        network_names = set(networks)
    else:
        network_names = set()
    if network_names != {"phoenix-internal"}:
        errors.add("dashboard_network_invalid")
    if dashboard.get("depends_on"):
        errors.add("dashboard_data_plane_dependency_forbidden")

    return sorted(errors)


def load_config(path: Path) -> Any:
    try:
        stat = path.stat()
    except OSError as exc:
        raise ContractError("compose_config_missing") from exc
    if stat.st_size <= 0 or stat.st_size > MAX_CONFIG_BYTES:
        raise ContractError("compose_config_size_invalid")
    try:
        return json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ContractError("compose_config_invalid") from exc


def main() -> int:
    if len(sys.argv) != 2:
        print("DASHBOARD_COMPOSE_ERROR: usage", file=sys.stderr)
        return 2
    try:
        config = load_config(Path(sys.argv[1]))
        errors = validate_compose(config)
    except ContractError as exc:
        print(f"DASHBOARD_COMPOSE_ERROR: {exc}", file=sys.stderr)
        return 2
    if errors:
        for error in errors:
            print(f"DASHBOARD_COMPOSE_ERROR: {error}", file=sys.stderr)
        return 2
    print("DASHBOARD_COMPOSE_OK: rendered service is isolated and evidence-only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
