import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import release_provenance


RELEASE_SHA = "a" * 40
RUN_ID = "30000000001"


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
        self, *, release_sha: str = RELEASE_SHA, run_id: str = RUN_ID
    ) -> None:
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
            (self.fragments / f"{name}.json").write_text(
                json.dumps(value), encoding="utf-8"
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
        self.assertIn("PUBLISH_IMMUTABLE_PHOENIX_IMAGES", workflow)
        self.assertIn("PHOENIX_PRELIVE_SHADOW_V5", workflow)
        self.assertIn('git merge-base --is-ancestor "$RELEASE_SHA" origin/main', workflow)
        self.assertNotIn("sha-${{ github.sha }}", workflow)
        self.assertGreaterEqual(
            workflow.count("ref: ${{ inputs.release_sha }}"), 4
        )
        self.assertEqual(workflow.count("packages: write"), 1)

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
            schema["properties"]["schema"]["const"],
            release_provenance.PROVENANCE_SCHEMA,
        )
        self.assertEqual(
            schema["properties"]["quarantine"]["properties"]["run_ids"]["contains"][
                "const"
            ],
            "29683234024",
        )


if __name__ == "__main__":
    unittest.main()
