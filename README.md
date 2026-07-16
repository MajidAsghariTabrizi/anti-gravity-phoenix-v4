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
python -m py_compile scripts/shadow_profitability_report.py
sh scripts/shadow-profitability-report-tests.sh
python -m py_compile scripts/shadow_route_discovery.py
sh scripts/shadow-route-discovery-tests.sh
python -m py_compile scripts/prelive_money_path_report.py
sh scripts/prelive-money-path-report-tests.sh
python -m unittest discover -s dashboard/tests -p "test_*.py" -v
python -m unittest scripts.tests.test_prelive_shadow_control scripts.tests.test_prelive_dashboard_live -v
sh scripts/prelive-shadow-control-tests.sh
python scripts/prelive_dashboard_snapshot.py --input fixtures/dashboard/latest-dashboard.json --output fixtures/dashboard/checked-dashboard.json --check
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
Bounded read-only route discovery and its evidence limits are documented in `docs/SHADOW_ROUTE_DISCOVERY.md`.
Profit-triggered independent RPC verification is documented in `docs/SHADOW_SECONDARY_VERIFICATION.md`.
Bounded technical and business money-path evidence is documented in `docs/PRELIVE_MONEY_PATH_OBSERVABILITY.md`.
The evidence-only technical and business Dashboard is documented in `docs/PRELIVE_DASHBOARD.md`.
The protected-service-safe continuous SHADOW control plane is documented in `docs/PRELIVE_SHADOW_CONTROL_PLANE.md`.

Current real Nitro feed status: Nitro relay parsing is implemented for first SHADOW runtime verification but not live-verified. Production relay mode can start for Linux VPS validation, but real-feed evidence is still required before any production-readiness or LIVE claim.

## Live Release Gate

Merging to `main` can deploy SHADOW. It cannot enable LIVE. The manual `Live Readiness Report` workflow validates release inputs and writes a report only. Final LIVE enablement remains a deliberate server-side operation after the runbook gates pass.

## Services

- `nitro-feed-relay`: one internal Nitro feed ingress.
- `feed-ingestor`: Go ordered feed normalizer, NATS publisher, metrics, health, readiness.
- `nats`: file-backed JetStream for durable normalized-feed delivery; internal network only.
- `phoenix-engine`: Rust strategy engine.
- `rpc-gateway`: single cold read-RPC gateway.
- `recorder`: feed/opportunity recorder.
- `replay`: deterministic offline replay CLI.
- `postgres`: durable storage.
- `prometheus`: metrics.
- `dashboard`: evidence-only Streamlit view with bounded redacted snapshots and no data-plane credentials or Docker access.

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
