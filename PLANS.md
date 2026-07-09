# Phoenix v4 Implementation Plan

Status is intentionally honest. A component marked blocked has source code and fixtures where possible, but local verification needs a missing toolchain or credential.

## Phase Status

- Phase 0 old-system audit: completed.
- Phase 1 agent and execution documents: completed.
- Phase 2 local Nitro feed relay: implemented in Compose, pinned to the verified official Nitro release documented in `docs/DEPENDENCIES.md`; live relay not run locally.
- Phase 3 Go feed ingestor: implemented with fixture decoder, Nitro relay WebSocket/envelope adapter, ordering, duplicate/gap/out-of-order handling, NATS Core publisher, readiness, and metrics.
- Phase 4 cold RPC gateway: implemented as Rust crate source with provider budgets, cache, coalescing, circuit breaker, priority model, and internal API. Local compile blocked by missing Rust toolchain.
- Phase 5 verified protocol registry: implemented schema and startup validation model. Uniswap V3 Arbitrum addresses are documented from official Uniswap docs. Sushi V3 production addresses remain configuration-only until verified from Sushi official package/source.
- Phase 6 origin detector: implemented for supported V3 `exactInputSingle` and `exactInput` calldata surfaces using configured routers.
- Phase 7 local V3 state engine: implemented integer segment simulator with explicit completeness guards; full TickMath parity remains blocked until official math package parity tests can run.
- Phase 8 pool graph: implemented in-memory affected-cycle lookup.
- Phase 9 local route simulation: implemented with `hot_path_external_rpc_calls_total` instrumentation fixed at zero unless a violation is explicitly recorded.
- Phase 10 optimizer: implemented discrete grid and local refinement; fixtures validate best amount is not the first candidate.
- Phase 11 profit model: implemented as one model with principal, flash premium, execution cost, ordering cost, and uncertainty reserve.
- Phase 12 opportunity model: implemented immutable route/leg/opportunity structs.
- Phase 13 PhoenixExecutor contract: implemented constrained executor with owner/searcher authorization, pause, reentrancy guard, approved assets, approved flash provider, approved pools/factories, callback checks, baseline balance accounting, min profit, and event parsing target.
- Phase 14 execution coordinator: implemented SHADOW/SIMULATE/LIVE gates. LIVE remains disabled by default.
- Phase 15 recorder and realized PnL: migrations implemented; reconciliation source separated from submission.
- Phase 16 feed recording and replay: implemented Rust source plus deterministic fixtures. Local compile blocked by missing Rust toolchain.
- Phase 17 metrics and funnel: implemented metrics names and docs.
- Phase 18 dashboard: implemented Streamlit dashboard reading PostgreSQL and metrics only.
- Phase 19 Docker and operations: implemented Compose, Dockerfiles, healthchecks, internal networking, and scripts.
- Phase 20 security: implemented docs and secret scan script.
- Phase 21 testing: Go and Python checks are runnable locally. Rust, Foundry, cargo audit, govulncheck, and Slither depend on locally unavailable tools.

## Next Release Gate

Before any LIVE mode attempt:

- Run all Rust and Foundry tests on a Linux host with `cargo` and `forge`.
- Provide Arbitrum RPC credentials and run fork/parity tests.
- Verify Sushi V3 factory/router/quoter addresses from official Sushi package/source.
- Run a shadow observation window with zero unexplained feed sequence gaps.
- Validate local simulator parity against verified V3 quoters.
- Confirm PhoenixExecutor bytecode and registry through the cold RPC gateway.

## Productionization Status

- Git model: implemented GitHub Flow docs, branch patterns, Conventional Commit guidance, line-ending policy, and PR template.
- CI: implemented split GitHub checks for hygiene, Go, Rust crates, Solidity, Python dashboard, Docker validation, and deterministic fixtures.
- Image publishing: implemented GHCR workflow for immutable `sha-<full git sha>` images and release manifest artifacts.
- SHADOW deploy: implemented workflow-run deployment over strict SSH using only production deploy secrets and exact release manifests.
- LIVE gate: implemented manual readiness-report workflow that cannot enable LIVE or receive signer material.
- Production Compose: implemented `compose.prod.yml` with GHCR image refs, `/etc/phoenix/phoenix.env`, loopback dashboard/Prometheus, log rotation, persistent storage, and defensive SHADOW defaults.
- Health/readiness: implemented feed-ingestor health/readiness; added conservative Rust service readiness endpoints; production not-ready remains truthful where NATS/PostgreSQL wiring is incomplete.
- Nitro feed: relay adapter parsing is implemented for first runtime verification, but production relay ingestion is explicitly blocked until real Arbitrum feed validation and unsupported payload coverage are verified.
- Migrations: implemented a small Go PostgreSQL migration runner with schema table, checksums, advisory lock, and transactional apply.
- Deployment scripts: implemented bootstrap, env validation, release deploy, health gate, and rollback scripts.

Current production release blockers:

- Official Nitro relay adapter is implemented for first runtime verification but not live-verified.
- Phoenix engine production NATS subscription/state bootstrap is not implemented.
- Recorder production PostgreSQL schema verification and NATS subscription are not implemented.
- Rust, Foundry, Docker, and production Linux scripts still require validation on a host with those tools.
