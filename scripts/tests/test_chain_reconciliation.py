from __future__ import annotations

import copy
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from jsonschema import Draft202012Validator

from scripts.phoenix_release.chain_reconciliation import (
    ReconciliationError,
    build_evidence,
    collect_provider_evidence,
    evidence_path,
    read_evidence,
    write_evidence,
)
from scripts.phoenix_release.gateway import (
    GatewayError,
    HostPaths,
    _readiness_chain_reconciliation,
    reconcile_chain_evidence,
)
from scripts.phoenix_release.cli import parser as release_parser


ACTIVE_SHA = "a" * 40
ASSETS_SHA = ACTIVE_SHA
MAIN_SHA = "c" * 40
EXECUTOR = "0x" + "1" * 40
OWNER_TRANSACTION = "0x" + "2" * 64
BLOCK_HASH = "0x" + "3" * 64
PROVIDERS = [
    "https://primary.example/rpc/private",
    "https://secondary.example/rpc/private",
]
ROOT = Path(__file__).resolve().parents[2]
FAIL_CLOSED_CONTROLS = {
    "active_attempts": 0,
    "armed": False,
    "execution_mode": "disarmed",
    "kill_switch": True,
    "open_routes": 0,
    "outbox_ack_pending": 0,
    "outbox_claimable": 0,
    "outbox_pending": 0,
    "unresolved_submissions": 0,
}


def rpc_result(
    url: str,
    method: str,
    params: list[object],
) -> object:
    del url
    if method == "eth_chainId":
        return "0xa4b1"
    if method == "eth_call":
        assert params == [
            {"to": EXECUTOR, "data": "0x5c975abb"},
            "latest",
        ]
        return "0x" + "0" * 63 + "1"
    if method == "eth_getTransactionReceipt":
        assert params == [OWNER_TRANSACTION]
        return {
            "blockHash": BLOCK_HASH,
            "blockNumber": "0x123",
            "status": "0x1",
            "transactionHash": OWNER_TRANSACTION,
        }
    if method == "eth_getTransactionByHash":
        assert params == [OWNER_TRANSACTION]
        return {
            "blockNumber": "0x123",
            "hash": OWNER_TRANSACTION,
            "input": "0x16c38b3c" + "0" * 64,
            "to": EXECUTOR,
        }
    raise AssertionError(method)


def provider_evidence():
    return collect_provider_evidence(
        PROVIDERS,
        EXECUTOR,
        OWNER_TRANSACTION,
        call=rpc_result,
    )


def valid_evidence():
    return build_evidence(
        active_release_sha=ACTIVE_SHA,
        release_assets_sha=ASSETS_SHA,
        protected_main_sha=MAIN_SHA,
        release_platform_manifest_sha256="sha256:" + "4" * 64,
        executor_address=EXECUTOR,
        owner_transaction_hash=OWNER_TRANSACTION,
        historical_contract_paused=False,
        runtime={
            "active_attempts": 0,
            "armed": False,
            "execution_mode": "disarmed",
            "kill_switch": True,
            "live_executor_stopped": True,
            "open_routes": 0,
            "unresolved_submissions": 0,
        },
        providers=provider_evidence(),
    )


class ProviderEvidenceTests(unittest.TestCase):
    def test_valid_two_provider_paused_reconciliation(self) -> None:
        observed = provider_evidence()
        self.assertEqual(len(observed), 2)
        self.assertNotEqual(
            observed[0]["provider_identity"],
            observed[1]["provider_identity"],
        )
        self.assertTrue(observed[0]["paused"])
        self.assertEqual(observed[0]["chain_id"], "0xa4b1")

    def test_evidence_matches_published_schema(self) -> None:
        schema = json.loads(
            (
                ROOT
                / "schemas/phoenix-chain-reconciliation.schema.json"
            ).read_text(encoding="utf-8")
        )
        Draft202012Validator.check_schema(schema)
        Draft202012Validator(schema).validate(valid_evidence())

    def test_historical_unpause_is_classified(self) -> None:
        observed = provider_evidence()
        self.assertEqual(observed[0]["classification"], "unpause")
        self.assertEqual(observed[0]["input_selector"], "0x16c38b3c")
        self.assertFalse(observed[0]["set_paused_value"])

    def test_provider_disagreement_rejects(self) -> None:
        def disagreeing(
            url: str,
            method: str,
            params: list[object],
        ) -> object:
            value = rpc_result(url, method, params)
            if (
                url == PROVIDERS[1]
                and method == "eth_getTransactionReceipt"
            ):
                value = dict(value)
                value["blockHash"] = "0x" + "5" * 64
            return value

        with self.assertRaisesRegex(
            ReconciliationError,
            "CHAIN_EVIDENCE_PROVIDER_DISAGREEMENT",
        ):
            collect_provider_evidence(
                PROVIDERS,
                EXECUTOR,
                OWNER_TRANSACTION,
                call=disagreeing,
            )

    def test_paused_false_rejects(self) -> None:
        def unpaused(
            url: str,
            method: str,
            params: list[object],
        ) -> object:
            if method == "eth_call":
                return "0x" + "0" * 64
            return rpc_result(url, method, params)

        with self.assertRaisesRegex(
            ReconciliationError,
            "CHAIN_EVIDENCE_CONTRACT_NOT_PAUSED",
        ):
            collect_provider_evidence(
                PROVIDERS,
                EXECUTOR,
                OWNER_TRANSACTION,
                call=unpaused,
            )

    def test_wrong_chain_rejects(self) -> None:
        def wrong_chain(
            url: str,
            method: str,
            params: list[object],
        ) -> object:
            if method == "eth_chainId":
                return "0x1"
            return rpc_result(url, method, params)

        with self.assertRaisesRegex(
            ReconciliationError,
            "CHAIN_EVIDENCE_CHAIN_ID_INVALID",
        ):
            collect_provider_evidence(
                PROVIDERS,
                EXECUTOR,
                OWNER_TRANSACTION,
                call=wrong_chain,
            )

    def test_executor_mismatch_rejects(self) -> None:
        def wrong_executor(
            url: str,
            method: str,
            params: list[object],
        ) -> object:
            value = rpc_result(url, method, params)
            if method == "eth_getTransactionByHash":
                value = dict(value)
                value["to"] = "0x" + "9" * 40
            return value

        with self.assertRaisesRegex(
            ReconciliationError,
            "CHAIN_EVIDENCE_TRANSACTION_INVALID",
        ):
            collect_provider_evidence(
                PROVIDERS,
                EXECUTOR,
                OWNER_TRANSACTION,
                call=wrong_executor,
            )


@unittest.skipUnless(os.name == "posix", "POSIX metadata contract")
class AppendOnlyEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.path = evidence_path(self.root, ACTIVE_SHA)
        self.uid = os.getuid()
        self.gid = os.getgid()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write(self, value=None) -> bool:
        return write_evidence(
            self.path,
            value or valid_evidence(),
            expected_uid=self.uid,
            expected_gid=self.gid,
        )

    def test_exact_idempotent_rerun(self) -> None:
        self.assertTrue(self.write())
        self.assertFalse(self.write())
        observed = read_evidence(
            self.path,
            expected=valid_evidence(),
            expected_uid=self.uid,
            expected_gid=self.gid,
        )
        self.assertEqual(observed, valid_evidence())

    def test_tampered_existing_evidence_rejects(self) -> None:
        self.write()
        os.chmod(self.path, 0o600)
        value = copy.deepcopy(valid_evidence())
        value["provider_agreement"] = False
        self.path.write_text(str(value), encoding="utf-8")
        os.chmod(self.path, 0o400)
        with self.assertRaises(ReconciliationError):
            self.write()

    def test_symlink_path_rejects(self) -> None:
        target = self.root / "target"
        target.write_text("{}", encoding="utf-8")
        self.path.parent.mkdir(mode=0o700)
        self.path.symlink_to(target)
        with self.assertRaisesRegex(
            ReconciliationError,
            "CHAIN_EVIDENCE_FILE_UNSAFE",
        ):
            self.write()

    def test_wrong_ownership_contract_rejects(self) -> None:
        self.write()
        with self.assertRaisesRegex(
            ReconciliationError,
            "CHAIN_EVIDENCE_DIRECTORY_UNSAFE",
        ):
            read_evidence(
                self.path,
                expected_uid=self.uid + 1,
                expected_gid=self.gid,
            )


class GatewayReconciliationTests(unittest.TestCase):
    def host_paths(self, root: Path) -> HostPaths:
        return HostPaths(
            state_root=root / "state",
            deploy_root=root / "deploy-root",
            env_file=root / "phoenix.env",
            libexec=root / "libexec",
        )

    @staticmethod
    def active_status() -> dict[str, object]:
        return {
            "active_build_run_id": 1,
            "active_release": ACTIVE_SHA,
            "autonomous_execution": False,
            "live_execution": False,
            "phoenix_mode": "SHADOW",
            "release_assets_sha": ASSETS_SHA,
        }

    def test_active_release_lock_is_required(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = self.host_paths(Path(directory))
            with (
                patch.dict(os.environ, {}, clear=True),
                patch(
                    "scripts.phoenix_release.gateway.status"
                ) as status_mock,
                self.assertRaisesRegex(
                    GatewayError,
                    "CHAIN_EVIDENCE_RELEASE_LOCK_REQUIRED",
                ),
            ):
                reconcile_chain_evidence(paths, MAIN_SHA)
            status_mock.assert_not_called()

    def test_cli_and_root_gateway_use_nonblocking_release_lock(self) -> None:
        arguments = release_parser().parse_args(
            ["reconcile-chain-evidence", MAIN_SHA]
        )
        self.assertEqual(arguments.command, "reconcile-chain-evidence")
        self.assertEqual(arguments.protected_main_sha, MAIN_SHA)
        wrapper = (
            ROOT / "scripts/phoenix-release-gateway.sh"
        ).read_text(encoding="utf-8")
        self.assertIn('if [ "$1" = reconcile-chain-evidence ]', wrapper)
        self.assertIn(
            "/usr/bin/flock -n /run/lock/phoenix-release.lock",
            wrapper,
        )
        self.assertIn("PHOENIX_RELEASE_LOCK_HELD=1", wrapper)

    def test_platform_installs_reconciliation_module(self) -> None:
        installer = (
            ROOT / "scripts/install-phoenix-release-platform.sh"
        ).read_text(encoding="utf-8")
        transport = (
            ROOT / "scripts/phoenix-release-transport.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("chain_reconciliation.py", installer)
        self.assertIn("reconcile-chain-evidence:2", transport)

    def test_active_attempt_or_unresolved_submission_rejects(self) -> None:
        for field in ("active_attempts", "unresolved_submissions"):
            with self.subTest(field=field):
                controls = dict(FAIL_CLOSED_CONTROLS)
                controls[field] = 1
                with tempfile.TemporaryDirectory() as directory:
                    paths = self.host_paths(Path(directory))
                    with (
                        patch.dict(
                            os.environ,
                            {"PHOENIX_RELEASE_LOCK_HELD": "1"},
                            clear=True,
                        ),
                        patch(
                            "scripts.phoenix_release.gateway.status",
                            return_value=self.active_status(),
                        ),
                        patch(
                            "scripts.phoenix_release.gateway.load_state",
                            return_value={
                                "contract_paused": False,
                                "owner_transaction_hash": OWNER_TRANSACTION,
                            },
                        ),
                        patch(
                            "scripts.phoenix_release.gateway._control_evidence",
                            return_value=controls,
                        ),
                        self.assertRaisesRegex(
                            GatewayError,
                            "READINESS_CONTROL_OPEN",
                        ),
                    ):
                        reconcile_chain_evidence(paths, MAIN_SHA)

    def test_active_pointer_drift_rejects(self) -> None:
        final = dict(self.active_status())
        final["active_release"] = "d" * 40
        with tempfile.TemporaryDirectory() as directory:
            paths = self.host_paths(Path(directory))
            with (
                patch.dict(
                    os.environ,
                    {"PHOENIX_RELEASE_LOCK_HELD": "1"},
                    clear=True,
                ),
                patch(
                    "scripts.phoenix_release.gateway.status",
                    side_effect=[self.active_status(), final],
                ),
                patch(
                    "scripts.phoenix_release.gateway.load_state",
                    return_value={
                        "contract_paused": False,
                        "owner_transaction_hash": OWNER_TRANSACTION,
                    },
                ),
                patch(
                    "scripts.phoenix_release.gateway._control_evidence",
                    return_value=FAIL_CLOSED_CONTROLS,
                ),
                patch(
                    "scripts.phoenix_release.gateway._live_executor_stopped"
                ),
                patch(
                    "scripts.phoenix_release.gateway._read_json",
                    return_value={"release_sha": MAIN_SHA},
                ),
                patch(
                    "scripts.phoenix_release.gateway._require_success"
                ),
                patch(
                    "scripts.phoenix_release.gateway.sha256_file",
                    return_value="sha256:" + "4" * 64,
                ),
                patch(
                    "scripts.phoenix_release.gateway._selected_environment",
                    return_value={
                        "LIVE_EXECUTOR_EXECUTOR_ADDRESS": EXECUTOR,
                        "PRODUCTION_RPC_URL": PROVIDERS[0],
                        "RPC_PROVIDER_URLS": ",".join(PROVIDERS),
                        "SECONDARY_RPC_URL": PROVIDERS[1],
                    },
                ),
                patch(
                    "scripts.phoenix_release.gateway.collect_provider_evidence",
                    return_value=provider_evidence(),
                ),
                self.assertRaisesRegex(
                    GatewayError,
                    "CHAIN_EVIDENCE_ACTIVE_POINTER_CHANGED",
                ),
            ):
                reconcile_chain_evidence(paths, MAIN_SHA)

    def test_active_pointer_mismatch_rejects(self) -> None:
        mismatched = dict(self.active_status())
        mismatched["release_assets_sha"] = "d" * 40
        with tempfile.TemporaryDirectory() as directory:
            paths = self.host_paths(Path(directory))
            with (
                patch.dict(
                    os.environ,
                    {"PHOENIX_RELEASE_LOCK_HELD": "1"},
                    clear=True,
                ),
                patch(
                    "scripts.phoenix_release.gateway.status",
                    return_value=mismatched,
                ),
                self.assertRaisesRegex(
                    GatewayError,
                    "ACTIVE_POINTER_MISMATCH",
                ),
            ):
                reconcile_chain_evidence(paths, MAIN_SHA)

    def test_readiness_without_evidence_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            paths = self.host_paths(Path(directory))
            paths.env_file.write_text(
                f"LIVE_EXECUTOR_EXECUTOR_ADDRESS={EXECUTOR}\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(
                GatewayError,
                "READINESS_CHAIN_RECONCILIATION_INVALID",
            ):
                _readiness_chain_reconciliation(
                    paths,
                    active_release=ACTIVE_SHA,
                    release_assets_sha=ASSETS_SHA,
                    candidate_sha=MAIN_SHA,
                    platform_manifest_sha256="sha256:" + "4" * 64,
                    controls=FAIL_CLOSED_CONTROLS,
                    historical_contract_paused=False,
                    owner_transaction_hash=OWNER_TRANSACTION,
                )


if __name__ == "__main__":
    unittest.main()
