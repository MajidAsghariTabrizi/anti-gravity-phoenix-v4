import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import production_context, release_provenance


RELEASE_SHA = "a" * 40
RUN_ID = "30000000001"
BASE_SHA = "b" * 40
BASE_RUN_ID = "30000000002"


class ReleaseProvenanceTests(unittest.TestCase):
    def test_release_contract_requires_the_seventh_live_executor_image(self) -> None:
        self.assertEqual(len(release_provenance.EXPECTED_IMAGES), 7)
        self.assertIn("live-executor", release_provenance.EXPECTED_IMAGES)
        self.assertIn("build-live-executor", release_provenance.EXPECTED_JOBS)

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.fragments = self.root / "fragments"
        self.assets = self.root / "assets"
        self.fragments.mkdir()
        self.assets.mkdir()
        self._write_fragments()
        for path in (
            self.assets / f"phoenix-release-assets-{RELEASE_SHA}.tar.gz",
            self.assets / "release-assets-manifest.json",
            self.assets / "release-assets-checksums.txt",
        ):
            path.write_bytes(f"fixture:{path.name}\n".encode("ascii"))

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_fragments(
        self,
        *,
        release_sha: str = RELEASE_SHA,
        run_id: str = RUN_ID,
        directory: Path | None = None,
    ) -> None:
        destination = directory or self.fragments
        for index, name in enumerate(release_provenance.EXPECTED_IMAGES, start=1):
            value = {
                "schema": release_provenance.FRAGMENT_SCHEMA,
                "release_sha": release_sha,
                "build_run_id": run_id,
                "release_intent": release_provenance.RELEASE_INTENT,
                "name": name,
                "repository": f"ghcr.io/majidasgharitabrizi/{name}",
                "tag": f"sha-{release_sha}",
                "digest": f"sha256:{index:064x}",
            }
            (destination / f"{name}.json").write_text(
                json.dumps(value), encoding="utf-8"
            )

    @staticmethod
    def _write_assets(directory: Path, release_sha: str) -> None:
        for path in (
            directory / f"phoenix-release-assets-{release_sha}.tar.gz",
            directory / "release-assets-manifest.json",
            directory / "release-assets-checksums.txt",
        ):
            path.write_bytes(f"fixture:{path.name}\n".encode("ascii"))

    def _build_full_release(
        self, root: Path, release_sha: str, run_id: str
    ) -> tuple[dict, dict, Path, Path]:
        fragments = root / "fragments"
        assets = root / "assets"
        fragments.mkdir(parents=True)
        assets.mkdir()
        self._write_fragments(
            release_sha=release_sha, run_id=run_id, directory=fragments
        )
        self._write_assets(assets, release_sha)
        manifest_path = root / "release-manifest.json"
        provenance_path = root / "release-provenance.json"
        with mock.patch.object(
            release_provenance.release_assets, "verify_release_assets"
        ):
            manifest, provenance = release_provenance.assemble_release(
                fragments,
                assets,
                release_sha,
                run_id,
                release_provenance.RELEASE_INTENT,
                manifest_path,
                provenance_path,
                created_at="2026-07-19T00:00:00Z",
            )
        return manifest, provenance, manifest_path, provenance_path

    def _assemble_inherited(self):
        base = self._build_full_release(self.root / "base", BASE_SHA, BASE_RUN_ID)
        base_manifest, base_provenance, base_manifest_path, base_provenance_path = base
        release_provenance.write_inherited_fragments(
            self.fragments,
            RELEASE_SHA,
            RUN_ID,
            release_provenance.RELEASE_INTENT,
            BASE_SHA,
            BASE_RUN_ID,
            base_manifest_path,
            base_provenance_path,
        )
        manifest_path = self.root / "inherited-release-manifest.json"
        provenance_path = self.root / "inherited-release-provenance.json"
        with mock.patch.object(
            release_provenance.release_assets, "verify_release_assets"
        ):
            manifest, provenance = release_provenance.assemble_release(
                self.fragments,
                self.assets,
                RELEASE_SHA,
                RUN_ID,
                release_provenance.RELEASE_INTENT,
                manifest_path,
                provenance_path,
                created_at="2026-07-20T00:00:00Z",
                protected_base_sha=BASE_SHA,
                protected_base_build_run_id=BASE_RUN_ID,
                protected_base_manifest=base_manifest_path,
                protected_base_provenance=base_provenance_path,
            )
        return (
            manifest,
            provenance,
            manifest_path,
            provenance_path,
            base_manifest,
            base_provenance,
            base_manifest_path,
            base_provenance_path,
        )

    def _assemble(self):
        manifest_path = self.root / "release-manifest.json"
        provenance_path = self.root / "release-provenance.json"
        with mock.patch.object(
            release_provenance.release_assets, "verify_release_assets"
        ):
            manifest, provenance = release_provenance.assemble_release(
                self.fragments,
                self.assets,
                RELEASE_SHA,
                RUN_ID,
                release_provenance.RELEASE_INTENT,
                manifest_path,
                provenance_path,
                created_at="2026-07-19T00:00:00Z",
            )
        return manifest, provenance

    def _run_evidence(self):
        return {
            "schema": release_provenance.RUN_EVIDENCE_SCHEMA,
            "repository": release_provenance.REPOSITORY,
            "workflow": release_provenance.WORKFLOW,
            "event": "workflow_dispatch",
            "run_id": RUN_ID,
            "head_sha": RELEASE_SHA,
            "release_intent": release_provenance.RELEASE_INTENT,
            "status": "completed",
            "conclusion": "success",
            "jobs": [
                {"name": name, "status": "completed", "conclusion": "success"}
                for name in release_provenance.EXPECTED_JOBS
            ],
            "artifacts": list(
                release_provenance._release_artifact_names(RELEASE_SHA)
            ),
        }

    def test_complete_same_run_release_is_canonical(self) -> None:
        manifest, provenance = self._assemble()
        release_provenance.validate_canonical_run(
            provenance, manifest, self._run_evidence()
        )

    def test_ordinary_release_inherits_both_protected_images(self) -> None:
        (
            manifest,
            provenance,
            manifest_path,
            _,
            base_manifest,
            _,
            _,
            _,
        ) = self._assemble_inherited()
        self.assertEqual(manifest["schema"], release_provenance.INHERITED_RELEASE_SCHEMA)
        self.assertEqual(manifest["protected_base_sha"], BASE_SHA)
        self.assertEqual(manifest["protected_base_build_run_id"], BASE_RUN_ID)
        self.assertEqual(provenance["built_images"], list(release_provenance.BUILT_IMAGES))
        self.assertEqual(
            provenance["inherited_images"], list(release_provenance.PROTECTED_IMAGES)
        )
        for name in release_provenance.PROTECTED_IMAGES:
            image = manifest["images"][name]
            self.assertEqual(image["origin"], "inherited")
            self.assertEqual(image["repository"], base_manifest["images"][name]["repository"])
            self.assertEqual(image["tag"], base_manifest["images"][name]["tag"])
            self.assertEqual(image["digest"], base_manifest["images"][name]["digest"])
        for name in release_provenance.BUILT_IMAGES:
            image = manifest["images"][name]
            self.assertEqual(image["origin"], "built")
            self.assertEqual(image["tag"], f"sha-{RELEASE_SHA}")
            self.assertEqual(image["source_sha"], RELEASE_SHA)
            self.assertEqual(image["source_build_run_id"], RUN_ID)
            self.assertEqual(image["oci_revision"], RELEASE_SHA)
        release_provenance.validate_canonical_run(
            provenance, manifest, self._run_evidence()
        )
        _, context_sha, references = production_context.load_manifest(manifest_path)
        self.assertEqual(context_sha, RELEASE_SHA)
        self.assertEqual(
            references["recorder"],
            f"{base_manifest['images']['recorder']['repository']}@"
            f"{base_manifest['images']['recorder']['digest']}",
        )

    def test_protected_base_dispatch_inputs_are_atomic(self) -> None:
        for base_sha, base_run in ((BASE_SHA, ""), ("", BASE_RUN_ID)):
            with self.subTest(base_sha=base_sha, base_run=base_run):
                with self.assertRaisesRegex(
                    release_provenance.ReleaseProvenanceError, "supplied together"
                ):
                    release_provenance.validate_dispatch(
                        RELEASE_SHA,
                        release_provenance.RELEASE_INTENT,
                        release_provenance.PUBLISH_CONFIRMATION,
                        RELEASE_SHA,
                        base_sha,
                        base_run,
                    )

    def test_wrong_protected_base_sha_or_run_is_rejected(self) -> None:
        _, _, base_manifest_path, base_provenance_path = self._build_full_release(
            self.root / "wrong-base", BASE_SHA, BASE_RUN_ID
        )
        for base_sha, base_run, diagnostic in (
            ("c" * 40, BASE_RUN_ID, "manifest SHA"),
            (BASE_SHA, "30000000003", "build run"),
        ):
            with self.subTest(base_sha=base_sha, base_run=base_run):
                with self.assertRaisesRegex(
                    release_provenance.ReleaseProvenanceError, diagnostic
                ):
                    release_provenance.write_inherited_fragments(
                        self.fragments,
                        RELEASE_SHA,
                        RUN_ID,
                        release_provenance.RELEASE_INTENT,
                        base_sha,
                        base_run,
                        base_manifest_path,
                        base_provenance_path,
                    )

    def test_protected_fragment_digest_repository_and_tag_must_match_base(self) -> None:
        _, _, base_manifest_path, base_provenance_path = self._build_full_release(
            self.root / "fragment-base", BASE_SHA, BASE_RUN_ID
        )
        release_provenance.write_inherited_fragments(
            self.fragments,
            RELEASE_SHA,
            RUN_ID,
            release_provenance.RELEASE_INTENT,
            BASE_SHA,
            BASE_RUN_ID,
            base_manifest_path,
            base_provenance_path,
        )
        path = self.fragments / "recorder.json"
        original = json.loads(path.read_text(encoding="utf-8"))
        mutations = (
            ("digest", f"sha256:{99:064x}"),
            ("repository", "ghcr.io/majidasgharitabrizi/not-recorder"),
            ("tag", f"sha-{'c' * 40}"),
        )
        for field, value in mutations:
            changed = copy.deepcopy(original)
            changed[field] = value
            path.write_text(json.dumps(changed), encoding="utf-8")
            with self.subTest(field=field):
                with mock.patch.object(
                    release_provenance.release_assets, "verify_release_assets"
                ):
                    with self.assertRaises(release_provenance.ReleaseProvenanceError):
                        release_provenance.assemble_release(
                            self.fragments,
                            self.assets,
                            RELEASE_SHA,
                            RUN_ID,
                            release_provenance.RELEASE_INTENT,
                            self.root / "bad-manifest.json",
                            self.root / "bad-provenance.json",
                            protected_base_sha=BASE_SHA,
                            protected_base_build_run_id=BASE_RUN_ID,
                            protected_base_manifest=base_manifest_path,
                            protected_base_provenance=base_provenance_path,
                        )
            path.write_text(json.dumps(original), encoding="utf-8")

    def test_non_protected_image_must_be_bound_to_candidate_release(self) -> None:
        manifest, provenance, *_ = self._assemble_inherited()
        changed = copy.deepcopy(manifest)
        image = changed["images"]["live-executor"]
        image["source_sha"] = "c" * 40
        image["tag"] = f"sha-{'c' * 40}"
        image["oci_revision"] = "c" * 40
        with self.assertRaisesRegex(
            release_provenance.ReleaseProvenanceError, "not bound"
        ):
            release_provenance.validate_provenance(provenance, changed)

    def test_deploy_pair_requires_exact_inherited_rollback_identity(self) -> None:
        (
            manifest,
            provenance,
            manifest_path,
            provenance_path,
            _,
            _,
            base_manifest_path,
            base_provenance_path,
        ) = self._assemble_inherited()
        release_provenance.validate_deploy_pair(
            manifest_path,
            provenance_path,
            RELEASE_SHA,
            RUN_ID,
            base_manifest_path,
            base_provenance_path,
            BASE_SHA,
            BASE_RUN_ID,
        )

        changed_manifest = copy.deepcopy(manifest)
        changed_provenance = copy.deepcopy(provenance)
        changed_manifest["images"]["feed-ingestor"]["digest"] = f"sha256:{98:064x}"
        manifest_bytes = release_provenance._canonical_json(changed_manifest)
        changed_provenance["release_manifest_sha256"] = (
            release_provenance._sha256_bytes(manifest_bytes)
        )
        manifest_path.write_bytes(manifest_bytes)
        provenance_path.write_bytes(release_provenance._canonical_json(changed_provenance))
        with self.assertRaisesRegex(
            release_provenance.ReleaseProvenanceError, "differs from rollback"
        ):
            release_provenance.validate_deploy_pair(
                manifest_path,
                provenance_path,
                RELEASE_SHA,
                RUN_ID,
                base_manifest_path,
                base_provenance_path,
                BASE_SHA,
                BASE_RUN_ID,
            )

    def test_legacy_full_build_deploy_contract_remains_supported(self) -> None:
        self._assemble()
        candidate_manifest_path = self.root / "release-manifest.json"
        candidate_provenance_path = self.root / "release-provenance.json"
        (
            _,
            _,
            rollback_manifest_path,
            rollback_provenance_path,
        ) = self._build_full_release(self.root / "legacy-rollback", BASE_SHA, BASE_RUN_ID)
        release_provenance.validate_deploy_pair(
            candidate_manifest_path,
            candidate_provenance_path,
            RELEASE_SHA,
            RUN_ID,
            rollback_manifest_path,
            rollback_provenance_path,
            BASE_SHA,
            BASE_RUN_ID,
        )
        candidate_manifest = json.loads(
            candidate_manifest_path.read_text(encoding="utf-8")
        )
        candidate_provenance = json.loads(
            candidate_provenance_path.read_text(encoding="utf-8")
        )
        candidate_manifest["images"]["recorder"]["digest"] = f"sha256:{95:064x}"
        manifest_bytes = release_provenance._canonical_json(candidate_manifest)
        candidate_provenance["release_manifest_sha256"] = (
            release_provenance._sha256_bytes(manifest_bytes)
        )
        candidate_manifest_path.write_bytes(manifest_bytes)
        candidate_provenance_path.write_bytes(
            release_provenance._canonical_json(candidate_provenance)
        )
        with self.assertRaisesRegex(
            release_provenance.ReleaseProvenanceError, "maintenance is required"
        ):
            release_provenance.validate_deploy_pair(
                candidate_manifest_path,
                candidate_provenance_path,
                RELEASE_SHA,
                RUN_ID,
                rollback_manifest_path,
                rollback_provenance_path,
                BASE_SHA,
                BASE_RUN_ID,
            )

    def test_deploy_pair_rejects_wrong_base_sha_run_and_hashes(self) -> None:
        (
            manifest,
            provenance,
            manifest_path,
            provenance_path,
            _,
            _,
            base_manifest_path,
            base_provenance_path,
        ) = self._assemble_inherited()
        cases = (
            ("sha", "c" * 40, "protected base"),
            ("run", "30000000003", "protected base"),
            ("manifest_hash", f"sha256:{97:064x}", "manifest hash"),
            ("provenance_hash", f"sha256:{96:064x}", "provenance hash"),
        )
        for field, value, diagnostic in cases:
            changed_manifest = copy.deepcopy(manifest)
            changed_provenance = copy.deepcopy(provenance)
            if field == "sha":
                changed_manifest["protected_base_sha"] = value
                changed_provenance["protected_base"]["release_sha"] = value
            elif field == "run":
                changed_manifest["protected_base_build_run_id"] = value
                changed_provenance["protected_base"]["build_run_id"] = value
            elif field == "manifest_hash":
                changed_provenance["protected_base"]["release_manifest_sha256"] = value
            else:
                changed_provenance["protected_base"]["release_provenance_sha256"] = value
            manifest_bytes = release_provenance._canonical_json(changed_manifest)
            changed_provenance["release_manifest_sha256"] = (
                release_provenance._sha256_bytes(manifest_bytes)
            )
            manifest_path.write_bytes(manifest_bytes)
            provenance_path.write_bytes(
                release_provenance._canonical_json(changed_provenance)
            )
            with self.subTest(field=field):
                with self.assertRaisesRegex(
                    release_provenance.ReleaseProvenanceError, diagnostic
                ):
                    release_provenance.validate_deploy_pair(
                        manifest_path,
                        provenance_path,
                        RELEASE_SHA,
                        RUN_ID,
                        base_manifest_path,
                        base_provenance_path,
                        BASE_SHA,
                        BASE_RUN_ID,
                    )

    def test_github_base_run_must_be_exact_and_successful(self) -> None:
        run = {
            "id": int(BASE_RUN_ID),
            "name": release_provenance.WORKFLOW,
            "path": release_provenance.WORKFLOW_PATH,
            "event": "workflow_dispatch",
            "head_sha": BASE_SHA,
            "status": "completed",
            "conclusion": "success",
            "repository": {"full_name": release_provenance.REPOSITORY},
        }
        release_provenance.validate_github_run(run, BASE_SHA, BASE_RUN_ID)
        for field, value in (
            ("head_sha", "c" * 40),
            ("conclusion", "failure"),
            ("event", "push"),
            ("path", ".github/workflows/other.yml"),
        ):
            changed = copy.deepcopy(run)
            changed[field] = value
            with self.subTest(field=field):
                with self.assertRaises(release_provenance.ReleaseProvenanceError):
                    release_provenance.validate_github_run(
                        changed, BASE_SHA, BASE_RUN_ID
                    )

    def test_explicit_dispatch_contract_rejects_missing_or_wrong_values(self) -> None:
        valid = (
            RELEASE_SHA,
            release_provenance.RELEASE_INTENT,
            release_provenance.PUBLISH_CONFIRMATION,
            RELEASE_SHA,
        )
        release_provenance.validate_dispatch(*valid)
        for index, bad_value in (
            (0, ""),
            (0, "not-a-sha"),
            (1, "UNBOUNDED_RELEASE"),
            (2, ""),
            (2, "PUBLISH"),
            (3, "b" * 40),
        ):
            values = list(valid)
            values[index] = bad_value
            with self.subTest(index=index, bad_value=bad_value):
                with self.assertRaises(release_provenance.ReleaseProvenanceError):
                    release_provenance.validate_dispatch(*values)

    def test_incomplete_fragment_set_is_rejected(self) -> None:
        (self.fragments / "recorder.json").unlink()
        with mock.patch.object(
            release_provenance.release_assets, "verify_release_assets"
        ):
            with self.assertRaisesRegex(
                release_provenance.ReleaseProvenanceError, "fragment set"
            ):
                release_provenance.assemble_release(
                    self.fragments,
                    self.assets,
                    RELEASE_SHA,
                    RUN_ID,
                    release_provenance.RELEASE_INTENT,
                    self.root / "manifest.json",
                    self.root / "provenance.json",
                )

    def test_mixed_fragment_sha_and_run_are_rejected(self) -> None:
        path = self.fragments / "recorder.json"
        fragment = json.loads(path.read_text(encoding="utf-8"))
        for field, value, diagnostic in (
            ("release_sha", "b" * 40, "mixed release SHA"),
            ("build_run_id", "30000000002", "mixed build run"),
        ):
            changed = dict(fragment)
            changed[field] = value
            if field == "release_sha":
                changed["tag"] = f"sha-{value}"
            path.write_text(json.dumps(changed), encoding="utf-8")
            with self.subTest(field=field):
                with mock.patch.object(
                    release_provenance.release_assets, "verify_release_assets"
                ):
                    with self.assertRaisesRegex(
                        release_provenance.ReleaseProvenanceError, diagnostic
                    ):
                        release_provenance.assemble_release(
                            self.fragments,
                            self.assets,
                            RELEASE_SHA,
                            RUN_ID,
                            release_provenance.RELEASE_INTENT,
                            self.root / "manifest.json",
                            self.root / "provenance.json",
                        )
            path.write_text(json.dumps(fragment), encoding="utf-8")

    def test_cancelled_or_missing_required_job_is_rejected(self) -> None:
        manifest, provenance = self._assemble()
        for mutation, diagnostic in (
            ("cancel", "build-recorder"),
            ("missing", "release-manifest"),
        ):
            evidence = self._run_evidence()
            if mutation == "cancel":
                job = next(item for item in evidence["jobs"] if item["name"] == diagnostic)
                job["conclusion"] = "cancelled"
            else:
                evidence["jobs"] = [
                    item for item in evidence["jobs"] if item["name"] != diagnostic
                ]
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(
                    release_provenance.ReleaseProvenanceError, diagnostic
                ):
                    release_provenance.validate_canonical_run(
                        provenance, manifest, evidence
                    )

    def test_mixed_run_and_mixed_sha_evidence_are_rejected(self) -> None:
        manifest, provenance = self._assemble()
        for field, value, diagnostic in (
            ("run_id", "30000000002", "mixed run"),
            ("head_sha", "b" * 40, "mixed SHA"),
        ):
            evidence = self._run_evidence()
            evidence[field] = value
            with self.subTest(field=field):
                with self.assertRaisesRegex(
                    release_provenance.ReleaseProvenanceError, diagnostic
                ):
                    release_provenance.validate_canonical_run(
                        provenance, manifest, evidence
                    )

    def test_missing_or_duplicate_release_artifact_is_rejected(self) -> None:
        manifest, provenance = self._assemble()
        required = f"release-fragment-recorder"
        for mutation, diagnostic in (
            ("missing", "missing"),
            ("duplicate", "duplicate"),
        ):
            evidence = self._run_evidence()
            if mutation == "missing":
                evidence["artifacts"].remove(required)
            else:
                evidence["artifacts"].append(required)
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(
                    release_provenance.ReleaseProvenanceError, diagnostic
                ):
                    release_provenance.validate_canonical_run(
                        provenance, manifest, evidence
                    )

    def test_incomplete_automatic_run_is_permanently_quarantined(self) -> None:
        manifest, provenance = self._assemble()
        changed = copy.deepcopy(provenance)
        changed["build_run_id"] = "29683234024"
        with self.assertRaisesRegex(
            release_provenance.ReleaseProvenanceError,
            "NON_CANONICAL_INCOMPLETE_BUILD",
        ):
            release_provenance.validate_provenance(changed, manifest)

    def test_placeholder_image_digest_is_rejected(self) -> None:
        manifest, provenance = self._assemble()
        manifest["images"]["recorder"]["digest"] = f"sha256:{'0' * 64}"
        with self.assertRaisesRegex(
            release_provenance.ReleaseProvenanceError, "digest"
        ):
            release_provenance.validate_provenance(provenance, manifest)

    def test_workflow_is_manual_exact_sha_and_least_privilege(self) -> None:
        workflow = (
            Path(__file__).resolve().parents[2]
            / ".github"
            / "workflows"
            / "build-images.yml"
        ).read_text(encoding="utf-8")
        self.assertNotIn("\n  push:", workflow)
        self.assertNotIn("\n  pull_request:", workflow)
        self.assertIn("release_sha:", workflow)
        self.assertIn("release_intent:", workflow)
        self.assertIn("confirm_publish:", workflow)
        self.assertIn("protected_base_sha:", workflow)
        self.assertIn("protected_base_build_run_id:", workflow)
        self.assertIn("PUBLISH_IMMUTABLE_PHOENIX_IMAGES", workflow)
        self.assertIn("PHOENIX_PRELIVE_SHADOW_V5", workflow)
        self.assertIn('git merge-base --is-ancestor "$RELEASE_SHA" origin/main', workflow)
        self.assertIn("validate-github-run", workflow)
        self.assertIn("inherit-protected", workflow)
        self.assertEqual(workflow.count("            protected: true"), 2)
        self.assertGreaterEqual(
            workflow.count(
                "if: ${{ inputs.protected_base_sha == '' || matrix.protected == false }}"
            ),
            7,
        )
        self.assertIn(
            "run-id: ${{ inputs.protected_base_build_run_id }}", workflow
        )
        self.assertNotIn("sha-${{ github.sha }}", workflow)
        self.assertGreaterEqual(
            workflow.count("ref: ${{ inputs.release_sha }}"), 4
        )
        self.assertEqual(workflow.count("packages: write"), 1)

    def test_deploy_inheritance_validation_precedes_ssh(self) -> None:
        workflow = (
            Path(__file__).resolve().parents[2]
            / ".github"
            / "workflows"
            / "deploy-shadow.yml"
        ).read_text(encoding="utf-8")
        validation = workflow.index("validate-deploy-pair")
        ssh_install = workflow.index("- name: Install SSH material")
        self.assertLess(validation, ssh_install)
        self.assertIn("Verify successful build run identities", workflow)
        self.assertIn("--release-provenance release/release-provenance.json", workflow)
        self.assertIn("--rollback-provenance rollback/release-provenance.json", workflow)

    def test_provenance_schema_is_strict_and_quarantines_incident_run(self) -> None:
        schema = json.loads(
            (
                Path(__file__).resolve().parents[2]
                / "schemas"
                / "phoenix-release-provenance.schema.json"
            ).read_text(encoding="utf-8")
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            set(schema["properties"]["schema"]["enum"]),
            {
                release_provenance.PROVENANCE_SCHEMA,
                release_provenance.INHERITED_PROVENANCE_SCHEMA,
            },
        )
        self.assertEqual(
            schema["properties"]["quarantine"]["properties"]["run_ids"]["contains"][
                "const"
            ],
            "29683234024",
        )

        manifest_schema = json.loads(
            (
                Path(__file__).resolve().parents[2]
                / "schemas"
                / "phoenix-release-manifest.schema.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(len(manifest_schema["oneOf"]), 2)
        inherited = manifest_schema["$defs"]["protectedInheritance"]
        self.assertFalse(inherited["additionalProperties"])
        self.assertIn("protected_base_sha", inherited["required"])
        self.assertIn("live-executor", manifest_schema["$defs"]["inheritedImages"]["required"])


if __name__ == "__main__":
    unittest.main()
