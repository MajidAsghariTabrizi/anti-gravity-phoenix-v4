import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import prelive_v5_release
from scripts import release_provenance


RELEASE_SHA = "b" * 40
RUN_ID = "30000000003"


class PreliveV5ReleaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.repo_root = Path(__file__).resolve().parents[2]
        cls.template_path = cls.repo_root / "deploy" / "prelive-v5-release.example.json"

    def template(self):
        return json.loads(self.template_path.read_text(encoding="utf-8"))

    @staticmethod
    def run_evidence() -> dict[str, object]:
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

    def test_template_is_valid_but_not_deployable_until_materialized(self) -> None:
        value = self.template()
        prelive_v5_release.validate_contract(value, allow_placeholders=True)
        with self.assertRaisesRegex(prelive_v5_release.V5ReleaseError, "partially|SHA"):
            prelive_v5_release.validate_contract(value)

    def test_fallback_is_exact_untouched_v4_environment(self) -> None:
        value = self.template()
        fallback = value["fallback_environment"]
        self.assertEqual(fallback["tag"], "phoenix-prelive-shadow-v4")
        self.assertEqual(fallback["migrations"], list(prelive_v5_release.FALLBACK_MIGRATIONS))
        self.assertFalse(fallback["database_reused_for_candidate"])
        self.assertFalse(fallback["mutate_during_candidate_validation"])
        for field, replacement in (
            ("release_sha", "c" * 40),
            ("database_reused_for_candidate", True),
            ("mutate_during_candidate_validation", True),
        ):
            changed = copy.deepcopy(value)
            changed["fallback_environment"][field] = replacement
            with self.subTest(field=field):
                with self.assertRaisesRegex(
                    prelive_v5_release.V5ReleaseError, "fallback"
                ):
                    prelive_v5_release.validate_contract(
                        changed, allow_placeholders=True
                    )

    def test_candidate_requires_fresh_database_without_import_or_backfill(self) -> None:
        value = self.template()
        candidate = value["candidate_environment"]
        self.assertEqual(candidate["migrations"], list(prelive_v5_release.CANDIDATE_MIGRATIONS))
        self.assertEqual(candidate["migration_apply_count"], 2)
        self.assertTrue(candidate["fresh_database_required"])
        self.assertFalse(candidate["historical_import"])
        self.assertFalse(candidate["pending_outbox_import"])
        self.assertFalse(candidate["backfill"])
        self.assertFalse(candidate["shares_database_with_fallback"])
        for field in (
            "historical_import",
            "pending_outbox_import",
            "backfill",
            "shares_database_with_fallback",
        ):
            changed = copy.deepcopy(value)
            changed["candidate_environment"][field] = True
            with self.subTest(field=field):
                with self.assertRaisesRegex(
                    prelive_v5_release.V5ReleaseError, "fresh"
                ):
                    prelive_v5_release.validate_contract(
                        changed, allow_placeholders=True
                    )

    def test_shadow_environment_and_execution_zero_are_fail_closed(self) -> None:
        environment = {
            **prelive_v5_release.REQUIRED_EXACT_ENV,
            **{name: "" for name in prelive_v5_release.REQUIRED_EMPTY_ENV},
            **{
                name: str(bounds[0])
                for name, bounds in prelive_v5_release.BOUNDED_INTEGER_ENV.items()
            },
        }
        prelive_v5_release.validate_runtime_environment(environment)
        for name, value in (
            ("PHOENIX_MODE", "LIVE"),
            ("LIVE_EXECUTION", "true"),
            ("SIGNER_PRIVATE_KEY", "configured"),
            ("WALLET_ADDRESS", "configured"),
            ("EXECUTOR_ADDRESS", "configured"),
            ("RECORDER_PERSISTENCE_POLICY", "raw"),
            ("PUBLIC_BROADCAST", "true"),
            ("RECORDER_AGGREGATE_FLUSH_SECONDS", "0"),
        ):
            changed = dict(environment)
            changed[name] = value
            with self.subTest(name=name):
                with self.assertRaises(prelive_v5_release.V5ReleaseError):
                    prelive_v5_release.validate_runtime_environment(changed)

    def test_materialization_binds_one_non_quarantined_build(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            fragments = root / "fragments"
            assets = root / "assets"
            fragments.mkdir()
            assets.mkdir()
            for index, name in enumerate(
                release_provenance.EXPECTED_IMAGES, start=1
            ):
                (fragments / f"{name}.json").write_text(
                    json.dumps(
                        {
                            "schema": release_provenance.FRAGMENT_SCHEMA,
                            "release_sha": RELEASE_SHA,
                            "build_run_id": RUN_ID,
                            "release_intent": release_provenance.RELEASE_INTENT,
                            "name": name,
                            "repository": f"ghcr.io/majidasgharitabrizi/{name}",
                            "tag": f"sha-{RELEASE_SHA}",
                            "digest": f"sha256:{index:064x}",
                        }
                    ),
                    encoding="utf-8",
                )
            for path in (
                assets / f"phoenix-release-assets-{RELEASE_SHA}.tar.gz",
                assets / "release-assets-manifest.json",
                assets / "release-assets-checksums.txt",
            ):
                path.write_text(path.name, encoding="ascii")
            manifest_path = root / "release-manifest.json"
            provenance_path = root / "release-provenance.json"
            run_evidence_path = root / "build-run-evidence.json"
            run_evidence_path.write_text(
                json.dumps(self.run_evidence()), encoding="utf-8"
            )
            with mock.patch.object(
                release_provenance.release_assets, "verify_release_assets"
            ):
                release_provenance.assemble_release(
                    fragments,
                    assets,
                    RELEASE_SHA,
                    RUN_ID,
                    release_provenance.RELEASE_INTENT,
                    manifest_path,
                    provenance_path,
                    created_at="2026-07-19T00:00:00Z",
                )
            materialized = prelive_v5_release.materialize_contract(
                self.template(),
                manifest_path,
                provenance_path,
                run_evidence_path,
            )
            prelive_v5_release.validate_contract(materialized)
            self.assertEqual(materialized["release"]["source_sha"], RELEASE_SHA)
            self.assertEqual(materialized["release"]["build_run_id"], RUN_ID)
            self.assertNotEqual(
                materialized["release"]["release_provenance_sha256"], "UNSET"
            )
            self.assertNotEqual(
                materialized["release"]["build_run_evidence_sha256"], "UNSET"
            )

            cancelled = self.run_evidence()
            cancelled["conclusion"] = "cancelled"
            run_evidence_path.write_text(json.dumps(cancelled), encoding="utf-8")
            with self.assertRaisesRegex(
                release_provenance.ReleaseProvenanceError,
                "complete successfully",
            ):
                prelive_v5_release.materialize_contract(
                    self.template(),
                    manifest_path,
                    provenance_path,
                    run_evidence_path,
                )

    def test_environment_level_rollback_never_downgrades_candidate_database(self) -> None:
        value = self.template()
        release = value["release"]
        release.update(
            {
                "source_sha": RELEASE_SHA,
                "build_run_id": RUN_ID,
                "build_run_evidence_sha256": f"sha256:{'3' * 64}",
                "release_manifest_sha256": f"sha256:{'1' * 64}",
                "release_provenance_sha256": f"sha256:{'2' * 64}",
            }
        )
        plan = prelive_v5_release.render_plan(value)
        self.assertEqual(plan["rollback"]["scope"], "environment")
        self.assertEqual(plan["rollback"]["target"], "environment-a")
        self.assertEqual(plan["rollback"]["candidate_database_action"], "none")
        self.assertLess(
            plan["ordered_gates"].index("shadow-gate-15m"),
            plan["ordered_gates"].index("shadow-gate-1h"),
        )

    def test_bridge_and_same_database_rollback_scope_are_absent(self) -> None:
        serialized = json.dumps(self.template(), sort_keys=True).lower()
        self.assertNotIn("m011-bridge", serialized)
        self.assertNotIn("same-database", serialized)
        self.assertNotIn("down migration", serialized)
        self.assertNotIn("historical transfer", serialized)

    def test_database_gate_is_read_only_and_never_names_old_data_source(self) -> None:
        gate = (
            self.repo_root / "scripts" / "prelive-v5-fresh-database-gate.sh"
        ).read_text(encoding="utf-8")
        upper = gate.upper()
        for forbidden in (
            "DELETE FROM",
            "TRUNCATE ",
            "DROP DATABASE",
            "VACUUM FULL",
            "PG_DUMP",
            "PG_RESTORE",
        ):
            self.assertNotIn(forbidden, upper)
        self.assertIn("PHOENIX_V5_CANDIDATE_DATABASE_NAME", gate)
        self.assertIn("PHOENIX_V4_FALLBACK_DATABASE_NAME", gate)
        self.assertIn("INITIALIZE_EMPTY_PHOENIX_V5_DATABASE", gate)

    def test_schema_is_strict_and_declares_environment_rollback(self) -> None:
        schema = json.loads(
            (
                self.repo_root
                / "schemas"
                / "phoenix-prelive-v5-release.schema.json"
            ).read_text(encoding="utf-8")
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(
            schema["properties"]["schema"]["const"], prelive_v5_release.SCHEMA
        )
        self.assertEqual(
            schema["properties"]["rollback"]["properties"]["scope"]["const"],
            "environment",
        )

    def test_ci_invokes_fresh_schema_and_existing_runtime_contracts(self) -> None:
        ci = (
            self.repo_root / ".github" / "workflows" / "ci.yml"
        ).read_text(encoding="utf-8")
        for required in (
            "TestFreshV5DatabaseInitializesFromZeroAndIsIdempotent",
            "--test postgres_outbox_integration",
            "--test jetstream_integration",
            "--test postgres_decision_integration",
        ):
            self.assertIn(required, ci)


if __name__ == "__main__":
    unittest.main()
