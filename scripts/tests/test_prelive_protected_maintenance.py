import json
import tempfile
import unittest
from pathlib import Path

from scripts import prelive_protected_maintenance as maintenance


RELEASE_SHA = "1" * 40
ROLLBACK_SHA = "2" * 40


def digest(value: int) -> str:
    return f"sha256:{value:064x}"


def image_reference(name: str, value: int) -> str:
    return f"ghcr.io/majidasgharitabrizi/{name}@{digest(value)}"


def manifest(release_sha: str, offset: int) -> dict:
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
            for index, name in enumerate(maintenance.OWNED_IMAGES, start=1)
        },
    }


def asset_manifest(release_sha: str) -> dict:
    return {
        "schema": maintenance.ASSET_SCHEMA,
        "release_sha": release_sha,
        "files": [
            {
                "path": path,
                "mode": "0644",
                "size_bytes": 1,
                "sha256": digest(index),
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

    def test_plan_has_exact_allowlist_and_order(self) -> None:
        self.assertEqual(
            self.plan["protected_allowlist"], ["feed-ingestor", "recorder"]
        )
        self.assertEqual(
            self.plan["maintenance_order"], ["recorder", "feed-ingestor"]
        )
        self.assertEqual(self.plan["quiesce_before_update"], ["feed-ingestor"])
        maintenance.validate_plan(self.plan)

    def test_third_protected_contract_change_fails_before_runtime(self) -> None:
        changed = asset_manifest(RELEASE_SHA)
        for item in changed["files"]:
            if item["path"] == "compose.prod.yml":
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
        release_images = dict(maintenance.FIXED_IMAGES)
        release_images.update(
            {
                "feed-ingestor": self.plan["images"]["release"]["feed-ingestor"],
                "recorder": self.plan["images"]["release"]["recorder"],
            }
        )
        rollback_images = dict(maintenance.FIXED_IMAGES)
        rollback_images.update(
            {
                "feed-ingestor": self.plan["images"]["rollback"]["feed-ingestor"],
                "recorder": self.plan["images"]["rollback"]["recorder"],
            }
        )
        release = {
            "schema": "phoenix.production-render.v1",
            "status": "ok",
            "release_sha": RELEASE_SHA,
            "chain_id": 42161,
            "mode": "SHADOW",
            "live_execution": False,
            "route_registry_hash": digest(600),
            "images": release_images,
        }
        rollback = {
            **release,
            "release_sha": ROLLBACK_SHA,
            "images": rollback_images,
        }
        maintenance.validate_render_pair(self.plan, release, rollback)
        release["live_execution"] = True
        with self.assertRaisesRegex(
            maintenance.MaintenanceError, "render_contract_invalid"
        ):
            maintenance.validate_render_pair(self.plan, release, rollback)

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
        maintenance.validate_transition(
            self.plan,
            baseline,
            self.candidate_final(),
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

    def test_rollback_requires_exact_v2_images_and_progress(self) -> None:
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
        final["metrics"] = metrics(1020, 10)
        maintenance.validate_transition(
            self.plan, baseline, final, "rollback", start
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
        snapshot = maintenance.build_snapshot(
            "pre",
            ROLLBACK_SHA,
            service_inputs,
            js_path,
            database_path,
            feed_metrics,
            recorder_metrics,
            safety_path,
            maintenance.MIN_DISK_FREE_BYTES * 2,
        )
        serialized = json.dumps(snapshot)
        self.assertNotIn("/secret/host/path", serialized)
        self.assertIn("identity_sha256", serialized)
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


if __name__ == "__main__":
    unittest.main()
