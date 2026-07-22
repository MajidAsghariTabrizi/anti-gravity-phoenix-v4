import copy
import json
import os
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import phoenix_shadow_deploy as gateway
from scripts import release_provenance


CANDIDATE_SHA = "a" * 40
ROLLBACK_SHA = "b" * 40
CANDIDATE_RUN = "30000000101"
ROLLBACK_RUN = "30000000100"
DEPLOY_RUN = "30000000200"
DEPLOY_ATTEMPT = "1"


def identity() -> gateway.Identity:
    return gateway.Identity.parse(
        [
            CANDIDATE_SHA,
            ROLLBACK_SHA,
            CANDIDATE_RUN,
            ROLLBACK_RUN,
            DEPLOY_RUN,
            DEPLOY_ATTEMPT,
        ]
    )


class IdentityTests(unittest.TestCase):
    def test_exact_identity_derives_stage_and_unit(self) -> None:
        value = identity()
        self.assertEqual(
            value.stage.as_posix(),
            f"/tmp/phoenix-shadow-deploy-{DEPLOY_RUN}-1-{CANDIDATE_SHA}",
        )
        self.assertEqual(value.unit, f"phoenix-shadow-deploy-{DEPLOY_RUN}-1")

    def test_malformed_identity_is_rejected(self) -> None:
        valid = [
            CANDIDATE_SHA,
            ROLLBACK_SHA,
            CANDIDATE_RUN,
            ROLLBACK_RUN,
            DEPLOY_RUN,
            DEPLOY_ATTEMPT,
        ]
        mutations = (
            valid[:-1],
            ["A" * 40, *valid[1:]],
            [CANDIDATE_SHA, CANDIDATE_SHA, *valid[2:]],
            [*valid[:2], "0", *valid[3:]],
            [*valid[:4], "run", DEPLOY_ATTEMPT],
            [*valid[:5], "1;id"],
        )
        for values in mutations:
            with self.subTest(values=values), self.assertRaises(gateway.GatewayError):
                gateway.Identity.parse(list(values))

    def test_failed_stage_cannot_be_reused_under_another_run_identity(self) -> None:
        first = identity()
        second = gateway.Identity.parse(
            [
                CANDIDATE_SHA,
                ROLLBACK_SHA,
                CANDIDATE_RUN,
                ROLLBACK_RUN,
                str(int(DEPLOY_RUN) + 1),
                DEPLOY_ATTEMPT,
            ]
        )
        self.assertNotEqual(first.stage, second.stage)
        self.assertNotEqual(first.unit, second.unit)


@unittest.skipUnless(os.name == "posix", "POSIX metadata semantics required")
class StageContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.stage = self.root / "stage"
        self.stage.mkdir(mode=0o700)
        self.value = identity()
        for name in self.value.staged_files:
            path = self.stage / name
            path.write_bytes(b"fixture\n")
            path.chmod(0o600)
        self.stage.chmod(0o700)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def inspect(self) -> None:
        gateway.inspect_stage(
            self.value,
            phoenix_uid=os.getuid(),
            phoenix_gid=os.getgid(),
            stage=self.stage,
        )

    def test_valid_stage_is_accepted(self) -> None:
        self.inspect()

    def test_symlink_stage_is_rejected(self) -> None:
        link = self.root / "stage-link"
        link.symlink_to(self.stage, target_is_directory=True)
        with self.assertRaisesRegex(gateway.GatewayError, "stage_directory_invalid"):
            gateway.inspect_stage(
                self.value,
                phoenix_uid=os.getuid(),
                phoenix_gid=os.getgid(),
                stage=link,
            )

    def test_symlinked_input_is_rejected(self) -> None:
        target = self.stage / "release-manifest.json"
        target.unlink()
        target.symlink_to(self.root / "outside")
        with self.assertRaisesRegex(gateway.GatewayError, "stage_file_invalid"):
            self.inspect()

    def test_fifo_input_is_rejected_without_opening_it(self) -> None:
        target = self.stage / "release-manifest.json"
        target.unlink()
        os.mkfifo(target, 0o600)
        with self.assertRaisesRegex(gateway.GatewayError, "stage_file_invalid"):
            self.inspect()

    def test_nested_extra_and_missing_members_are_rejected(self) -> None:
        mutations = ("nested", "extra", "missing")
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                if mutation == "nested":
                    changed = self.stage / "nested"
                    changed.mkdir()
                elif mutation == "extra":
                    changed = self.stage / "extra.json"
                    changed.write_text("{}", encoding="utf-8")
                    changed.chmod(0o600)
                else:
                    changed = self.stage / "release-manifest.json"
                    changed.unlink()
                with self.assertRaisesRegex(
                    gateway.GatewayError, "stage_member_set_invalid"
                ):
                    self.inspect()
                if mutation == "missing":
                    changed.write_text("{}", encoding="utf-8")
                    changed.chmod(0o600)
                elif changed.is_dir():
                    changed.rmdir()
                else:
                    changed.unlink()

    def test_hard_link_is_rejected(self) -> None:
        source = self.stage / "release-manifest.json"
        linked = self.root / "hard-link"
        os.link(source, linked)
        with self.assertRaisesRegex(gateway.GatewayError, "stage_file_invalid"):
            self.inspect()

    def test_wrong_owner_contract_is_rejected(self) -> None:
        with self.assertRaisesRegex(gateway.GatewayError, "stage_directory_invalid"):
            gateway.inspect_stage(
                self.value,
                phoenix_uid=os.getuid() + 1,
                phoenix_gid=os.getgid(),
                stage=self.stage,
            )

    def test_unsafe_mode_is_rejected(self) -> None:
        target = self.stage / "release-manifest.json"
        target.chmod(0o640)
        with self.assertRaisesRegex(gateway.GatewayError, "stage_file_invalid"):
            self.inspect()

    def test_oversized_input_is_rejected(self) -> None:
        target = self.stage / "release-manifest.json"
        with target.open("wb") as handle:
            handle.truncate(gateway.MAX_JSON_BYTES + 1)
        target.chmod(0o600)
        with self.assertRaisesRegex(gateway.GatewayError, "stage_file_invalid"):
            self.inspect()

    def test_content_toctou_is_rejected_during_stage_lock(self) -> None:
        target = self.stage / "release-manifest.json"
        stage_fd = os.open(self.stage, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        descriptor = os.open(target, os.O_RDONLY)
        try:
            with mock.patch.object(
                gateway,
                "_read_fd_bounded",
                side_effect=(b"first123", b"second12"),
            ):
                with self.assertRaisesRegex(
                    gateway.GatewayError, "stage_file_changed_during_lock"
                ):
                    gateway._read_stable_stage_payload(
                        stage_fd,
                        target.name,
                        descriptor,
                        gateway.MAX_JSON_BYTES,
                    )
        finally:
            os.close(descriptor)
            os.close(stage_fd)

    def test_broken_symlink_state_root_is_rejected(self) -> None:
        state_root = self.root / "state-root"
        state_root.symlink_to(self.root / "missing", target_is_directory=True)
        with self.assertRaisesRegex(gateway.GatewayError, "state_root_invalid"):
            gateway._safe_root_directory(state_root)


@unittest.skipUnless(os.name == "posix", "POSIX metadata semantics required")
class InstallationContractTests(unittest.TestCase):
    def test_valid_installation_and_user_writable_rejection(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gateway_path = root / "phoenix-shadow-deploy-gateway"
            libexec = root / "libexec"
            libexec.mkdir(mode=0o750)
            libexec.chmod(0o750)
            gateway_path.write_text("#!/bin/sh\n", encoding="ascii")
            gateway_path.chmod(0o755)
            for name, mode in gateway.TRUSTED_HELPERS.items():
                path = libexec / name
                path.write_text("fixture\n", encoding="ascii")
                path.chmod(mode)
            gateway.verify_installation(
                gateway_path,
                libexec,
                expected_uid=os.getuid(),
                expected_gid=os.getgid(),
            )
            gateway_path.chmod(0o775)
            with self.assertRaisesRegex(
                gateway.GatewayError, "trusted_installation_invalid"
            ):
                gateway.verify_installation(
                    gateway_path,
                    libexec,
                    expected_uid=os.getuid(),
                    expected_gid=os.getgid(),
                )


class ReleaseContractTests(unittest.TestCase):
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
            (directory / f"{name}.json").write_text(
                json.dumps(value), encoding="utf-8"
            )

    def _full_release(
        self, directory: Path, release_sha: str, run_id: str
    ) -> tuple[dict, dict, Path, Path]:
        fragments = directory / "fragments"
        assets = directory / "assets"
        self._write_fragments(fragments, release_sha, run_id)
        self._write_assets(assets, release_sha)
        manifest_path = directory / "release-manifest.json"
        provenance_path = directory / "release-provenance.json"
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
                created_at="2026-07-22T00:00:00Z",
            )
        return manifest, provenance, manifest_path, provenance_path

    def _inherited_inputs(self) -> tuple[Path, dict, dict]:
        (
            rollback_manifest,
            _,
            rollback_manifest_path,
            rollback_provenance_path,
        ) = self._full_release(self.root / "rollback", ROLLBACK_SHA, ROLLBACK_RUN)
        candidate = self.root / "candidate"
        fragments = candidate / "fragments"
        assets = candidate / "assets"
        self._write_fragments(fragments, CANDIDATE_SHA, CANDIDATE_RUN)
        self._write_assets(assets, CANDIDATE_SHA)
        release_provenance.write_inherited_fragments(
            fragments,
            CANDIDATE_SHA,
            CANDIDATE_RUN,
            release_provenance.RELEASE_INTENT,
            ROLLBACK_SHA,
            ROLLBACK_RUN,
            rollback_manifest_path,
            rollback_provenance_path,
        )
        candidate_manifest_path = candidate / "release-manifest.json"
        candidate_provenance_path = candidate / "release-provenance.json"
        with mock.patch.object(
            release_provenance.release_assets, "verify_release_assets"
        ):
            candidate_manifest, candidate_provenance = (
                release_provenance.assemble_release(
                    fragments,
                    assets,
                    CANDIDATE_SHA,
                    CANDIDATE_RUN,
                    release_provenance.RELEASE_INTENT,
                    candidate_manifest_path,
                    candidate_provenance_path,
                    created_at="2026-07-22T00:00:00Z",
                    protected_base_sha=ROLLBACK_SHA,
                    protected_base_build_run_id=ROLLBACK_RUN,
                    protected_base_manifest=rollback_manifest_path,
                    protected_base_provenance=rollback_provenance_path,
                )
            )
        inputs = self.root / "inputs"
        inputs.mkdir()
        shutil_map = {
            candidate_manifest_path: inputs / "release-manifest.json",
            candidate_provenance_path: inputs / "release-provenance.json",
            rollback_manifest_path: inputs / "rollback-manifest.json",
            rollback_provenance_path: inputs / "rollback-provenance.json",
        }
        for source, target in shutil_map.items():
            target.write_bytes(source.read_bytes())
        for source in assets.iterdir():
            (inputs / source.name).write_bytes(source.read_bytes())
        return inputs, candidate_manifest, rollback_manifest

    def test_valid_seven_image_inherited_release_is_accepted(self) -> None:
        inputs, _, _ = self._inherited_inputs()
        with mock.patch.object(gateway.release_assets, "verify_release_assets"):
            gateway.validate_release_inputs(identity(), inputs)

    def test_legacy_seven_image_v1_release_remains_accepted(self) -> None:
        _, _, rollback_manifest, rollback_provenance = self._full_release(
            self.root / "rollback-v1", ROLLBACK_SHA, ROLLBACK_RUN
        )
        _, _, candidate_manifest, candidate_provenance = self._full_release(
            self.root / "candidate-v1", CANDIDATE_SHA, CANDIDATE_RUN
        )
        inputs = self.root / "legacy-inputs"
        inputs.mkdir()
        for source, name in (
            (candidate_manifest, "release-manifest.json"),
            (candidate_provenance, "release-provenance.json"),
            (rollback_manifest, "rollback-manifest.json"),
            (rollback_provenance, "rollback-provenance.json"),
        ):
            (inputs / name).write_bytes(source.read_bytes())
        for source in (self.root / "candidate-v1" / "assets").iterdir():
            (inputs / source.name).write_bytes(source.read_bytes())
        with mock.patch.object(gateway.release_assets, "verify_release_assets"):
            gateway.validate_release_inputs(identity(), inputs)

    def test_release_identity_mismatch_is_rejected(self) -> None:
        inputs, _, _ = self._inherited_inputs()
        mutations = (
            gateway.Identity.parse(
                [
                    "c" * 40,
                    ROLLBACK_SHA,
                    CANDIDATE_RUN,
                    ROLLBACK_RUN,
                    DEPLOY_RUN,
                    DEPLOY_ATTEMPT,
                ]
            ),
            gateway.Identity.parse(
                [
                    CANDIDATE_SHA,
                    "c" * 40,
                    CANDIDATE_RUN,
                    ROLLBACK_RUN,
                    DEPLOY_RUN,
                    DEPLOY_ATTEMPT,
                ]
            ),
            gateway.Identity.parse(
                [
                    CANDIDATE_SHA,
                    ROLLBACK_SHA,
                    "30000000991",
                    ROLLBACK_RUN,
                    DEPLOY_RUN,
                    DEPLOY_ATTEMPT,
                ]
            ),
        )
        for changed_identity in mutations:
            with self.subTest(changed_identity=changed_identity), mock.patch.object(
                gateway.release_assets, "verify_release_assets"
            ):
                with self.assertRaisesRegex(
                    gateway.GatewayError, "release_pair_invalid"
                ):
                    gateway.validate_release_inputs(changed_identity, inputs)

    def test_canonical_image_set_and_bindings_are_enforced(self) -> None:
        mutations = (
            ("missing_live_executor", "live-executor", None),
            ("unexpected_eighth", "unexpected-image", {}),
            ("protected_repository", "feed-ingestor", "repository"),
            ("protected_tag", "recorder", "tag"),
            ("protected_digest", "feed-ingestor", "digest"),
            ("protected_source_sha", "recorder", "source_sha"),
            ("protected_source_run", "feed-ingestor", "source_build_run_id"),
            ("protected_oci_revision", "recorder", "oci_revision"),
            ("non_protected_binding", "dashboard", "source_sha"),
            ("non_protected_run_binding", "rpc-gateway", "source_build_run_id"),
        )
        for label, image_name, field in mutations:
            with self.subTest(label=label):
                inputs, manifest, _ = self._inherited_inputs()
                changed = copy.deepcopy(manifest)
                if label == "missing_live_executor":
                    del changed["images"][image_name]
                elif label == "unexpected_eighth":
                    changed["images"][image_name] = {}
                elif field == "digest":
                    changed["images"][image_name][field] = f"sha256:{'f' * 64}"
                elif field in ("source_sha", "oci_revision"):
                    changed["images"][image_name][field] = "c" * 40
                elif field == "source_build_run_id":
                    changed["images"][image_name][field] = "30000000999"
                else:
                    changed["images"][image_name][field] += "-changed"
                (inputs / "release-manifest.json").write_text(
                    json.dumps(changed), encoding="utf-8"
                )
                with mock.patch.object(gateway.release_assets, "verify_release_assets"):
                    with self.assertRaisesRegex(
                        gateway.GatewayError, "release_pair_invalid"
                    ):
                        gateway.validate_release_inputs(identity(), inputs)
                self.temporary.cleanup()
                self.temporary = tempfile.TemporaryDirectory()
                self.root = Path(self.temporary.name)

    def test_tampered_archive_and_checksum_are_rejected(self) -> None:
        inputs, _, _ = self._inherited_inputs()
        with mock.patch.object(
            gateway.release_assets,
            "verify_release_assets",
            side_effect=ValueError("checksum mismatch"),
        ):
            with self.assertRaisesRegex(gateway.GatewayError, "release_assets_invalid"):
                gateway.validate_release_inputs(identity(), inputs)

    def test_gateway_has_no_duplicate_component_registry(self) -> None:
        source = Path(gateway.__file__).read_text(encoding="utf-8")
        self.assertNotIn("EXPECTED_IMAGES", source)
        self.assertNotIn('"feed-ingestor"', source)
        self.assertNotIn('"recorder"', source)
        self.assertIn("release_provenance.validate_deploy_pair", source)


class HostSafetyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        deploy = self.root / "deploy"
        deploy.mkdir()
        (deploy / "current-release").write_text(f"{ROLLBACK_SHA}\n", encoding="ascii")
        (deploy / "release-assets.sha").write_text(
            f"{ROLLBACK_SHA}\n", encoding="ascii"
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_pointer_mismatches_fail_before_tree_validation(self) -> None:
        for name, code in (
            ("current-release", "current_release_mismatch"),
            ("release-assets.sha", "release_assets_pointer_mismatch"),
        ):
            with self.subTest(name=name):
                path = self.root / "deploy" / name
                original = path.read_text(encoding="ascii")
                path.write_text(f"{'c' * 40}\n", encoding="ascii")
                with mock.patch.object(gateway, "validate_shadow_controls"), mock.patch.object(
                    gateway, "validate_immutable_tree"
                ):
                    with self.assertRaisesRegex(gateway.GatewayError, code):
                        gateway.validate_host(
                            identity(),
                            deploy_root=self.root,
                            env_file=self.root / "env",
                            service_probe=lambda _: False,
                            enforce_control_permissions=False,
                        )
                path.write_text(original, encoding="ascii")

    def test_live_executor_and_migration_runner_are_rejected(self) -> None:
        for active, code in (
            ("live-executor", "live_executor_active"),
            ("migration-runner", "migration_runner_active"),
        ):
            with self.subTest(active=active), mock.patch.object(
                gateway, "validate_shadow_controls"
            ), mock.patch.object(gateway, "validate_immutable_tree"):
                with self.assertRaisesRegex(gateway.GatewayError, code):
                    gateway.validate_host(
                        identity(),
                        deploy_root=self.root,
                        env_file=self.root / "env",
                        service_probe=lambda service: service == active,
                        enforce_control_permissions=False,
                    )

    def test_unsafe_shadow_controls_are_rejected(self) -> None:
        with mock.patch.object(
            gateway,
            "validate_shadow_controls",
            side_effect=gateway.GatewayError("shadow_controls_invalid"),
        ), mock.patch.object(gateway, "validate_immutable_tree"):
            with self.assertRaisesRegex(gateway.GatewayError, "shadow_controls_invalid"):
                gateway.validate_host(
                    identity(),
                    deploy_root=self.root,
                    env_file=self.root / "env",
                    service_probe=lambda _: False,
                    enforce_control_permissions=False,
                )

    def test_shadow_value_contract_is_fail_closed_and_sanitized(self) -> None:
        safe = {
            "PHOENIX_MODE": "SHADOW",
            "LIVE_EXECUTION": "false",
            "PHOENIX_ENV": "production",
            "CHAIN_ID": "42161",
            "LIVE_EXECUTOR_ARMED": "false",
            "LIVE_EXECUTOR_KILL_SWITCH": "true",
            "SIGNER_PRIVATE_KEY": "",
            "LIVE_EXECUTOR_SIGNER_FILE": "",
            "WALLET_ADDRESS": "",
            "EXECUTOR_ADDRESS": "",
        }
        gateway.validate_shadow_values(safe)
        for name, unsafe in (
            ("PHOENIX_MODE", "LIVE"),
            ("LIVE_EXECUTION", "true"),
            ("LIVE_EXECUTOR_ARMED", "true"),
            ("LIVE_EXECUTOR_KILL_SWITCH", "false"),
            ("SIGNER_PRIVATE_KEY", "nonempty-test-value"),
        ):
            with self.subTest(name=name):
                changed = dict(safe)
                changed[name] = unsafe
                with self.assertRaisesRegex(
                    gateway.GatewayError, "^shadow_controls_invalid$"
                ) as captured:
                    gateway.validate_shadow_values(changed)
                self.assertNotIn(unsafe, str(captured.exception))


class OrchestrationTests(unittest.TestCase):
    def test_rollback_uses_only_installed_root_helper(self) -> None:
        with mock.patch.object(gateway, "_run_checked") as run, mock.patch.object(
            gateway, "_read_pointer", return_value=ROLLBACK_SHA
        ), mock.patch.object(gateway, "validate_shadow_controls"):
            result = gateway._restore_rollback(identity(), {"PATH": "/usr/bin"})
        self.assertEqual(result, "succeeded")
        self.assertEqual(
            run.call_args.args[0],
            ["/bin/sh", str(gateway.LIBEXEC_DIR / "rollback-release.sh")],
        )

    def test_candidate_tree_is_verified_before_deployment(self) -> None:
        source = Path(gateway.__file__).read_text(encoding="utf-8")
        verification = source.index("validate_immutable_tree(candidate_tree")
        deployment = source.index('["/bin/sh", str(deploy_script), identity.candidate_sha]')
        self.assertLess(verification, deployment)
        self.assertIn("rollback_result = _restore_rollback", source)
        self.assertIn("except Exception as exc:", source)
        self.assertIn('else "internal_error"', source)


if __name__ == "__main__":
    unittest.main()
