import io
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from copy import deepcopy
from pathlib import Path
from types import SimpleNamespace

from scripts import prelive_protected_maintenance as maintenance


RELEASE_SHA = "a7f19ab165d93dafb4bcc20463f9d010f587281a"
ROLLBACK_SHA = "ddbc3e6820f565b41d0d0a2323f67a4187b3dd45"
RELEASE_COMPOSE_DIGEST = (
    "sha256:0d2ca16746393814bb62f6555ee0aad08635cd3d3e8ec33c0bbff9103338b930"
)
ROLLBACK_COMPOSE_DIGEST = (
    "sha256:e06e8644ed6eb1432630b996ec47156605f191b5dca4c80b40357cb69e87cd95"
)


def digest(value: int) -> str:
    return f"sha256:{value:064x}"


def image_reference(name: str, value: int) -> str:
    return f"ghcr.io/majidasgharitabrizi/{name}@{digest(value)}"


def manifest(
    release_sha: str,
    offset: int,
    image_names: tuple[str, ...] = maintenance.LEGACY_RELEASE_IMAGES,
) -> dict:
    return {
        "schema": maintenance.RELEASE_SCHEMA,
        "release_sha": release_sha,
        "created_at": "2026-07-16T00:00:00Z",
        "images": {
            name: {
                "repository": f"ghcr.io/majidasgharitabrizi/{name}",
                "tag": f"sha-{release_sha}",
                "digest": digest(offset + index),
            }
            for index, name in enumerate(image_names, start=1)
        },
    }


def asset_manifest(release_sha: str) -> dict:
    compose_digest = {
        RELEASE_SHA: RELEASE_COMPOSE_DIGEST,
        ROLLBACK_SHA: ROLLBACK_COMPOSE_DIGEST,
    }[release_sha]
    return {
        "schema": maintenance.ASSET_SCHEMA,
        "release_sha": release_sha,
        "files": [
            {
                "path": path,
                "mode": "0644",
                "size_bytes": 1,
                "sha256": (
                    compose_digest
                    if path == maintenance.COMPOSE_CONTRACT_PATH
                    else digest(index)
                ),
            }
            for index, path in enumerate(maintenance.CONTRACT_PATHS, start=100)
        ],
    }


def safety() -> dict:
    return {
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
    }


def service(
    name: str,
    configured_image: str,
    identity: int,
    *,
    status: str = "running",
    health: str = "healthy",
) -> dict:
    mounts = []
    if name == "nats":
        mounts = [
            {
                "type": "volume",
                "destination": "/data/jetstream",
                "identity_sha256": digest(800),
            }
        ]
    elif name == "postgres":
        mounts = [
            {
                "type": "bind",
                "destination": "/var/lib/postgresql/data",
                "identity_sha256": digest(801),
            }
        ]
    return {
        "container_id": f"{identity:064x}",
        "configured_image": configured_image,
        "local_image_id": digest(identity + 500),
        "created_at": "2026-07-16T00:00:00Z",
        "started_at": "2026-07-16T00:01:00Z",
        "restart_count": 0,
        "oom_killed": False,
        "status": status,
        "health": health,
        "mounts": mounts,
        "networks": [{"name": "phoenix-internal", "network_id": f"{900:064x}"}],
    }


def jetstream(sequence: int, *, redelivered: int = 0) -> dict:
    return {
        "streams": {
            "PHOENIX_FEED_TX": {
                "config_sha256": digest(700),
                "messages": 0,
                "first_seq": sequence,
                "last_seq": sequence,
            },
            "PHOENIX_ENGINE_INPUT": {
                "config_sha256": digest(701),
                "messages": 0,
                "first_seq": sequence,
                "last_seq": sequence,
            },
        },
        "consumers": {
            "PHOENIX_RECORDER": {
                "config_sha256": digest(702),
                "pending": 0,
                "ack_pending": 0,
                "redelivered": redelivered,
                "delivered_stream_seq": sequence,
                "ack_floor_stream_seq": sequence,
            },
            "PHOENIX_ENGINE_SHADOW": {
                "config_sha256": digest(703),
                "pending": 0,
                "ack_pending": 0,
                "redelivered": 0,
                "delivered_stream_seq": sequence,
                "ack_floor_stream_seq": sequence,
            },
        },
    }


def database(feed_events: int, sequence: int) -> dict:
    counts = {name: 0 for name in maintenance.DATABASE_COUNTS}
    counts["origin_transactions"] = feed_events
    counts["feed_events"] = feed_events
    return {
        "migrations": [
            {
                "version": Path(path).stem,
                "checksum": digest(
                    maintenance.CONTRACT_PATHS.index(path) + 100
                ).removeprefix("sha256:"),
            }
            for path in maintenance.EXPECTED_MIGRATIONS
        ],
        "counts": counts,
        "max_feed_sequence": sequence,
    }


def metrics(sequence: int, count: int) -> dict:
    return {
        "feed": {
            "feed_last_sequence": sequence,
            "feed_jetstream_publish_success_total": count,
            "feed_sequence_regressions_total": 0,
            "feed_sequence_gaps_total": 0,
            "feed_decode_failures_total": 0,
            "feed_readiness": 1,
        },
        "recorder": {
            "recorder_messages_persisted_total": count,
            "recorder_last_persisted_feed_sequence": sequence,
            "recorder_database_failures_total": 0,
            "recorder_jetstream_ack_failures_total": 0,
            "recorder_poison_messages_total": 0,
            "recorder_readiness": 1,
        },
    }


def rendered_metadata(plan: dict, role: str) -> dict:
    return {
        "schema": "phoenix.production-render.v1",
        "status": "ok",
        "release_sha": plan[f"{role}_sha"],
        "chain_id": 42161,
        "mode": "SHADOW",
        "live_execution": False,
        "expected_services": list(maintenance.COMPOSE_SERVICES),
        "route_registry_hash": digest(600),
        "images": maintenance._expected_compose_images(plan, role),
    }


def protected_compose_service(image: str) -> dict:
    return {
        "image": image,
        "entrypoint": ["/usr/local/bin/service"],
        "command": ["--serve"],
        "environment": {
            "PHOENIX_MODE": "SHADOW",
            "LIVE_EXECUTION": "false",
            "SIGNER_PRIVATE_KEY": "",
            "WALLET_ADDRESS": "",
            "EXECUTOR_ADDRESS": "",
        },
        "depends_on": {"nats": {"condition": "service_healthy"}},
        "healthcheck": {
            "test": ["CMD", "healthcheck"],
            "interval": "10s",
            "timeout": "3s",
            "retries": 5,
        },
        "volumes": [
            {
                "type": "bind",
                "source": "/opt/phoenix/data/protected",
                "target": "/var/lib/phoenix",
                "read_only": False,
            }
        ],
        "networks": {"phoenix-internal": None},
        "restart": "unless-stopped",
        "user": "1000:1000",
        "privileged": False,
        "read_only": True,
        "cap_drop": ["ALL"],
        "security_opt": ["no-new-privileges:true"],
        "logging": {
            "driver": "json-file",
            "options": {"max-file": "5", "max-size": "50m"},
        },
        "ports": [
            {
                "mode": "ingress",
                "target": 9000,
                "published": "9000",
                "protocol": "tcp",
            }
        ],
    }


def optional_compose_service(image: str) -> dict:
    return {
        "image": image,
        "command": ["--observe"],
        "environment": {
            "PHOENIX_MODE": "SHADOW",
            "LIVE_EXECUTION": "false",
            "SIGNER_PRIVATE_KEY": "",
            "WALLET_ADDRESS": "",
            "EXECUTOR_ADDRESS": "",
        },
        "networks": {"phoenix-internal": None},
        "restart": "unless-stopped",
        "logging": {
            "driver": "json-file",
            "options": {"max-file": "5", "max-size": "50m"},
        },
    }


def rendered_compose(plan: dict, role: str) -> dict:
    images = maintenance._expected_compose_images(plan, role)
    services = {
        service: protected_compose_service(images[service])
        for service in (
            *maintenance.FIXED_SERVICES,
            *maintenance.MUTABLE_SERVICES,
            maintenance.MIGRATION_SERVICE,
        )
    }
    services.update(
        {
            service: optional_compose_service(images[service])
            for service in maintenance.OPTIONAL_SERVICES
        }
    )
    if role == "release":
        services["prometheus"]["user"] = "65534:65534"
    project = f"phoenix-release-{plan[f'{role}_sha']}"
    return {
        "name": project,
        "x-common-env": {"env_file": ["/etc/phoenix/phoenix.env"]},
        "x-logging": {
            "driver": "json-file",
            "options": {"max-file": "5", "max-size": "50m"},
        },
        "services": services,
        "networks": {
            "phoenix-internal": {
                "name": f"{project}_phoenix-internal",
                "driver": "bridge",
            }
        },
        "volumes": {
            "nats-jetstream": {
                "name": "phoenix-nats-jetstream",
            }
        },
    }


class ProtectedMaintenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.release_manifest = self.write("release.json", manifest(RELEASE_SHA, 10))
        self.rollback_manifest = self.write("rollback.json", manifest(ROLLBACK_SHA, 30))
        self.release_assets = self.write(
            "release-assets.json", asset_manifest(RELEASE_SHA)
        )
        self.rollback_assets = self.write(
            "rollback-assets.json", asset_manifest(ROLLBACK_SHA)
        )
        self.plan = maintenance.build_plan(
            self.release_manifest,
            self.rollback_manifest,
            self.release_assets,
            self.rollback_assets,
            RELEASE_SHA,
            ROLLBACK_SHA,
        )

    def tearDown(self) -> None:
        self.directory.cleanup()

    def write(self, name: str, value: dict) -> Path:
        path = self.root / name
        path.write_text(json.dumps(value), encoding="utf-8")
        return path

    def build_plan_from_manifests(
        self, release_manifest: dict, rollback_manifest: dict
    ) -> dict:
        return maintenance.build_plan(
            self.write("release-generation.json", release_manifest),
            self.write("rollback-generation.json", rollback_manifest),
            self.release_assets,
            self.rollback_assets,
            RELEASE_SHA,
            ROLLBACK_SHA,
        )

    def build_generation_plan(
        self, release_images: tuple[str, ...], rollback_images: tuple[str, ...]
    ) -> dict:
        return self.build_plan_from_manifests(
            manifest(RELEASE_SHA, 10, release_images),
            manifest(ROLLBACK_SHA, 30, rollback_images),
        )

    def baseline(self) -> dict:
        return {
            "schema_version": maintenance.SNAPSHOT_SCHEMA,
            "phase": "pre",
            "release_sha": ROLLBACK_SHA,
            "observed_at": "2026-07-16T00:02:00Z",
            "disk_free_bytes": maintenance.MIN_DISK_FREE_BYTES * 2,
            "services": {
                "nitro-feed-relay": service(
                    "nitro-feed-relay",
                    maintenance.FIXED_IMAGES["nitro-feed-relay"],
                    1,
                ),
                "feed-ingestor": service(
                    "feed-ingestor",
                    self.plan["images"]["rollback"]["feed-ingestor"],
                    2,
                ),
                "nats": service("nats", maintenance.FIXED_IMAGES["nats"], 3),
                "postgres": service(
                    "postgres", maintenance.FIXED_IMAGES["postgres"], 4
                ),
                "recorder": service(
                    "recorder", self.plan["images"]["rollback"]["recorder"], 5
                ),
            },
            "jetstream": jetstream(1000),
            "database": database(100, 1000),
            "metrics": metrics(1000, 100),
            "safety": safety(),
            "protected_storage_identity_sha256": digest(802),
        }

    def candidate_start(self) -> dict:
        value = self.baseline()
        value["phase"] = "post-start"
        value["release_sha"] = RELEASE_SHA
        value["services"]["feed-ingestor"] = service(
            "feed-ingestor", self.plan["images"]["release"]["feed-ingestor"], 12
        )
        value["services"]["recorder"] = service(
            "recorder", self.plan["images"]["release"]["recorder"], 15
        )
        value["jetstream"] = jetstream(1001)
        value["database"] = database(101, 1001)
        value["metrics"] = metrics(1001, 1)
        return value

    def candidate_final(self) -> dict:
        value = self.candidate_start()
        value["phase"] = "final"
        value["observed_at"] = "2026-07-16T00:03:00Z"
        value["jetstream"] = jetstream(1010)
        value["database"] = database(110, 1010)
        value["metrics"] = metrics(1010, 10)
        return value

    def test_reviewed_six_and_seven_image_generation_pairs_pass(self) -> None:
        cases = (
            (
                "legacy-to-legacy",
                maintenance.LEGACY_RELEASE_IMAGES,
                maintenance.LEGACY_RELEASE_IMAGES,
            ),
            (
                "current-to-legacy",
                maintenance.CURRENT_RELEASE_IMAGES,
                maintenance.LEGACY_RELEASE_IMAGES,
            ),
            (
                "current-to-current",
                maintenance.CURRENT_RELEASE_IMAGES,
                maintenance.CURRENT_RELEASE_IMAGES,
            ),
        )
        for name, release_images, rollback_images in cases:
            with self.subTest(name=name):
                plan = self.build_generation_plan(release_images, rollback_images)
                maintenance.validate_plan(plan)
                self.assertEqual(set(plan["images"]["release"]), set(release_images))
                self.assertEqual(set(plan["images"]["rollback"]), set(rollback_images))

    def test_partial_and_unexpected_candidate_image_sets_fail_closed(self) -> None:
        cases = (
            (
                "legacy-production-image-missing",
                tuple(
                    name
                    for name in maintenance.LEGACY_RELEASE_IMAGES
                    if name != "dashboard"
                ),
            ),
            (
                "live-executor-present-but-recorder-missing",
                tuple(
                    name
                    for name in maintenance.CURRENT_RELEASE_IMAGES
                    if name != "recorder"
                ),
            ),
            (
                "unexpected-eighth-image",
                maintenance.CURRENT_RELEASE_IMAGES + ("unreviewed-image",),
            ),
        )
        for name, release_images in cases:
            with self.subTest(name=name):
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, "release_manifest_invalid"
                ):
                    self.build_generation_plan(
                        release_images, maintenance.LEGACY_RELEASE_IMAGES
                    )

    def test_unexpected_rollback_image_fails_closed(self) -> None:
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "release_manifest_invalid"
        ):
            self.build_generation_plan(
                maintenance.CURRENT_RELEASE_IMAGES,
                maintenance.CURRENT_RELEASE_IMAGES + ("unreviewed-image",),
            )

    def test_live_executor_difference_is_release_evidence_only(self) -> None:
        plan = self.build_generation_plan(
            maintenance.CURRENT_RELEASE_IMAGES,
            maintenance.CURRENT_RELEASE_IMAGES,
        )
        self.assertNotEqual(
            plan["images"]["release"]["live-executor"],
            plan["images"]["rollback"]["live-executor"],
        )
        self.assertEqual(
            plan["protected_allowlist"], ["feed-ingestor", "recorder"]
        )
        self.assertNotIn("live-executor", plan["protected_allowlist"])
        self.assertNotIn("live-executor", plan["fixed_services"])
        self.assertNotIn("live-executor", plan["optional_services"])

    def test_both_mutable_service_digest_changes_remain_required(self) -> None:
        for service in maintenance.MUTABLE_SERVICES:
            with self.subTest(service=service):
                release = manifest(
                    RELEASE_SHA, 10, maintenance.CURRENT_RELEASE_IMAGES
                )
                rollback = manifest(
                    ROLLBACK_SHA, 30, maintenance.LEGACY_RELEASE_IMAGES
                )
                rollback["images"][service]["digest"] = release["images"][service][
                    "digest"
                ]
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, "protected_allowlist_mismatch"
                ):
                    self.build_plan_from_manifests(release, rollback)

    def test_seven_image_plan_keeps_production_render_set_exact(self) -> None:
        plan = self.build_generation_plan(
            maintenance.CURRENT_RELEASE_IMAGES,
            maintenance.CURRENT_RELEASE_IMAGES,
        )
        release_metadata = rendered_metadata(plan, "release")
        rollback_metadata = rendered_metadata(plan, "rollback")
        release_compose = rendered_compose(plan, "release")
        rollback_compose = rendered_compose(plan, "rollback")
        self.assertEqual(
            release_metadata["expected_services"], list(maintenance.COMPOSE_SERVICES)
        )
        self.assertNotIn("live-executor", release_metadata["images"])
        self.assertNotIn("live-executor", release_compose["services"])
        maintenance.validate_render_pair(
            plan,
            release_metadata,
            rollback_metadata,
            release_compose,
            rollback_compose,
        )

        release_metadata["images"]["live-executor"] = plan["images"]["release"][
            "live-executor"
        ]
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "render_contract_invalid"
        ):
            maintenance.validate_render_pair(
                plan,
                release_metadata,
                rollback_metadata,
                release_compose,
                rollback_compose,
            )

    def test_image_reference_output_excludes_live_executor(self) -> None:
        plan = self.build_generation_plan(
            maintenance.CURRENT_RELEASE_IMAGES,
            maintenance.CURRENT_RELEASE_IMAGES,
        )
        plan_path = self.write("seven-image-plan.json", plan)
        output = io.StringIO()
        with redirect_stdout(output):
            maintenance.command_image_refs(SimpleNamespace(plan=str(plan_path)))
        rows = [line.split("\t") for line in output.getvalue().splitlines()]
        self.assertNotIn("live-executor", {row[1] for row in rows})
        for role in ("release", "rollback"):
            self.assertEqual(
                [row[1] for row in rows if row[0] == role],
                list(maintenance.MAINTENANCE_IMAGE_REFERENCES),
            )

    def test_plan_has_exact_allowlist_and_order(self) -> None:
        self.assertEqual(
            self.plan["protected_allowlist"], ["feed-ingestor", "recorder"]
        )
        self.assertEqual(
            self.plan["maintenance_order"], ["recorder", "feed-ingestor"]
        )
        self.assertEqual(self.plan["quiesce_before_update"], ["feed-ingestor"])
        self.assertEqual(
            set(self.plan["contract_sha256"]),
            set(maintenance.EXACT_HASH_CONTRACT_PATHS),
        )
        self.assertNotIn(
            maintenance.COMPOSE_CONTRACT_PATH, self.plan["contract_sha256"]
        )
        self.assertEqual(
            self.plan["compose_source_sha256"],
            {
                "release": RELEASE_COMPOSE_DIGEST,
                "rollback": ROLLBACK_COMPOSE_DIGEST,
            },
        )
        maintenance.validate_plan(self.plan)

    def test_noncompose_protected_contract_change_fails_before_runtime(self) -> None:
        changed = asset_manifest(RELEASE_SHA)
        for item in changed["files"]:
            if item["path"] == "deploy/nats-server.conf":
                item["sha256"] = digest(999)
        changed_path = self.write("changed-assets.json", changed)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "protected_contract_changed"
        ):
            maintenance.build_plan(
                self.release_manifest,
                self.rollback_manifest,
                changed_path,
                self.rollback_assets,
                RELEASE_SHA,
                ROLLBACK_SHA,
            )

    def test_exact_v4_v3_compose_source_difference_requires_render_review(self) -> None:
        self.assertNotEqual(
            self.plan["compose_source_sha256"]["release"],
            self.plan["compose_source_sha256"]["rollback"],
        )
        maintenance.validate_render_pair(
            self.plan,
            rendered_metadata(self.plan, "release"),
            rendered_metadata(self.plan, "rollback"),
            rendered_compose(self.plan, "release"),
            rendered_compose(self.plan, "rollback"),
        )

    def test_missing_rollback_assets_fail_closed(self) -> None:
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "release_assets_missing"
        ):
            maintenance.build_plan(
                self.release_manifest,
                self.rollback_manifest,
                self.release_assets,
                self.root / "missing.json",
                RELEASE_SHA,
                ROLLBACK_SHA,
            )

    def test_mutable_image_reference_is_rejected(self) -> None:
        value = manifest(RELEASE_SHA, 10)
        value["images"]["feed-ingestor"]["tag"] = "latest"
        path = self.write("mutable.json", value)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "mutable_image_reference"
        ):
            maintenance.build_plan(
                path,
                self.rollback_manifest,
                self.release_assets,
                self.rollback_assets,
                RELEASE_SHA,
                ROLLBACK_SHA,
            )

    def test_render_pair_requires_shadow_and_fixed_images(self) -> None:
        release_metadata = rendered_metadata(self.plan, "release")
        rollback_metadata = rendered_metadata(self.plan, "rollback")
        release_compose = rendered_compose(self.plan, "release")
        rollback_compose = rendered_compose(self.plan, "rollback")
        maintenance.validate_render_pair(
            self.plan,
            release_metadata,
            rollback_metadata,
            release_compose,
            rollback_compose,
        )
        release_metadata["live_execution"] = True
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "render_contract_invalid"
        ):
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                release_compose,
                rollback_compose,
            )

    def test_every_protected_service_field_mutation_fails_closed(self) -> None:
        release_metadata = rendered_metadata(self.plan, "release")
        rollback_metadata = rendered_metadata(self.plan, "rollback")
        rollback_compose = rendered_compose(self.plan, "rollback")
        mutations = {
            "mount": lambda service: service["volumes"][0].update(
                source="/opt/phoenix/data/changed"
            ),
            "environment": lambda service: service["environment"].update(
                NATS_URL="nats://changed.invalid:4222"
            ),
            "command": lambda service: service["command"].append("--changed"),
            "entrypoint": lambda service: service["entrypoint"].append("--changed"),
            "dependency": lambda service: service["depends_on"].update(
                postgres={"condition": "service_started"}
            ),
            "healthcheck": lambda service: service["healthcheck"].update(retries=99),
            "network": lambda service: service["networks"].update(
                unexpected=None
            ),
            "restart": lambda service: service.update(restart="always"),
            "user": lambda service: service.update(user="0:0"),
            "privilege": lambda service: service.update(privileged=True),
            "logging": lambda service: service["logging"].update(driver="none"),
            "security": lambda service: service["security_opt"].append(
                "label=disable"
            ),
            "ports": lambda service: service["ports"].append(
                {
                    "mode": "ingress",
                    "target": 9999,
                    "published": "9999",
                    "protocol": "tcp",
                }
            ),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                release_compose = rendered_compose(self.plan, "release")
                mutate(release_compose["services"]["postgres"])
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError,
                    "protected_compose_service_changed:postgres",
                ):
                    maintenance.validate_render_pair(
                        self.plan,
                        release_metadata,
                        rollback_metadata,
                        release_compose,
                        rollback_compose,
                    )

    def test_nats_mount_and_protected_mutable_definition_changes_fail(self) -> None:
        release_metadata = rendered_metadata(self.plan, "release")
        rollback_metadata = rendered_metadata(self.plan, "rollback")
        rollback_compose = rendered_compose(self.plan, "rollback")
        for service in ("nats", "feed-ingestor", "recorder"):
            with self.subTest(service=service):
                release_compose = rendered_compose(self.plan, "release")
                release_compose["services"][service]["volumes"][0][
                    "source"
                ] = f"/opt/phoenix/data/{service}-changed"
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError,
                    f"protected_compose_service_changed:{service}",
                ):
                    maintenance.validate_render_pair(
                        self.plan,
                        release_metadata,
                        rollback_metadata,
                        release_compose,
                        rollback_compose,
                    )

    def test_service_set_network_volume_and_migration_changes_fail(self) -> None:
        release_metadata = rendered_metadata(self.plan, "release")
        rollback_metadata = rendered_metadata(self.plan, "rollback")
        rollback_compose = rendered_compose(self.plan, "rollback")
        cases = []

        added = rendered_compose(self.plan, "release")
        added["services"]["unexpected"] = optional_compose_service(
            maintenance.PROMETHEUS_IMAGE
        )
        cases.append(
            ("addition", added, "protected_compose_service_set_changed")
        )

        deleted = rendered_compose(self.plan, "release")
        del deleted["services"]["postgres"]
        cases.append(
            ("deletion", deleted, "protected_compose_service_set_changed")
        )

        networks = rendered_compose(self.plan, "release")
        networks["networks"]["phoenix-internal"]["driver"] = "overlay"
        cases.append(
            ("networks", networks, "protected_compose_networks_changed")
        )

        volumes = rendered_compose(self.plan, "release")
        volumes["volumes"]["nats-jetstream"]["name"] = "changed"
        cases.append(("volumes", volumes, "protected_compose_volumes_changed"))

        migration = rendered_compose(self.plan, "release")
        migration["services"]["migration-runner"]["command"].append("--changed")
        cases.append(
            (
                "migration",
                migration,
                "protected_compose_service_changed:migration-runner",
            )
        )

        extensions = rendered_compose(self.plan, "release")
        extensions["x-logging"]["options"]["max-size"] = "500m"
        cases.append(
            (
                "extensions",
                extensions,
                "protected_compose_extensions_changed",
            )
        )

        for name, release_compose, expected in cases:
            with self.subTest(name=name):
                with self.assertRaisesRegex(maintenance.MaintenanceError, expected):
                    maintenance.validate_render_pair(
                        self.plan,
                        release_metadata,
                        rollback_metadata,
                        release_compose,
                        rollback_compose,
                    )

    def test_unreviewed_optional_change_and_mutable_tag_fail(self) -> None:
        release_metadata = rendered_metadata(self.plan, "release")
        rollback_metadata = rendered_metadata(self.plan, "rollback")
        rollback_compose = rendered_compose(self.plan, "rollback")

        optional = rendered_compose(self.plan, "release")
        optional["services"]["rpc-gateway"]["command"].append("--changed")
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "protected_compose_service_changed:rpc-gateway",
        ):
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                optional,
                rollback_compose,
            )

        wrong_prometheus_user = rendered_compose(self.plan, "release")
        wrong_prometheus_user["services"]["prometheus"]["user"] = "0:0"
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "protected_compose_service_changed:prometheus",
        ):
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                wrong_prometheus_user,
                rollback_compose,
            )

        mutable = rendered_compose(self.plan, "release")
        mutable["services"]["feed-ingestor"]["image"] = (
            "ghcr.io/majidasgharitabrizi/feed-ingestor:latest"
        )
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "protected_compose_service_changed:feed-ingestor",
        ):
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                mutable,
                rollback_compose,
            )

        fixed = rendered_compose(self.plan, "release")
        fixed["services"]["postgres"]["image"] = (
            "postgres@sha256:" + ("9" * 64)
        )
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "protected_compose_service_changed:postgres",
        ):
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                fixed,
                rollback_compose,
            )

    def test_malformed_render_route_change_and_secret_diagnostics_fail_closed(
        self,
    ) -> None:
        release_metadata = rendered_metadata(self.plan, "release")
        rollback_metadata = rendered_metadata(self.plan, "rollback")
        release_compose = rendered_compose(self.plan, "release")
        rollback_compose = rendered_compose(self.plan, "rollback")

        malformed = deepcopy(release_compose)
        del malformed["volumes"]
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "protected_compose_invalid"
        ):
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                malformed,
                rollback_compose,
            )

        changed_route = deepcopy(release_metadata)
        changed_route["route_registry_hash"] = digest(601)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "route_contract_changed"
        ):
            maintenance.validate_render_pair(
                self.plan,
                changed_route,
                rollback_metadata,
                release_compose,
                rollback_compose,
            )

        provider_value = "https://provider.invalid/redacted-test-only"
        secret_bearing = deepcopy(release_compose)
        secret_bearing["services"]["postgres"]["environment"][
            "DATABASE_URL"
        ] = provider_value
        with self.assertRaises(maintenance.MaintenanceError) as captured:
            maintenance.validate_render_pair(
                self.plan,
                release_metadata,
                rollback_metadata,
                secret_bearing,
                rollback_compose,
            )
        self.assertEqual(
            str(captured.exception), "protected_compose_service_changed:postgres"
        )
        self.assertNotIn(provider_value, str(captured.exception))

    def test_recorder_then_feed_transition_is_bounded(self) -> None:
        baseline = self.baseline()
        recorder_stage = self.baseline()
        recorder_stage["phase"] = "recorder"
        recorder_stage["release_sha"] = RELEASE_SHA
        recorder_stage["services"]["feed-ingestor"]["status"] = "exited"
        recorder_stage["services"]["recorder"] = service(
            "recorder", self.plan["images"]["release"]["recorder"], 15
        )
        recorder_stage["metrics"]["feed"] = {
            name: None for name in maintenance.FEED_METRICS
        }
        maintenance.validate_transition(
            self.plan, baseline, recorder_stage, "recorder", None
        )
        recorder_storage_drift = json.loads(json.dumps(recorder_stage))
        recorder_storage_drift["protected_storage_identity_sha256"] = digest(998)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "protected_storage_metadata_changed"
        ):
            maintenance.validate_transition(
                self.plan, baseline, recorder_storage_drift, "recorder", None
            )
        maintenance.validate_transition(
            self.plan,
            baseline,
            self.candidate_final(),
            "final",
            self.candidate_start(),
        )

        changed_storage = self.candidate_final()
        changed_storage["protected_storage_identity_sha256"] = digest(999)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "protected_storage_metadata_changed"
        ):
            maintenance.validate_transition(
                self.plan,
                baseline,
                changed_storage,
                "final",
                self.candidate_start(),
            )

    def test_progress_allows_database_snapshot_to_trail_later_metrics(self) -> None:
        final = self.candidate_final()
        final["database"] = database(110, 1010)
        final["metrics"] = metrics(1013, 10)
        self.assertGreater(
            final["metrics"]["recorder"]["recorder_last_persisted_feed_sequence"],
            final["database"]["max_feed_sequence"],
        )
        maintenance.validate_transition(
            self.plan,
            self.baseline(),
            final,
            "final",
            self.candidate_start(),
        )

    def test_progress_requires_database_sequence_and_feed_events(self) -> None:
        start = self.candidate_start()
        unchanged_sequence = self.candidate_final()
        unchanged_sequence["database"]["max_feed_sequence"] = start["database"][
            "max_feed_sequence"
        ]
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "database_feed_sequence_not_progressing",
        ):
            maintenance.validate_transition(
                self.plan, self.baseline(), unchanged_sequence, "final", start
            )

        unchanged_events = self.candidate_final()
        unchanged_events["database"]["counts"]["feed_events"] = start["database"][
            "counts"
        ]["feed_events"]
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "database_feed_events_not_progressing",
        ):
            maintenance.validate_transition(
                self.plan, self.baseline(), unchanged_events, "final", start
            )

    def test_progress_rejects_database_sequence_regression(self) -> None:
        start = self.candidate_start()
        start["database"]["max_feed_sequence"] = 990
        final = self.candidate_final()
        final["database"]["max_feed_sequence"] = 995
        with self.assertRaisesRegex(
            maintenance.MaintenanceError,
            "database_feed_sequence_regressed",
        ):
            maintenance.validate_transition(
                self.plan, self.baseline(), final, "final", start
            )

    def test_progress_requires_recorder_and_feed_counters(self) -> None:
        start = self.candidate_start()
        stalled_metrics = (
            ("feed", "feed_last_sequence", "feed_sequence_not_progressing"),
            (
                "feed",
                "feed_jetstream_publish_success_total",
                "feed_publish_not_progressing",
            ),
            (
                "recorder",
                "recorder_messages_persisted_total",
                "recorder_persist_count_not_progressing",
            ),
            (
                "recorder",
                "recorder_last_persisted_feed_sequence",
                "recorder_sequence_not_progressing",
            ),
        )
        for group, metric, expected in stalled_metrics:
            with self.subTest(metric=metric):
                stalled = self.candidate_final()
                stalled["metrics"][group][metric] = start["metrics"][group][metric]
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, expected
                ):
                    maintenance.validate_transition(
                        self.plan, self.baseline(), stalled, "final", start
                    )

    def test_progress_requires_readiness(self) -> None:
        for group, metric, expected in (
            ("feed", "feed_readiness", "feed_readiness_not_ready"),
            ("recorder", "recorder_readiness", "recorder_readiness_not_ready"),
        ):
            with self.subTest(group=group):
                final = self.candidate_final()
                final["metrics"][group][metric] = 0
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, expected
                ):
                    maintenance.validate_transition(
                        self.plan,
                        self.baseline(),
                        final,
                        "final",
                        self.candidate_start(),
                    )

    def test_progress_rejects_every_runtime_error_counter(self) -> None:
        error_metrics = (
            (
                "feed",
                "feed_sequence_regressions_total",
            ),
            ("feed", "feed_sequence_gaps_total"),
            ("feed", "feed_decode_failures_total"),
            ("recorder", "recorder_database_failures_total"),
            ("recorder", "recorder_jetstream_ack_failures_total"),
            ("recorder", "recorder_poison_messages_total"),
        )
        for group, metric in error_metrics:
            with self.subTest(metric=metric):
                final = self.candidate_final()
                final["metrics"][group][metric] = 1
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, "runtime_integrity_failed"
                ):
                    maintenance.validate_transition(
                        self.plan,
                        self.baseline(),
                        final,
                        "final",
                        self.candidate_start(),
                    )

    def test_progress_rejects_consumer_backlog_above_bounds(self) -> None:
        for field, value in (
            ("pending", maintenance.MAX_RECORDER_PENDING + 1),
            ("ack_pending", maintenance.MAX_ACK_PENDING + 1),
        ):
            with self.subTest(field=field):
                final = self.candidate_final()
                final["jetstream"]["consumers"]["PHOENIX_RECORDER"][field] = value
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, "consumer_backlog_unbounded"
                ):
                    maintenance.validate_transition(
                        self.plan,
                        self.baseline(),
                        final,
                        "final",
                        self.candidate_start(),
                    )

    def test_progress_rejects_storage_or_fixed_service_identity_change(self) -> None:
        storage_changed = self.candidate_final()
        storage_changed["protected_storage_identity_sha256"] = digest(999)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "protected_storage_metadata_changed"
        ):
            maintenance.validate_transition(
                self.plan,
                self.baseline(),
                storage_changed,
                "final",
                self.candidate_start(),
            )

        identity_changed = self.candidate_final()
        identity_changed["services"]["postgres"]["container_id"] = f"{99:064x}"
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "fixed_service_identity_changed"
        ):
            maintenance.validate_transition(
                self.plan,
                self.baseline(),
                identity_changed,
                "final",
                self.candidate_start(),
            )

    def test_fixed_identity_mount_and_execution_drift_fail(self) -> None:
        baseline = self.baseline()
        for service_name in maintenance.FIXED_SERVICES:
            with self.subTest(service=service_name):
                final = self.candidate_final()
                final["services"][service_name]["container_id"] = f"{99:064x}"
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, "fixed_service_identity_changed"
                ):
                    maintenance.validate_transition(
                        self.plan, baseline, final, "final", self.candidate_start()
                    )

        final = self.candidate_final()
        final["services"]["nats"]["mounts"][0]["identity_sha256"] = digest(999)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "mount_identity_changed"
        ):
            maintenance.validate_transition(
                self.plan, baseline, final, "final", self.candidate_start()
            )

    def test_rollback_requires_exact_v3_images_and_progress(self) -> None:
        baseline = self.baseline()
        start = self.baseline()
        start["phase"] = "rollback-start"
        start["services"]["feed-ingestor"] = service(
            "feed-ingestor", self.plan["images"]["rollback"]["feed-ingestor"], 22
        )
        start["services"]["recorder"] = service(
            "recorder", self.plan["images"]["rollback"]["recorder"], 25
        )
        start["jetstream"] = jetstream(1011)
        start["database"] = database(111, 1011)
        start["metrics"] = metrics(1011, 1)

        final = json.loads(json.dumps(start))
        final["phase"] = "rollback-final"
        final["observed_at"] = "2026-07-16T00:04:00Z"
        final["jetstream"] = jetstream(1020)
        final["database"] = database(120, 1020)
        final["metrics"] = metrics(1022, 10)
        self.assertGreater(
            final["metrics"]["recorder"]["recorder_last_persisted_feed_sequence"],
            final["database"]["max_feed_sequence"],
        )
        maintenance.validate_transition(
            self.plan, baseline, final, "rollback", start
        )

        changed_storage = json.loads(json.dumps(final))
        changed_storage["protected_storage_identity_sha256"] = digest(999)
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "protected_storage_metadata_changed"
        ):
            maintenance.validate_transition(
                self.plan, baseline, changed_storage, "rollback", start
            )

        wrong = json.loads(json.dumps(final))
        wrong["services"]["recorder"]["configured_image"] = self.plan["images"][
            "release"
        ]["recorder"]
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "mutable_service_transition_invalid"
        ):
            maintenance.validate_transition(
                self.plan, baseline, wrong, "rollback", start
            )

    def test_every_execution_configuration_value_fails_closed(self) -> None:
        mutations = {
            "mode": "LIVE",
            "live_execution": True,
            "signer_configured": True,
            "wallet_configured": True,
            "executor_configured": True,
            "public_submission_configured": True,
            "private_submission_configured": True,
            "broadcast_configured": True,
            "execution_eligible": True,
            "execution_request_created": True,
            "optional_services_stopped": False,
        }
        for name, changed in mutations.items():
            with self.subTest(name=name):
                value = safety()
                value[name] = changed
                with self.assertRaisesRegex(
                    maintenance.MaintenanceError, "safety_invariant_failed"
                ):
                    maintenance.normalize_safety(value)

        final = self.candidate_final()
        final["database"]["counts"]["execution_attempts"] = 1
        baseline = self.baseline()
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "execution_activity_detected"
        ):
            maintenance.validate_transition(
                self.plan, baseline, final, "final", self.candidate_start()
            )

    def test_redelivery_explosion_and_sequence_regression_fail(self) -> None:
        final = self.candidate_final()
        final["jetstream"]["consumers"]["PHOENIX_RECORDER"]["redelivered"] = (
            maintenance.MAX_REDELIVERY_DELTA + 1
        )
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "jetstream_consumer_changed"
        ):
            maintenance.validate_transition(
                self.plan,
                self.baseline(),
                final,
                "final",
                self.candidate_start(),
            )

        final = self.candidate_final()
        final["metrics"]["feed"]["feed_sequence_regressions_total"] = 1
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "runtime_integrity_failed"
        ):
            maintenance.validate_transition(
                self.plan,
                self.baseline(),
                final,
                "final",
                self.candidate_start(),
            )

    def test_unexpected_consumer_is_rejected(self) -> None:
        value = {
            "streams": [
                {
                    "name": name,
                    "config": {"name": name},
                    "state": {"messages": 0, "first_seq": 1, "last_seq": 1},
                }
                for name in maintenance.STREAM_NAMES
            ],
            "consumers": [
                {
                    "name": name,
                    "config": {"durable_name": name},
                    "num_pending": 0,
                    "num_ack_pending": 0,
                    "num_redelivered": 0,
                }
                for name in maintenance.CONSUMER_NAMES
            ],
        }
        value["consumers"].append(
            {
                "name": "PHOENIX_SHADOW_DISPATCH",
                "config": {"durable_name": "PHOENIX_SHADOW_DISPATCH"},
                "num_pending": 0,
                "num_ack_pending": 0,
                "num_redelivered": 0,
            }
        )
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "unexpected_jetstream_resource"
        ):
            maintenance.normalize_jetstream(value)

    def test_snapshot_redacts_mount_sources(self) -> None:
        service_inputs = []
        for index, name in enumerate(maintenance.PROTECTED_SERVICES, start=1):
            configured = (
                maintenance.FIXED_IMAGES[name]
                if name in maintenance.FIXED_IMAGES
                else self.plan["images"]["rollback"][name]
            )
            raw = {
                "container_id": f"{index:064x}",
                "configured_image": configured,
                "local_image_id": digest(index + 500),
                "created_at": "2026-07-16T00:00:00Z",
                "started_at": "2026-07-16T00:01:00Z",
                "restart_count": 0,
                "oom_killed": False,
                "status": "running",
                "health": "healthy",
                "mounts": [],
                "networks": {
                    "phoenix-internal": {"NetworkID": f"{900:064x}"}
                },
            }
            if name == "postgres":
                raw["mounts"] = [
                    {
                        "Type": "bind",
                        "Source": "/secret/host/path",
                        "Destination": "/var/lib/postgresql/data",
                        "Mode": "rw",
                        "RW": True,
                        "Propagation": "rprivate",
                    }
                ]
            path = self.write(f"{name}.json", raw)
            service_inputs.append(f"{name}={path}")

        js = {
            "streams": [
                {
                    "name": name,
                    "config": {"name": name},
                    "state": {"messages": 0, "first_seq": 1000, "last_seq": 1000},
                }
                for name in maintenance.STREAM_NAMES
            ],
            "consumers": [
                {
                    "name": name,
                    "config": {"durable_name": name},
                    "num_pending": 0,
                    "num_ack_pending": 0,
                    "num_redelivered": 0,
                    "delivered": {"stream_seq": 1000},
                    "ack_floor": {"stream_seq": 1000},
                }
                for name in maintenance.CONSUMER_NAMES
            ],
        }
        js_path = self.write("jetstream.json", js)
        database_path = self.write("database.json", database(100, 1000))
        safety_path = self.write("safety.json", safety())
        feed_metrics = self.root / "feed.metrics"
        recorder_metrics = self.root / "recorder.metrics"
        feed_metrics.write_text(
            "\n".join(
                f"{name} {self.baseline()['metrics']['feed'][name]}"
                for name in maintenance.FEED_METRICS
            ),
            encoding="utf-8",
        )
        recorder_metrics.write_text(
            "\n".join(
                f"{name} {self.baseline()['metrics']['recorder'][name]}"
                for name in maintenance.RECORDER_METRICS
            ),
            encoding="utf-8",
        )
        storage_metadata = self.root / "storage.metadata"
        storage_metadata.write_text(
            "postgres-path|.|999|999|41c0\n"
            "nats-volume|phoenix-nats-jetstream|local|local\n",
            encoding="ascii",
        )
        snapshot = maintenance.build_snapshot(
            "pre",
            ROLLBACK_SHA,
            service_inputs,
            js_path,
            database_path,
            feed_metrics,
            recorder_metrics,
            safety_path,
            storage_metadata,
            maintenance.MIN_DISK_FREE_BYTES * 2,
        )
        serialized = json.dumps(snapshot)
        self.assertNotIn("/secret/host/path", serialized)
        self.assertIn("identity_sha256", serialized)
        self.assertIn("protected_storage_identity_sha256", snapshot)
        maintenance.validate_snapshot(snapshot)

    def test_incomplete_storage_evidence_blocks_snapshot_validation(self) -> None:
        snapshot = self.baseline()
        snapshot.pop("protected_storage_identity_sha256")
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "snapshot_invalid"
        ):
            maintenance.validate_snapshot(snapshot)

    def test_context_is_truthful_and_non_executing(self) -> None:
        metadata = {
            "release_sha": RELEASE_SHA,
            "mode": "SHADOW",
            "live_execution": False,
            "route_registry_hash": digest(600),
        }
        value = self.candidate_final()
        context = maintenance.build_context(self.plan, value, metadata)
        self.assertEqual(context["status"], "protected_maintenance_complete")
        self.assertTrue(context["optional_services_stopped"])
        self.assertFalse(context["execution_eligible"])
        self.assertFalse(context["execution_request_created"])
        self.assertEqual(
            context["protected_storage_identity_sha256"], digest(802)
        )


if __name__ == "__main__":
    unittest.main()
