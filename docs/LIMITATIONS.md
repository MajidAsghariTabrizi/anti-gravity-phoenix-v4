# Limitations

- Phoenix v4.0 only supports origin-aware two-pool cross-DEX V3 arbitrage/backrun candidates.
- No liquidations, sandwiching, frontrunning, broad scanning, triangular execution, CEX signals, ML, Curve, Camelot, or Timeboost bidding are implemented.
- Sushi V3 Arbitrum production addresses are not hardcoded because they were not verified from a stable official source in this workspace.
- Protobuf schema is present; local Go publishing uses canonical JSON because generated Protobuf tooling is not installed here.
- Foundry local verification is blocked on this machine by missing `forge`.
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
