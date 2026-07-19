import hashlib
import json
import os
import tarfile
import tempfile
import unittest
from pathlib import Path

from scripts import release_assets


RELEASE_SHA = "1" * 40


class ReleaseAssetsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.repo_root = Path(__file__).resolve().parents[2]
        cls.contract_artifact = (
            cls.repo_root / "fork-sandbox" / "abi" / "PhoenixExecutor.json"
        )

    def build(self, output: Path):
        return release_assets.build_release_assets(
            self.repo_root,
            RELEASE_SHA,
            output,
            self.contract_artifact,
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
            self.assertIn("contracts/PhoenixExecutor.compiled.json", paths)
            self.assertIn("schemas/phoenix-release-assets.schema.json", paths)
            self.assertIn("scripts/prelive-shadow-control.sh", paths)
            self.assertIn("scripts/prelive-protected-maintenance.sh", paths)
            self.assertIn("scripts/prelive_protected_maintenance.py", paths)
            self.assertIn("scripts/provision-production-host.sh", paths)
            self.assertIn("scripts/install-production-release-context.sh", paths)
            self.assertIn("scripts/prelive-protected-maintenance-launch.sh", paths)
            self.assertIn("scripts/prelive-protected-maintenance-unit.sh", paths)
            self.assertIn("scripts/prelive-v5-fresh-database-gate.sh", paths)
            self.assertIn("scripts/prelive_v5_release.py", paths)
            self.assertIn("scripts/release_provenance.py", paths)
            self.assertIn("deploy/prelive-v5-release.example.json", paths)
            self.assertIn("schemas/phoenix-release-provenance.schema.json", paths)
            self.assertIn("schemas/phoenix-prelive-v5-release.schema.json", paths)
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
