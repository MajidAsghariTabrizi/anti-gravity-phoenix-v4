import json
import re
import tempfile
import unittest
from copy import deepcopy
from pathlib import Path
from unittest import mock

from scripts import prelive_protected_maintenance as maintenance
from scripts import prelive_shadow_contract_transition as transition
from scripts import production_context


REPO_ROOT = Path(__file__).resolve().parents[2]
ROUTE_PATH = REPO_ROOT / "fixtures/routes/weth_usdc_uniswap_v3.json"
PROOF_PATH = REPO_ROOT / transition.ROUTE_PROOF_PATH
SHELL_PATH = REPO_ROOT / "scripts/prelive-shadow-contract-transition.sh"


def digest(value: int) -> str:
    return f"sha256:{value:064x}"


def image_references(names: tuple[str, ...], offset: int) -> dict[str, str]:
    return {
        name: f"ghcr.io/majidasgharitabrizi/{name}@{digest(offset + index)}"
        for index, name in enumerate(names, start=1)
    }


def assets(route_digest: str) -> dict[str, dict[str, object]]:
    values = {
        path: {
            "path": path,
            "mode": "0644",
            "size_bytes": 1,
            "sha256": digest(100 + index),
        }
        for index, path in enumerate(maintenance.CONTRACT_PATHS)
    }
    values[transition.ROUTE_PROOF_PATH]["sha256"] = route_digest
    return values


def release_evidence() -> dict:
    return {
        "sha": transition.RELEASE_SHA,
        "run_id": transition.RELEASE_RUN_ID,
        "images": image_references(transition.CURRENT_IMAGES, 10),
        "assets": assets(transition.CANDIDATE_PROOF_SHA256),
        "proof": PROOF_PATH.read_bytes(),
        "artifact_sha256": {
            "archive": digest(1),
            "assets_manifest": digest(2),
            "checksums": digest(3),
            "release_manifest": digest(4),
            "provenance": digest(5),
            "run_evidence": digest(6),
        },
    }


def rollback_evidence() -> dict:
    return {
        "sha": transition.ROLLBACK_SHA,
        "run_id": transition.ROLLBACK_RUN_ID,
        "images": image_references(transition.LEGACY_IMAGES, 30),
        "assets": assets(transition.ROLLBACK_PROOF_SHA256),
        "proof": b"not used after immutable verification",
        "artifact_sha256": {
            "archive": digest(11),
            "assets_manifest": digest(12),
            "checksums": digest(13),
            "release_manifest": digest(14),
            "provenance": digest(15),
            "run_evidence": digest(16),
        },
    }


def reviewed_plan() -> dict:
    return transition.build_plan_from_evidence(
        release_evidence(), rollback_evidence(), ROUTE_PATH
    )


def rollback_route_registry() -> list[dict]:
    route = deepcopy(json.loads(ROUTE_PATH.read_text(encoding="utf-8"))[0])
    route.pop("settlement_asset")
    route.pop("settlement_asset_decimals")
    for leg in route["legs"]:
        leg.pop("token_in_decimals")
        leg.pop("token_out_decimals")
        leg.pop("tick_spacing")
    route["strategy"] = {
        "min_input_amount": "1000000",
        "max_input_amount": "1000000000000000000",
        "max_evaluations": 32,
        "minimum_net_profit": "1",
        "flash_premium_bps": 5,
        "minimum_slippage_bps": 10,
        "protocol_fees": "0",
        "estimated_execution_gas": 500000,
        "l1_data_fee": "1",
        "contract_overhead": "1",
        "failed_attempt_gas_cost": "1",
        "failure_probability_bps": 500,
        "stale_state_loss": "1",
        "stale_quote_probability_bps": 100,
        "state_drift_reserve": "1",
        "latency_reserve": "1",
        "uncertainty_reserve": "1",
        "replacement_transaction_cost": "1",
        "probability_of_success_bps": 8000,
        "max_gas_price_wei": "1000000000000",
        "max_quote_age_ms": 2000,
        "max_simulation_age_ms": 2000,
        "min_confidence_bps": 9000,
    }
    routes = [route]
    _parsed, route_hash = production_context.validate_route_registry(
        json.dumps(routes)
    )
    if route_hash != transition.ROLLBACK_ROUTE_HASH:
        raise AssertionError("embedded rollback route contract drifted")
    return routes


def normalized_service(name: str, image: str, identity: int) -> dict:
    mounts = []
    if name == "nats":
        mounts = [
            {
                "type": "volume",
                "destination": "/data/jetstream",
                "identity_sha256": digest(900),
            }
        ]
    elif name == "postgres":
        mounts = [
            {
                "type": "bind",
                "destination": "/var/lib/postgresql/data",
                "identity_sha256": digest(901),
            }
        ]
    return {
        "container_id": f"{identity:064x}",
        "configured_image": image,
        "local_image_id": digest(identity + 1000),
        "created_at": "2026-07-20T00:00:00Z",
        "started_at": "2026-07-20T00:01:00Z",
        "restart_count": 0,
        "oom_killed": False,
        "status": "running",
        "health": "healthy",
        "mounts": mounts,
        "networks": [
            {"name": "phoenix-internal", "network_id": f"{950:064x}"}
        ],
    }


def baseline(plan: dict) -> dict:
    counts = {name: 0 for name in maintenance.DATABASE_COUNTS}
    counts["origin_transactions"] = 100
    counts["feed_events"] = 100
    return {
        "schema_version": maintenance.SNAPSHOT_SCHEMA,
        "phase": "pre",
        "release_sha": transition.ROLLBACK_SHA,
        "observed_at": "2026-07-20T00:02:00Z",
        "disk_free_bytes": maintenance.MIN_DISK_FREE_BYTES * 2,
        "services": {
            "nitro-feed-relay": normalized_service(
                "nitro-feed-relay", maintenance.FIXED_IMAGES["nitro-feed-relay"], 1
            ),
            "feed-ingestor": normalized_service(
                "feed-ingestor", plan["images"]["rollback"]["feed-ingestor"], 2
            ),
            "nats": normalized_service("nats", maintenance.FIXED_IMAGES["nats"], 3),
            "postgres": normalized_service(
                "postgres", maintenance.FIXED_IMAGES["postgres"], 4
            ),
            "recorder": normalized_service(
                "recorder", plan["images"]["rollback"]["recorder"], 5
            ),
        },
        "jetstream": {
            "streams": {
                name: {
                    "config_sha256": digest(700 + index),
                    "messages": 0,
                    "first_seq": 1000,
                    "last_seq": 1000,
                }
                for index, name in enumerate(maintenance.STREAM_NAMES)
            },
            "consumers": {
                name: {
                    "config_sha256": digest(710 + index),
                    "pending": 0,
                    "ack_pending": 0,
                    "redelivered": 0,
                    "delivered_stream_seq": 1000,
                    "ack_floor_stream_seq": 1000,
                }
                for index, name in enumerate(maintenance.CONSUMER_NAMES)
            },
        },
        "database": {
            "migrations": transition._expected_migrations(plan),
            "counts": counts,
            "max_feed_sequence": 1000,
        },
        "metrics": {
            "feed": {
                "feed_last_sequence": 1000,
                "feed_jetstream_publish_success_total": 100,
                "feed_sequence_regressions_total": 0,
                "feed_sequence_gaps_total": 0,
                "feed_decode_failures_total": 0,
                "feed_readiness": 1,
            },
            "recorder": {
                "recorder_messages_persisted_total": 100,
                "recorder_last_persisted_feed_sequence": 1000,
                "recorder_database_failures_total": 0,
                "recorder_jetstream_ack_failures_total": 0,
                "recorder_poison_messages_total": 0,
                "recorder_readiness": 1,
            },
        },
        "safety": {
            "mode": "SHADOW",
            "live_execution": False,
            "signer_configured": False,
            "wallet_configured": False,
            "executor_configured": False,
            "public_submission_configured": False,
            "private_submission_configured": False,
            "broadcast_configured": False,
            "execution_eligible": False,
            "execution_request_created": False,
            "optional_services_stopped": True,
        },
        "protected_storage_identity_sha256": digest(902),
    }


def runtime(plan: dict, role: str) -> dict:
    phase = "candidate" if role == "release" else "rollback"
    services: dict[str, dict] = {}
    base = baseline(plan)
    for name in transition.FIXED_SERVICES:
        services[name] = deepcopy(base["services"][name])
    for index, name in enumerate(transition.START_ORDER, start=20):
        if name == "prometheus":
            image = maintenance.PROMETHEUS_IMAGE
        else:
            image = plan["images"][role][transition.OWNED_RUNTIME_IMAGES[name]]
        services[name] = normalized_service(name, image, index)
    return {
        "schema_version": transition.RUNTIME_SCHEMA,
        "phase": phase,
        "release_sha": plan[f"{role}_sha"],
        "services": services,
        "route_registry_sha256": plan["route_contract"][
            f"{role}_registry_sha256"
        ],
        "live_executor_running": False,
        "migration_runner_running": False,
    }


def progress_snapshots(plan: dict, role: str) -> tuple[dict, dict]:
    start = baseline(plan)
    start["phase"] = "post-start" if role == "release" else "rollback-start"
    start["release_sha"] = plan[f"{role}_sha"]
    start["services"]["feed-ingestor"] = normalized_service(
        "feed-ingestor", plan["images"][role]["feed-ingestor"], 22
    )
    start["services"]["recorder"] = normalized_service(
        "recorder", plan["images"][role]["recorder"], 25
    )
    current = deepcopy(start)
    current["phase"] = "final" if role == "release" else "rollback-final"
    current["database"]["counts"]["feed_events"] = 110
    current["database"]["max_feed_sequence"] = 1010
    current["metrics"]["feed"]["feed_last_sequence"] = 1010
    current["metrics"]["feed"]["feed_jetstream_publish_success_total"] = 110
    current["metrics"]["recorder"]["recorder_messages_persisted_total"] = 110
    current["metrics"]["recorder"]["recorder_last_persisted_feed_sequence"] = 1010
    for stream in current["jetstream"]["streams"].values():
        stream["last_seq"] = 1010
    for consumer in current["jetstream"]["consumers"].values():
        consumer["delivered_stream_seq"] = 1010
        consumer["ack_floor_stream_seq"] = 1010
    return start, current


def compose_metadata(plan: dict, role: str) -> dict:
    compatibility = transition._plan_for_compose(plan)
    return {
        "schema": "phoenix.production-render.v1",
        "status": "ok",
        "release_sha": plan[f"{role}_sha"],
        "chain_id": 42161,
        "mode": "SHADOW",
        "live_execution": False,
        "expected_services": list(maintenance.COMPOSE_SERVICES),
        "route_registry_hash": plan["route_contract"][
            f"{role}_registry_sha256"
        ],
        "images": maintenance._expected_compose_images(compatibility, role),
    }


def rendered_compose(plan: dict, role: str) -> dict:
    compatibility = transition._plan_for_compose(plan)
    images = maintenance._expected_compose_images(compatibility, role)
    routes = (
        plan["route_contract"]["release_registry"]
        if role == "release"
        else rollback_route_registry()
    )
    route_json = json.dumps(routes, separators=(",", ":"), sort_keys=True)
    services = {
        name: {
            "image": images[name],
            "environment": {
                "PHOENIX_MODE": "SHADOW",
                "LIVE_EXECUTION": "false",
                "SIGNER_PRIVATE_KEY": "",
                "WALLET_ADDRESS": "",
                "EXECUTOR_ADDRESS": "",
            },
            "networks": {"phoenix-internal": None},
            "restart": "unless-stopped",
        }
        for name in maintenance.COMPOSE_SERVICES
    }
    for name in transition.ROUTE_ENV_SERVICES:
        services[name]["environment"][transition.ROUTE_ENV_NAME] = route_json
    if role == "release":
        services["phoenix-engine"]["environment"][
            transition.ENGINE_CONCURRENCY_ENV_NAME
        ] = transition.REVIEWED_ENGINE_CONCURRENCY_DEFAULT
    services["prometheus"]["user"] = "65534:65534"
    return {
        "name": f"phoenix-{role}",
        "services": services,
        "networks": {"phoenix-internal": {"name": f"phoenix-{role}_phoenix-internal"}},
        "volumes": {"nats-jetstream": {"name": "phoenix-nats-jetstream"}},
        "x-common-env": {
            "env_file": [
                "/var/lib/phoenix/transition/candidate.env"
                if role == "release"
                else "/etc/phoenix/phoenix.env"
            ]
        },
        "x-logging": {"driver": "json-file"},
    }


class ShadowContractTransitionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.plan = reviewed_plan()
        self.shell = SHELL_PATH.read_text(encoding="utf-8")

    def test_exact_reviewed_pair_passes_plan_validation(self) -> None:
        self.assertEqual(self.plan["release_sha"], transition.RELEASE_SHA)
        self.assertEqual(self.plan["rollback_sha"], transition.ROLLBACK_SHA)
        transition.validate_plan(deepcopy(self.plan))

    def test_any_other_release_or_rollback_identity_fails(self) -> None:
        for role in ("release", "rollback"):
            evidence = release_evidence()
            previous = rollback_evidence()
            (evidence if role == "release" else previous)["sha"] = "f" * 40
            with self.subTest(role=role), self.assertRaisesRegex(
                transition.TransitionError, "release_pair_not_reviewed"
            ):
                transition.build_plan_from_evidence(evidence, previous, ROUTE_PATH)

    def test_only_reviewed_route_proof_contract_may_change(self) -> None:
        release = release_evidence()
        changed = "deploy/nats-server.conf"
        release["assets"][changed]["sha256"] = digest(999)
        with self.assertRaisesRegex(
            transition.TransitionError, "protected_contract_changed"
        ):
            transition.build_plan_from_evidence(
                release, rollback_evidence(), ROUTE_PATH
            )
        self.assertEqual(
            self.plan["contracts"]["permitted_transition"]["path"],
            transition.ROUTE_PROOF_PATH,
        )

    def test_partial_or_malformed_route_evidence_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            malformed = Path(directory) / "route.json"
            malformed.write_text('[{"route_id":"partial"}]', encoding="utf-8")
            with self.assertRaisesRegex(
                transition.TransitionError, "candidate_route_mapping_invalid"
            ):
                transition._load_candidate_route_mapping(
                    malformed, PROOF_PATH.read_bytes()
                )

    def test_missing_deterministic_route_mapping_fails_closed(self) -> None:
        with mock.patch.object(
            transition.shadow_route_discovery, "suggested_route", return_value=None
        ):
            with self.assertRaisesRegex(
                transition.TransitionError, "candidate_route_mapping_missing"
            ):
                transition._load_candidate_route_mapping(
                    ROUTE_PATH, PROOF_PATH.read_bytes()
                )

    def test_semantic_compose_allows_only_reviewed_route_hash_delta(self) -> None:
        release_compose = rendered_compose(self.plan, "release")
        rollback_compose = rendered_compose(self.plan, "rollback")
        transition.validate_render_pair(
            self.plan,
            compose_metadata(self.plan, "release"),
            compose_metadata(self.plan, "rollback"),
            release_compose,
            rollback_compose,
        )
        release_compose["services"]["postgres"]["command"] = ["changed"]
        with self.assertRaisesRegex(
            transition.TransitionError,
            "protected_compose_service_changed:postgres",
        ):
            transition.validate_render_pair(
                self.plan,
                compose_metadata(self.plan, "release"),
                compose_metadata(self.plan, "rollback"),
                release_compose,
                rollback_compose,
            )

    def test_semantic_compose_rejects_unreviewed_route_or_engine_delta(self) -> None:
        for mutation, error in (
            (
                lambda value: value["services"]["postgres"]["environment"].__setitem__(
                    transition.ROUTE_ENV_NAME, "[]"
                ),
                "route_render_contract_invalid:release:postgres",
            ),
            (
                lambda value: value["services"]["dashboard"]["environment"].__setitem__(
                    transition.ROUTE_ENV_NAME,
                    json.dumps(self.plan["route_contract"]["release_registry"]),
                ),
                "route_render_contract_invalid:release:service_set",
            ),
            (
                lambda value: value["services"]["phoenix-engine"][
                    "environment"
                ].__setitem__(transition.ENGINE_CONCURRENCY_ENV_NAME, "2"),
                "protected_compose_service_changed:phoenix-engine",
            ),
            (
                lambda value: value["x-common-env"].__setitem__(
                    "env_file", ["relative.env"]
                ),
                "protected_compose_extensions_changed",
            ),
        ):
            with self.subTest(error=error):
                release_compose = rendered_compose(self.plan, "release")
                mutation(release_compose)
                with self.assertRaisesRegex(
                    transition.TransitionError, re.escape(error)
                ):
                    transition.validate_render_pair(
                        self.plan,
                        compose_metadata(self.plan, "release"),
                        compose_metadata(self.plan, "rollback"),
                        release_compose,
                        rendered_compose(self.plan, "rollback"),
                    )

    def test_environment_rewrite_preserves_every_non_route_value(self) -> None:
        rollback_route = json.dumps(
            rollback_route_registry(), separators=(",", ":"), sort_keys=True
        )
        source_text = "\n".join(
            (
                "PHOENIX_MODE=SHADOW",
                "LIVE_EXECUTION=false",
                "CHAIN_ID=42161",
                "POSTGRES_DB=phoenix_v5_654dad17",
                "POSTGRES_DSN='postgres://operator:secret@postgres/phoenix_v5_654dad17'",
                "RECORDER_PERSISTENCE_POLICY=money_path_v1",
                "LIVE_EXECUTOR_ARMED=false",
                "LIVE_EXECUTOR_KILL_SWITCH=true",
                "SIGNER_PRIVATE_KEY=",
                "WALLET_ADDRESS=",
                "EXECUTOR_ADDRESS=",
                "PUBLIC_TRANSACTION_SUBMISSION=",
                "PRIVATE_RELAY_SUBMISSION=",
                "TRANSACTION_BROADCAST_URL=",
                f"ENGINE_ROUTE_REGISTRY_JSON='{rollback_route}'",
                "",
            )
        )
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "rollback.env"
            output = Path(directory) / "candidate.env"
            source.write_text(source_text, encoding="utf-8")
            summary = transition.install_candidate_route_env(
                self.plan, source, output
            )
            _before_lines, before = transition._parse_env(source)
            _after_lines, after = transition._parse_env(output)
        before.pop("ENGINE_ROUTE_REGISTRY_JSON")
        after.pop("ENGINE_ROUTE_REGISTRY_JSON")
        self.assertEqual(before, after)
        self.assertEqual(
            summary["route_registry_sha256"], transition.CANDIDATE_ROUTE_HASH
        )

    def test_live_arming_or_credentials_are_rejected(self) -> None:
        values = {
            "PHOENIX_MODE": "SHADOW",
            "LIVE_EXECUTION": "false",
            "CHAIN_ID": "42161",
            "POSTGRES_DB": transition.DATABASE_NAME,
            "POSTGRES_DSN": "configured",
            "RECORDER_PERSISTENCE_POLICY": "money_path_v1",
            "LIVE_EXECUTOR_ARMED": "true",
            "LIVE_EXECUTOR_KILL_SWITCH": "true",
            "ENGINE_ROUTE_REGISTRY_JSON": json.dumps(rollback_route_registry()),
        }
        with self.assertRaisesRegex(
            transition.TransitionError, "shadow_safety_contract_invalid"
        ):
            transition.validate_environment(self.plan, values, "rollback")
        values["LIVE_EXECUTOR_ARMED"] = "false"
        values["SIGNER_PRIVATE_KEY"] = "secret"
        with self.assertRaisesRegex(
            transition.TransitionError, "live_configuration_present"
        ):
            transition.validate_environment(self.plan, values, "rollback")

    def test_fixed_service_identity_or_storage_change_fails(self) -> None:
        current = runtime(self.plan, "release")
        transition.validate_runtime_transition(
            self.plan, baseline(self.plan), current, "release"
        )
        current["services"]["postgres"]["container_id"] = "f" * 64
        with self.assertRaisesRegex(
            transition.TransitionError, "fixed_service_identity_changed"
        ):
            transition.validate_runtime_transition(
                self.plan, baseline(self.plan), current, "release"
            )

    def test_candidate_and_rollback_progress_fail_closed_on_evidence_drift(self) -> None:
        for role in ("release", "rollback"):
            start, current = progress_snapshots(self.plan, role)
            transition.validate_data_transition(
                self.plan, baseline(self.plan), start, current, role
            )
            no_progress = deepcopy(current)
            no_progress["database"]["max_feed_sequence"] = 1000
            with self.subTest(role=role, failure="progress"), self.assertRaisesRegex(
                transition.TransitionError, "database_feed_sequence_not_progressing"
            ):
                transition.validate_data_transition(
                    self.plan, baseline(self.plan), start, no_progress, role
                )
            storage_drift = deepcopy(current)
            storage_drift["protected_storage_identity_sha256"] = digest(999)
            with self.subTest(role=role, failure="storage"), self.assertRaisesRegex(
                transition.TransitionError, "protected_storage_metadata_changed"
            ):
                transition.validate_data_transition(
                    self.plan, baseline(self.plan), start, storage_drift, role
                )
            unhealthy = deepcopy(current)
            unhealthy["metrics"]["feed"]["feed_readiness"] = 0
            with self.subTest(role=role, failure="health"), self.assertRaisesRegex(
                transition.TransitionError, "feed_readiness_not_ready"
            ):
                transition.validate_data_transition(
                    self.plan, baseline(self.plan), start, unhealthy, role
                )

    def test_fixed_services_are_never_stopped_or_recreated(self) -> None:
        mutation_commands = [
            line.strip()
            for line in self.shell.splitlines()
            if re.search(r"\b(stop|up -d)\b", line)
        ]
        for service in transition.FIXED_SERVICES:
            self.assertFalse(
                any(re.search(rf"\b{re.escape(service)}\b", line) for line in mutation_commands),
                service,
            )

    def test_live_executor_and_migration_runner_are_never_started(self) -> None:
        self.assertNotRegex(self.shell, r"up -d[^\n]*live-executor")
        self.assertNotRegex(self.shell, r"up -d[^\n]*migration-runner")
        self.assertNotIn("LIVE_EXECUTOR_ARMED=true", self.shell)
        self.assertNotIn("--profile live-canary", self.shell)

    def test_live_executor_schemas_are_never_applied(self) -> None:
        for line in self.shell.splitlines():
            if "psql" in line:
                self.assertNotIn("live-executor/schema", line)
        self.assertNotRegex(self.shell, r"psql[^\n]*(001_live_canary|002_approval_evidence)")

    def test_candidate_and_rollback_service_order_is_exact(self) -> None:
        self.assertEqual(
            transition.START_ORDER,
            (
                "recorder",
                "feed-ingestor",
                "rpc-gateway",
                "phoenix-engine",
                "shadow-dispatcher",
                "dashboard",
                "prometheus",
            ),
        )
        apply_flow = self.shell.split("mutation_started=1", 1)[1]
        observed = [
            apply_flow.index('start_service "$env_file" "$release_env" recorder'),
            apply_flow.index('start_service "$env_file" "$release_env" feed-ingestor'),
            apply_flow.index(
                "for candidate_service in rpc-gateway phoenix-engine shadow-dispatcher dashboard prometheus"
            ),
        ]
        self.assertEqual(observed, sorted(observed))
        rollback = self.shell.split("rollback_transition()", 1)[1].split(
            "unexpected_exit()", 1
        )[0]
        self.assertLess(
            rollback.index('start_service "$env_file" "$rollback_env" recorder'),
            rollback.index('start_service "$env_file" "$rollback_env" feed-ingestor'),
        )
        self.assertIn(
            "for rollback_start in rpc-gateway phoenix-engine shadow-dispatcher dashboard prometheus",
            rollback,
        )

    def test_post_mutation_failure_is_wired_to_automatic_rollback(self) -> None:
        self.assertIn("trap unexpected_exit EXIT", self.shell)
        self.assertIn('mutation_started=1', self.shell)
        handler = self.shell.split("unexpected_exit()", 1)[1].split(
            "trap unexpected_exit EXIT", 1
        )[0]
        self.assertIn("rollback_transition", handler)
        self.assertIn('[ "$mutation_started" -eq 1 ]', handler)

    def test_no_destructive_docker_or_postgresql_operation_exists(self) -> None:
        forbidden = (
            r"docker\s+(?:compose\s+)?down\b",
            r"docker\s+(?:system|volume|container|image)\s+prune\b",
            r"docker\s+volume\s+rm\b",
            r"\bDROP\s+(?:DATABASE|TABLE)\b",
            r"\bTRUNCATE\b",
            r"\bDELETE\s+FROM\b",
            r"\bRESET\s+MASTER\b",
        )
        for pattern in forbidden:
            self.assertNotRegex(self.shell, re.compile(pattern, re.IGNORECASE))

    def test_new_operator_files_are_not_immutable_release_assets(self) -> None:
        from scripts import release_assets

        self.assertNotIn(
            "scripts/prelive-shadow-contract-transition.sh", release_assets.STATIC_PATHS
        )
        self.assertNotIn(
            "scripts/prelive_shadow_contract_transition.py",
            release_assets.STATIC_PATHS,
        )


if __name__ == "__main__":
    unittest.main()
