import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class LiveExecutorSafetyTests(unittest.TestCase):
    def test_production_compose_remains_shadow_only(self) -> None:
        production = (ROOT / "compose.prod.yml").read_text(encoding="utf-8")
        self.assertNotIn("live-executor:", production)
        self.assertRegex(production, r"PHOENIX_MODE:\s+SHADOW")
        self.assertRegex(production, r'LIVE_EXECUTION:\s+"false"')

    def test_live_service_requires_disabled_profile_and_safe_defaults(self) -> None:
        overlay = (ROOT / "compose.live-canary.yml").read_text(encoding="utf-8")
        self.assertIn('profiles: ["live-canary"]', overlay)
        self.assertIn(
            "PHOENIX_MODE: ${LIVE_EXECUTOR_PHOENIX_MODE:-SHADOW}", overlay
        )
        self.assertIn(
            "LIVE_EXECUTION: ${LIVE_EXECUTOR_LIVE_EXECUTION:-false}", overlay
        )
        self.assertIn(
            "LIVE_EXECUTOR_ARMED: ${LIVE_EXECUTOR_ARMED:-false}", overlay
        )
        self.assertIn(
            "LIVE_EXECUTOR_KILL_SWITCH: ${LIVE_EXECUTOR_KILL_SWITCH:-true}",
            overlay,
        )
        self.assertIn(
            "LIVE_EXECUTOR_EXECUTOR_CODE_HASH: ${LIVE_EXECUTOR_EXECUTOR_CODE_HASH:-}",
            overlay,
        )
        self.assertNotIn("env_file:", overlay)
        self.assertNotRegex(overlay, r"(?m)^\s+SIGNER_PRIVATE_KEY\s*:")
        self.assertIn(
            "SIGNER_PRIVATE_KEY_FILE: /run/secrets/phoenix-live-executor-signer",
            overlay,
        )
        self.assertIn(
            "source: ${LIVE_EXECUTOR_SIGNER_FILE:?LIVE_EXECUTOR_SIGNER_FILE is required}",
            overlay,
        )
        self.assertIn("target: /run/secrets/phoenix-live-executor-signer", overlay)
        self.assertIn("create_host_path: false", overlay)
        self.assertIn('user: "65532:65532"', overlay)
        self.assertIn("restart: \"no\"", overlay)
        self.assertIn("read_only: true", overlay)
        self.assertIn("cap_drop: [ALL]", overlay)
        self.assertIn("no-new-privileges:true", overlay)
        self.assertNotRegex(overlay, r"ports:\s*\n")

    def test_autonomous_live_profile_is_explicit_continuous_and_file_signed(self) -> None:
        overlay = (ROOT / "compose.live-autonomous.yml").read_text(encoding="utf-8")
        self.assertRegex(
            overlay,
            r'(?ms)^  economic-monitor:\n(?:(?!^  \S).)*^    user: "1000:1000"$',
        )
        for required in (
            'profiles: ["live-autonomous"]',
            "PHOENIX_MODE: LIVE",
            'LIVE_EXECUTION: "true"',
            'AUTONOMOUS_EXECUTION: "true"',
            'LIVE_EXECUTOR_ARMED: "true"',
            'LIVE_EXECUTOR_KILL_SWITCH: "false"',
            "PRODUCTION_RPC_URL: https://arbitrum.nownodes.io/",
            "LIVE_EXECUTOR_RPC_ALLOWLIST: https://arbitrum.nownodes.io/",
            "LIVE_EXECUTOR_EXPECTED_OWNER: ${LIVE_EXECUTOR_EXPECTED_OWNER:?LIVE_EXECUTOR_EXPECTED_OWNER is required}",
            "LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER: ${LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER:?LIVE_EXECUTOR_EXPECTED_FLASH_PROVIDER is required}",
            "SIGNER_PRIVATE_KEY_FILE: /run/secrets/phoenix-live-executor-signer",
            "source: ${LIVE_EXECUTOR_SIGNER_FILE:?LIVE_EXECUTOR_SIGNER_FILE is required}",
            "create_host_path: false",
            "restart: unless-stopped",
            'user: "65532:65532"',
            "read_only: true",
            "cap_drop: [ALL]",
            "no-new-privileges:true",
        ):
            self.assertIn(required, overlay)
        self.assertNotIn("SECONDARY_RPC_URL:", overlay)
        self.assertEqual(
            overlay.count("PRODUCTION_RPC_URL: https://arbitrum.nownodes.io/"), 3
        )
        self.assertEqual(
            overlay.count("LIVE_EXECUTOR_RPC_ALLOWLIST: https://arbitrum.nownodes.io/"),
            3,
        )
        production = (ROOT / "compose.prod.yml").read_text(encoding="utf-8")
        self.assertIn("RPC_AUTHORITY_MODE: single_primary", production)
        self.assertIn(
            "RPC_AUTH_PROVIDER_ID: production-nownodes-arbitrum", production
        )
        self.assertIn("RPC_AUTH_PROVIDER_URL: https://arbitrum.nownodes.io/", production)
        self.assertNotIn("RPC_AUTH_CONFIRMATION_PROVIDER", production)
        self.assertNotIn("SECONDARY_RPC_URL:", production)
        self.assertNotRegex(overlay, r"(?m)^\s+SIGNER_PRIVATE_KEY\s*:")
        self.assertNotRegex(overlay, r"ports:\s*\n")

        for service in ("autonomous-control", "live-executor"):
            block = re.search(
                rf"(?ms)^  {service}:\s*\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\s*\n|\Z)",
                overlay,
            )
            self.assertIsNotNone(block)
            self.assertIn(
                "PHOENIX_OPERATOR_MODE: ${PHOENIX_MODE:?PHOENIX_MODE is required}",
                block.group("body"),
            )
            self.assertIn(
                "PHOENIX_OPERATOR_LIVE_EXECUTION: ${LIVE_EXECUTION:?LIVE_EXECUTION is required}",
                block.group("body"),
            )
            self.assertIn(
                "PHOENIX_OPERATOR_AUTONOMOUS_EXECUTION: ${AUTONOMOUS_EXECUTION:?AUTONOMOUS_EXECUTION is required}",
                block.group("body"),
            )
        self.assertEqual(overlay.count("PHOENIX_OPERATOR_MODE:"), 2)
        self.assertEqual(overlay.count("PHOENIX_OPERATOR_LIVE_EXECUTION:"), 2)
        self.assertEqual(overlay.count("PHOENIX_OPERATOR_AUTONOMOUS_EXECUTION:"), 2)

    def test_canary_schema_does_not_change_root_migrations(self) -> None:
        root_migrations = sorted(path.name for path in (ROOT / "migrations").glob("*.sql"))
        self.assertEqual(
            root_migrations[-1], "016_dashboard_ingest_timestamp_indexes.sql"
        )
        self.assertEqual(len(root_migrations), 17)
        schema = (ROOT / "live-executor/schema/001_live_canary.sql").read_text(
            encoding="utf-8"
        )
        approval_schema = (
            ROOT / "live-executor/schema/002_approval_evidence.sql"
        ).read_text(encoding="utf-8")
        autonomous_schema = (
            ROOT / "live-executor/schema/003_autonomous_hunter_contracts.sql"
        ).read_text(encoding="utf-8")
        runtime_schema = (
            ROOT / "live-executor/schema/004_autonomous_live_runtime.sql"
        ).read_text(encoding="utf-8")
        self.assertIn("armed BOOLEAN NOT NULL DEFAULT false", schema)
        self.assertIn("kill_switch BOOLEAN NOT NULL DEFAULT true", schema)
        self.assertIn("WHERE status = 'approved'", schema)
        self.assertIn("opportunity_id UUID NOT NULL UNIQUE", schema)
        self.assertIn("live_canary_one_active_attempt", schema)
        active_index = schema[schema.index("CREATE UNIQUE INDEX IF NOT EXISTS live_canary_one_active_attempt") :]
        self.assertTrue(
            all(
                status in active_index
                for status in (
                    "'claimed'",
                    "'nonce_allocated'",
                    "'submission_unknown'",
                    "'pending'",
                    "'timed_out'",
                )
            )
        )
        self.assertIn(
            "outcome_status TEXT NOT NULL CHECK (outcome_status IN ('confirmed', 'reverted'))",
            schema,
        )
        self.assertIn("net_pnl_wei = -actual_fee_wei", schema)
        for field in (
            "route_fingerprint",
            "selected_size",
            "token_path",
            "executor_address",
            "executor_code_hash",
            "calldata_hash",
            "simulation_result_hash",
            "plan_hash",
            "pinned_block_number",
            "pinned_block_hash",
            "approval_deadline",
        ):
            self.assertIn(field, approval_schema)
        self.assertIn("selected_size = flash_amount", approval_schema)
        self.assertIn("approval_deadline <= deadline", approval_schema)
        self.assertIn(
            "live_canary_execution_request_simulation_result", approval_schema
        )
        self.assertIn("live_canary_execution_request_plan", approval_schema)
        for table in (
            "autonomous_global_control",
            "autonomous_route_controls",
            "autonomous_candidates",
            "autonomous_approvals",
            "autonomous_outcome_attributions",
        ):
            self.assertIn(table, autonomous_schema)
        self.assertIn("'phoenix.live-canary-schema.v2'", autonomous_schema)
        self.assertIn("'phoenix.live-canary-schema.v3'", autonomous_schema)
        self.assertIn("'phoenix.live-canary-schema.v4'", runtime_schema)
        self.assertIn("autonomous_candidate_transition", runtime_schema)
        self.assertIn("autonomous_outcome_chain_pnl_v4", runtime_schema)
        store = (ROOT / "live-executor/src/store.rs").read_text(encoding="utf-8")
        self.assertIn("AT TIME ZONE 'UTC'", store)

    def test_approval_cli_accepts_no_calldata_and_runtime_checks_before_nonce(self) -> None:
        cli = (
            ROOT / "live-executor/src/approve_execution_request_main.rs"
        ).read_text(encoding="utf-8")
        approval = (ROOT / "live-executor/src/approval.rs").read_text(
            encoding="utf-8"
        )
        engine = (ROOT / "live-executor/src/engine.rs").read_text(encoding="utf-8")
        self.assertNotIn("--calldata", cli)
        self.assertIn("APPROVAL_CONFIRMATION", cli)
        self.assertIn("APPROVE_ONE_SIMULATED_PHOENIX_CANARY", approval)
        validation = engine.index("validate_and_encode(&request")
        nonce = engine.index(".pending_nonce(")
        self.assertLess(validation, nonce)
        self.assertIn("calldata_hash_mismatch", engine)

    def test_profit_and_gas_accounting_use_arbitrum_weth(self) -> None:
        library = (ROOT / "live-executor/src/lib.rs").read_text(encoding="utf-8")
        config = (ROOT / "live-executor/src/config.rs").read_text(encoding="utf-8")
        self.assertIn(
            'ARBITRUM_WETH_ADDRESS: &str = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"',
            library,
        )
        self.assertIn("ConfigError::UnsupportedProfitAsset", config)

    def test_raw_submission_method_has_one_runtime_implementation(self) -> None:
        method = "eth_sendRaw" + "Transaction"
        matches = []
        for path in ROOT.rglob("*"):
            if (
                not path.is_file()
                or ".git" in path.parts
                or "target" in path.parts
                or path.suffix not in {".rs", ".py", ".sh", ".yml", ".yaml"}
            ):
                continue
            if method in path.read_text(encoding="utf-8", errors="ignore"):
                matches.append(path.relative_to(ROOT).as_posix())
        self.assertEqual(
            set(matches),
            {
                "live-executor/src/rpc.rs",
                "scripts/fork-sandbox-validate.sh",
                "scripts/shadow-positive-route-evidence-tests.sh",
            },
        )

    def test_actions_contains_no_signer_key_value(self) -> None:
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        key_name = "SIGNER_" + "PRIVATE_KEY"
        assignments = re.findall(
            rf"(?m)^\s*{re.escape(key_name)}[ \t]*[:=]", workflow
        )
        self.assertEqual(assignments, [])
        self.assertIn(
            'environment.get("SIGNER_PRIVATE_KEY_FILE") != signer_target',
            workflow,
        )
        self.assertIn('if "SIGNER_PRIVATE_KEY" in environment:', workflow)

    def test_autonomous_deployment_is_exact_release_gated_and_preflight_first(self) -> None:
        legacy = (
            ROOT / ".github/workflows/deploy-autonomous-live.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("deprecated and disabled", legacy)
        self.assertNotIn("scp ", legacy)
        workflow = (
            ROOT / ".github/workflows/phoenix-release-controller.yml"
        ).read_text(encoding="utf-8")
        for required in (
            "environment: production-live",
            "verify-source-ci",
            "validate-deploy-pair",
            "phoenix-release-assets-${{ needs.prepare.outputs.release_sha }}",
            "rollback-manifest.json",
            "rollback-provenance.json",
            '"receive ${RELEASE_SHA}"',
            '"resume ${RELEASE_SHA}"',
        ):
            self.assertIn(required, workflow)

        gateway = (ROOT / "scripts/phoenix_release/gateway.py").read_text(
            encoding="utf-8"
        )
        active_context = gateway.index("_host_preflight(paths, request)")
        immutable_install = gateway.index(
            "_install_candidate(paths, request, rpc_provider_secret)"
        )
        candidate_installed = gateway.index(
            'state["current_phase"] == "CANDIDATE_INSTALLED"'
        )
        live_deploy = gateway.index(
            'str(paths.deploy_dir / "deploy-release.sh")',
            candidate_installed,
        )
        self.assertLess(active_context, immutable_install)
        self.assertLess(immutable_install, live_deploy)
        self.assertIn("validate-deploy-pair", gateway)
        self.assertIn("verify-tree", gateway)
        self.assertIn("ACTIVE_RELEASE_CHANGED", gateway)

        deployment = (ROOT / "scripts/deploy-release.sh").read_text(encoding="utf-8")
        candidate_mode_live = deployment.index(
            'production_mode.py" live --env-file "$candidate_live_env"'
        )
        candidate_overlay = deployment.index(
            '--overlay-file "$overlay_file"', candidate_mode_live
        )
        disarmed_control = deployment.index("autonomous-control disarmed-deploy")
        previous_release = deployment.index('cp "$current_file" "$previous_file"')
        mode_live = deployment.index(
            'production_mode.py" live --env-file "$env_file"'
        )
        migration = deployment.index("autonomous-control migrate")
        burn_in = deployment.index("run_live_engine_burn_in ||")
        self.assertLess(candidate_mode_live, candidate_overlay)
        self.assertLess(candidate_overlay, previous_release)
        self.assertLess(previous_release, mode_live)
        self.assertLess(mode_live, migration)
        self.assertLess(migration, disarmed_control)
        self.assertLess(disarmed_control, burn_in)
        self.assertNotIn("live-executor activate", deployment)
        self.assertNotIn("live-executor owner-unpause", deployment)
        executor_start = deployment.index("compose up -d --no-deps live-executor")
        self.assertGreater(executor_start, disarmed_control)
        self.assertIn("verify_runtime_control_phase DISARMED_EVIDENCE", deployment)
        self.assertNotIn("LIVE_EXECUTOR_SIGNER_FILE", deployment)
        self.assertIn("PHOENIX_HEALTH_EXPECTED_MODE=DISARMED_EVIDENCE", deployment)
        self.assertIn(
            '"$active_environment_identity_after" = '
            '"$active_environment_identity_before"',
            deployment,
        )

    def test_autonomous_rollback_disarms_reconciles_then_restores(self) -> None:
        rollback = (ROOT / "scripts/rollback-release.sh").read_text(encoding="utf-8")
        disarm = rollback.index("autonomous-control disarm")
        reconciliation = rollback.index("autonomous-control reconciliation-status")
        executor_stop = rollback.index("stop -t 30 live-executor")
        shadow_mode = rollback.index("production_mode.py\" shadow")
        immutable_verify = rollback.index("verify-tree")
        self.assertLess(disarm, reconciliation)
        self.assertLess(reconciliation, executor_stop)
        self.assertLess(executor_stop, shadow_mode)
        self.assertLess(shadow_mode, immutable_verify)
        self.assertNotIn("TRUNCATE", rollback)

    def test_jetstream_ci_proves_postgres_through_the_published_port(self) -> None:
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        job = workflow.split("\n  jetstream-integration:\n", 1)[1]
        host_probe = job.index("docker run --rm --network host")
        migration_test = job.index(
            "TestFreshV5DatabaseInitializesFromZeroAndIsIdempotent"
        )
        self.assertLess(host_probe, migration_test)
        self.assertIn("-h 127.0.0.1 -U phoenix_test -d phoenix_test", job)
        self.assertIn("-c 'SELECT 1'", job)
        self.assertNotIn("docker exec phoenix-ci-postgres pg_isready", job)

    def test_config_failures_are_logged_by_sanitized_code_only(self) -> None:
        runtime = (ROOT / "live-executor/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("error_code = error.code()", runtime)
        self.assertNotIn("error = ?error", runtime)
        self.assertNotIn("error = %error", runtime)

    def test_operator_example_uses_only_the_signer_file_source(self) -> None:
        example = (ROOT / ".env.example").read_text(encoding="utf-8")
        self.assertIn("LIVE_EXECUTOR_SIGNER_FILE=", example)
        self.assertNotIn("LIVE_EXECUTOR_SIGNER_PRIVATE_KEY=", example)

    def test_isolated_submission_fixture_is_loopback_only(self) -> None:
        fixture = (
            ROOT / "scripts/live-executor-isolated-fork-tests.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("http://127.0.0.1:", fixture)
        self.assertIn("CONFIRMED_LOCAL_ANVIL", fixture)
        self.assertNotIn("--fork-url", fixture)
        self.assertNotIn("SIGNER_" + "PRIVATE_KEY", fixture)
        constructor_args = fixture.index("--constructor-args")
        self.assertLess(fixture.index("--broadcast"), constructor_args)
        self.assertLess(fixture.index("--json"), constructor_args)

    def test_runtime_does_not_log_raw_payload_or_rpc_url(self) -> None:
        rpc = (ROOT / "live-executor/src/rpc.rs").read_text(encoding="utf-8")
        signer = (ROOT / "live-executor/src/signer.rs").read_text(encoding="utf-8")
        self.assertNotIn("tracing::", rpc)
        self.assertNotIn("println!", rpc)
        self.assertIn(".redirect(Policy::none())", rpc)
        self.assertIn(".no_proxy()", rpc)
        self.assertIn('.field("raw", &"<redacted>")', signer)


if __name__ == "__main__":
    unittest.main()
