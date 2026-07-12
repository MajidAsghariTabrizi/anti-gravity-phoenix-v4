# Limitations

- Phoenix v4.0 only supports origin-aware two-pool cross-DEX V3 arbitrage/backrun candidates.
- No liquidations, sandwiching, frontrunning, broad scanning, triangular execution, CEX signals, ML, Curve, Camelot, or Timeboost bidding are implemented.
- Sushi V3 Arbitrum production addresses are not hardcoded because they were not verified from a stable official source in this workspace.
- Protobuf schema is present; local Go publishing uses canonical JSON because generated Protobuf tooling is not installed here.
- Foundry local verification is blocked on this machine by missing `forge`.
- Live Nitro relay operation, Arbitrum fork tests, and simulator/quoter parity tests require a Linux host and RPC credentials.
- Production latency benchmarks are not measured.
- Recorder delivery uses a single-node, one-replica JetStream work queue. It supports restart replay and confirmed acknowledgements after PostgreSQL commit, but host or Docker-volume loss is not replicated and messages older than the bounded 24-hour stream age can expire.
- Core NATS losses from releases before the JetStream cutover cannot be reconstructed. The first VPS JetStream smoke must pass before durable production readiness is claimed.
- Recorder readiness proves the current PostgreSQL, stream, durable consumer, fetch loop, persistence, acknowledgement, and integrity state. It does not prove historical completeness before the migration.
