# Anti-Gravity Phoenix v4

Phoenix v4 is a low-cost, event-driven, origin-aware DEX backrun searcher for Arbitrum One.

Initial scope:

- Uniswap V3 and SushiSwap V3 families only.
- Two-pool cross-DEX V3 arbitrage/backrun cycles only.
- Aave V3 may be used only as flash-liquidity plumbing.
- SHADOW mode first. No profitability is guaranteed.

LIVE execution is disabled by default with `LIVE_EXECUTION=false`.

## Bootstrap

```bash
cp .env.example .env
make verify
docker compose up --build
```

On this Windows workspace, Go and Python checks can run. Rust and Foundry checks require a host with `cargo`, `rustc`, and `forge`.

## Shadow Mode

```bash
PHOENIX_MODE=SHADOW LIVE_EXECUTION=false docker compose up --build
```

## Services

- `nitro-feed-relay`: one internal Nitro feed ingress.
- `feed-ingestor`: Go ordered feed normalizer, NATS publisher, metrics.
- `nats`: NATS Core, no JetStream in hot path.
- `phoenix-engine`: Rust strategy engine.
- `rpc-gateway`: single cold read-RPC gateway.
- `recorder`: feed/opportunity recorder.
- `replay`: deterministic replay CLI.
- `postgres`: durable storage.
- `prometheus`: metrics.
- `dashboard`: Streamlit dashboard from PostgreSQL and metrics only.

## Release Gate Before LIVE

- feed stability
- zero unexplained sequence gaps during the target observation window
- simulator parity validation
- state reconciliation health
- contract tests passing
- fork tests passing when RPC credentials are provided
- no unresolved critical security findings
- sufficient gas model confidence
- positive shadow opportunity statistics

