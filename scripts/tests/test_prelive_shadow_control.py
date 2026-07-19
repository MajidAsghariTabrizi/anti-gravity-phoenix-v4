from __future__ import annotations

from copy import deepcopy
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
sys.path.insert(0, str(REPO))
sys.path.insert(0, str(SCRIPTS))

import prelive_shadow_control as control  # noqa: E402
from dashboard import snapshot_model as dashboard_model  # noqa: E402


FIXTURE = REPO / "fixtures" / "control-plane" / "valid-evidence.json"


class PlanTests(unittest.TestCase):
    def test_modes_are_exact_and_partition_services(self) -> None:
        expected = {"15m": 900, "1h": 3600, "6h": 21600, "24h": 86400, "continuous": None}
        for mode, duration in expected.items():
            with self.subTest(mode=mode):
                plan = control.validate_plan(control.mode_plan(mode))
                self.assertEqual(plan["duration_seconds"], duration)
                self.assertEqual(plan["continuous"], mode == "continuous")
                self.assertEqual(
                    set(plan["protected_services"]) | set(plan["optional_services"]),
                    set(plan["full_services"]),
                )
                self.assertFalse(
                    set(plan["protected_services"]) & set(plan["optional_services"])
                )
                self.assertEqual(plan["start_order"], list(control.OPTIONAL_SERVICES))
                self.assertEqual(plan["stop_order"], list(reversed(control.OPTIONAL_SERVICES)))

    def test_unknown_mode_is_rejected(self) -> None:
        with self.assertRaisesRegex(control.ControlEvidenceError, "mode_invalid"):
            control.mode_plan("5m")

    def test_plan_cli_is_deterministic(self) -> None:
        command = [
            sys.executable,
            str(SCRIPTS / "prelive_shadow_control.py"),
            "plan",
            "--mode",
            "6h",
        ]
        first = subprocess.run(command, check=True, capture_output=True).stdout
        second = subprocess.run(command, check=True, capture_output=True).stdout
        self.assertEqual(first, second)
        self.assertEqual(json.loads(first)["duration_seconds"], 21600)


class EvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.evidence = control.load_json(FIXTURE)

    def assert_code(self, expected: str, value: dict) -> None:
        with self.assertRaises(control.ControlEvidenceError) as raised:
            control.validate_evidence(value)
        self.assertEqual(raised.exception.code, expected)

    def test_valid_fixture(self) -> None:
        validated = control.validate_evidence(self.evidence)
        self.assertEqual(validated["status"], "completed")

    def test_shortened_finite_run_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        started = datetime.fromisoformat(value["started_at"].replace("Z", "+00:00"))
        value["ended_at"] = (started + timedelta(seconds=899)).isoformat().replace(
            "+00:00", "Z"
        )
        self.assert_code("duration_contract_invalid", value)

    def test_continuous_mode_cannot_claim_completion(self) -> None:
        value = deepcopy(self.evidence)
        value["mode"] = "continuous"
        value["run_id"] = "shadow-continuous-111111111111"
        value["planned_duration_seconds"] = None
        self.assert_code("duration_contract_invalid", value)

    def test_safety_drift_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["safety"]["execution_request_created"] = True
        self.assert_code("safety_invariant_failed", value)

    def test_nonzero_execution_request_boundary_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["safety"]["execution_request_count_after"] = "1"
        self.assert_code("safety_invariant_failed", value)

    def test_protected_identity_drift_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["protected_identity"]["after_sha256"] = "sha256:" + "9" * 64
        value["protected_identity"]["stable"] = False
        self.assert_code("protected_identity_changed", value)

    def test_failed_preflight_cannot_complete(self) -> None:
        value = deepcopy(self.evidence)
        value["preflight"][0]["status"] = "fail"
        self.assert_code("preflight_incomplete", value)

    def test_unhealthy_runtime_cannot_complete(self) -> None:
        value = deepcopy(self.evidence)
        value["service_states"]["during"][5]["state"] = "running_unhealthy"
        self.assert_code("runtime_not_healthy", value)

    def test_database_growth_must_reconcile(self) -> None:
        value = deepcopy(self.evidence)
        value["metrics"]["database"]["growth_bytes"] = "2"
        self.assert_code("evidence_accounting_invalid", value)

    def test_database_size_reduction_is_reported_as_signed_growth(self) -> None:
        value = deepcopy(self.evidence)
        value["metrics"]["database"] = {
            "size_start_bytes": "1200",
            "size_end_bytes": "1000",
            "growth_bytes": "-200",
        }
        self.assertEqual(
            control.validate_evidence(value)["metrics"]["database"]["growth_bytes"],
            "-200",
        )

    def test_database_clock_is_not_compared_to_the_host_clock(self) -> None:
        value = deepcopy(self.evidence)
        value["database_clock"]["preflight_baseline"] = "2026-07-16T00:00:05Z"
        value["database_clock"]["first_sample"] = "2026-07-16T00:00:10Z"
        value["samples"]["first_observed_at"] = "2026-07-16T00:00:10Z"
        self.assertEqual(control.validate_evidence(value)["status"], "completed")

    def test_sensitive_error_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["bounded_errors"].append(
            {
                "observed_at": "2026-07-16T00:15:00Z",
                "service": "rpc-gateway",
                "class": "provider_error",
                "message": "provider at https://private.invalid failed",
            }
        )
        self.assert_code("sensitive_evidence", value)

    def test_duplicate_artifact_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["artifacts"].append(deepcopy(value["artifacts"][0]))
        self.assert_code("duplicate_identity", value)

    def test_unknown_artifact_identifies_exact_field_and_kind(self) -> None:
        value = deepcopy(self.evidence)
        value["artifacts"][0]["kind"] = "future_simulation_report"
        self.assert_code(
            "artifact_kind_invalid:artifacts.kind:future_simulation_report",
            value,
        )

    def test_unsafe_artifact_kind_is_rejected_without_echoing_value(self) -> None:
        for unsafe_kind in (
            "https://private.invalid/report",
            "WALLET_ADDRESS",
        ):
            with self.subTest(unsafe_kind=unsafe_kind):
                value = deepcopy(self.evidence)
                value["artifacts"][0]["kind"] = unsafe_kind
                with self.assertRaises(control.ControlEvidenceError) as raised:
                    control.validate_evidence(value)
                self.assertEqual(
                    raised.exception.code,
                    "artifact_kind_invalid:artifacts.kind",
                )
                self.assertNotIn(unsafe_kind, raised.exception.code)

    def test_unsafe_artifact_path_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["artifacts"][0]["path"] = "../technical.json"
        self.assert_code("evidence_shape_invalid", value)

    def test_oversized_artifact_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        value["artifacts"][0]["size_bytes"] = control.MAX_INPUT_BYTES + 1
        self.assert_code("evidence_shape_invalid", value)

    def test_malformed_artifact_is_rejected(self) -> None:
        value = deepcopy(self.evidence)
        del value["artifacts"][0]["sha256"]
        self.assert_code("evidence_shape_invalid", value)

    def test_duplicate_json_key_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema_version":"one","schema_version":"two"}\n')
            with self.assertRaises(control.ControlEvidenceError) as raised:
                control.load_json(path)
            self.assertEqual(raised.exception.code, "duplicate_json_key")

    def test_atomic_promotion_requires_same_directory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_dir = root / "source"
            output_dir = root / "output"
            source_dir.mkdir()
            output_dir.mkdir()
            source = source_dir / "candidate.json"
            source.write_bytes(control.canonical_bytes(self.evidence))
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "prelive_shadow_control.py"),
                    "promote-evidence",
                    "--input",
                    str(source),
                    "--output",
                    str(output_dir / "latest.json"),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn("output_path_invalid", result.stderr)

    def test_schema_declares_strict_top_level_contract(self) -> None:
        schema = json.loads(
            (REPO / "schemas" / "prelive-shadow-control-evidence.schema.json").read_text()
        )
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(set(schema["required"]), set(self.evidence))
        self.assertEqual(
            schema["properties"]["schema_version"]["const"], control.EVIDENCE_SCHEMA
        )

    def test_fork_report_is_canonical_across_contracts_and_fixture(self) -> None:
        schema = json.loads(
            (REPO / "schemas" / "prelive-shadow-control-evidence.schema.json").read_text()
        )
        schema_kinds = schema["$defs"]["artifact"]["properties"]["kind"]["enum"]
        fixture_kinds = {row["kind"] for row in self.evidence["artifacts"]}
        self.assertEqual(schema_kinds, sorted(control.ARTIFACT_KINDS))
        self.assertIn("fork_simulation_report", dashboard_model.ARTIFACT_KINDS)
        self.assertIn("fork_simulation_report", fixture_kinds)

    def test_final_assembly_accepts_dashboard_fork_simulation_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            release = self.evidence["release"]
            metadata = {
                "schema": "phoenix.production-render.v1",
                "status": "ok",
                "mode": "SHADOW",
                "live_execution": False,
                "release_sha": release["git_sha"],
                "route_registry_hash": release["route_registry_hash"],
                "images": {
                    row["service"]: (
                        f"ghcr.io/phoenix/{row['service']}@{row['digest']}"
                    )
                    for row in release["images"]
                },
            }
            identity = {
                "schema_version": control.PROTECTED_IDENTITY_SCHEMA,
                "services": [],
                "jetstream_sha256": "sha256:" + "a" * 64,
                "fingerprint_sha256": "sha256:" + "b" * 64,
            }
            sample_safety = {
                key: nested
                for key, nested in self.evidence["safety"].items()
                if key
                not in {
                    "execution_request_count_before",
                    "execution_request_count_after",
                }
            }
            samples = []
            for observed_at, database_size, errors in (
                (self.evidence["started_at"], "1000", []),
                (
                    self.evidence["ended_at"],
                    "1200",
                    self.evidence["bounded_errors"],
                ),
            ):
                metrics = deepcopy(self.evidence["metrics"])
                metrics["database"] = {"size_bytes": database_size}
                samples.append(
                    {
                        "schema_version": control.SAMPLE_SCHEMA,
                        "observed_at": observed_at,
                        "safety": sample_safety,
                        "funnels": self.evidence["funnels"],
                        "metrics": metrics,
                        "bounded_errors": errors,
                    }
                )

            json_inputs = {
                "release-metadata.json": metadata,
                "identity-before.json": identity,
                "identity-after.json": identity,
                "states-before.json": self.evidence["service_states"]["before"],
                "states-during.json": self.evidence["service_states"]["during"],
                "states-after.json": self.evidence["service_states"]["after"],
                "artifacts.json": self.evidence["artifacts"],
            }
            for filename, payload in json_inputs.items():
                (root / filename).write_bytes(control.canonical_bytes(payload))
            (root / "release-manifest.json").write_text('{"release":"bounded"}\n')
            (root / "release-checksum.txt").write_text("bounded-checksum\n")
            (root / "preflight.tsv").write_text(
                "".join(
                    f"{row['check']}\t{row['status']}\t{row['observed_at']}\n"
                    for row in self.evidence["preflight"]
                )
            )
            (root / "samples.ndjson").write_bytes(
                b"".join(control.canonical_bytes(sample) for sample in samples)
            )
            output = root / "evidence.json"
            command = [
                sys.executable,
                str(SCRIPTS / "prelive_shadow_control.py"),
                "assemble-evidence",
                "--mode",
                "15m",
                "--status",
                "completed",
                "--started-at",
                self.evidence["started_at"],
                "--ended-at",
                self.evidence["ended_at"],
                "--database-clock-baseline",
                self.evidence["database_clock"]["preflight_baseline"],
                "--execution-request-count-before",
                "0",
                "--execution-request-count-after",
                "0",
                "--release-metadata",
                str(root / "release-metadata.json"),
                "--release-manifest",
                str(root / "release-manifest.json"),
                "--release-checksum",
                str(root / "release-checksum.txt"),
                "--preflight",
                str(root / "preflight.tsv"),
                "--identity-before",
                str(root / "identity-before.json"),
                "--identity-after",
                str(root / "identity-after.json"),
                "--states-before",
                str(root / "states-before.json"),
                "--states-during",
                str(root / "states-during.json"),
                "--states-after",
                str(root / "states-after.json"),
                "--samples",
                str(root / "samples.ndjson"),
                "--artifacts",
                str(root / "artifacts.json"),
                "--output",
                str(output),
            ]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            assembled = control.load_json(output)
            self.assertEqual(assembled["status"], "completed")
            self.assertIn(
                "fork_simulation_report",
                {row["kind"] for row in assembled["artifacts"]},
            )


class RuntimeNormalizationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.observed = "2026-07-16T00:00:00Z"

    def test_service_states_are_normalized_without_raw_runtime_data(self) -> None:
        rows = []
        for index, service in enumerate(control.FULL_SERVICES):
            status = "running" if service in control.PROTECTED_SERVICES else "missing"
            health = "healthy" if status == "running" else "missing"
            rows.append(
                f"{service}\t{status}\t{health}\tsha256:{index + 1:064x}\t0\t0"
            )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "states.tsv"
            path.write_text("\n".join(rows) + "\n")
            states = control.normalize_service_states(path, self.observed)
        self.assertEqual(len(states), len(control.FULL_SERVICES))
        self.assertEqual(states[0]["state"], "running_healthy")
        self.assertEqual(states[-1]["state"], "missing")
        self.assertNotIn("container_id", json.dumps(states))

    def test_protected_fingerprint_is_stable_and_redacted(self) -> None:
        service_rows = []
        for index, service in enumerate(control.PROTECTED_SERVICES):
            mounts = json.dumps(
                [{"Type": "volume", "Name": f"volume-{index}", "Destination": "/data"}],
                separators=(",", ":"),
            )
            service_rows.append(
                "|".join(
                    [
                        service,
                        f"{index + 1:064x}",
                        f"sha256:{index + 11:064x}",
                        "2026-07-15T23:00:00Z",
                        "2026-07-15T23:01:00Z",
                        "0",
                        mounts,
                    ]
                )
            )
        jsz = {
            "streams": [
                {"name": "PHOENIX_FEED_TX", "config": {"name": "PHOENIX_FEED_TX", "subjects": ["phoenix.feed.tx.v1"], "storage": "file", "retention": "workqueue", "max_age": 1, "max_bytes": 2, "max_msgs": 3}},
                {"name": "PHOENIX_ENGINE_INPUT", "config": {"name": "PHOENIX_ENGINE_INPUT", "subjects": ["phoenix.engine.input.v1"], "storage": "file", "retention": "workqueue", "max_age": 1, "max_bytes": 2, "max_msgs": 3}},
            ],
            "consumers": [
                {"name": "PHOENIX_RECORDER", "config": {"durable_name": "PHOENIX_RECORDER", "ack_policy": "explicit", "deliver_policy": "all", "filter_subject": "phoenix.feed.tx.v1", "max_ack_pending": 512}},
                {"name": "PHOENIX_ENGINE_SHADOW", "config": {"durable_name": "PHOENIX_ENGINE_SHADOW", "ack_policy": "explicit", "deliver_policy": "all", "filter_subject": "phoenix.engine.input.v1", "max_ack_pending": 512}},
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            services = root / "services.txt"
            jetstream = root / "jsz.json"
            services.write_text("\n".join(service_rows) + "\n")
            jetstream.write_text(json.dumps(jsz))
            first = control.protected_identity(services, jetstream)
            second = control.protected_identity(services, jetstream)
        self.assertEqual(first, second)
        serialized = json.dumps(first)
        self.assertNotIn("volume-", serialized)
        self.assertNotIn(f"{1:064x}", serialized)
        self.assertRegex(first["fingerprint_sha256"], r"^sha256:[0-9a-f]{64}$")

    def test_jetstream_contract_has_exact_real_resources(self) -> None:
        self.assertEqual(
            control.JETSTREAM_STREAM_NAMES,
            ("PHOENIX_FEED_TX", "PHOENIX_ENGINE_INPUT"),
        )
        self.assertEqual(
            control.JETSTREAM_CONSUMER_NAMES,
            ("PHOENIX_RECORDER", "PHOENIX_ENGINE_SHADOW"),
        )
        self.assertNotIn("PHOENIX_SHADOW_DISPATCH", control.JETSTREAM_CONSUMER_NAMES)
        jsz = control.load_json(REPO / "fixtures" / "control-plane" / "jetstream.json")
        identity = control._nats_resource_identity(jsz)
        metrics = control.jetstream_runtime_metrics(jsz)
        self.assertEqual(
            [row["name"] for row in identity["streams"]],
            list(control.JETSTREAM_STREAM_NAMES),
        )
        self.assertEqual(
            [row["name"] for row in identity["consumers"]],
            list(control.JETSTREAM_CONSUMER_NAMES),
        )
        self.assertEqual(metrics["streams"], "2")
        self.assertEqual(metrics["consumers"], "2")

    def test_missing_required_jetstream_resource_is_rejected(self) -> None:
        fixture = control.load_json(REPO / "fixtures" / "control-plane" / "jetstream.json")
        for resource_type, names in (
            ("streams", control.JETSTREAM_STREAM_NAMES),
            ("consumers", control.JETSTREAM_CONSUMER_NAMES),
        ):
            for name in names:
                with self.subTest(resource_type=resource_type, name=name):
                    jsz = deepcopy(fixture)
                    jsz[resource_type] = [
                        row
                        for row in jsz[resource_type]
                        if row.get("name") != name
                    ]
                    with self.assertRaises(control.ControlEvidenceError):
                        control._nats_resource_identity(jsz)
                    with self.assertRaises(control.ControlEvidenceError):
                        control.jetstream_runtime_metrics(jsz)

    def test_malformed_jetstream_config_is_rejected_without_type_error(self) -> None:
        jsz = control.load_json(REPO / "fixtures" / "control-plane" / "jetstream.json")
        jsz["streams"][0]["config"] = "malformed"
        with self.assertRaises(control.ControlEvidenceError) as raised:
            control.jetstream_runtime_metrics(jsz)
        self.assertIn(raised.exception.code, {"jetstream_identity_invalid", "jetstream_metrics_invalid"})

    def test_missing_jetstream_runtime_count_is_not_invented_as_zero(self) -> None:
        jsz = control.load_json(REPO / "fixtures" / "control-plane" / "jetstream.json")
        del jsz["consumers"][0]["num_pending"]
        with self.assertRaisesRegex(control.ControlEvidenceError, "jetstream_metrics_invalid"):
            control.jetstream_runtime_metrics(jsz)


class AttemptLogTests(unittest.TestCase):
    def test_attempt_log_is_bounded_private_and_redacted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "runtime.log"
            output = root / "positive-route-test.log"
            secret = "runtime-test-only-value"
            source.write_text(
                "dial https://rpc.invalid/private-token\n"
                "POSTGRES_DSN=postgres://phoenix:placeholder@postgres/phoenix\n"
                'ENGINE_ROUTE_REGISTRY_JSON=[{"route_id":"private"}]\n'
                "wallet=0x1111111111111111111111111111111111111111\n"
                f"opaque {secret}\n"
                "POSITIVE_ROUTE_EVIDENCE_FOUND\n",
                encoding="utf-8",
            )
            with mock.patch.dict(
                os.environ, {"PHOENIX_TEST_SECRET": secret}, clear=False
            ):
                metadata = control.retain_attempt_log(
                    source,
                    output,
                    "positive-route-test",
                    "evidence_found",
                    0,
                )
            retained = output.read_text(encoding="utf-8")
            self.assertEqual(metadata["terminal_reason"], "evidence_found")
            self.assertLessEqual(output.stat().st_size, control.MAX_ATTEMPT_LOG_BYTES)
            if os.name != "nt":
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            self.assertIn("terminal_reason=evidence_found", retained)
            self.assertIn("POSITIVE_ROUTE_EVIDENCE_FOUND", retained)
            self.assertNotIn(secret, retained)
            self.assertNotIn("rpc.invalid", retained)
            self.assertNotIn("postgres://", retained)
            self.assertNotIn("route_id", retained)
            self.assertNotIn("0x1111111111111111111111111111111111111111", retained)

    def test_attempt_log_retains_failure_reason_when_input_is_truncated(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "runtime.log"
            output = root / "positive-route-failure.log"
            source.write_bytes(
                (b"bounded runtime failure evidence\n" * 40_000)
                + b"terminal child failure\n"
            )
            metadata = control.retain_attempt_log(
                source,
                output,
                "positive-route-failure",
                "child_exit",
                1,
            )
            retained = output.read_text(encoding="utf-8")
            self.assertTrue(metadata["input_truncated"])
            self.assertTrue(metadata["output_truncated"])
            self.assertIn("terminal_reason=child_exit", retained)
            self.assertIn("source_exit_code=1", retained)
            self.assertLessEqual(output.stat().st_size, control.MAX_ATTEMPT_LOG_BYTES)


class SampleTests(unittest.TestCase):
    def test_money_path_fixture_produces_valid_sample(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "money.json"
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "prelive_money_path_report.py"),
                    "--source",
                    str(REPO / "fixtures" / "reports" / "prelive_money_path_source.json"),
                    "--metrics-input",
                    str(REPO / "fixtures" / "reports" / "prelive_money_path_metrics.json"),
                    "--format",
                    "json",
                    "--window-hours",
                    "24",
                ],
                check=True,
                capture_output=True,
            )
            output.write_bytes(result.stdout)
            sample = control.sample_from_money_path(
                control.load_json(output),
                control.load_json(REPO / "fixtures" / "control-plane" / "jetstream.json"),
            )
        control.validate_sample(sample)
        self.assertEqual(sample["funnels"]["candidate"]["feed_inputs"], "100")
        self.assertEqual(sample["funnels"]["candidate"]["candidates"], "10")
        self.assertEqual(sample["funnels"]["profitability"]["complete"], "8")
        self.assertEqual(sample["funnels"]["profitability"]["primary_profitable"], "7")
        self.assertEqual(sample["metrics"]["jetstream"]["streams"], "2")
        self.assertEqual(sample["metrics"]["jetstream"]["consumers"], "2")
        self.assertEqual(sample["metrics"]["money_path_ingress"]["feed_inputs_total"], "100")
        self.assertEqual(
            sample["metrics"]["money_path_ingress"]["dispatcher_rows_published_total"],
            "5",
        )
        self.assertFalse(sample["safety"]["execution_request_created"])

    def test_sample_append_rejects_non_monotonic_time(self) -> None:
        report_path = REPO / "fixtures" / "reports" / "prelive_money_path_source.json"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sample = {
                "schema_version": control.SAMPLE_SCHEMA,
                "observed_at": "2026-07-16T00:00:00Z",
                "safety": {
                    "mode": "SHADOW",
                    "live_execution": False,
                    "execution_eligible": False,
                    "execution_request_created": False,
                    "signer_configured": False,
                    "wallet_configured": False,
                    "executor_configured": False,
                    "submission_method_invocations": "0",
                },
                "funnels": {
                    "candidate": {"feed_inputs": "1", "supported_swaps": "1", "route_matches": "1", "candidates": "1"},
                    "profitability": {"candidates": "1", "complete": "1", "primary_profitable": "1", "accepted": "1"},
                    "verification": {"requested": "1", "agreed": "1", "disagreed": "0", "unavailable": "0"},
                    "fork": {"planned": "1", "simulated": "1", "passed": "1", "profitable": "1", "reverted": "0"},
                },
                "metrics": {
                    "rpc": {"requests": "1", "success": "1", "timeouts": "0", "rate_limited": "0", "unavailable": "0", "disagreements": "0"},
                    "jetstream": {"streams": "2", "consumers": "2", "pending": "0", "ack_pending": "0", "redeliveries": "0"},
                    "database": {"size_bytes": "1"},
                    "feed": {"messages": "1", "gaps": "0", "missing_sequences": "0", "decode_failures": "0"},
                    "money_path_ingress": {
                        "aggregate_flush_failures_total": "0",
                        "aggregate_flush_total": "1",
                        "bounded_sample_failures_total": "0",
                        "bounded_samples_total": "0",
                        "database_bytes_per_input_estimate": "0",
                        "database_bytes_per_input_estimate_available": "0",
                        "dispatcher_backlog_refresh_failures_total": "0",
                        "dispatcher_backlog_refresh_total": "1",
                        "dispatcher_backlog_stale_seconds": "0",
                        "dispatcher_batch_cycle_seconds": "0",
                        "dispatcher_oldest_claimable_age_seconds": "0",
                        "dispatcher_pending_rows_estimate": "0",
                        "dispatcher_rows_published_total": "0",
                        "feed_inputs_total": "1",
                        "irrelevant_filtered_total": "1",
                        "persistence_ratio": "0",
                        "raw_rows_avoided_total": "3",
                        "relevant_route_inputs_total": "0",
                        "relevant_transaction_failures_total": "0",
                        "relevant_transactions_committed_total": "0",
                        "sample_limit_reached_total": "0",
                        "unsupported_interesting_total": "0",
                    },
                },
                "bounded_errors": [],
            }
            samples = root / "samples.ndjson"
            control.append_sample(samples, sample)
            with self.assertRaisesRegex(control.ControlEvidenceError, "samples_invalid"):
                control.append_sample(samples, sample)
        self.assertTrue(report_path.is_file())


if __name__ == "__main__":
    unittest.main()
