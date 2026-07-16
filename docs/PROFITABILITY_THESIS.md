# Profitability Thesis

## Status

Phoenix has a SHADOW profitability evidence foundation. It does not have evidence of a profitable strategy, a production decision loop, an approved executor deployment, a signer, or LIVE readiness.

Profitability remains unproven until the SHADOW evidence gates pass.

## Implemented System

- Feed Ingestor accepts real Arbitrum Nitro feed messages, normalizes supported transactions, and publishes them to JetStream.
- Recorder durably consumes `phoenix.feed.tx` and persists feed evidence in PostgreSQL before acknowledging it.
- Engine library code decodes one supported Uniswap V3 `exactInputSingle` origin shape, maps touched pools to configured two-pool routes, performs bounded local arithmetic, sizes candidates, evaluates conservative economics, applies deterministic SHADOW policy, and models simulation evidence.
- Replay parses recorded-style NDJSON evidence, orders it by block, sequence, and case ID, evaluates the same economics and decision code, and emits stable decision and clustered evidence reports.
- RPC Gateway has deterministic provider priority, budgets, coalescing, cache primitives, circuit breakers, block-pinned economic request identity, archive capability checks, and provider disagreement contracts.
- PostgreSQL migration `003_shadow_profitability_evidence.sql` adds additive SHADOW decision, RPC-quality, and replay-run evidence tables.
- The dashboard exposes SHADOW economics and quality metrics under the label `SHADOW / SIMULATED — NOT REALIZED CAPITAL PNL`.

## Not Yet Implemented

- The production Engine binary does not consume JetStream or persist decisions. It currently initializes configuration/readiness and then idles.
- Pool state reconstruction is a library model, not a live reconciled Arbitrum state service.
- No production route registry, verified token allowlist, verified pool allowlist, or verified contract deployment is configured.
- No live block-pinned RPC transport calls the economic RPC contracts.
- No contract-level or fork simulation service is connected to the decision engine.
- No real SHADOW opportunity sample, latency distribution, or economic outcome sample has been collected.
- No wallet, signer, nonce manager, submission path, or realized-PnL reconciliation is enabled.

## Falsifiable Thesis

The selected beachhead is origin-aware two-pool V3 arbitrage. The hypothesis is that a supported pending swap can create a short-lived price discrepancy between two allowlisted V3 pools that remains positive after pool and protocol fees, flash liquidity fees, price impact, slippage reserve, Arbitrum execution gas, L1 data fee, contract overhead, failure reserve, stale-state risk, latency, and uncertainty.

The hypothesis is rejected if any of these persist under clean SHADOW operation:

- conservative aggregate net PnL is non-positive;
- severe-case losses are catastrophic relative to configurable risk limits;
- positive aggregate PnL is dominated by one or two outliers;
- hypothetical inclusion latency removes the modeled edge;
- deterministic contract/fork simulation has a materially low success rate;
- required pool state cannot be reconstructed without ambiguous or stale reads;
- provider disagreement is frequent or unexplained;
- supported origin coverage is too narrow to produce an independent sample;
- competitive opportunity half-life is below measured end-to-end latency;
- expected results rely on unverified fee, gas, token, pool, or contract assumptions.

## Evidence Path

1. Wire Engine consumption and complete evidence persistence in a separate reviewed implementation.
2. Reconcile and hash block-pinned pool state outside the Nitro decode hot path.
3. Run deterministic replay and contract/fork simulation against the same evidence.
4. Operate clean SHADOW for at least seven full days, preferably fourteen.
5. Evaluate clustered confidence intervals, concentration, drawdown, sensitivity, and out-of-sample behavior.
6. Abandon or revise the strategy when an evidence gate fails. Do not compensate by removing costs or weakening readiness.

## Safety Boundary

This branch establishes economic truth contracts only. `PHOENIX_MODE=SHADOW` and `LIVE_EXECUTION=false` remain the deployment defaults. No wallet, key, signer, funded account, contract deployment, or automatic LIVE activation is part of this work.
