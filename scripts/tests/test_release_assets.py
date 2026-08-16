import hashlib
import json
import os
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import release_assets


RELEASE_SHA = "1" * 40
LIVE_CANARY_ASSETS = (
    "compose.live-canary.yml",
    "docs/AUTOMATED_ECONOMIC_CONTROL.md",
    "live-executor/schema/001_live_canary.sql",
    "live-executor/schema/002_approval_evidence.sql",
    "live-executor/schema/003_autonomous_hunter_contracts.sql",
    "live-executor/schema/004_autonomous_live_runtime.sql",
    "live-executor/schema/005_closed_loop_economic_control.sql",
    "live-executor/schema/006_atlas_aave_revenue_lanes.sql",
    "live-executor/schema/007_aave_economic_diagnostics.sql",
    "live-executor/schema/008_revenue_provider_authority.sql",
    "live-executor/schema/009_single_primary_provider_authority.sql",
    "compose.live-autonomous.yml",
    "deploy/phoenix-economic-activation.path",
    "deploy/phoenix-economic-activation.service",
    "scripts/activate-economic-canary.sh",
    "scripts/economic_activation_runner.py",
    "scripts/economic-dashboard-loop.sh",
    "scripts/sql/economic-dashboard-snapshot.sql",
)


class ReleaseAssetsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.repo_root = Path(__file__).resolve().parents[2]
        cls.contract_artifact = (
            cls.repo_root / "fork-sandbox" / "abi" / "PhoenixExecutor.json"
        )

    def build(self, output: Path):
        artifact = output / "PhoenixExecutor.json"
        artifact.parent.mkdir(parents=True, exist_ok=True)
        artifact.write_text(
            json.dumps(
                {
                    "abi": json.loads(self.contract_artifact.read_text(encoding="utf-8")),
                    "bytecode": {"object": "0x6000600055"},
                    "deployedBytecode": {
                        "object": "0x" + "00" * (7 * 32),
                        "immutableReferences": {
                            "atlas": [
                                {"start": 0, "length": 32},
                                {"start": 32, "length": 32},
                                {"start": 64, "length": 32},
                                {"start": 96, "length": 32},
                            ],
                            "weth": [
                                {"start": 128, "length": 32},
                                {"start": 160, "length": 32},
                                {"start": 192, "length": 32},
                            ],
                        },
                    },
                }
            ),
            encoding="utf-8",
        )
        return release_assets.build_release_assets(
            self.repo_root,
            RELEASE_SHA,
            output,
            artifact,
            require_rotation_assets=True,
        )

    def test_bundle_is_deterministic_and_verifies(self) -> None:
        with (
            tempfile.TemporaryDirectory() as first_raw,
            tempfile.TemporaryDirectory() as second_raw,
        ):
            first = self.build(Path(first_raw))
            second = self.build(Path(second_raw))
            self.assertEqual(first[0].read_bytes(), second[0].read_bytes())
            self.assertEqual(first[1].read_bytes(), second[1].read_bytes())
            self.assertEqual(first[2].read_bytes(), second[2].read_bytes())
            release_assets.verify_release_assets(*first, RELEASE_SHA)

    def test_manifest_is_strict_bounded_and_contains_required_assets(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            archive, manifest_path, checksums = self.build(Path(raw))
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            self.assertEqual(set(manifest), {"schema", "release_sha", "files"})
            self.assertEqual(manifest["schema"], release_assets.SCHEMA)
            self.assertEqual(manifest["release_sha"], RELEASE_SHA)
            paths = [item["path"] for item in manifest["files"]]
            self.assertEqual(paths, sorted(paths))
            self.assertEqual(len(paths), len(set(paths)))
            self.assertIn("compose.prod.yml", paths)
            self.assertIn("release-components.json", paths)
            for required in LIVE_CANARY_ASSETS:
                self.assertIn(required, paths)
            self.assertIn("contracts/PhoenixExecutor.compiled.json", paths)
            for rotation_asset in (
                "contracts/PhoenixExecutor.creation.bin",
                "contracts/PhoenixExecutor.creation.sha256",
                "contracts/PhoenixExecutor.runtime.bin",
                "contracts/PhoenixExecutor.runtime.sha256",
                "config/phoenix-executor-rotation-plan.json",
                "config/phoenix-executor-rotation-artifacts.json",
                "scripts/phoenix_executor_rotation_context.py",
                "scripts/rotate-phoenix-executor-live.sh",
            ):
                self.assertIn(rotation_asset, paths)
            payloads = release_assets._rotation_payloads(
                Path(raw) / "PhoenixExecutor.json", RELEASE_SHA, RELEASE_SHA, True
            )
            runtime = payloads[release_assets.ROTATION_RUNTIME_TARGET]
            self.assertEqual(len(runtime), 7 * 32)
            self.assertIn(
                bytes.fromhex("8ad1ae9d97c79aa68a0a151e83ff3942f68f86c1"),
                runtime,
            )
            self.assertIn(
                bytes.fromhex("82af49447d8a07e3bd95bd0d56f35241523fbab1"),
                runtime,
            )

            self.assertIn("schemas/phoenix-release-assets.schema.json", paths)
            self.assertIn("scripts/prelive-shadow-control.sh", paths)
            self.assertIn("scripts/prelive-protected-maintenance.sh", paths)
            self.assertIn("scripts/prelive_protected_maintenance.py", paths)
            self.assertIn("scripts/provision-production-host.sh", paths)
            self.assertIn("scripts/install-production-release-context.sh", paths)
            self.assertIn("scripts/install-autonomous-live-deploy-gateway.sh", paths)
            self.assertIn("scripts/phoenix-autonomous-live-deploy-gateway.sh", paths)
            self.assertIn("scripts/install-phoenix-release-platform.sh", paths)
            self.assertIn("scripts/phoenix-release-gateway.sh", paths)
            self.assertIn("scripts/phoenix-release-transport.sh", paths)
            self.assertIn("scripts/phoenix_release/model.py", paths)
            self.assertIn("scripts/phoenix_release/gateway.py", paths)
            self.assertIn("scripts/production_mode.py", paths)
            self.assertIn("docs/AUTONOMOUS_LIVE_OPERATIONS.md", paths)
            self.assertIn("scripts/prelive-protected-maintenance-launch.sh", paths)
            self.assertIn("scripts/prelive-protected-maintenance-unit.sh", paths)
            self.assertIn("scripts/prelive-v5-fresh-database-gate.sh", paths)
            self.assertIn("scripts/prelive_v5_release.py", paths)
            self.assertIn("scripts/release_provenance.py", paths)
            self.assertIn("scripts/change_impact.py", paths)
            self.assertIn("scripts/release_components.py", paths)
            self.assertIn("scripts/required-service-absence.sh", paths)
            self.assertIn("deploy/prelive-v5-release.example.json", paths)
            self.assertIn("schemas/phoenix-release-manifest.schema.json", paths)
            self.assertIn("schemas/phoenix-release-provenance.schema.json", paths)
            self.assertIn("schemas/release-components.schema.json", paths)
            self.assertIn("schemas/phoenix-prelive-v5-release.schema.json", paths)
            for hunter_asset in (
                "config/phoenix-route-universe-v1.json",
                "config/phoenix-route-policy-3000-500-v1.json",
                "config/phoenix-route-policy-v1.json",
                "fixtures/routes/arbitrum_uniswap_v3_weth_usdc_discovery_v1.json",
                "fixtures/routes/weth_usdc_uniswap_v3_forward_v1.json",
                "fixtures/routes/weth_usdc_uniswap_v3.json",
                "docs/AUTONOMOUS_HUNTER_CONTRACTS_V1.md",
                "docs/AUTONOMOUS_HUNTER_A1_REVENUE_EVIDENCE.md",
                "fixtures/autonomous-hunter/v1/fixture-manifest.json",
                "fixtures/autonomous-hunter/v1/valid/autonomous-candidate.json",
                "fixtures/autonomous-hunter/v1/invalid/automatic-approval-mutated-plan.json",
                "schemas/phoenix-autonomous-hunter-v1.schema.json",
                "scripts/hunter_contracts.py",
                "fixtures/hunter-a1/v1/autonomous-candidate.json",
                "fixtures/hunter-a1/v1/revenue-replay-evidence.json",
                "phoenix-engine/examples/hunter_a1_replay.rs",
            ):
                self.assertIn(hunter_asset, paths)
            self.assertTrue(
                all(
                    item["size_bytes"] <= release_assets.MAX_FILE_BYTES
                    for item in manifest["files"]
                )
            )
            self.assertTrue(
                all(
                    item["sha256"].startswith("sha256:")
                    for item in manifest["files"]
                )
            )
            release_assets.verify_release_assets(archive, manifest_path, checksums, RELEASE_SHA)

    def test_protected_build_rejects_an_abi_only_rotation_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            with self.assertRaisesRegex(
                release_assets.ReleaseAssetError,
                "PhoenixExecutor artifact is invalid",
            ):
                release_assets.build_release_assets(
                    self.repo_root,
                    RELEASE_SHA,
                    Path(raw),
                    self.contract_artifact,
                    require_rotation_assets=True,
                )

    def test_live_canary_assets_have_exact_bytes_hashes_modes_and_archive_paths(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            archive, manifest_path, _ = self.build(Path(raw))
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            entries = {item["path"]: item for item in manifest["files"]}
            root_name = f"phoenix-release-{RELEASE_SHA}"
            with tarfile.open(archive, mode="r:gz") as bundle:
                members = {member.name: member for member in bundle.getmembers()}
                for relative in LIVE_CANARY_ASSETS:
                    source_bytes = (self.repo_root / relative).read_bytes()
                    entry = entries[relative]
                    expected_mode = (
                        "0755"
                        if relative.startswith("scripts/")
                        and relative.endswith((".sh", ".py"))
                        else "0644"
                    )
                    self.assertEqual(entry["mode"], expected_mode)
                    self.assertEqual(entry["size_bytes"], len(source_bytes))
                    self.assertEqual(entry["sha256"], release_assets._sha256(source_bytes))
                    archive_path = f"{root_name}/{relative}"
                    member = members[archive_path]
                    self.assertTrue(member.isfile())
                    self.assertEqual(member.mode, int(expected_mode, 8))
                    extracted = bundle.extractfile(member)
                    self.assertIsNotNone(extracted)
                    self.assertEqual(extracted.read(), source_bytes)

    def test_missing_live_canary_asset_fails_closed(self) -> None:
        replaced = tuple(
            "missing/compose.live-canary.yml" if path == LIVE_CANARY_ASSETS[0] else path
            for path in release_assets.STATIC_PATHS
        )
        with tempfile.TemporaryDirectory() as raw, mock.patch.object(
            release_assets, "STATIC_PATHS", replaced
        ):
            with self.assertRaisesRegex(
                release_assets.ReleaseAssetError, "missing or not a regular file"
            ):
                self.build(Path(raw))

    @unittest.skipUnless(os.name == "posix", "POSIX symlink fixture")
    def test_symlinked_live_canary_asset_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "outside.sql"
            source.write_text("SELECT 1;\n", encoding="ascii")
            target = root / LIVE_CANARY_ASSETS[1]
            target.parent.mkdir(parents=True)
            target.symlink_to(source)
            contract = root / "contract.json"
            contract.write_text("{}\n", encoding="ascii")
            with (
                mock.patch.object(release_assets, "STATIC_PATHS", (LIVE_CANARY_ASSETS[1],)),
                mock.patch.object(release_assets, "GLOB_PATHS", ()),
                self.assertRaisesRegex(
                    release_assets.ReleaseAssetError, "missing or not a regular file"
                ),
            ):
                release_assets.build_release_assets(
                    root, RELEASE_SHA, root / "output", contract
                )

    def test_extracted_tree_is_exact_and_integrity_checked(self) -> None:
        with tempfile.TemporaryDirectory() as raw, tempfile.TemporaryDirectory() as tree_raw:
            archive, manifest, _ = self.build(Path(raw))
            with tarfile.open(archive, mode="r:gz") as bundle:
                bundle.extractall(tree_raw, filter="data")
            root = Path(tree_raw) / f"phoenix-release-{RELEASE_SHA}"
            release_assets.verify_release_tree(root, manifest, RELEASE_SHA)
            (root / "unexpected.txt").write_text("unexpected", encoding="ascii")
            with self.assertRaisesRegex(release_assets.ReleaseAssetError, "member set"):
                release_assets.verify_release_tree(root, manifest, RELEASE_SHA)

    def test_generated_python_bytecode_is_rejected_explicitly(self) -> None:
        for candidate in (
            "scripts/__pycache__/release_assets.cpython-312.pyc",
            "scripts/__PYCACHE__/unexpected.txt",
            "scripts/release_assets.pyc",
            "scripts/release_assets.pyo",
        ):
            with self.subTest(candidate=candidate), self.assertRaisesRegex(
                release_assets.ReleaseAssetError, "generated Python bytecode"
            ):
                release_assets._validate_relative_path(candidate)

        with tempfile.TemporaryDirectory() as raw, tempfile.TemporaryDirectory() as tree_raw:
            archive, manifest, _ = self.build(Path(raw))
            with tarfile.open(archive, mode="r:gz") as bundle:
                bundle.extractall(tree_raw, filter="data")
            root = Path(tree_raw) / f"phoenix-release-{RELEASE_SHA}"
            cache = root / "scripts" / "__pycache__"
            cache.mkdir()
            (cache / "release_assets.cpython-312.pyc").write_bytes(b"generated")
            with self.assertRaisesRegex(
                release_assets.ReleaseAssetError, "generated Python bytecode"
            ):
                release_assets.verify_release_tree(root, manifest, RELEASE_SHA)

    def test_modified_live_canary_asset_in_release_tree_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw, tempfile.TemporaryDirectory() as tree_raw:
            archive, manifest, _ = self.build(Path(raw))
            with tarfile.open(archive, mode="r:gz") as bundle:
                bundle.extractall(tree_raw, filter="data")
            root = Path(tree_raw) / f"phoenix-release-{RELEASE_SHA}"
            target = root / LIVE_CANARY_ASSETS[2]
            target.write_bytes(target.read_bytes() + b"-- modified\n")
            with self.assertRaisesRegex(release_assets.ReleaseAssetError, "payload mismatch"):
                release_assets.verify_release_tree(root, manifest, RELEASE_SHA)

    @unittest.skipUnless(os.name == "posix", "POSIX mode enforcement")
    def test_extracted_tree_rejects_mode_drift(self) -> None:
        with tempfile.TemporaryDirectory() as raw, tempfile.TemporaryDirectory() as tree_raw:
            archive, manifest, _ = self.build(Path(raw))
            with tarfile.open(archive, mode="r:gz") as bundle:
                bundle.extractall(tree_raw, filter="data")
            root = Path(tree_raw) / f"phoenix-release-{RELEASE_SHA}"
            target = root / "scripts" / "deploy-release.sh"
            target.chmod(0o644)
            with self.assertRaisesRegex(release_assets.ReleaseAssetError, "mode mismatch"):
                release_assets.verify_release_tree(root, manifest, RELEASE_SHA)

    def test_archive_corruption_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            archive, manifest, checksums = self.build(Path(raw))
            damaged = bytearray(archive.read_bytes())
            damaged[len(damaged) // 2] ^= 1
            archive.write_bytes(damaged)
            with self.assertRaisesRegex(release_assets.ReleaseAssetError, "checksum mismatch"):
                release_assets.verify_release_assets(archive, manifest, checksums, RELEASE_SHA)

    def test_wrong_release_identity_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            archive, manifest, checksums = self.build(Path(raw))
            with self.assertRaisesRegex(release_assets.ReleaseAssetError, "identity is invalid"):
                release_assets.verify_release_assets(
                    archive, manifest, checksums, "2" * 40
                )

    def test_checksum_contract_rejects_extra_lines(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            archive, manifest, checksums = self.build(Path(raw))
            checksums.write_text(
                checksums.read_text(encoding="ascii") + f"{'0' * 64}  extra\n",
                encoding="ascii",
            )
            with self.assertRaisesRegex(
                release_assets.ReleaseAssetError, "checksum file is invalid"
            ):
                release_assets.verify_release_assets(archive, manifest, checksums, RELEASE_SHA)

    def test_manifest_contract_rejects_additional_properties(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            _, manifest, _ = self.build(Path(raw))
            value = json.loads(manifest.read_text(encoding="utf-8"))
            value["unexpected"] = True
            manifest.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(release_assets.ReleaseAssetError, "contract is invalid"):
                release_assets._load_manifest(manifest, RELEASE_SHA)

    def test_path_policy_rejects_traversal_and_sensitive_names(self) -> None:
        for candidate in (
            "../escape",
            "/absolute",
            "nested//double",
            "nested\\windows",
            "config/.env",
            "scripts/__pycache__/module.pyc",
            "scripts/module.pyo",
        ):
            with self.subTest(candidate=candidate):
                with self.assertRaises(release_assets.ReleaseAssetError):
                    release_assets._validate_relative_path(candidate)

    def test_checksum_file_matches_the_built_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            archive, manifest, checksums = self.build(Path(raw))
            lines = checksums.read_text(encoding="ascii").splitlines()
            expected = {
                archive.name: hashlib.sha256(archive.read_bytes()).hexdigest(),
                manifest.name: hashlib.sha256(manifest.read_bytes()).hexdigest(),
            }
            observed = {line.split("  ", 1)[1]: line.split("  ", 1)[0] for line in lines}
            self.assertEqual(observed, expected)

    def test_schema_file_declares_strict_manifest_contract(self) -> None:
        schema = json.loads(
            (self.repo_root / "schemas" / "phoenix-release-assets.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(schema["properties"]["schema"]["const"], release_assets.SCHEMA)
        self.assertEqual(schema["properties"]["files"]["maxItems"], release_assets.MAX_FILES)


if __name__ == "__main__":
    unittest.main()
