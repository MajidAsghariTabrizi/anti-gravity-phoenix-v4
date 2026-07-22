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
    "dashboard",
    "feed-ingestor",
    "fork-sandbox",
    "live-executor",
    "phoenix-engine",
    "recorder",
    "rpc-gateway",
)
EXPECTED_PROTECTED = ("feed-ingestor", "recorder")


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
    def test_registry_is_the_exact_seven_image_contract(self) -> None:
        self.assertEqual(release_components.RELEASE_IMAGES, EXPECTED_IMAGES)
        self.assertEqual(release_components.PROTECTED_IMAGES, EXPECTED_PROTECTED)
        self.assertEqual(len(release_components.COMPONENTS_BY_NAME), 7)
        self.assertTrue(all(item["release_included"] for item in release_components.COMPONENTS))
        live = release_components.COMPONENTS_BY_NAME["live-executor"]
        self.assertTrue(live["live_canary_only"])
        self.assertTrue(live["release_included"])
        self.assertTrue(live["production_compose"])
        self.assertNotIn(
            "live-executor",
            tuple(item["name"] for item in release_components.DEFAULT_PRODUCTION_COMPONENTS),
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

    def test_six_eight_and_duplicate_component_registries_fail_closed(self) -> None:
        for mutation in ("six", "eight", "duplicate"):
            changed = copy.deepcopy(release_components.REGISTRY)
            if mutation == "six":
                changed["components"] = [
                    item for item in changed["components"] if item["name"] != "live-executor"
                ]
            elif mutation == "eight":
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
        ci = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        self.assertIn("python3 scripts/release_components.py build-matrix", build)
        self.assertIn("fromJSON(needs.preflight.outputs.build_matrix)", build)
        for name in EXPECTED_IMAGES:
            self.assertNotIn(name, build)
        self.assertIn("\n  push:\n    branches: [main]", ci)
        self.assertEqual(workflow_job_names(ci), release_components.REQUIRED_CI_JOBS)

        drifted = ci.replace("    name: rust-recorder", "    name: rust-recorder-drift", 1)
        named_jobs = tuple(
            re.findall(r"^    name: ([a-z0-9-]+)\r?$", drifted.split("\njobs:\n", 1)[1], re.MULTILINE)
        )
        self.assertNotEqual(named_jobs, release_components.REQUIRED_CI_JOBS)

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
        for definition in ("legacyImages", "inheritedImages"):
            self.assertEqual(
                tuple(sorted(manifest["$defs"][definition]["required"])),
                EXPECTED_IMAGES,
            )
            self.assertEqual(
                tuple(sorted(manifest["$defs"][definition]["properties"])),
                EXPECTED_IMAGES,
            )
        provenance = json.loads(
            (ROOT / "schemas/phoenix-release-provenance.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            tuple(sorted(provenance["properties"]["image_fragments"]["required"])),
            EXPECTED_IMAGES,
        )
        self.assertEqual(
            tuple(provenance["properties"]["built_images"]["items"]["enum"]),
            release_components.BUILT_IMAGES,
        )
        self.assertEqual(
            tuple(provenance["properties"]["inherited_images"]["items"]["enum"]),
            EXPECTED_PROTECTED,
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

    def _rendered_compose(self, release_values: dict[str, str], route_raw: str) -> dict:
        images: dict[str, str] = {}
        for service in production_context.EXPECTED_SERVICES:
            env_name = production_context.RENDERED_OWNED_IMAGES.get(service)
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
            "RPC_STATE_REQUESTS_PER_MINUTE": "12"
        }
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
                output=str(release_env),
            )
        )
        release_values = production_context.read_env(release_env, "RELEASE_ENV_MISSING")
        for env_name, reference in release_values.items():
            if env_name.endswith("_IMAGE"):
                self.assertRegex(reference, r"^ghcr\.io/.+@sha256:[0-9a-f]{64}$")

        route_raw = json.dumps(
            json.loads((ROOT / "fixtures/routes/weth_usdc_uniswap_v3.json").read_text(encoding="utf-8")),
            separators=(",", ":"),
        )
        operator_env = candidate / "operator.env"
        operator_env.write_text(
            "\n".join(
                (
                    "PHOENIX_MODE=SHADOW",
                    "LIVE_EXECUTION=false",
                    "CHAIN_ID=42161",
                    "SIGNER_PRIVATE_KEY=",
                    "WALLET_ADDRESS=",
                    "EXECUTOR_ADDRESS=",
                    "ENGINE_ROUTER_ADDRESSES=0x1111111111111111111111111111111111111111",
                    "RECORDER_PERSISTENCE_POLICY=money_path_v1",
                    f"ENGINE_ROUTE_REGISTRY_JSON={route_raw}",
                )
            )
            + "\n",
            encoding="utf-8",
        )
        compose_path = candidate / "compose.json"
        compose_path.write_text(
            json.dumps(self._rendered_compose(release_values, route_raw)), encoding="utf-8"
        )
        metadata = candidate / "render.json"
        production_context.validate_render(
            argparse.Namespace(
                compose_config=str(compose_path),
                env_file=str(operator_env),
                release_env=str(release_env),
                manifest=str(manifest_path),
                metadata_output=str(metadata),
            )
        )
        rendered = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(rendered["status"], "ok")
        self.assertEqual(rendered["release_sha"], RELEASE_SHA)


if __name__ == "__main__":
    unittest.main()
