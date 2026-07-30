import datetime as dt
import json
import os
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest import mock

from scripts import economic_activation_runner as runner


RELEASE_SHA = "a" * 40
ENGINE_DIGEST = f"sha256:{'b' * 64}"
REQUEST_ID = "12345678-1234-4234-9234-123456789abc"


def timestamp(value: dt.datetime) -> str:
    return value.isoformat(timespec="microseconds").replace("+00:00", "Z")


def request_value(
    *,
    created_at: dt.datetime | None = None,
    expires_at: dt.datetime | None = None,
) -> dict[str, object]:
    now = dt.datetime.now(dt.timezone.utc)
    created_at = created_at or now - dt.timedelta(seconds=1)
    expires_at = expires_at or now + dt.timedelta(seconds=30)
    value: dict[str, object] = {
        "schema_version": runner.REQUEST_SCHEMA,
        "request_id": REQUEST_ID,
        "binding": {
            "release_sha": RELEASE_SHA,
            "engine_image_digest": ENGINE_DIGEST,
            "route_fingerprint": "reviewed-route",
        },
        "evidence": {},
        "candidate": {
            "candidate_hash": "c" * 64,
            "fork_result_hash": "d" * 64,
        },
        "created_at": timestamp(created_at),
        "expires_at": timestamp(expires_at),
        "request_hash": "0" * 64,
    }
    value["request_hash"] = runner._contract_hash(
        value,
        "request_hash",
        "economic-activation-request",
        runner.REQUEST_SCHEMA,
    )
    return value


def materialization(request: dict[str, object]) -> bytes:
    now = dt.datetime.now(dt.timezone.utc)
    binding = dict(request["binding"])  # type: ignore[arg-type]
    binding["expires_at"] = timestamp(now + dt.timedelta(minutes=5))
    readiness: dict[str, object] = {
        "schema_version": runner.READINESS_SCHEMA,
        "binding": binding,
        "evidence": {},
        "readiness_hash": "0" * 64,
    }
    readiness["readiness_hash"] = runner._contract_hash(
        readiness,
        "readiness_hash",
        "canary-readiness",
        runner.READINESS_SCHEMA,
    )
    authorization: dict[str, object] = {
        "schema_version": runner.AUTHORIZATION_SCHEMA,
        "authorization": {
            "route_fingerprint": "reviewed-route",
            "route_policy_hash": None,
            "executor_code_hash": None,
            "maximum_reviewed_input_wei": 10_000_000_000_000_000,
            "one_transaction_at_a_time": True,
            "reviewed_ladder_only": True,
            "automatic_disarm_required": True,
            "expires_at": timestamp(now + dt.timedelta(minutes=5)),
        },
        "authorization_hash": "0" * 64,
    }
    authorization["authorization_hash"] = runner._contract_hash(
        authorization,
        "authorization_hash",
        "automation-authorization",
        runner.AUTHORIZATION_SCHEMA,
    )
    return runner._canonical(
        {
            "schema_version": runner.MATERIALIZATION_SCHEMA,
            "request_id": request["request_id"],
            "request_hash": request["request_hash"],
            "readiness": readiness,
            "authorization": authorization,
        }
    )


class EconomicActivationRunnerTests(unittest.TestCase):
    def test_canonical_materialization_is_bound_and_accepted(self) -> None:
        request = request_value()
        readiness, authorization = runner._validate_materialization(
            materialization(request), request
        )
        self.assertEqual(readiness["binding"], request["binding"] | {
            "expires_at": readiness["binding"]["expires_at"]  # type: ignore[index]
        })
        self.assertEqual(
            authorization["authorization"]["route_fingerprint"],  # type: ignore[index]
            "reviewed-route",
        )

    def test_materialization_mutation_is_rejected(self) -> None:
        request = request_value()
        value = json.loads(materialization(request))
        value["authorization"]["authorization"]["one_transaction_at_a_time"] = False
        value["authorization"]["authorization_hash"] = runner._contract_hash(
            value["authorization"],
            "authorization_hash",
            "automation-authorization",
            runner.AUTHORIZATION_SCHEMA,
        )
        with self.assertRaisesRegex(
            runner.ActivationRunnerError, "materialization_bounds_invalid"
        ):
            runner._validate_materialization(runner._canonical(value), request)

    def test_runner_commands_are_fixed_and_do_not_use_a_shell(self) -> None:
        paths = runner.RunnerPaths(
            deploy=Path("/opt/phoenix/deploy"),
            environment=Path("/etc/phoenix/phoenix.env"),
            python=Path("/usr/bin/python3"),
        )
        command = runner._fixed_compose_command(paths, Path("/fixed/request.json"))
        self.assertEqual(command[0].replace("\\", "/"), "/usr/bin/python3")
        self.assertEqual(command[-2:], ["autonomous-control", "materialize-activation-contracts"])
        self.assertNotIn("sh", command)
        self.assertNotIn("bash", command)
        self.assertEqual(
            str(paths.deploy / "activate-economic-canary.sh").replace("\\", "/"),
            "/opt/phoenix/deploy/activate-economic-canary.sh",
        )

    @unittest.skipUnless(os.name == "posix", "POSIX metadata contract")
    def test_valid_malformed_expired_and_oversized_requests(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            path = root / f"activation-request-{REQUEST_ID}.json"
            value = request_value()
            path.write_bytes(runner._canonical(value) + b"\n")
            path.chmod(0o600)
            with (
                mock.patch.object(runner, "REQUEST_OWNER_UID", os.getuid()),
                mock.patch.object(runner, "REQUEST_OWNER_GID", os.getgid()),
            ):
                observed, _ = runner._load_request(path)
                self.assertEqual(observed["request_hash"], value["request_hash"])

                malformed = dict(value)
                malformed.pop("candidate")
                path.write_bytes(runner._canonical(malformed))
                with self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_contract_invalid"
                ):
                    runner._load_request(path)

                expired = request_value(
                    created_at=dt.datetime.now(dt.timezone.utc)
                    - dt.timedelta(seconds=90),
                    expires_at=dt.datetime.now(dt.timezone.utc)
                    - dt.timedelta(seconds=30),
                )
                path.write_bytes(runner._canonical(expired))
                with self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_expired"
                ):
                    runner._load_request(path)

                path.write_bytes(b"x" * (runner.MAX_REQUEST_BYTES + 1))
                with self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_metadata_invalid"
                ):
                    runner._load_request(path)

    @unittest.skipUnless(os.name == "posix", "POSIX metadata contract")
    def test_symlink_hardlink_and_wrong_ownership_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / f"activation-request-{REQUEST_ID}.json"
            source.write_bytes(runner._canonical(request_value()))
            source.chmod(0o600)
            alias_id = str(uuid.uuid4())
            symlink = root / f"activation-request-{alias_id}.json"
            symlink.symlink_to(source)
            with (
                mock.patch.object(runner, "REQUEST_OWNER_UID", os.getuid()),
                mock.patch.object(runner, "REQUEST_OWNER_GID", os.getgid()),
                self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_metadata_invalid"
                ),
            ):
                runner._load_request(symlink)

            hardlink = root / f"activation-request-{uuid.uuid4()}.json"
            os.link(source, hardlink)
            with (
                mock.patch.object(runner, "REQUEST_OWNER_UID", os.getuid()),
                mock.patch.object(runner, "REQUEST_OWNER_GID", os.getgid()),
                self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_metadata_invalid"
                ),
            ):
                runner._load_request(source)

            hardlink.unlink()
            with (
                mock.patch.object(runner, "REQUEST_OWNER_UID", os.getuid() + 1),
                mock.patch.object(runner, "REQUEST_OWNER_GID", os.getgid()),
                self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_metadata_invalid"
                ),
            ):
                runner._load_request(source)

    @unittest.skipUnless(os.name == "posix", "POSIX replay marker")
    def test_replayed_candidate_and_fork_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            consumed = root / "consumed"
            consumed.mkdir()
            paths = runner.RunnerPaths(state=root)
            with mock.patch.object(os, "fchown"):
                runner._consume_marker(paths, request_value())
                with self.assertRaisesRegex(
                    runner.ActivationRunnerError, "request_replayed"
                ):
                    runner._consume_marker(paths, request_value())


if __name__ == "__main__":
    unittest.main()
