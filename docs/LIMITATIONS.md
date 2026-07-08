# Limitations

- Phoenix v4.0 only supports origin-aware two-pool cross-DEX V3 arbitrage/backrun candidates.
- No liquidations, sandwiching, frontrunning, broad scanning, triangular execution, CEX signals, ML, Curve, Camelot, or Timeboost bidding are implemented.
- Sushi V3 Arbitrum production addresses are not hardcoded because they were not verified from a stable official source in this workspace.
- Protobuf schema is present; local Go publishing uses canonical JSON because generated Protobuf tooling is not installed here.
- Rust and Foundry local verification is blocked on this machine by missing `cargo`, `rustc`, and `forge`.
- Live Nitro relay operation, Arbitrum fork tests, and simulator/quoter parity tests require a Linux host and RPC credentials.
- Production latency benchmarks are not measured.

