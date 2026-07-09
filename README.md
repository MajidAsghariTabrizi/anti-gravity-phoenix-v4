# Anti-Gravity Phoenix v4

Phoenix v4 is a low-cost, event-driven, origin-aware DEX backrun searcher for Arbitrum One.

Initial scope:

- Uniswap V3 and SushiSwap V3 families only.
- Two-pool cross-DEX V3 arbitrage/backrun cycles only.
- Aave V3 may be used only as flash-liquidity plumbing.
- SHADOW mode first. No profitability is guaranteed.

LIVE execution is disabled by default with `LIVE_EXECUTION=false`.

## Local Fixture Development

Local development uses deterministic fixtures and local builds:

```bash
cp .env.example .env
docker compose up --build
```

The default local feed source is `fixtures/feed/profitable.ndjson`. This path is intentionally not allowed in production.

## Local Verification

```bash
make verify
```

On this Windows workspace, Go and Python checks can run. Rust, Foundry, Docker, and shell-based production validation require a host with the corresponding tools.

Useful direct checks:

```bash
powershell -ExecutionPolicy Bypass -File .\scripts\secret-scan.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\forbidden-file-check.ps1
cd feed-ingestor && go test ./...
cd migration-runner && go test ./...
python -m py_compile dashboard/app.py
```

## Shadow Production

Production uses `compose.prod.yml`, immutable GHCR images, `/etc/phoenix/phoenix.env`, release manifests, deploy health gates, and rollback scripts.

Default production safety:

- `PHOENIX_MODE=SHADOW`
- `LIVE_EXECUTION=false`
- no signer key in GitHub Actions
- no fixture feed in production
- no production source builds on the VPS

Production deployment is documented in `docs/PRODUCTION_BOOTSTRAP.md` and `docs/RELEASE_AND_ROLLBACK.md`.

Current real Nitro feed status: Nitro relay parsing is implemented for first runtime verification but not live-verified. Production feed startup is blocked by design until real-feed evidence exists.

## Live Release Gate

Merging to `main` can deploy SHADOW. It cannot enable LIVE. The manual `Live Readiness Report` workflow validates release inputs and writes a report only. Final LIVE enablement remains a deliberate server-side operation after the runbook gates pass.

## Services

- `nitro-feed-relay`: one internal Nitro feed ingress.
- `feed-ingestor`: Go ordered feed normalizer, NATS publisher, metrics, health, readiness.
- `nats`: NATS Core, no JetStream in hot path.
- `phoenix-engine`: Rust strategy engine.
- `rpc-gateway`: single cold read-RPC gateway.
- `recorder`: feed/opportunity recorder.
- `replay`: deterministic offline replay CLI.
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
