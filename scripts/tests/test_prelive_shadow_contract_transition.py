import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
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
RECORDER_CONTAINER_ID = "a" * 64
EXPECTED_RELEASE_SHA = "f1bb82681b02c9f6371c0a8de8c1f498fb307034"
EXPECTED_RELEASE_RUN_ID = "29896352400"
EXPECTED_ROLLBACK_SHA = "654dad176fe705d90628b418750a122b8ae30283"
EXPECTED_ROLLBACK_RUN_ID = "29689298132"
RETIRED_RELEASE_SHA = "ac4868ae86227dc61ea003cf0e4e96032be9c96c"
RETIRED_RELEASE_RUN_ID = "29819429567"
EXPECTED_CANDIDATE_PROOF_SHA256 = (
    "sha256:2a1e6ef082c74fecd30673be1261939208f9e0c21a51a76683c2717e55beee8a"
)
EXPECTED_ROLLBACK_PROOF_SHA256 = (
    "sha256:0027d6367df0c00767a351794f48c98861b5c86caa2a3413f0e7eefaddd6afbb"
)
EXPECTED_CANDIDATE_ROUTE_HASH = (
    "sha256:796a9a497990ada50c08d7050ced9e502a236fab769fd0687a2497b1f4e4349c"
)
EXPECTED_ROLLBACK_ROUTE_HASH = (
    "sha256:ad8786f06023a37294a93a697bacfa6287b3a98fbde70ef9bf169e20202dc8ee"
)
EXPECTED_IMAGE_DIGESTS = {
    "release": {
        "dashboard": "sha256:e1d8db8966339a4c30c334382c43b819e9158c75c0ddd952abd1afc4a0d48636",
        "feed-ingestor": "sha256:2b571944435c0aa0c5fe1cd1c999643bc81a22d132c17b6d8cf4f73aaebd064a",
        "fork-sandbox": "sha256:b8f6391441a9626bb0eeffb61a8918d6edd9dc78ff3dc86a28220b66d4a7a2e6",
        "live-executor": "sha256:dcc6cd27e155dca89ad15fd4e4575fe88bc015afdeea991619337419359a1d8a",
        "phoenix-engine": "sha256:d9b765b9d4d77dade941a93c4115caac749926746b482e537b21411314a484f0",
        "recorder": "sha256:87be00afe97786ed3eecebbb022e3c7d41594d80ceb85a491fba49dfb3fa32f7",
        "rpc-gateway": "sha256:016c51bbd5908e31e8b6d8d0b90efc3f5fce583a0512d0aa8d564d3f73da0ac5",
    },
    "rollback": {
        "dashboard": "sha256:1ea82888c883e52bedf2932cd19034cba4a90777a79e76f1b4fae7a9a510d359",
        "feed-ingestor": "sha256:ac14d8f0fb521c03a7c11c8763892bd4ff00974a57e3432ecca86c53ffe75c55",
        "fork-sandbox": "sha256:560fc9aac37342a524b85b634a3b18416ead8679c8bae2cbdfaaa46ce7558e50",
        "phoenix-engine": "sha256:feba279ae3b1040a31c77edf58b57b6410a642e47709b280716e825b0c524268",
        "recorder": "sha256:7d3120a8c0a9f40640f36e803758fc54359d20674ca1d45eeee7c23687467d28",
        "rpc-gateway": "sha256:57e0b8119a967b8ccdfc903ae80bef0b131874b4b1bd4c9bca605fc0dac6c5e8",
    },
}
EXPECTED_IMAGE_REFERENCES = {
    role: {
        name: f"ghcr.io/majidasgharitabrizi/{name}@{digest_value}"
        for name, digest_value in digests.items()
    }
    for role, digests in EXPECTED_IMAGE_DIGESTS.items()
}
EXPECTED_ARTIFACT_SHA256 = {
    "release": {
        "archive": (
            "sha256:26d93556d7a00e66a1da5c3ed5c4f0002b496232f1150629975617db83cb94bb"
        ),
        "assets_manifest": (
            "sha256:8aeddfdcdeb1bb8e17fd2347f3ec33808678fa0129e38eb6c3e3c1f534955b2d"
        ),
        "checksums": (
            "sha256:83c41ca421ce0c2cd3369b8a4bd6a417af8ae513b0f460dc5f92990ad4d53caf"
        ),
        "provenance": (
            "sha256:bd2a695f1fd111719e2754906a8fed6215bda5e657e6bfc927e84f2489bf8868"
        ),
        "release_manifest": (
            "sha256:d154f84df7b88ee204c2d4e982c0f7c9dc65857b8aeb2e7026b57d242018024d"
        ),
        "run_evidence": (
            "sha256:143839da4e3e868d88d00d30cfcbc05936d39ed68ae2572c657c084c0ec4bd42"
        ),
    },
    "rollback": {
        "archive": (
            "sha256:046653f4bfa02fbd61e036fff3281fc0c2630acb06fcc9e0794cf834f64cbd51"
        ),
        "assets_manifest": (
            "sha256:ab042e2a485f8dc1551bd8caa62063275e57636315c160983c63b1248975eb10"
        ),
        "checksums": (
            "sha256:f168d7589732d09801499aecfe1561d247659ff6972b9dfb8147a78c21a02249"
        ),
        "provenance": (
            "sha256:2fd04da00e3bb193f045a5618884dfe31e028b30615e777a5f7654b746faa539"
        ),
        "release_manifest": (
            "sha256:f96334966fe2de08cf1a1fd6a65c9f7c9910b7af737b5ec73d6359cdca39e2a7"
        ),
        "run_evidence": (
            "sha256:872f86a3f2144699d976f014faec67c841f1cf2c133f171e04a96cbbf2341796"
        ),
    },
}


def digest(value: int) -> str:
    return f"sha256:{value:064x}"


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
        "sha": EXPECTED_RELEASE_SHA,
        "run_id": EXPECTED_RELEASE_RUN_ID,
        "images": dict(EXPECTED_IMAGE_REFERENCES["release"]),
        "assets": assets(EXPECTED_CANDIDATE_PROOF_SHA256),
        "proof": PROOF_PATH.read_bytes(),
        "artifact_sha256": dict(EXPECTED_ARTIFACT_SHA256["release"]),
    }


def rollback_evidence() -> dict:
    return {
        "sha": EXPECTED_ROLLBACK_SHA,
        "run_id": EXPECTED_ROLLBACK_RUN_ID,
        "images": dict(EXPECTED_IMAGE_REFERENCES["rollback"]),
        "assets": assets(EXPECTED_ROLLBACK_PROOF_SHA256),
        "proof": b"not used after immutable verification",
        "artifact_sha256": dict(EXPECTED_ARTIFACT_SHA256["rollback"]),
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


def current_from(start: dict, role: str) -> dict:
    current = deepcopy(start)
    current["phase"] = "final" if role == "release" else "rollback-final"
    return current


def raw_jetstream(num_waiting: int = 0, num_ack_pending: int = 0) -> dict:
    return {
        "streams": [
            {
                "name": name,
                "config": {"name": name, "subjects": [f"fixture.{index}"]},
                "state": {"messages": 0, "first_seq": 1000, "last_seq": 1000},
            }
            for index, name in enumerate(maintenance.STREAM_NAMES)
        ],
        "consumers": [
            {
                "name": name,
                "config": {"durable_name": name},
                "num_pending": 0,
                "num_ack_pending": (
                    num_ack_pending if name == "PHOENIX_RECORDER" else 0
                ),
                "num_redelivered": 0,
                "num_waiting": num_waiting if name == "PHOENIX_RECORDER" else 0,
                "delivered": {"stream_seq": 1000},
                "ack_floor": {"stream_seq": 1000},
            }
            for name in maintenance.CONSUMER_NAMES
        ],
    }


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
    services["recorder"]["environment"].update(
        {
            "PHOENIX_ENV": "production",
            "RECORDER_DAEMON": "true",
            "RECORDER_PERSISTENCE_POLICY": "money_path_v1",
            "RECORDER_HEALTH_ADDR": "0.0.0.0:9400",
            "POSTGRES_DSN": "postgresql://fixture.invalid/phoenix",
            "PGSSLMODE": "prefer",
            "NATS_URL": "nats://fixture.invalid:4222",
            "RECORDER_BATCH_MAX_SIZE": "256",
            "RECORDER_BATCH_MAX_WAIT_MS": "100",
            "RECORDER_AGGREGATE_FLUSH_SECONDS": "60",
            "RECORDER_AGGREGATE_FLUSH_EVENTS": "10000",
            "RECORDER_MAX_SAMPLES_PER_DETAIL_PER_DAY": "100",
            "RECORDER_MAX_SAMPLE_JSON_BYTES": "1024",
            "ENGINE_ROUTER_ADDRESSES": (
                "0xe592427a0aece92de3edee1f18e0157c05861564,"
                "0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45,"
                "0xa51afafe0263b40edaef0df8781ea9aa03e381a3"
            ),
        }
    )
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
        self.assertEqual(transition.RELEASE_SHA, EXPECTED_RELEASE_SHA)
        self.assertEqual(transition.RELEASE_RUN_ID, EXPECTED_RELEASE_RUN_ID)
        self.assertEqual(transition.ROLLBACK_SHA, EXPECTED_ROLLBACK_SHA)
        self.assertEqual(transition.ROLLBACK_RUN_ID, EXPECTED_ROLLBACK_RUN_ID)
        self.assertEqual(
            transition.CANDIDATE_PROOF_SHA256, EXPECTED_CANDIDATE_PROOF_SHA256
        )
        self.assertEqual(
            transition.ROLLBACK_PROOF_SHA256, EXPECTED_ROLLBACK_PROOF_SHA256
        )
        self.assertEqual(
            transition.CANDIDATE_ROUTE_HASH, EXPECTED_CANDIDATE_ROUTE_HASH
        )
        self.assertEqual(transition.ROLLBACK_ROUTE_HASH, EXPECTED_ROLLBACK_ROUTE_HASH)
        self.assertEqual(self.plan["release_sha"], EXPECTED_RELEASE_SHA)
        self.assertEqual(self.plan["rollback_sha"], EXPECTED_ROLLBACK_SHA)
        self.assertEqual(self.plan["images"], EXPECTED_IMAGE_REFERENCES)
        self.assertEqual(self.plan["artifacts"], EXPECTED_ARTIFACT_SHA256)
        self.assertIn(f"release_sha={EXPECTED_RELEASE_SHA}", self.shell)
        self.assertNotIn(RETIRED_RELEASE_SHA, self.shell)
        self.assertNotIn(RETIRED_RELEASE_RUN_ID, self.shell)
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

    def test_recorder_handoff_requires_stopped_container_and_no_waiter(self) -> None:
        with self.assertRaisesRegex(
            transition.TransitionError, "recorder_pull_subscription_attached"
        ):
            transition.validate_recorder_handoff(
                raw_jetstream(num_waiting=1), "exited", RECORDER_CONTAINER_ID
            )
        with self.assertRaisesRegex(
            transition.TransitionError, "recorder_container_not_stopped"
        ):
            transition.validate_recorder_handoff(
                raw_jetstream(), "running", RECORDER_CONTAINER_ID
            )

    def test_recorder_handoff_permits_detached_zero_ack_consumer(self) -> None:
        value = transition.validate_recorder_handoff(
            raw_jetstream(), "exited", RECORDER_CONTAINER_ID
        )
        self.assertEqual(value["status"], "detached")
        self.assertEqual(value["container_id"], RECORDER_CONTAINER_ID)
        self.assertEqual(value["num_ack_pending"], 0)
        self.assertEqual(value["num_waiting"], 0)
        with self.assertRaisesRegex(
            transition.TransitionError, "recorder_ack_pending_not_zero"
        ):
            transition.validate_recorder_handoff(
                raw_jetstream(num_ack_pending=1), "exited", RECORDER_CONTAINER_ID
            )

    def test_recorder_config_check_materializes_exact_rendered_environment(self) -> None:
        compose = rendered_compose(self.plan, "release")
        expected = compose["services"]["recorder"]["environment"]
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "recorder.env"
            transition.prepare_recorder_config_check(self.plan, compose, output)
            _lines, actual = transition._parse_env(output)
            self.assertEqual(actual, expected)
            if os.name == "posix":
                self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_recorder_config_evidence_is_sanitized_and_candidate_exact(self) -> None:
        compose = rendered_compose(self.plan, "release")
        evidence = transition.build_recorder_config_evidence(
            self.plan,
            compose,
            {
                "schema": transition.RECORDER_CONFIG_CHECK_SCHEMA,
                "status": "ok",
                "error_code": "ok",
                "environment_name": None,
            },
            digest(990),
            transition.RELEASE_SHA,
            0,
        )
        self.assertEqual(evidence["status"], "ok")
        self.assertEqual(
            evidence["image"]["reference"],
            self.plan["images"]["release"]["recorder"],
        )
        self.assertFalse(
            evidence["environment"]["duplicate_name_detection"]["detected"]
        )
        self.assertEqual(
            evidence["environment"]["structured_configuration"][
                transition.ROUTE_ENV_NAME
            ]["sha256"],
            transition.CANDIDATE_ROUTE_HASH,
        )
        serialized = json.dumps(evidence, sort_keys=True)
        environment = compose["services"]["recorder"]["environment"]
        for value in (
            environment["POSTGRES_DSN"],
            environment["NATS_URL"],
            environment["ENGINE_ROUTER_ADDRESSES"],
            environment[transition.ROUTE_ENV_NAME],
        ):
            self.assertNotIn(value, serialized)

    def test_recorder_config_failures_retain_only_bounded_diagnostics(self) -> None:
        cases = (
            ("required_environment_missing", "POSTGRES_DSN"),
            ("route_registry_invalid", transition.ROUTE_ENV_NAME),
            ("router_addresses_invalid", "ENGINE_ROUTER_ADDRESSES"),
            ("numeric_environment_invalid", "RECORDER_BATCH_MAX_SIZE"),
        )
        compose = rendered_compose(self.plan, "release")
        for error_code, environment_name in cases:
            with self.subTest(error_code=error_code):
                evidence = transition.build_recorder_config_evidence(
                    self.plan,
                    compose,
                    {
                        "schema": transition.RECORDER_CONFIG_CHECK_SCHEMA,
                        "status": "error",
                        "error_code": error_code,
                        "environment_name": environment_name,
                    },
                    digest(991),
                    transition.RELEASE_SHA,
                    1,
                )
                self.assertEqual(evidence["status"], "error")
                self.assertEqual(
                    evidence["config_check"]["environment_name"], environment_name
                )
                self.assertNotIn(
                    compose["services"]["recorder"]["environment"][
                        transition.ROUTE_ENV_NAME
                    ],
                    json.dumps(evidence),
                )

    def test_recorder_config_preflight_is_isolated_and_precedes_plan_success(self) -> None:
        render = self.shell.index('"$release_tree/scripts/render-production-compose.sh"')
        preflight = self.shell.index("prepare-recorder-config-check", render)
        plan_exit = self.shell.index('if [ "$mode" = plan ]; then', preflight)
        mutation = self.shell.index("mutation_started=1", plan_exit)
        self.assertLess(render, preflight)
        self.assertLess(preflight, plan_exit)
        self.assertLess(plan_exit, mutation)

        segment = self.shell[render:plan_exit]
        self.assertIn('--env-file "$candidate_env"', segment)
        self.assertIn('--release-env "$release_env"', segment)
        self.assertIn('--compose-config "$state_dir/release.compose.json"', segment)
        self.assertIn('--env-file "$recorder_config_env"', segment)
        self.assertIn('"$candidate_recorder_image" --config-check', segment)
        self.assertIn("--network none", segment)
        self.assertIn("--read-only", segment)
        self.assertIn("--cap-drop ALL", segment)
        self.assertIn('--cidfile "$recorder_config_cid_file"', segment)
        self.assertIn('docker rm --force "$recorder_config_cid"', segment)
        self.assertLess(
            segment.index('docker rm --force "$recorder_config_cid"'),
            segment.index('[ "$recorder_config_valid" -eq 1 ]'),
        )
        self.assertNotRegex(segment, r"compose_with[^\n]*(?:stop|up -d)")
        self.assertIn('[ "$recorder_config_valid" -eq 1 ]', segment)

    def test_compose_runtime_candidate_image_wins_over_stale_operator_image(
        self,
    ) -> None:
        shell_executable = shutil.which("sh")
        if shell_executable is None and os.name == "nt":
            git_sh = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / (
                "Git/bin/sh.exe"
            )
            if git_sh.is_file():
                shell_executable = str(git_sh)
        if shell_executable is None:
            self.skipTest("POSIX sh is unavailable")

        functions = self.shell[
            self.shell.index("compose_with() (") : self.shell.index(
                "\nread_postgres_identity() ("
            )
        ]
        rollback_image = EXPECTED_IMAGE_REFERENCES["rollback"]["recorder"]
        candidate_image = EXPECTED_IMAGE_REFERENCES["release"]["recorder"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            operator_env = root / "operator.env"
            release_env = root / "release.env"
            up_capture = root / "up-image.txt"
            harness = root / "compose-precedence.sh"
            operator_env.write_text(
                f"RECORDER_IMAGE={rollback_image}\n"
                f"PHOENIX_RELEASE_SHA={EXPECTED_ROLLBACK_SHA}\n",
                encoding="utf-8",
            )
            release_env.write_text(
                f"RECORDER_IMAGE={candidate_image}\n"
                f"PHOENIX_RELEASE_SHA={EXPECTED_RELEASE_SHA}\n",
                encoding="utf-8",
            )
            harness.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env sh
                    set -eu
                    deploy_dir=$1
                    operator_env=$2
                    release_env=$3
                    up_capture=$4
                    rollback_image=$5
                    candidate_image=$6
                    python_bin=$7
                    export up_capture

                    python3() {
                      "$python_bin" "$@"
                    }

                    docker() {
                      [ "$1" = compose ] || return 1
                      [ "$2" = --project-directory ] &&
                        [ "$4" = --env-file ] &&
                        [ "$6" = --env-file ] &&
                        [ "$8" = -f ] || return 1
                      [ -z "${RECORDER_IMAGE+x}" ] &&
                        [ -z "${PHOENIX_RELEASE_SHA+x}" ] || return 1
                      resolved_image=$(awk -F= '
                        $1 == "RECORDER_IMAGE" {
                          value = substr($0, index($0, "=") + 1)
                        }
                        END { if (value == "") exit 1; print value }
                      ' "$5" "$7") || return 1
                      case "${10}" in
                        config)
                          printf '{"services":{"recorder":{"image":"%s"}}}\n' \
                            "$resolved_image"
                          ;;
                        up) printf '%s\n' "$resolved_image" >"$up_capture" ;;
                        *) return 1 ;;
                      esac
                    }
                    """
                )
                + functions
                + textwrap.dedent(
                    """\

                    RECORDER_IMAGE=$rollback_image
                    PHOENIX_RELEASE_SHA=654dad176fe705d90628b418750a122b8ae30283
                    export RECORDER_IMAGE PHOENIX_RELEASE_SHA
                    rendered_image=$(compose_service_image \
                      "$operator_env" "$release_env" recorder)
                    [ "$rendered_image" = "$candidate_image" ]
                    compose_with "$operator_env" "$release_env" \
                      up -d --no-deps recorder
                    [ "$(cat "$up_capture")" = "$candidate_image" ]
                    """
                ),
                encoding="utf-8",
                newline="\n",
            )
            completed = subprocess.run(
                [
                    shell_executable,
                    str(harness),
                    str(root),
                    str(operator_env),
                    str(release_env),
                    str(up_capture),
                    rollback_image,
                    candidate_image,
                    sys.executable,
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=30,
            )
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_compose_runtime_sanitizes_image_and_release_interpolation(self) -> None:
        compose = self.shell.split("compose_with() (", 1)[1].split(
            "compose_service_image() (", 1
        )[0]
        for name in (
            "FEED_INGESTOR_IMAGE",
            "PHOENIX_ENGINE_IMAGE",
            "RPC_GATEWAY_IMAGE",
            "RECORDER_IMAGE",
            "DASHBOARD_IMAGE",
            "PHOENIX_RELEASE_SHA",
        ):
            with self.subTest(name=name):
                self.assertRegex(compose, rf"\b{re.escape(name)}\b")
        self.assertLess(
            compose.index('--env-file "$compose_operator_env"'),
            compose.index('--env-file "$compose_release_env"'),
        )

    def test_postgres_identity_source_is_isolated_from_compose(self) -> None:
        source = self.shell.split("read_postgres_identity() (", 1)[1].split(
            "container_id() (", 1
        )[0]
        self.assertIn('. "$identity_env_file"', source)
        self.assertIn("unset POSTGRES_DB POSTGRES_USER", source)
        self.assertNotIn("set -a", self.shell)
        self.assertNotIn('. "$env_file"', self.shell)

    def test_candidate_runtime_image_gate_precedes_every_service_stop(self) -> None:
        runtime_gate = self.shell.index("candidate_runtime_recorder_image=")
        plan_exit = self.shell.index('if [ "$mode" = plan ]; then', runtime_gate)
        first_top_level_stop = self.shell.index(
            'stop "$optional_service"', plan_exit
        )
        self.assertLess(runtime_gate, plan_exit)
        self.assertLess(runtime_gate, first_top_level_stop)
        segment = self.shell[runtime_gate:plan_exit]
        self.assertIn(
            'compose_service_image \\\n  "$candidate_env" "$release_env" recorder',
            segment,
        )
        self.assertIn(
            '[ "$candidate_runtime_recorder_image" = "$candidate_recorder_image" ]',
            segment,
        )

    def test_candidate_recorder_identity_is_checked_before_health_wait(self) -> None:
        start = self.shell.split("start_candidate_recorder() {", 1)[1].split(
            "restore_initial_optional_services()", 1
        )[0]
        self.assertLess(
            start.index("up -d --no-deps recorder"),
            start.index("assert_candidate_recorder_identity"),
        )
        self.assertLess(
            start.index("assert_candidate_recorder_identity"),
            start.index("wait_service_healthy"),
        )
        identity = self.shell.split("assert_candidate_recorder_identity() (", 1)[
            1
        ].split("service_healthy() (", 1)[0]
        self.assertIn("{{.Config.Image}}|{{.Image}}", identity)
        self.assertIn('"$identity_reference" = "$identity_expected_reference"', identity)
        self.assertIn('"$identity_image_id" = "$identity_expected_image_id"', identity)
        self.assertIn("org.opencontainers.image.revision", identity)
        self.assertIn('"$identity_revision" = "$identity_expected_revision"', identity)
        apply_flow = self.shell.split("mutation_started=1", 1)[1]
        self.assertIn(
            '"$candidate_recorder_image_id" "$release_sha"', apply_flow
        )

    def test_recorder_handoff_poll_is_bounded_and_precedes_candidate_start(self) -> None:
        handoff = self.shell.split("wait_recorder_handoff()", 1)[1].split(
            "capture_recorder_health_failure_diagnostics()", 1
        )[0]
        self.assertIn("handoff_wait_seconds=60", self.shell)
        self.assertIn("handoff_deadline=", handoff)
        self.assertIn('while [ "$(date +%s)" -lt "$handoff_deadline" ]', handoff)
        self.assertIn("validate-recorder-handoff", handoff)
        self.assertIn('--container-id "$handoff_container_id"', handoff)
        self.assertRegex(handoff, r"HANDOFF_TIMEOUT[\s\S]*exit 1")
        apply_flow = self.shell.split("mutation_started=1", 1)[1]
        self.assertLess(
            apply_flow.index("wait_recorder_handoff"),
            apply_flow.index("start_candidate_recorder"),
        )

    def test_candidate_health_failure_publishes_sanitized_diagnostics(self) -> None:
        diagnostics = self.shell.split(
            "capture_recorder_health_failure_diagnostics()", 1
        )[1].split("read_validation_error_code()", 1)[0]
        self.assertIn("capture_service_inspect", diagnostics)
        self.assertIn("logs --no-color --tail 300 recorder", diagnostics)
        self.assertIn("capture_jetstream", diagnostics)
        self.assertIn("redact-diagnostic-log", diagnostics)
        self.assertIn(
            "$evidence_dir/$diagnostic_role-recorder-health-inspect.json",
            diagnostics,
        )
        self.assertIn("$evidence_dir/$diagnostic_role-recorder-health.log", diagnostics)
        self.assertIn(
            "$evidence_dir/$diagnostic_role-recorder-health-jetstream.json",
            diagnostics,
        )
        candidate_failure = self.shell.split(
            "if ! start_candidate_recorder \\", 1
        )[1].split("\nfi", 1)[0]
        self.assertLess(
            candidate_failure.index("capture_recorder_health_failure_diagnostics"),
            candidate_failure.index("fail 'candidate Recorder did not become healthy'"),
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recorder.raw.log"
            environment = root / "phoenix.env"
            output = root / "recorder.log"
            source.write_text(
                "\n".join(
                    (
                        "ordinary recorder diagnostic",
                        "POSTGRES_DSN=postgres://operator:password@postgres/db",
                        "rpc=wss://user:token@example.invalid/feed",
                        "provider rejected top-secret-token",
                        "SIGNER_PRIVATE_KEY=0x" + "a" * 64,
                    )
                ),
                encoding="utf-8",
            )
            environment.write_text(
                "POSTGRES_DSN=postgres://operator:password@postgres/db\n"
                "API_TOKEN=top-secret-token\n",
                encoding="utf-8",
            )
            transition.redact_diagnostic_log(source, environment, output)
            retained = output.read_text(encoding="utf-8")
        self.assertIn("ordinary recorder diagnostic", retained)
        self.assertNotIn("password", retained)
        self.assertNotIn("top-secret-token", retained)
        self.assertNotIn("wss://", retained)
        self.assertNotIn("0x" + "a" * 64, retained)

    def test_recorder_persistence_progress_passes_without_database_growth(self) -> None:
        start, _unused = progress_snapshots(self.plan, "release")
        current = current_from(start, "release")
        current["metrics"]["recorder"]["recorder_messages_persisted_total"] += 1
        transition.validate_data_transition(
            self.plan, baseline(self.plan), start, current, "release"
        )

    def test_irrelevant_consumed_traffic_passes_with_selective_database(self) -> None:
        start, _unused = progress_snapshots(self.plan, "release")
        current = current_from(start, "release")
        current["metrics"]["feed"]["feed_last_sequence"] += 1
        current["metrics"]["feed"]["feed_jetstream_publish_success_total"] += 1
        current["jetstream"]["streams"]["PHOENIX_FEED_TX"]["last_seq"] += 1
        consumer = current["jetstream"]["consumers"]["PHOENIX_RECORDER"]
        consumer["delivered_stream_seq"] += 1
        consumer["ack_floor_stream_seq"] += 1
        consumer["ack_pending"] = maintenance.MAX_ACK_PENDING
        transition.validate_data_transition(
            self.plan, baseline(self.plan), start, current, "release"
        )

    def test_fully_quiet_healthy_interval_requires_bounded_wait(self) -> None:
        start, _unused = progress_snapshots(self.plan, "release")
        current = current_from(start, "release")
        with self.assertRaisesRegex(
            transition.TransitionError, "quiet_interval_not_elapsed"
        ):
            transition.validate_data_transition(
                self.plan, baseline(self.plan), start, current, "release"
            )
        transition.validate_data_transition(
            self.plan,
            baseline(self.plan),
            start,
            current,
            "release",
            allow_quiet=True,
        )
        wait = self.shell.split("wait_for_progress()", 1)[1].split(
            "start_service()", 1
        )[0]
        self.assertGreater(wait.index("--allow-quiet"), wait.index("done"))

    def test_feed_progress_without_consumer_progress_fails(self) -> None:
        start, _unused = progress_snapshots(self.plan, "release")
        current = current_from(start, "release")
        current["metrics"]["feed"]["feed_last_sequence"] += 1
        current["jetstream"]["streams"]["PHOENIX_FEED_TX"]["last_seq"] += 1
        with self.assertRaisesRegex(
            transition.TransitionError, "feed_progress_without_consumer_progress"
        ):
            transition.validate_data_transition(
                self.plan, baseline(self.plan), start, current, "release"
            )

    def test_ack_backlog_or_redelivery_growth_fails(self) -> None:
        start, current = progress_snapshots(self.plan, "release")
        backlog = deepcopy(current)
        backlog["jetstream"]["consumers"]["PHOENIX_RECORDER"][
            "ack_pending"
        ] = maintenance.MAX_ACK_PENDING + 1
        with self.assertRaisesRegex(
            transition.TransitionError, "consumer_backlog_unbounded"
        ):
            transition.validate_data_transition(
                self.plan, baseline(self.plan), start, backlog, "release"
            )
        redelivery = deepcopy(current)
        redelivery["jetstream"]["consumers"]["PHOENIX_RECORDER"][
            "redelivered"
        ] += 1
        with self.assertRaisesRegex(
            transition.TransitionError, "consumer_redelivery_increased"
        ):
            transition.validate_data_transition(
                self.plan, baseline(self.plan), start, redelivery, "release"
            )
        redelivery_start = deepcopy(start)
        redelivery_start["jetstream"]["consumers"]["PHOENIX_RECORDER"][
            "redelivered"
        ] = 1
        redelivery_current = deepcopy(current)
        redelivery_current["jetstream"]["consumers"]["PHOENIX_RECORDER"][
            "redelivered"
        ] = 1
        with self.assertRaisesRegex(
            transition.TransitionError, "consumer_redelivery_increased"
        ):
            transition.validate_data_transition(
                self.plan,
                baseline(self.plan),
                redelivery_start,
                redelivery_current,
                "release",
            )

    def test_duplicate_execution_health_and_storage_fail_closed(self) -> None:
        start, current = progress_snapshots(self.plan, "release")
        mutations = (
            (
                lambda value: value["database"]["counts"].__setitem__(
                    "duplicate_feed_events", 1
                ),
                "database_integrity_failed",
            ),
            (
                lambda value: value["database"]["counts"].__setitem__(
                    "execution_attempts", 1
                ),
                "execution_activity_detected",
            ),
            (
                lambda value: value["metrics"]["feed"].__setitem__(
                    "feed_readiness", 0
                ),
                "feed_readiness_not_ready",
            ),
            (
                lambda value: value["metrics"]["recorder"].__setitem__(
                    "recorder_database_failures_total", 1
                ),
                "runtime_integrity_failed",
            ),
            (
                lambda value: value.__setitem__(
                    "protected_storage_identity_sha256", digest(999)
                ),
                "protected_storage_metadata_changed",
            ),
        )
        for mutate, error in mutations:
            candidate = deepcopy(current)
            mutate(candidate)
            with self.subTest(error=error), self.assertRaisesRegex(
                transition.TransitionError, error
            ):
                transition.validate_data_transition(
                    self.plan, baseline(self.plan), start, candidate, "release"
                )
        regressed_start = deepcopy(start)
        regressed_start["database"]["counts"]["feed_events"] -= 1
        with self.assertRaisesRegex(
            transition.TransitionError, "database_count_regressed:feed_events"
        ):
            transition.validate_data_transition(
                self.plan,
                baseline(self.plan),
                regressed_start,
                current,
                "release",
            )

    def test_sparse_rollback_traffic_no_longer_causes_false_failure(self) -> None:
        start, _unused = progress_snapshots(self.plan, "rollback")
        consumed = current_from(start, "rollback")
        consumed["jetstream"]["streams"]["PHOENIX_FEED_TX"]["last_seq"] += 1
        consumer = consumed["jetstream"]["consumers"]["PHOENIX_RECORDER"]
        consumer["delivered_stream_seq"] += 1
        consumer["ack_floor_stream_seq"] += 1
        transition.validate_data_transition(
            self.plan, baseline(self.plan), start, consumed, "rollback"
        )
        quiet = current_from(start, "rollback")
        transition.validate_data_transition(
            self.plan,
            baseline(self.plan),
            start,
            quiet,
            "rollback",
            allow_quiet=True,
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
            apply_flow.index("start_candidate_recorder"),
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

    def test_no_destructive_docker_postgresql_or_nats_operation_exists(self) -> None:
        forbidden = (
            r"docker\s+(?:compose\s+)?down\b",
            r"docker\s+(?:compose\s+)?pause\b",
            r"docker\s+(?:system|volume|container|image)\s+prune\b",
            r"docker\s+volume\s+rm\b",
            r"\bDROP\s+(?:DATABASE|TABLE)\b",
            r"\bTRUNCATE\b",
            r"\bDELETE\s+FROM\b",
            r"\bUPDATE\s+[A-Za-z_]",
            r"\bRESET\s+MASTER\b",
            r"\bnats\s+(?:stream|consumer)\s+"
            r"(?:add|edit|rm|delete|purge|reset|pause|resume)\b",
            r"\b(?:stream|consumer)\s+"
            r"(?:add|edit|rm|delete|purge|reset|pause|resume)\b",
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
