import argparse
import copy
import json
import re
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from jsonschema import Draft7Validator, Draft202012Validator

from scripts import prelive_protected_maintenance
from scripts import production_context
from scripts import release_assets
from scripts import release_components
from scripts import release_provenance


ROOT = Path(__file__).resolve().parents[2]
RELEASE_SHA = "a" * 40
RELEASE_RUN = "30000001001"
ROLLBACK_SHA = "b" * 40
ROLLBACK_RUN = "30000001002"
SOURCE_CI_RUN = "30000001003"

EXPECTED_IMAGES = (
    "atlas-observer",
    "dashboard",
    "feed-ingestor",
    "fork-sandbox",
    "live-executor",
    "phoenix-engine",
    "recorder",
    "rpc-gateway",
)
EXPECTED_PROTECTED = ("feed-ingestor", "recorder")
PREVIOUS_IMAGES = tuple(name for name in EXPECTED_IMAGES if name != "atlas-observer")


def source_ci(release_sha: str, run_id: str = SOURCE_CI_RUN) -> dict:
    run = {
        "id": int(run_id),
        "run_attempt": 1,
        "name": release_provenance.CI_WORKFLOW,
        "path": release_provenance.CI_WORKFLOW_PATH,
        "event": release_provenance.CI_EVENT,
        "head_branch": release_provenance.CI_BRANCH,
        "head_sha": release_sha,
        "status": "completed",
        "conclusion": "success",
        "repository": {"full_name": release_provenance.REPOSITORY},
    }
    jobs = {
        "jobs": [
            {"name": name, "status": "completed", "conclusion": "success"}
            for name in release_provenance.REQUIRED_CI_JOBS
        ]
    }
    return release_provenance.validate_source_ci_run(
        run, jobs, release_sha, run_id, "1"
    )


def workflow_job_names(source: str) -> tuple[str, ...]:
    jobs_source = source.split("\njobs:\n", 1)[1]
    return tuple(re.findall(r"^  ([a-z0-9-]+):\r?$", jobs_source, re.MULTILINE))


class ReleaseComponentRegistryTests(unittest.TestCase):
    def test_aave_image_build_contexts_include_canonical_fixtures(self) -> None:
        atlas_dockerfile = (ROOT / "deploy/atlas-observer.Dockerfile").read_text(
            encoding="utf-8"
        )
        atlas_ignore = (
            ROOT / "deploy/atlas-observer.Dockerfile.dockerignore"
        ).read_text(encoding="utf-8")
        self.assertIn(
            "COPY fixtures/replay/aave_profit_path_counterfactual_v1.json "
            "/src/fixtures/replay/aave_profit_path_counterfactual_v1.json",
            atlas_dockerfile,
        )
        self.assertIn(
            "!fixtures/replay/aave_profit_path_counterfactual_v1.json",
            atlas_ignore,
        )

        fork_dockerfile = (ROOT / "deploy/fork-sandbox.Dockerfile").read_text(
            encoding="utf-8"
        )
        fork_ignore = (
            ROOT / "deploy/fork-sandbox.Dockerfile.dockerignore"
        ).read_text(encoding="utf-8")
        self.assertIn("COPY fixtures/routes ./fixtures/routes", fork_dockerfile)
        self.assertIn("!fixtures/routes/**", fork_ignore)

    def test_registry_is_the_exact_eight_image_contract(self) -> None:
        self.assertEqual(release_components.RELEASE_IMAGES, EXPECTED_IMAGES)
        self.assertEqual(release_components.PROTECTED_IMAGES, EXPECTED_PROTECTED)
        self.assertEqual(len(release_components.COMPONENTS_BY_NAME), 8)
        self.assertTrue(all(item["release_included"] for item in release_components.COMPONENTS))
        live = release_components.COMPONENTS_BY_NAME["live-executor"]
        self.assertTrue(live["live_canary_only"])
        self.assertTrue(live["release_included"])
        self.assertTrue(live["production_compose"])
        self.assertNotIn(
            "live-executor",
            tuple(item["name"] for item in release_components.DEFAULT_PRODUCTION_COMPONENTS),
        )
        self.assertEqual(
            tuple(
                item["name"]
                for item in release_components.IMAGE_ENVIRONMENT_COMPONENTS
            ),
            (
                "feed-ingestor",
                "phoenix-engine",
                "rpc-gateway",
                "recorder",
                "dashboard",
                "live-executor",
                "atlas-observer",
            ),
        )
        self.assertEqual(
            tuple(item["name"] for item in release_components.OPTIONAL_LIVE_COMPONENTS),
            ("live-executor",),
        )
        for component in release_components.COMPONENTS:
            self.assertTrue((ROOT / component["dockerfile"]).is_file())
            self.assertEqual(
                component["repository"],
                f"ghcr.io/majidasgharitabrizi/{component['name']}",
            )

    def test_build_matrix_round_trips_every_registry_component(self) -> None:
        matrix = release_components.build_matrix()["include"]
        self.assertEqual(tuple(item["image"] for item in matrix), EXPECTED_IMAGES)
        for item in matrix:
            component = release_components.COMPONENTS_BY_NAME[item["image"]]
            self.assertEqual(item["repository"], component["repository"])
            self.assertEqual(item["context"], component["build_context"])
            self.assertEqual(item["dockerfile"], component["dockerfile"])
            self.assertEqual(item["protected"], component["protected"])
            self.assertTrue(item["build"])
            self.assertEqual(
                item["live_canary_only"], component["live_canary_only"]
            )

        selective = release_components.build_matrix(
            built_images={"dashboard", "rpc-gateway"}
        )["include"]
        self.assertEqual(
            tuple(item["image"] for item in selective if item["build"]),
            ("dashboard", "rpc-gateway"),
        )

    def test_generation_expansion_builds_only_the_missing_image(self) -> None:
        plan = {
            "schema": "phoenix.change-impact.v1",
            "built_images": [],
            "inherited_images": list(EXPECTED_IMAGES),
        }
        resolved = release_components.resolve_protected_build_plan(
            plan, PREVIOUS_IMAGES
        )
        self.assertEqual(resolved["built_images"], ["atlas-observer"])
        self.assertEqual(
            resolved["inherited_images"], list(PREVIOUS_IMAGES)
        )

    def test_current_generation_build_plan_is_unchanged(self) -> None:
        plan = {
            "schema": "phoenix.change-impact.v1",
            "built_images": ["dashboard"],
            "inherited_images": [
                name for name in EXPECTED_IMAGES if name != "dashboard"
            ],
        }
        resolved = release_components.resolve_protected_build_plan(
            plan, EXPECTED_IMAGES
        )
        self.assertEqual(resolved["built_images"], ["dashboard"])
        self.assertEqual(
            resolved["inherited_images"],
            [name for name in EXPECTED_IMAGES if name != "dashboard"],
        )

    def test_generation_contraction_drops_only_the_non_target_image(self) -> None:
        with mock.patch.dict(
            release_components.__dict__,
            {"RELEASE_IMAGES": PREVIOUS_IMAGES},
        ):
            resolved = release_components.resolve_protected_build_plan(
                {
                    "schema": "phoenix.change-impact.v1",
                    "built_images": ["atlas-observer", "rpc-gateway"],
                    "inherited_images": [
                        name
                        for name in EXPECTED_IMAGES
                        if name not in {"atlas-observer", "rpc-gateway"}
                    ],
                },
                PREVIOUS_IMAGES,
            )
        self.assertEqual(resolved["built_images"], ["rpc-gateway"])
        self.assertEqual(
            resolved["inherited_images"],
            [name for name in PREVIOUS_IMAGES if name != "rpc-gateway"],
        )

    def test_incomplete_or_overlapping_build_plan_fails_closed(self) -> None:
        for plan in (
            {
                "built_images": [],
                "inherited_images": [
                    name for name in PREVIOUS_IMAGES if name != "dashboard"
                ],
            },
            {
                "built_images": ["atlas-observer"],
                "inherited_images": list(EXPECTED_IMAGES),
            },
        ):
            with self.subTest(plan=plan), self.assertRaises(
                release_components.ReleaseComponentError
            ):
                release_components.resolve_protected_build_plan(
                    plan, PREVIOUS_IMAGES
                )

    def test_seven_nine_and_duplicate_component_registries_fail_closed(self) -> None:
        for mutation in ("seven", "nine", "duplicate"):
            changed = copy.deepcopy(release_components.REGISTRY)
            if mutation == "seven":
                changed["components"] = [
                    item for item in changed["components"] if item["name"] != "live-executor"
                ]
            elif mutation == "nine":
                extra = copy.deepcopy(changed["components"][-1])
                extra.update(
                    {
                        "name": "unexpected-image",
                        "repository": "ghcr.io/majidasgharitabrizi/unexpected-image",
                        "production_compose": False,
                        "production_order": None,
                        "production_services": [],
                        "image_environment": None,
                    }
                )
                changed["components"].append(extra)
            else:
                changed["components"][-1] = copy.deepcopy(changed["components"][0])
            with tempfile.TemporaryDirectory() as raw:
                path = Path(raw) / "release-components.json"
                path.write_text(json.dumps(changed), encoding="utf-8")
                with self.subTest(mutation=mutation), self.assertRaises(
                    release_components.ReleaseComponentError
                ):
                    release_components.load_registry(path)

    def test_workflows_use_registry_and_main_push_runs_all_required_jobs(self) -> None:
        build = (ROOT / ".github/workflows/build-images.yml").read_text(encoding="utf-8")
        controller = (
            ROOT / ".github/workflows/phoenix-release-controller.yml"
        ).read_text(encoding="utf-8")
        ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("python3 scripts/release_components.py build-matrix", build)
        self.assertIn("fromJSON(needs.preflight.outputs.build_matrix)", build)
        self.assertIn("PHOENIX_RELEASE_IMAGE_SET: current", build)
        self.assertIn("PHOENIX_RELEASE_IMAGE_SET: current", controller)
        self.assertNotIn("PHOENIX_RELEASE_IMAGE_SET: legacy", build)
        self.assertNotIn("PHOENIX_RELEASE_IMAGE_SET: legacy", controller)
        for name in EXPECTED_IMAGES:
            self.assertNotIn(name, build)
        self.assertIn("\n  push:\n    branches: [main]", ci)
        self.assertEqual(workflow_job_names(ci), release_components.REQUIRED_CI_JOBS)

        drifted = ci.replace("    name: rust-recorder", "    name: rust-recorder-drift", 1)
        named_jobs = tuple(
            re.findall(r"^    name: ([a-z0-9-]+)\r?$", drifted.split("\njobs:\n", 1)[1], re.MULTILINE)
        )
        self.assertNotEqual(named_jobs, release_components.REQUIRED_CI_JOBS)

    def test_buildkit_bootstrap_has_bounded_registry_retry(self) -> None:
        build = (ROOT / ".github/workflows/build-images.yml").read_text(encoding="utf-8")
        prefetch = "      - name: Prefetch BuildKit image with bounded retry"
        setup = "      - name: Setup Docker Buildx"

        self.assertIn(prefetch, build)
        self.assertIn("for attempt in 1 2 3; do", build)
        self.assertIn('timeout 120s docker pull "$buildkit_image"', build)
        self.assertIn('sleep "$((attempt * 5))"', build)
        self.assertLess(build.index(prefetch), build.index(setup))

    def test_current_and_legacy_manifest_environment_contracts(self) -> None:
        def write_manifest(path: Path, image_names: tuple[str, ...]) -> None:
            images = {}
            for index, name in enumerate(image_names, start=1):
                component = release_components.COMPONENTS_BY_NAME[name]
                images[name] = {
                    "repository": component["repository"],
                    "tag": f"sha-{RELEASE_SHA}",
                    "digest": f"sha256:{index:064x}",
                }
            path.write_text(
                json.dumps(
                    {
                        "schema": "phoenix.release.v1",
                        "release_sha": RELEASE_SHA,
                        "created_at": "2026-07-24T00:00:00Z",
                        "images": images,
                    }
                ),
                encoding="utf-8",
            )

        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            current_manifest = root / "current.json"
            current_env = root / "current.env"
            write_manifest(current_manifest, release_components.RELEASE_IMAGES)
            production_context.manifest_env(
                argparse.Namespace(
                    manifest=str(current_manifest),
                    expected_sha=RELEASE_SHA,
                    output=str(current_env),
                )
            )
            current_values = production_context.read_env(
                current_env, "RELEASE_ENV_MISSING"
            )
            expected_environment_names = {
                component["image_environment"]
                for component in release_components.IMAGE_ENVIRONMENT_COMPONENTS
            }
            self.assertEqual(
                set(current_values),
                expected_environment_names | {"PHOENIX_RELEASE_SHA"},
            )
            self.assertEqual(
                current_values["LIVE_EXECUTOR_IMAGE"],
                "ghcr.io/majidasgharitabrizi/live-executor@"
                f"{json.loads(current_manifest.read_text(encoding='utf-8'))['images']['live-executor']['digest']}",
            )

            shadow_only = dict(current_values)
            shadow_only.pop("LIVE_EXECUTOR_IMAGE")
            _, _, current_references = production_context.load_manifest(
                current_manifest
            )
            production_context.validate_release_env(
                shadow_only, RELEASE_SHA, current_references
            )

            legacy_manifest = root / "legacy.json"
            legacy_env = root / "legacy.env"
            write_manifest(legacy_manifest, release_components.LEGACY_RELEASE_IMAGES)
            production_context.manifest_env(
                argparse.Namespace(
                    manifest=str(legacy_manifest),
                    expected_sha=RELEASE_SHA,
                    output=str(legacy_env),
                )
            )
            legacy_values = production_context.read_env(
                legacy_env, "RELEASE_ENV_MISSING"
            )
            self.assertIn("LIVE_EXECUTOR_IMAGE", legacy_values)
            self.assertNotIn("ATLAS_OBSERVER_IMAGE", legacy_values)
            _, _, legacy_references = production_context.load_manifest(legacy_manifest)
            production_context.validate_release_env(
                legacy_values, RELEASE_SHA, legacy_references
            )
            legacy_values["ATLAS_OBSERVER_IMAGE"] = current_values[
                "ATLAS_OBSERVER_IMAGE"
            ]
            with self.assertRaisesRegex(
                production_context.ContextError, "RELEASE_IMAGE_MISMATCH"
            ):
                production_context.validate_release_env(
                    legacy_values, RELEASE_SHA, legacy_references
                )

    def test_runtime_and_maintenance_contracts_derive_from_registry(self) -> None:
        self.assertEqual(production_context.RELEASE_IMAGES, EXPECTED_IMAGES)
        self.assertEqual(production_context.PROTECTED_IMAGES, EXPECTED_PROTECTED)
        self.assertEqual(
            prelive_protected_maintenance.CURRENT_RELEASE_IMAGES, EXPECTED_IMAGES
        )
        self.assertEqual(
            prelive_protected_maintenance.LEGACY_RELEASE_IMAGES,
            release_components.LEGACY_RELEASE_IMAGES,
        )
        expected_rendered = {
            service: component["image_environment"]
            for component in release_components.DEFAULT_PRODUCTION_COMPONENTS
            for service in component["production_services"]
        }
        self.assertEqual(production_context.RENDERED_OWNED_IMAGES, expected_rendered)

    def test_expected_services_follow_manifest_generation(self) -> None:
        self.assertIn(
            "atlas-observer",
            production_context.expected_services_for_references(None, "SHADOW"),
        )
        legacy_references = {
            name: "ghcr.io/example/component@sha256:" + "1" * 64
            for name in release_components.LEGACY_RELEASE_IMAGES
        }
        legacy_services = production_context.expected_services_for_references(
            legacy_references, "SHADOW"
        )
        self.assertNotIn("atlas-observer", legacy_services)
        self.assertNotIn("live-executor", legacy_services)
        self.assertIn(
            "live-executor",
            production_context.expected_services_for_references(
                legacy_references, "DISARMED_EVIDENCE"
            ),
        )
        self.assertIn(
            "live-executor",
            production_context.expected_services_for_references(
                legacy_references, "LIVE"
            ),
        )

    def test_atlas_compose_binding_is_real_or_fail_closed(self) -> None:
        compose = (ROOT / "compose.prod.yml").read_text(encoding="utf-8")
        self.assertIn(
            "image: ${ATLAS_OBSERVER_IMAGE:-phoenix.invalid/atlas-observer:legacy-disabled}",
            compose,
        )
        self.assertNotIn("${ATLAS_OBSERVER_IMAGE:-${DASHBOARD_IMAGE", compose)

    def test_runtime_topology_covers_modes_and_generation_transitions(self) -> None:
        legacy = release_components.LEGACY_RELEASE_IMAGES
        current = release_components.CURRENT_RELEASE_IMAGES

        legacy_shadow = release_components.runtime_topology(legacy, "SHADOW")
        self.assertEqual(legacy_shadow["generation"], "legacy")
        self.assertNotIn("atlas-observer", legacy_shadow["running_services"])
        self.assertNotIn("atlas-observer", legacy_shadow["start_services"])
        self.assertIn("atlas-observer", legacy_shadow["intentional_absence"])
        self.assertIn("live-executor", legacy_shadow["intentional_absence"])
        self.assertNotIn("atlas-observer", legacy_shadow["pull_services"])
        self.assertIn("migration-runner", legacy_shadow["pull_services"])
        self.assertNotIn("autonomous-control", legacy_shadow["pull_services"])

        legacy_disarmed = release_components.runtime_topology(
            legacy, "DISARMED_EVIDENCE"
        )
        self.assertNotIn("atlas-observer", legacy_disarmed["pull_services"])
        self.assertIn("live-executor", legacy_disarmed["pull_services"])
        self.assertIn("economic-monitor", legacy_disarmed["pull_services"])
        self.assertIn("economic-supervisor", legacy_disarmed["pull_services"])
        self.assertIn("autonomous-control", legacy_disarmed["pull_services"])

        current_disarmed = release_components.runtime_topology(
            current, "DISARMED_EVIDENCE", source_image_names=legacy
        )
        self.assertEqual(current_disarmed["source_generation"], "legacy")
        self.assertIn("atlas-observer", current_disarmed["start_services"])
        self.assertIn("atlas-observer", current_disarmed["health_services"])
        self.assertIn("atlas-observer", current_disarmed["pull_services"])
        self.assertEqual(
            current_disarmed["service_contracts"]["atlas-observer"],
            {
                "image_binding": "atlas-observer",
                "lifecycle": "persistent",
                "protection": "ordinary",
                "health": "binary:/usr/local/bin/atlas-aave-hunter;http://127.0.0.1:9700/readyz",
                "cursor": "monotonic-auction-ledger;monotonic-aave-cursor",
                "expected_modes": ["SHADOW", "DISARMED_EVIDENCE", "LIVE"],
            },
        )
        self.assertEqual(
            current_disarmed["mutable_protected_services"],
            ["feed-ingestor", "recorder"],
        )
        self.assertEqual(
            current_disarmed["stop_services"],
            list(reversed(current_disarmed["start_services"])),
        )
        self.assertNotIn("live-executor", current_disarmed["running_services"])
        self.assertEqual(current_disarmed["remove_services"], [])

        current_live = release_components.runtime_topology(current, "LIVE")
        self.assertIn("live-executor", current_live["running_services"])
        self.assertIn("live-executor", current_live["inspect_services"])
        self.assertNotIn("live-executor", current_live["intentional_absence"])

        rollback = release_components.runtime_topology(
            legacy, "SHADOW", source_image_names=current
        )
        self.assertEqual(rollback["remove_services"], ["atlas-observer"])
        self.assertIn("atlas-observer", rollback["intentional_absence"])

        unchanged = release_components.runtime_topology(
            current, "SHADOW", source_image_names=current
        )
        self.assertEqual(unchanged["remove_services"], [])

    def test_telegram_ops_ships_but_is_not_a_preinstall_health_probe(self) -> None:
        # The rehearsal health contract probes the ACTIVE deployment, so a
        # service introduced by the candidate release must not gate its own
        # introduction. It must still ship, start, and be readiness-monitored.
        for mode in ("SHADOW", "DISARMED_EVIDENCE", "LIVE"):
            topology = release_components.runtime_topology(
                release_components.CURRENT_RELEASE_IMAGES, mode
            )
            if mode == "SHADOW":
                self.assertNotIn(
                    "phoenix-telegram-ops", topology["running_services"]
                )
                continue
            self.assertIn(
                "phoenix-telegram-ops", topology["running_services"], mode
            )
            self.assertIn(
                "phoenix-telegram-ops", topology["start_services"], mode
            )
            self.assertNotIn(
                "phoenix-telegram-ops", topology["health_services"], mode
            )
            self.assertNotIn(
                "phoenix-telegram-ops", topology["inspect_services"], mode
            )
            self.assertNotIn(
                "phoenix-telegram-ops", topology["health_contracts"], mode
            )
            self.assertEqual(
                topology["service_contracts"]["phoenix-telegram-ops"][
                    "image_binding"
                ],
                "dashboard",
            )

    def test_runtime_topology_rejects_partial_or_unknown_generations(self) -> None:
        for names in (
            {"dashboard"},
            set(release_components.CURRENT_RELEASE_IMAGES) | {"unknown-image"},
        ):
            with self.subTest(names=names), self.assertRaises(
                release_components.ReleaseComponentError
            ):
                release_components.runtime_topology(names, "SHADOW")

    def test_release_lifecycle_scripts_consume_topology_authority(self) -> None:
        validator = (ROOT / "scripts/validate-production-release-context.sh").read_text(
            encoding="utf-8"
        )
        deploy = (ROOT / "scripts/deploy-release.sh").read_text(encoding="utf-8")
        rollback = (ROOT / "scripts/rollback-release.sh").read_text(encoding="utf-8")
        health = (ROOT / "scripts/production-healthcheck.sh").read_text(
            encoding="utf-8"
        )
        for source in (validator, deploy, rollback):
            self.assertIn("release_components.py\" topology", source)
        self.assertIn('python3 "$release_components" topology', health)
        self.assertNotIn("optional_services='", deploy)
        self.assertNotIn("optional_services='", rollback)
        self.assertNotIn("services='nitro-feed-relay", validator)
        self.assertIn('--field remove_services', deploy)
        self.assertIn('--field remove_services', rollback)
        self.assertIn('rm -f "$service"', deploy)
        self.assertIn('rm -f "$service"', rollback)
        self.assertIn("/usr/local/bin/atlas-aave-hunter", health)

    def test_legacy_inherited_bridge_manifest_is_schema_valid(self) -> None:
        schema = json.loads(
            (ROOT / "schemas/phoenix-release-manifest.schema.json").read_text(
                encoding="utf-8"
            )
        )
        images = {}
        for index, name in enumerate(
            release_components.LEGACY_RELEASE_IMAGES, start=1
        ):
            images[name] = {
                "repository": release_components.COMPONENTS_BY_NAME[name][
                    "repository"
                ],
                "tag": f"sha-{RELEASE_SHA}",
                "digest": f"sha256:{index:064x}",
                "origin": "built",
                "source_sha": RELEASE_SHA,
                "source_build_run_id": RELEASE_RUN,
                "oci_revision": RELEASE_SHA,
            }
        bridge = {
            "schema": "phoenix.release.v2",
            "release_sha": RELEASE_SHA,
            "build_run_id": RELEASE_RUN,
            "created_at": "2026-08-01T00:00:00Z",
            "protected_base_sha": ROLLBACK_SHA,
            "protected_base_build_run_id": ROLLBACK_RUN,
            "images": images,
        }
        Draft202012Validator(schema).validate(bridge)

    def test_schemas_are_valid_and_component_sets_cannot_drift(self) -> None:
        registry_schema = json.loads(
            (ROOT / "schemas/release-components.schema.json").read_text(encoding="utf-8")
        )
        Draft202012Validator.check_schema(registry_schema)
        Draft202012Validator(registry_schema).validate(release_components.REGISTRY)
        for path in sorted((ROOT / "schemas").glob("*.json")):
            schema = json.loads(path.read_text(encoding="utf-8"))
            if schema.get("$schema") == "http://json-schema.org/draft-07/schema#":
                Draft7Validator.check_schema(schema)
            else:
                self.assertEqual(
                    schema.get("$schema"),
                    "https://json-schema.org/draft/2020-12/schema",
                    path.name,
                )
                Draft202012Validator.check_schema(schema)

        manifest = json.loads(
            (ROOT / "schemas/phoenix-release-manifest.schema.json").read_text(
                encoding="utf-8"
            )
        )
        for definition in ("currentImages", "inheritedImages"):
            self.assertEqual(
                tuple(sorted(manifest["$defs"][definition]["required"])),
                EXPECTED_IMAGES,
            )
            self.assertEqual(
                tuple(sorted(manifest["$defs"][definition]["properties"])),
                EXPECTED_IMAGES,
            )
        self.assertEqual(
            tuple(sorted(manifest["$defs"]["legacyImages"]["required"])),
            PREVIOUS_IMAGES,
        )
        self.assertEqual(
            tuple(sorted(manifest["$defs"]["legacyImages"]["properties"])),
            PREVIOUS_IMAGES,
        )
        provenance = json.loads(
            (ROOT / "schemas/phoenix-release-provenance.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            tuple(sorted(provenance["$defs"]["currentFragments"]["required"])),
            EXPECTED_IMAGES,
        )
        self.assertEqual(
            tuple(provenance["properties"]["built_images"]["items"]["enum"]),
            EXPECTED_IMAGES,
        )
        self.assertEqual(
            tuple(provenance["properties"]["inherited_images"]["items"]["enum"]),
            EXPECTED_IMAGES,
        )
        self.assertEqual(
            tuple(provenance["$defs"]["ciJobName"]["enum"]),
            release_components.REQUIRED_CI_JOBS,
        )
        self.assertTrue(provenance["$defs"]["sourceCi"]["properties"]["jobs"]["uniqueItems"])

    def test_codeowners_covers_sensitive_release_surfaces(self) -> None:
        codeowners = (ROOT / ".github/CODEOWNERS").read_text(encoding="utf-8")
        required = (
            "/.github/CODEOWNERS",
            "/.github/workflows/**",
            "/release-components.json",
            "/scripts/release_assets.py",
            "/scripts/release_provenance.py",
            "/scripts/production_context.py",
            "/schemas/phoenix-release-*.json",
            "/contracts/**",
            "/live-executor/**",
            "/scripts/install-production-release-context.sh",
            "/scripts/install-shadow-deploy-gateway.sh",
            "/scripts/phoenix-shadow-deploy-gateway.sh",
            "/scripts/phoenix_shadow_deploy.py",
        )
        for path in required:
            self.assertRegex(codeowners, rf"(?m)^{re.escape(path)}\s+@")
        docs = (ROOT / "docs/CI_CD.md").read_text(encoding="utf-8")
        self.assertIn("Require review from Code Owners", docs)
        self.assertRegex(docs, r"does\s+not enforce review by itself")

    def test_registry_and_loader_ship_in_release_and_gateway_contexts(self) -> None:
        self.assertIn("release-components.json", release_assets.STATIC_PATHS)
        self.assertIn("scripts/release_components.py", release_assets.STATIC_PATHS)
        self.assertIn("release-components.json", (ROOT / "scripts/install-shadow-deploy-gateway.sh").read_text(encoding="utf-8"))
        self.assertIn("release_components.py", (ROOT / "scripts/install-production-release-context.sh").read_text(encoding="utf-8"))


class ReleaseRoundTripTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def _write_assets(directory: Path, release_sha: str) -> None:
        directory.mkdir(parents=True)
        for name in (
            f"phoenix-release-assets-{release_sha}.tar.gz",
            "release-assets-manifest.json",
            "release-assets-checksums.txt",
        ):
            (directory / name).write_text(f"fixture:{name}\n", encoding="ascii")

    @staticmethod
    def _write_fragments(directory: Path, release_sha: str, run_id: str) -> None:
        directory.mkdir(parents=True)
        for index, matrix in enumerate(release_components.build_matrix()["include"], start=1):
            name = matrix["image"]
            fragment = {
                "schema": release_provenance.FRAGMENT_SCHEMA,
                "release_sha": release_sha,
                "build_run_id": run_id,
                "release_intent": release_provenance.RELEASE_INTENT,
                "name": name,
                "repository": matrix["repository"],
                "tag": f"sha-{release_sha}",
                "digest": f"sha256:{index:064x}",
            }
            (directory / f"{name}.json").write_text(
                json.dumps(fragment), encoding="utf-8"
            )

    def _full_release(self) -> tuple[dict, dict, Path, Path]:
        directory = self.root / "rollback"
        fragments = directory / "fragments"
        assets = directory / "assets"
        self._write_fragments(fragments, ROLLBACK_SHA, ROLLBACK_RUN)
        self._write_assets(assets, ROLLBACK_SHA)
        manifest_path = directory / "release-manifest.json"
        provenance_path = directory / "release-provenance.json"
        with mock.patch.object(release_provenance.release_assets, "verify_release_assets"):
            manifest, provenance = release_provenance.assemble_release(
                fragments,
                assets,
                ROLLBACK_SHA,
                ROLLBACK_RUN,
                release_provenance.RELEASE_INTENT,
                manifest_path,
                provenance_path,
                source_ci(ROLLBACK_SHA, "30000001004"),
                created_at="2026-07-22T00:00:00Z",
            )
        provenance["schema"] = release_provenance.LEGACY_PROVENANCE_SCHEMA
        provenance.pop("source_ci")
        provenance["required_release_artifacts"] = list(
            release_provenance._release_artifact_names(
                ROLLBACK_SHA, include_source_ci=False
            )
        )
        provenance_path.write_bytes(release_provenance._canonical_json(provenance))
        return manifest, provenance, manifest_path, provenance_path

    def _rendered_compose(
        self,
        release_values: dict[str, str],
        route_raw: str,
        mode: str = "SHADOW",
    ) -> dict:
        images: dict[str, str] = {}
        expected_services = production_context.expected_services_for_references(
            None, mode
        )
        for service in expected_services:
            env_name = production_context.RENDERED_OWNED_IMAGES.get(service)
            if service == "live-executor":
                env_name = "LIVE_EXECUTOR_IMAGE"
            if env_name is not None:
                images[service] = release_values[env_name]
            else:
                images[service] = production_context.EXTERNAL_IMAGES[service]
        services = {name: {"image": image, "environment": {}} for name, image in images.items()}
        common = {
            "PHOENIX_MODE": "SHADOW",
            "LIVE_EXECUTION": "false",
            "SIGNER_PRIVATE_KEY": "",
            "WALLET_ADDRESS": "",
            "EXECUTOR_ADDRESS": "",
        }
        services["phoenix-engine"]["environment"] = {
            **common,
            "CHAIN_ID": "42161",
            "ENGINE_ROUTE_REGISTRY_JSON": route_raw,
        }
        services["shadow-dispatcher"]["environment"] = dict(common)
        services["recorder"]["environment"] = {
            **common,
            "ENGINE_ROUTE_REGISTRY_JSON": route_raw,
            "ENGINE_ROUTER_ADDRESSES": "0x1111111111111111111111111111111111111111",
            "RECORDER_PERSISTENCE_POLICY": "money_path_v1",
        }
        services["rpc-gateway"]["environment"] = {
            "RPC_AUTHORITY_MODE": "single_primary",
            "RPC_AUTH_PROVIDER_HEADER_FILE": (
                "/run/secrets/phoenix-rpc-provider-slot-1-api-key"
            ),
            "RPC_AUTH_PROVIDER_HEADER_NAME": "api-key",
            "RPC_AUTH_PROVIDER_ID": "production-nownodes-arbitrum",
            "RPC_AUTH_PROVIDER_PRIORITY": "100",
            "RPC_AUTH_PROVIDER_URL": "https://arbitrum.nownodes.io/",
            "RPC_STATE_REQUESTS_PER_MINUTE": "12",
        }
        if mode != "SHADOW":
            executor_address = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            services["phoenix-engine"]["environment"].update(
                {
                    "PHOENIX_MODE": "LIVE",
                    "LIVE_EXECUTION": "true",
                    "AUTONOMOUS_EXECUTION": "true",
                    "EXECUTOR_ADDRESS": executor_address,
                }
            )
            services["live-executor"].update(
                {
                    "environment": {
                        "PHOENIX_MODE": "LIVE",
                        "LIVE_EXECUTION": "true",
                        "AUTONOMOUS_EXECUTION": "true",
                        "LIVE_EXECUTOR_ARMED": "true",
                        "LIVE_EXECUTOR_KILL_SWITCH": "false",
                        "LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME": "true",
                        "SIGNER_PRIVATE_KEY": "",
                        "SIGNER_PRIVATE_KEY_FILE": "/run/secrets/phoenix-live-executor-signer",
                        "WALLET_ADDRESS": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "EXECUTOR_ADDRESS": executor_address,
                        "LIVE_EXECUTOR_EXECUTOR_CODE_HASH": "0x" + "c" * 64,
                        "LIVE_EXECUTOR_EXPECTED_OWNER": "0x" + "d" * 40,
                        "LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER": "0x" + "e" * 40,
                        "PRODUCTION_RPC_URL": "https://arbitrum.nownodes.io/",
                        "LIVE_EXECUTOR_RPC_ALLOWLIST": "https://arbitrum.nownodes.io/",
                    },
                    "volumes": [
                        {
                            "type": "bind",
                            "source": "/run/secrets/phoenix-signer",
                            "target": "/run/secrets/phoenix-live-executor-signer",
                            "read_only": True,
                        }
                    ],
                    "read_only": True,
                    "user": "65532:65532",
                    "cap_drop": ["ALL"],
                    "security_opt": ["no-new-privileges:true"],
                    "restart": "unless-stopped",
                }
            )
        return {"services": services}

    def test_registry_to_inherited_deploy_and_compose_round_trip(self) -> None:
        rollback, rollback_provenance, rollback_manifest_path, rollback_provenance_path = self._full_release()
        candidate = self.root / "candidate"
        fragments = candidate / "fragments"
        assets = candidate / "assets"
        self._write_fragments(fragments, RELEASE_SHA, RELEASE_RUN)
        self._write_assets(assets, RELEASE_SHA)
        release_provenance.write_inherited_fragments(
            fragments,
            RELEASE_SHA,
            RELEASE_RUN,
            release_provenance.RELEASE_INTENT,
            ROLLBACK_SHA,
            ROLLBACK_RUN,
            rollback_manifest_path,
            rollback_provenance_path,
        )
        manifest_path = candidate / "release-manifest.json"
        provenance_path = candidate / "release-provenance.json"
        with mock.patch.object(release_provenance.release_assets, "verify_release_assets"):
            manifest, provenance = release_provenance.assemble_release(
                fragments,
                assets,
                RELEASE_SHA,
                RELEASE_RUN,
                release_provenance.RELEASE_INTENT,
                manifest_path,
                provenance_path,
                source_ci(RELEASE_SHA),
                created_at="2026-07-22T00:00:00Z",
                protected_base_sha=ROLLBACK_SHA,
                protected_base_build_run_id=ROLLBACK_RUN,
                protected_base_manifest=rollback_manifest_path,
                protected_base_provenance=rollback_provenance_path,
            )

        manifest_schema = json.loads(
            (ROOT / "schemas/phoenix-release-manifest.schema.json").read_text(encoding="utf-8")
        )
        provenance_schema = json.loads(
            (ROOT / "schemas/phoenix-release-provenance.schema.json").read_text(encoding="utf-8")
        )
        Draft202012Validator(manifest_schema).validate(manifest)
        Draft202012Validator(provenance_schema).validate(provenance)
        Draft202012Validator(manifest_schema).validate(rollback)
        Draft202012Validator(provenance_schema).validate(rollback_provenance)
        release_provenance.validate_deploy_pair(
            manifest_path,
            provenance_path,
            RELEASE_SHA,
            RELEASE_RUN,
            rollback_manifest_path,
            rollback_provenance_path,
            ROLLBACK_SHA,
            ROLLBACK_RUN,
        )

        self.assertEqual(tuple(sorted(manifest["images"])), EXPECTED_IMAGES)
        for name in EXPECTED_PROTECTED:
            self.assertEqual(manifest["images"][name]["origin"], "inherited")
            self.assertEqual(
                release_provenance._normalized_image_identity(manifest, name, RELEASE_RUN),
                release_provenance._normalized_image_identity(rollback, name, ROLLBACK_RUN),
            )
        for name in release_components.BUILT_IMAGES:
            image = manifest["images"][name]
            self.assertEqual(image["origin"], "built")
            self.assertEqual(image["source_sha"], RELEASE_SHA)
            self.assertEqual(image["source_build_run_id"], RELEASE_RUN)
            self.assertEqual(image["oci_revision"], RELEASE_SHA)
        self.assertIn("live-executor", manifest["images"])
        self.assertNotIn("live-executor", production_context.EXPECTED_SERVICES)

        release_env = candidate / "release.env"
        production_context.manifest_env(
            argparse.Namespace(
                manifest=str(manifest_path),
                expected_sha=RELEASE_SHA,
                route_registry=str(
                    ROOT / "fixtures/routes/weth_usdc_uniswap_v3.json"
                ),
                output=str(release_env),
            )
        )
        release_values = production_context.read_env(release_env, "RELEASE_ENV_MISSING")
        self.assertEqual(
            set(release_values),
            {
                component["image_environment"]
                for component in release_components.IMAGE_ENVIRONMENT_COMPONENTS
            }
            | {"ENGINE_ROUTE_REGISTRY_JSON", "PHOENIX_RELEASE_SHA"},
        )
        self.assertEqual(
            release_values["LIVE_EXECUTOR_IMAGE"],
            f"{manifest['images']['live-executor']['repository']}@"
            f"{manifest['images']['live-executor']['digest']}",
        )
        for env_name, reference in release_values.items():
            if env_name.endswith("_IMAGE"):
                self.assertRegex(
                    reference,
                    r"^ghcr\.io/.+@sha256:[0-9a-f]{64}$",
                )

        route_raw = json.dumps(
            json.loads(
                (
                    ROOT
                    / "fixtures/routes/weth_usdc_uniswap_v3.json"
                ).read_text(encoding="utf-8")
            ),
            separators=(",", ":"),
            sort_keys=True,
        )
        self.assertEqual(release_values["ENGINE_ROUTE_REGISTRY_JSON"], route_raw)
        operator_route_raw = json.dumps(
            json.loads(route_raw)[:1],
            separators=(",", ":"),
            sort_keys=True,
        )
        operator_env = candidate / "operator.env"
        operator_env.write_text(
            "\n".join(
                (
                    "PHOENIX_MODE=SHADOW",
                    "LIVE_EXECUTION=false",
                    "AUTONOMOUS_EXECUTION=false",
                    "CHAIN_ID=42161",
                    "SIGNER_PRIVATE_KEY=",
                    "WALLET_ADDRESS=",
                    "EXECUTOR_ADDRESS=",
                    "LIVE_EXECUTOR_WALLET_ADDRESS=0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "LIVE_EXECUTOR_EXECUTOR_ADDRESS=0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    f"LIVE_EXECUTOR_EXECUTOR_CODE_HASH=0x{'c' * 64}",
                    f"LIVE_EXECUTOR_EXPECTED_OWNER=0x{'d' * 40}",
                    f"LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER=0x{'e' * 40}",
                    "LIVE_EXECUTOR_SIGNER_FILE=/run/secrets/phoenix-signer",
                    "PRODUCTION_RPC_URL=https://primary.invalid",
                    "SECONDARY_RPC_URL=https://secondary.invalid",
                    "ENGINE_ROUTER_ADDRESSES=0x1111111111111111111111111111111111111111",
                    "RECORDER_PERSISTENCE_POLICY=money_path_v1",
                    f"ENGINE_ROUTE_REGISTRY_JSON={operator_route_raw}",
                )
            )
            + "\n",
            encoding="utf-8",
        )
        compose_path = candidate / "compose.json"
        compose_path.write_text(
            json.dumps(self._rendered_compose(release_values, route_raw)),
            encoding="utf-8",
        )
        metadata = candidate / "render.json"
        production_context.validate_render(
            argparse.Namespace(
                compose_config=str(compose_path),
                env_file=str(operator_env),
                release_env=str(release_env),
                manifest=str(manifest_path),
                expected_mode=None,
                metadata_output=str(metadata),
            )
        )
        rendered = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(rendered["status"], "ok")
        self.assertEqual(rendered["release_sha"], RELEASE_SHA)

        compose_path.write_text(
            json.dumps(
                self._rendered_compose(
                    release_values, route_raw, "DISARMED_EVIDENCE"
                )
            ),
            encoding="utf-8",
        )
        disarmed_args = argparse.Namespace(
            compose_config=str(compose_path),
            env_file=str(operator_env),
            release_env=str(release_env),
            manifest=str(manifest_path),
            expected_mode="DISARMED_EVIDENCE",
            metadata_output=str(metadata),
        )
        production_context.validate_render(disarmed_args)
        rendered = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(rendered["mode"], "DISARMED_EVIDENCE")
        self.assertFalse(rendered["live_execution"])
        self.assertFalse(rendered["autonomous_execution"])
        self.assertIn("live-executor", rendered["expected_services"])

        live_operator = candidate / "operator-live.env"
        live_operator.write_text(
            operator_env.read_text(encoding="utf-8")
            .replace("PHOENIX_MODE=SHADOW", "PHOENIX_MODE=LIVE")
            .replace("LIVE_EXECUTION=false", "LIVE_EXECUTION=true")
            .replace("AUTONOMOUS_EXECUTION=false", "AUTONOMOUS_EXECUTION=true"),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            production_context.ContextError,
            "DISARMED_EVIDENCE_OPERATOR_MODE_INVALID",
        ):
            production_context.validate_render(
                argparse.Namespace(**{**vars(disarmed_args), "env_file": str(live_operator)})
            )
        with self.assertRaisesRegex(
            production_context.ContextError, "AUTONOMOUS_LIVE_MODE_REQUIRED"
        ):
            production_context.validate_render(
                argparse.Namespace(**{**vars(disarmed_args), "expected_mode": "LIVE"})
            )
        production_context.validate_render(
            argparse.Namespace(
                **{
                    **vars(disarmed_args),
                    "env_file": str(live_operator),
                    "expected_mode": "LIVE",
                }
            )
        )
        rendered = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(rendered["mode"], "LIVE")
        self.assertTrue(rendered["live_execution"])
        self.assertTrue(rendered["autonomous_execution"])

        rendered_live = self._rendered_compose(
            release_values, route_raw, "DISARMED_EVIDENCE"
        )
        rendered_live["services"]["live-executor"]["environment"][
            "SECONDARY_RPC_URL"
        ] = "https://legacy-secondary.invalid/"
        compose_path.write_text(json.dumps(rendered_live), encoding="utf-8")
        with self.assertRaisesRegex(
            production_context.ContextError,
            "AUTONOMOUS_EXECUTOR_IDENTITY_MISMATCH",
        ):
            production_context.validate_render(
                argparse.Namespace(
                    **{
                        **vars(disarmed_args),
                        "env_file": str(live_operator),
                        "expected_mode": "LIVE",
                    }
                )
            )

        rendered_live["services"]["live-executor"]["environment"].pop(
            "SECONDARY_RPC_URL"
        )
        rendered_live["services"]["live-executor"]["environment"][
            "LIVE_EXECUTOR_RPC_ALLOWLIST"
        ] = "https://legacy-secondary.invalid/"
        compose_path.write_text(json.dumps(rendered_live), encoding="utf-8")
        with self.assertRaisesRegex(
            production_context.ContextError,
            "AUTONOMOUS_EXECUTOR_IDENTITY_MISMATCH",
        ):
            production_context.validate_render(
                argparse.Namespace(
                    **{
                        **vars(disarmed_args),
                        "env_file": str(live_operator),
                        "expected_mode": "LIVE",
                    }
                )
            )

        for name, invalid in (
            ("RPC_AUTHORITY_MODE", "dual_provider"),
            ("RPC_AUTH_PROVIDER_ID", "production-slot-0"),
            ("RPC_AUTH_PROVIDER_URL", "https://legacy-primary.invalid/"),
            ("RPC_AUTH_PROVIDER_PRIORITY", "99"),
            ("RPC_AUTH_PROVIDER_HEADER_NAME", "authorization"),
            ("RPC_AUTH_PROVIDER_HEADER_FILE", "/tmp/unsafe"),
            ("RPC_PROVIDER_URLS", "https://legacy-primary.invalid/"),
            ("RPC_PROVIDER_WEIGHTS", "1"),
            ("RPC_PROVIDER_IDS", "production-slot-0"),
        ):
            invalid_render = self._rendered_compose(release_values, route_raw)
            invalid_render["services"]["rpc-gateway"]["environment"][name] = invalid
            compose_path.write_text(json.dumps(invalid_render), encoding="utf-8")
            with self.subTest(name=name), self.assertRaisesRegex(
                production_context.ContextError,
                "RPC_AUTHORITY_RENDER_INVALID",
            ):
                production_context.validate_render(
                    argparse.Namespace(
                        compose_config=str(compose_path),
                        env_file=str(operator_env),
                        release_env=str(release_env),
                        manifest=str(manifest_path),
                        expected_mode=None,
                        metadata_output=str(metadata),
                    )
                )

    def test_release_only_manifest_inherits_every_schema_valid_image(self) -> None:
        rollback, _, rollback_manifest_path, rollback_provenance_path = (
            self._full_release()
        )
        candidate = self.root / "release-only"
        fragments = candidate / "fragments"
        assets = candidate / "assets"
        self._write_fragments(fragments, RELEASE_SHA, RELEASE_RUN)
        self._write_assets(assets, RELEASE_SHA)
        release_provenance.write_inherited_fragments(
            fragments,
            RELEASE_SHA,
            RELEASE_RUN,
            release_provenance.RELEASE_INTENT,
            ROLLBACK_SHA,
            ROLLBACK_RUN,
            rollback_manifest_path,
            rollback_provenance_path,
            list(release_components.RELEASE_IMAGES),
        )
        manifest_path = candidate / "release-manifest.json"
        provenance_path = candidate / "release-provenance.json"
        with mock.patch.object(
            release_provenance.release_assets, "verify_release_assets"
        ):
            manifest, provenance = release_provenance.assemble_release(
                fragments,
                assets,
                RELEASE_SHA,
                RELEASE_RUN,
                release_provenance.RELEASE_INTENT,
                manifest_path,
                provenance_path,
                source_ci(RELEASE_SHA),
                created_at="2026-07-22T00:00:00Z",
                protected_base_sha=ROLLBACK_SHA,
                protected_base_build_run_id=ROLLBACK_RUN,
                protected_base_manifest=rollback_manifest_path,
                protected_base_provenance=rollback_provenance_path,
                built_images=[],
            )

        manifest_schema = json.loads(
            (
                ROOT / "schemas/phoenix-release-manifest.schema.json"
            ).read_text(encoding="utf-8")
        )
        provenance_schema = json.loads(
            (
                ROOT / "schemas/phoenix-release-provenance.schema.json"
            ).read_text(encoding="utf-8")
        )
        Draft202012Validator(manifest_schema).validate(manifest)
        Draft202012Validator(provenance_schema).validate(provenance)
        self.assertEqual(provenance["built_images"], [])
        self.assertEqual(
            provenance["inherited_images"],
            list(release_components.RELEASE_IMAGES),
        )
        for name in release_components.RELEASE_IMAGES:
            self.assertEqual(manifest["images"][name]["origin"], "inherited")
            self.assertEqual(
                manifest["images"][name]["digest"],
                rollback["images"][name]["digest"],
            )


if __name__ == "__main__":
    unittest.main()
