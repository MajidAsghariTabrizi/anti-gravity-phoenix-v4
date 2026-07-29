from __future__ import annotations

from pathlib import Path
import unittest

from scripts import change_impact, release_components


class ChangeImpactTests(unittest.TestCase):
    ROOT = Path(__file__).resolve().parents[2]

    def test_release_only_change_runs_no_application_job_or_image(self) -> None:
        plan = change_impact.classify(
            [
                ".github/workflows/phoenix-release-controller.yml",
                "scripts/phoenix_release/gateway.py",
            ]
        )
        self.assertEqual(plan["classification"], "release-only")
        self.assertTrue(plan["release_required"])
        self.assertEqual(plan["built_images"], [])
        self.assertEqual(
            plan["inherited_images"],
            sorted(release_components.RELEASE_IMAGES),
        )
        self.assertTrue(plan["jobs"]["hygiene"])
        self.assertFalse(plan["jobs"]["rust-fork-sandbox"])

    def test_docs_only_change_creates_no_release(self) -> None:
        plan = change_impact.classify(["README.md", "docs/CI_CD.md"])
        self.assertEqual(plan["classification"], "docs-only")
        self.assertFalse(plan["release_required"])
        self.assertFalse(any(plan["jobs"].values()))
        self.assertEqual(plan["built_images"], [])

    def test_rpc_change_propagates_to_real_dependants_only(self) -> None:
        plan = change_impact.classify(["rpc-gateway/src/lib.rs"])
        self.assertEqual(
            plan["built_images"],
            [
                "fork-sandbox",
                "live-executor",
                "phoenix-engine",
                "rpc-gateway",
            ],
        )
        self.assertTrue(plan["jobs"]["rust-rpc-gateway"])
        self.assertTrue(plan["jobs"]["rust-phoenix"])
        self.assertTrue(plan["jobs"]["rust-replay"])
        self.assertTrue(plan["jobs"]["rust-fork-sandbox"])
        self.assertFalse(plan["jobs"]["go"])

    def test_specific_dockerfile_builds_only_its_image(self) -> None:
        plan = change_impact.classify(["deploy/dashboard.Dockerfile"])
        self.assertTrue(plan["docker_static"])
        self.assertEqual(plan["built_images"], ["dashboard"])
        self.assertTrue(plan["jobs"]["docker-validation"])

    def test_root_ignore_is_static_when_every_image_has_a_specific_contract(
        self,
    ) -> None:
        plan = change_impact.classify([".dockerignore"])
        self.assertEqual(plan["built_images"], [])
        self.assertTrue(plan["docker_static"])
        self.assertTrue(plan["jobs"]["docker-validation"])

    def test_rust_docker_contract_builds_only_rust_images(self) -> None:
        plan = change_impact.classify(
            ["deploy/rust.Dockerfile.dockerignore"]
        )
        self.assertEqual(
            plan["built_images"],
            [
                "live-executor",
                "phoenix-engine",
                "recorder",
                "rpc-gateway",
            ],
        )
        self.assertTrue(plan["jobs"]["docker-validation"])

    def test_sql_change_avoids_unrelated_language_suites(self) -> None:
        plan = change_impact.classify(["migrations/006_candidate.sql"])
        self.assertTrue(plan["jobs"]["hygiene"])
        self.assertTrue(plan["jobs"]["docker-validation"])
        for name in (
            "go",
            "rust-phoenix",
            "rust-recorder",
            "rust-fork-sandbox",
            "integration-fixtures",
            "jetstream-integration",
        ):
            self.assertFalse(plan["jobs"][name], name)
        self.assertEqual(
            plan["built_images"],
            [
                "feed-ingestor",
                "fork-sandbox",
                "live-executor",
                "phoenix-engine",
                "recorder",
                "rpc-gateway",
            ],
        )

    def test_narrow_rust_contexts_exclude_local_build_outputs(self) -> None:
        for name in (
            "deploy/rust.Dockerfile.dockerignore",
            "deploy/fork-sandbox.Dockerfile.dockerignore",
        ):
            contract = (self.ROOT / name).read_text(encoding="utf-8")
            self.assertIn("**/target", contract, name)
            self.assertIn("**/target/**", contract, name)
        rust_contract = (
            self.ROOT / "deploy/rust.Dockerfile.dockerignore"
        ).read_text(encoding="utf-8")
        self.assertIn("!scripts/sql/prelive-money-path-report.sql", rust_contract)
        rust_dockerfile = (
            self.ROOT / "deploy/rust.Dockerfile"
        ).read_text(encoding="utf-8")
        self.assertNotIn("cargo fetch", rust_dockerfile)
        self.assertIn("cargo build --locked --release", rust_dockerfile)

    def test_release_registry_change_is_conservative(self) -> None:
        plan = change_impact.classify(["release-components.json"])
        self.assertEqual(
            plan["built_images"], sorted(release_components.RELEASE_IMAGES)
        )
        self.assertTrue(all(plan["jobs"][name] for name in ("hygiene", "docker-validation")))

    def test_unknown_path_fails_conservative(self) -> None:
        plan = change_impact.classify(["new-runtime/component.bin"])
        self.assertEqual(plan["unknown_paths"], ["new-runtime/component.bin"])
        self.assertTrue(all(plan["jobs"].values()))
        self.assertEqual(
            plan["built_images"], sorted(release_components.RELEASE_IMAGES)
        )

    def test_unsafe_or_empty_paths_reject(self) -> None:
        for paths in ([], ["../escape"], ["/absolute"], ["bad\\path"]):
            with self.subTest(paths=paths):
                with self.assertRaises(change_impact.ImpactError):
                    change_impact.classify(paths)


if __name__ == "__main__":
    unittest.main()
