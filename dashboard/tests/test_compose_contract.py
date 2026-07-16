from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HELPER_PATH = ROOT / "scripts" / "verify_dashboard_compose.py"
SPEC = importlib.util.spec_from_file_location("verify_dashboard_compose", HELPER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Dashboard Compose verifier")
HELPER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HELPER)


VALID_CONFIG = {
    "services": {
        "dashboard": {
            "image": "dashboard-image",
            "volumes": [
                {
                    "type": "bind",
                    "source": "/opt/phoenix/evidence/dashboard",
                    "target": "/evidence",
                    "read_only": True,
                }
            ],
            "ports": [
                {
                    "host_ip": "127.0.0.1",
                    "published": "8501",
                    "target": 8501,
                }
            ],
            "networks": {"phoenix-internal": None},
            "read_only": True,
            "tmpfs": ["/tmp:size=64m,mode=1777,noexec,nosuid,nodev"],
            "cap_drop": ["ALL"],
            "security_opt": ["no-new-privileges:true"],
        }
    }
}


class ComposeContractTests(unittest.TestCase):
    def test_dashboard_code_has_no_direct_data_plane_or_container_client(self) -> None:
        source = (ROOT / "dashboard" / "app.py").read_text(encoding="utf-8")
        requirements = (ROOT / "dashboard" / "requirements.txt").read_text(
            encoding="utf-8"
        )
        for forbidden in (
            "POSTGRES_DSN",
            "PROMETHEUS_METRICS_URL",
            "psycopg",
            "requests",
            "urllib",
            "subprocess",
            "docker.sock",
        ):
            with self.subTest(forbidden=forbidden):
                self.assertNotIn(forbidden, source)
                self.assertNotIn(forbidden, requirements)

    def test_valid_isolated_service_passes(self) -> None:
        self.assertEqual(HELPER.validate_compose(copy.deepcopy(VALID_CONFIG)), [])

    def test_sensitive_environment_and_endpoint_are_rejected(self) -> None:
        config = copy.deepcopy(VALID_CONFIG)
        config["services"]["dashboard"]["environment"] = {
            "POSTGRES_DSN": "redacted",
            "STATUS_URL": "http://internal.invalid",
        }
        self.assertEqual(
            HELPER.validate_compose(config),
            ["dashboard_endpoint_environment", "dashboard_sensitive_environment"],
        )

    def test_env_file_socket_and_data_plane_dependency_are_rejected(self) -> None:
        config = copy.deepcopy(VALID_CONFIG)
        dashboard = config["services"]["dashboard"]
        dashboard["env_file"] = ["production.env"]
        dashboard["depends_on"] = {"postgres": {"condition": "service_healthy"}}
        dashboard["volumes"] = [
            {
                "type": "bind",
                "source": "/var/run/docker.sock",
                "target": "/var/run/docker.sock",
                "read_only": True,
            }
        ]
        self.assertEqual(
            HELPER.validate_compose(config),
            [
                "dashboard_data_plane_dependency_forbidden",
                "dashboard_docker_socket_forbidden",
                "dashboard_env_file_forbidden",
                "dashboard_evidence_mount_invalid",
            ],
        )

    def test_public_port_and_writable_or_privileged_runtime_are_rejected(self) -> None:
        config = copy.deepcopy(VALID_CONFIG)
        dashboard = config["services"]["dashboard"]
        dashboard["ports"][0]["host_ip"] = "0.0.0.0"
        dashboard["read_only"] = False
        dashboard["cap_drop"] = []
        dashboard["security_opt"] = []
        dashboard["tmpfs"] = ["/tmp"]
        self.assertEqual(
            HELPER.validate_compose(config),
            [
                "dashboard_capabilities_not_dropped",
                "dashboard_loopback_port_invalid",
                "dashboard_privilege_escalation_not_blocked",
                "dashboard_root_filesystem_writable",
                "dashboard_tmpfs_invalid",
            ],
        )


if __name__ == "__main__":
    unittest.main()
