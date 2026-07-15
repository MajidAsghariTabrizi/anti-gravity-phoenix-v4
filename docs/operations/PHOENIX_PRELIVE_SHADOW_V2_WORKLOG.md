# Phoenix Pre-Live SHADOW v2 Worklog

## Phase 0

- phase: Phase 0 - Repository, release, and architecture truth; immutable starting SHA `4cf4375452bffd9b3e10b635ab687353dec6cab8`
- branch: `chore/prelive-baseline-audit`
- commit SHA: `956bfb13af9db06f64041a0dc968afb64077b0c2`
- files changed: `docs/architecture/PRELIVE_SHADOW_V2_BASELINE.md`
- tests run: `git diff --check`; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `sh -n scripts/*.sh`; `sh ./scripts/shadow-engine-canary-control-tests.sh`; `sh ./scripts/shadow-engine-isolated-canary-tests.sh`; `sh ./scripts/shadow-positive-route-evidence-tests.sh`
- test result: PASS - all available Phase 0 gates completed successfully
- known blocker: repository production intent uses `/opt/phoenix/deploy` while supplied runtime evidence reports `/opt/phoenix/app`; no route-registry hash exists; deploy and rollback can recreate protected services
- next gate: Phase 1 must prove one production renderer, release context, route hash, immutable image contract, generated-file hygiene, and protected-service-safe operations

## Phase 1

- phase: Phase 1 - Production Compose, release, and route-registry truth
- branch: `fix/production-compose-route-registry-truth`
- commit SHA: `f244ccecbd2dde33d7ea470d112a78e6feb45304` (implementation), `c25333eb4950649dd53bb1e5a3eaf22e9988e4ea` (operations documentation)
- files changed: `.env.example`; `.github/workflows/ci.yml`; `.gitignore`; `compose.prod.yml`; `docs/DEPENDENCIES.md`; `docs/RELEASE_AND_ROLLBACK.md`; `docs/SHADOW_POSITIVE_ROUTE_EVIDENCE.md`; `scripts/bootstrap-production.sh`; `scripts/deploy-release.sh`; `scripts/forbidden-file-check.ps1`; `scripts/forbidden-file-check.sh`; `scripts/production-compose-context-tests.sh`; `scripts/production_context.py`; `scripts/render-production-compose.sh`; `scripts/rollback-release.sh`; `scripts/shadow-positive-route-evidence-tests.sh`; `scripts/shadow-positive-route-evidence.sh`; `scripts/validate-production-env-tests.sh`; `scripts/validate-production-env.sh`; `scripts/validate-production-release-context.sh`
- tests run: `python -m py_compile scripts/production_context.py`; `sh -n scripts/*.sh`; `sh ./scripts/production-compose-context-tests.sh`; `sh ./scripts/validate-production-env-tests.sh`; `sh ./scripts/shadow-engine-canary-control-tests.sh`; `sh ./scripts/shadow-engine-isolated-canary-tests.sh`; `sh ./scripts/shadow-positive-route-evidence-tests.sh`; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `git diff --check`
- test result: PASS - all locally available deterministic Phase 1 gates completed successfully; no runtime service was started
- known blocker: Docker is unavailable locally, so Docker-backed Compose route preservation and the complete Linux CI matrix must run in GitHub Actions
- next gate: push the exact branch tip, require every `ci.yml` job to pass, then merge Phase 1 into the cumulative integration branch without deploying

## Phase 2

- phase: Phase 2 - Transient dependency exhaustion quarantine
- branch: `fix/engine-transient-exhaustion-quarantine`
- implementation commits: `b15ee3c576f601ae557da88af33f7314ee9c29cf`; `0b9310c8b899e9f69734a744f156fcb20c4aedab`; `7fb548aa99c04858845106c1b93972954d74f0b7`; `d9412a4516b233e714a91c36f820adb8377c1548`
- files changed: `dashboard/app.py`; `deploy/rust.Dockerfile`; `docs/ENGINE_DEPENDENCY_EXHAUSTION.md`; `docs/RUNBOOK.md`; `fixtures/engine/dependency_exhaustion_soak.json`; `migration-runner/internal/runner/runner_test.go`; `migrations/006_dependency_exhaustion_quarantine.sql`; `phoenix-engine/src/engine_input.rs`; `phoenix-engine/src/main.rs`; `phoenix-engine/src/metrics/mod.rs`; `phoenix-engine/src/persistence.rs`; `phoenix-engine/src/runtime.rs`; `phoenix-engine/tests/postgres_decision_integration.rs`; `recorder/tests/postgres_outbox_integration.rs`; `scripts/shadow-engine-live-smoke.sh`
- tests run: `gofmt -w migration-runner/internal/runner/runner_test.go`; `go vet ./...` and `go test ./...` in `migration-runner`; `python -m py_compile dashboard/app.py`; `sh -n scripts/*.sh`; `sh ./scripts/shadow-engine-canary-control-tests.sh`; `sh ./scripts/shadow-engine-isolated-canary-tests.sh`; `sh ./scripts/shadow-positive-route-evidence-tests.sh`; `sh ./scripts/production-compose-context-tests.sh`; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `git diff --check`; complete implementation diff review
- test result: PASS - all locally available deterministic Phase 2 gates completed successfully; GitHub Actions run `29405910600` passed all 11 `ci.yml` jobs at exact implementation SHA `d9412a4516b233e714a91c36f820adb8377c1548`, including Rust formatting, clippy with `-D warnings`, the 211-delivery soak, PostgreSQL integration, dashboard import, and all Phoenix-owned Docker builds
- known blocker: none for the Phase 2 implementation gate; Cargo, rustc, rustfmt, Docker, and the dashboard Python dependency set remain unavailable locally, so the exact final documentation-only branch tip still requires authoritative GitHub Actions verification
- next gate: commit this closure record, push the exact branch tip, require every `ci.yml` job to pass at that tip, then merge Phase 2 into the cumulative integration branch without deploying

## Phase 3

- phase: Phase 3 - Canonical SHADOW profitability truth and evidence
- branch: `feat/shadow-profitability-truth`
- implementation commits: `71341e9f2cd8fc180ee177c3bd97d00dbe79e569`; `4d5ca9cbd36020ed791da6c8bd26a76198017240`; `79a08eedbc3ff150c0d0c91f52dcfb099e3a908b`; `b5526fb94b4383d850f7c4129c42c7a342cb5207`; `e7c7d88902197de49d0dace6d947a3b93e75a4c3`
- files changed: `.github/workflows/ci.yml`; `README.md`; `docs/LIMITATIONS.md`; `docs/RUNBOOK.md`; `docs/SHADOW_ECONOMICS.md`; `docs/SHADOW_PROFITABILITY_REPORTS.md`; `fixtures/reports/shadow_profitability_rows.ndjson`; `migration-runner/internal/runner/runner_test.go`; `migrations/007_canonical_profitability_truth.sql`; `phoenix-engine/src/decision/mod.rs`; `phoenix-engine/src/economics/mod.rs`; `phoenix-engine/src/metrics/mod.rs`; `phoenix-engine/src/opportunity/mod.rs`; `phoenix-engine/src/optimizer/mod.rs`; `phoenix-engine/src/persistence.rs`; `phoenix-engine/src/rpc_evaluator.rs`; `phoenix-engine/tests/e2e_fixtures.rs`; `phoenix-engine/tests/postgres_decision_integration.rs`; `recorder/tests/postgres_outbox_integration.rs`; `replay/src/evidence.rs`; `replay/src/lib.rs`; `rpc-gateway/src/runtime.rs`; `rpc-gateway/src/shadow_state.rs`; `scripts/bootstrap-production.sh`; `scripts/production-compose-context-tests.sh`; `scripts/shadow-profitability-report-tests.sh`; `scripts/shadow-profitability-report.sh`; `scripts/shadow_profitability_report.py`; `scripts/sql/shadow-profitability-report.sql`
- tests run: `gofmt -w migration-runner/internal/runner/runner_test.go`; `go vet ./...` and `go test ./... -count=1` in `feed-ingestor` and `migration-runner`; deterministic decoder fixture tests; `python -m py_compile dashboard/app.py scripts/shadow_profitability_report.py`; `sh -n scripts/*.sh`; `sh ./scripts/shadow-profitability-report-tests.sh`; `sh ./scripts/shadow-engine-canary-control-tests.sh`; `sh ./scripts/shadow-engine-isolated-canary-tests.sh`; `sh ./scripts/shadow-positive-route-evidence-tests.sh`; `sh ./scripts/production-compose-context-tests.sh`; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `git diff --check`; complete implementation diff review
- test result: PASS - all locally available deterministic Phase 3 gates completed successfully; GitHub Actions run `29413591901` passed all 11 `ci.yml` jobs at exact implementation SHA `e7c7d88902197de49d0dace6d947a3b93e75a4c3`, including Rust formatting, clippy with `-D warnings`, unit and PostgreSQL integration tests, deterministic fixtures, bounded report tests, Solidity, Go, dashboard import, JetStream integration, and every Phoenix-owned Docker build
- evidence truth: canonical records use checked signed integer arithmetic and explicit settlement assets; historical rows without evidence remain incomplete with null financial fields; reports are deterministic, bounded, read-only, and label expected, counterfactual, fork-simulated, and not-realized values distinctly
- safety status: `PHOENIX_MODE=SHADOW`; `LIVE_EXECUTION=false`; signer, wallet, and executor values remain blank; `execution_eligible=false`; `execution_request_created=false`; no fixture fallback, transaction signing, submission, public-RPC hot-path read, deployment, or live-runtime evidence was introduced or claimed
- known blocker: none for the Phase 3 implementation gate; Cargo, rustc, rustfmt, Docker, Foundry, and the dashboard dependency set remain unavailable locally, so the exact final documentation-only branch tip still requires authoritative GitHub Actions verification
- next gate: commit this closure record, push the exact branch tip, require every `ci.yml` job to pass at that tip, then merge Phase 3 into the cumulative integration branch without deploying

## Phase 4

- phase: Phase 4 - Arbitrum route discovery and ranking
- branch: `feat/arbitrum-route-discovery`
- commit SHA: `9a14c15a109b33a2da3a244614232f40007b2663` (implementation)
- files changed: `.github/workflows/ci.yml`; `README.md`; `docs/LIMITATIONS.md`; `docs/RUNBOOK.md`; `docs/SHADOW_ROUTE_DISCOVERY.md`; `fixtures/reports/shadow_route_discovery_decoded.ndjson`; `fixtures/reports/shadow_route_discovery_enrichment.ndjson`; `fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json`; `migration-runner/internal/runner/runner_test.go`; `migrations/008_shadow_route_discovery_indexes.sql`; `phoenix-engine/src/positive_route_evidence.rs`; `phoenix-engine/src/shadow_positive_route_evidence_main.rs`; `phoenix-engine/tests/positive_route_evidence.rs`; `phoenix-engine/tests/postgres_decision_integration.rs`; `recorder/tests/postgres_outbox_integration.rs`; `scripts/bootstrap-production.sh`; `scripts/production-compose-context-tests.sh`; `scripts/shadow-route-discovery-tests.sh`; `scripts/shadow-route-discovery.sh`; `scripts/shadow_route_discovery.py`; `scripts/sql/shadow-route-discovery-enrichment.sql`
- tests run: `python -m py_compile scripts/shadow_route_discovery.py`; `sh -n scripts/*.sh`; `sh ./scripts/shadow-route-discovery-tests.sh`; existing canary, positive-route, profitability-report, and production-context shell suites; `gofmt -l .`, `go vet ./...`, and `go test ./...` in `feed-ingestor` and `migration-runner`; Phoenix and Recorder `cargo fmt --check`, Clippy with `-D warnings`, and full tests; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `git diff --check`; complete implementation diff review
- test result: PASS - all locally available deterministic Phase 4 gates completed successfully; GitHub Actions run `29422208523` passed all 11 `ci.yml` jobs at exact implementation SHA `9a14c15a109b33a2da3a244614232f40007b2663`, including PostgreSQL integration, deterministic fixtures, bounded route discovery, Solidity, Go, dashboard import, JetStream integration, production Compose validation, and every Phoenix-owned Docker build
- known blocker: none for the Phase 4 implementation gate; Docker, Foundry, and PowerShell 7 remain unavailable locally, so the exact final documentation-only branch tip still requires authoritative GitHub Actions verification
- next gate: commit this closure record, push the exact branch tip, require every `ci.yml` job to pass at that tip, then merge Phase 4 into the cumulative integration branch without deploying
