# Old Anti-Gravity System Audit

The old repository was inspected as read-only reference. Phoenix does not copy the old architecture.

## Component Findings

| Component | Purpose | Useful concept retained | Design problem eliminated | Phoenix decision |
| --- | --- | --- | --- | --- |
| `block_emitter.py` | Polls `eth_blockNumber` and publishes block numbers over ZeroMQ. | One centralized event emitter concept. | Public RPC polling is the hot trigger, uses `PEACE` five-second polling and fallback storms. | Replaced with one Nitro relay, Go feed ingestor, and NATS Core ordered tx events. |
| `market_sentinel.py` | Uses Binance ETH price volatility and heartbeat to decide scan cadence. | Explicit system mode concept. | External CEX volatility is unrelated to state-created DEX backruns. | Discarded for v4.0 search path; shadow/live mode is execution safety only. |
| Smart RPC manager classes | Rotate provider URLs after errors. | Centralized retry, health, and fallback are useful. | Repeated per-bot managers create herd traffic and unbounded provider pressure. | Replaced with one cold `rpc-gateway` with budgets, cache, coalescing, and circuit breakers. |
| `arb_engine.py` | Two-leg DEX arbitrage scanner/executor. | Multicall batching and explicit net/gas thinking. | Broad token scanning, hot-path Quoter RPC, fixed 1,000 USDC amount, float profit math, scanner/executor route divergence, estimated profit recorded as execution profit. | Conceptually replaced by event-driven affected-route simulation and immutable opportunities. |
| `tri_arb_engine.py` | Three-hop arbitrage scanner/executor. | Batching lessons and route representation lessons. | Out of v4.0 scope; broad scanning, fixed flash size, Curve/Camelot mixing, Quoter hot path. | Discarded for v4.0 strategy; graph leaves an extension point only. |
| `scanner.py`, `radiant_scanner.py`, `lodestar_scanner.py` | Poll lending protocol logs and classify liquidation targets. | Batch classification and checkpoint ideas. | Liquidations are out of v4.0 scope and scan cadence depends on RPC. | Discarded for v4.0. |
| `gravity_bot.py`, `radiant_bot.py`, `lodestar_bot.py` | Liquidation executors. | Separation between pre-flight checks and receipt confirmation. | Multiple copied bots, hardcoded fee tier, `eth_call` treated as go/no-go, static fallback gas, random jitter. | Discarded for v4.0; one execution coordinator handles opportunities. |
| `db_manager.py` | SQLite WAL database and dashboard query helpers. | WAL lesson, durable event recording, dashboard separation. | Uses floats for profit, combines estimated profit with execution records, schema is tied to old strategies. | Replaced with PostgreSQL migrations and explicit opportunity lifecycle. |
| `dashboard.py` | Streamlit mission-control UI. | Operational dashboard remains valuable. | Reads old SQLite schema and could compete with execution process resources. | Replaced with PostgreSQL/metrics-only dashboard. |
| `contracts/DexArbitrageur.sol` | Aave flash-loan two-leg router executor. | Min repayment gate and flash callback gate. | Generic low-level router calls, owner-only wallet shape, route reconstruction risk, no pool/factory allowlist, naive balance baseline. | Replaced with constrained `PhoenixExecutor.sol`. |
| `contracts/TriArbitrageur.sol` | Multi-hop router executor. | Encoded ordered leg idea. | Generic arbitrary external calls and v4.0 scope violation. | Discarded. |
| `contracts/FlashLoanLiquidator.sol`, `RadiantLiquidator.sol` | Liquidation executors. | Flash-loan callback lessons. | Out of v4.0 scope. | Discarded. |
| Hardhat scripts/config | Old deployment workflow. | Environment-driven deployment. | Root `.env` and scripts are tied to old contracts. | Replaced by Foundry package and placeholder-only `.env.example`. |
| PM2 config | Process supervision. | Local process orchestration lesson. | Multiple independent bots and dashboard compete for RPC. | Replaced by Docker Compose services and internal networking. |

## Secret Exposure Report

No secret values are copied here.

| File path | Secret category | Remediation action |
| --- | --- | --- |
| `.env` | RPC provider URLs/credentials | Keep ignored; rotate if ever shared; move Phoenix values to a new uncommitted `.env`. |
| `.env` | Private key | Do not reuse in Phoenix; rotate if repository was exposed; load signer key only from secret manager or uncommitted env. |
| `.env` | Telegram bot token/chat identifier | Keep out of Phoenix; rotate if exposed; dashboard alerts should use placeholders only. |
| `hardhat.config.js` | Reads `PRIVATE_KEY` and `RPC_URL` | Phoenix keeps placeholders only and does not import old config. |

## Lessons Applied

- RPC scarcity is a cold-path constraint, not the search trigger.
- A single route object must survive detection, simulation, optimization, execution, and reconciliation.
- Estimated gross profit is never realized PnL.
- Free provider limits should slow bootstrap/reconciliation, not the hot opportunity loop.
- Unsupported origins are measured instead of guessed.

