# Benchmarks

Production latency numbers are not measured in this workspace.

Reasons:

- `cargo` is not installed locally.
- `rustc` is not installed locally.
- Docker is not installed locally.
- Live feed and RPC credentials are not configured.

Reproducible commands on a Linux host:

```bash
make bench
cargo test --manifest-path phoenix-engine/Cargo.toml --release bench_decision_path -- --ignored --nocapture
```

Required benchmark targets:

- origin decode
- affected route lookup
- V3 simulation
- 25 optimizer evaluations
- complete decision path excluding network submission

