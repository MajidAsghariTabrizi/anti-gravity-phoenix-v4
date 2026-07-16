# Limitations

- Phoenix v4.0 only supports origin-aware two-pool cross-DEX V3 arbitrage/backrun candidates.
- No liquidations, sandwiching, frontrunning, broad scanning, triangular execution, CEX signals, ML, Curve, Camelot, or Timeboost bidding are implemented.
- Sushi V3 Arbitrum production addresses are not hardcoded because they were not verified from a stable official source in this workspace.
- Protobuf schema is present; local Go publishing uses canonical JSON because generated Protobuf tooling is not installed here.
- Foundry local verification is blocked on this machine by missing `forge`.
- Independent verification proves different configured logical provider IDs,
  not different upstream operators or infrastructure. That distinction still
  requires a reviewed provider inventory and live Arbitrum observation.
- Profitability rows written before migration 009 do not contain the new route
  hash or independent-verification lifecycle and remain explicit historical
  nulls in reports.
- Live Nitro relay operation, Arbitrum fork tests, and simulator/quoter parity tests require a Linux host and RPC credentials.
- Production latency benchmarks are not measured.
- The production Engine consumes its durable JetStream input, evaluates configured routes with block-pinned RPC state, and persists SHADOW decisions and canonical profitability facts. Its current V3 state model remains bounded to the reconciled tick, and it does not run contract or fork simulation in the production decision path; those missing execution proofs remain fail-closed rejection evidence.
- Gas and L1 fee inputs must already be expressed in the route settlement-asset unit. No cross-asset fee conversion service is implemented, so routes without a valid unit conversion cannot support complete profitability evidence.
- The committed replay cases are synthetic test coverage and are not profitability evidence.
- No wallet, signer, contract deployment, or LIVE submission service is configured.
- Recorder delivery uses a single-node, one-replica JetStream work queue. It supports restart replay and confirmed acknowledgements after PostgreSQL commit, but host or Docker-volume loss is not replicated and messages older than the bounded 24-hour stream age can expire.
- Core NATS losses from releases before the JetStream cutover cannot be reconstructed. The first VPS JetStream smoke must pass before durable production readiness is claimed.
- Recorder readiness proves the current PostgreSQL, stream, durable consumer, fetch loop, persistence, acknowledgement, and integrity state. It does not prove historical completeness before the migration.
- SHADOW route discovery ranks only reviewed official Uniswap V3-compatible, exact-input, two-pool, two-token cycles supported by persisted decoder history. Missing source-block metadata, liquidity checkpoints, profitability facts, RPC-quality rows, or feed-gap overlap remain explicit unavailable evidence; a suggestion is not a profitability, execution, or LIVE-readiness claim.
- PRE-LIVE money-path runtime counters reset with their owning process and are not durable history. The bounded report pairs them with read-only PostgreSQL aggregates, but no real production soak or realized-profit evidence exists until the later controlled runtime phases complete.
- The PRE-LIVE Dashboard consumes only validated, bounded, redacted snapshots. The Phase 9 controller can produce continuous snapshots without giving the Dashboard data-plane or Docker access, but no real VPS control-plane run or staged soak has yet occurred. Missing history, rate intervals, percentile evidence, stale data, or failed collection remains visibly blocked rather than falling back to fixtures or invented zeroes.
- The pre-live integration changes feed-ingestor and Recorder, while both are protected against container recreation during deployment and soak. Exact release and rollback manifests must prove identical protected image digests before the manual workflow can reach SSH. A separately authorized protected-service maintenance gate is therefore required before this release can be deployed; optional-service rollout cannot bypass that blocker.
