# Limitations

- Phoenix v4.0 only supports origin-aware two-pool cross-DEX V3 arbitrage/backrun candidates.
- No liquidations, sandwiching, frontrunning, broad scanning, triangular execution, CEX signals, ML, Curve, Camelot, or Timeboost bidding are implemented.
- Sushi V3 Arbitrum production addresses are not hardcoded because they were not verified from a stable official source in this workspace.
- Protobuf schema is present; local Go publishing uses canonical JSON because generated Protobuf tooling is not installed here.
- Foundry local verification is blocked on this machine by missing `forge`.
- Live Nitro relay operation, Arbitrum fork tests, and simulator/quoter parity tests require a Linux host and RPC credentials.
- Production latency benchmarks are not measured.
- The Recorder consumes Core NATS, which provides best-effort at-most-once delivery. It retries an in-memory message while PostgreSQL is unavailable, but process crashes, subscriber disconnects, slow-consumer drops, and publications before subscription cannot be replayed. Durable recovery requires an explicit, tested JetStream or equivalent persistence design.
- Recorder readiness proves PostgreSQL reachability, schema compatibility, NATS connectivity, and an active subscription. It does not prove durable delivery or historical completeness.
