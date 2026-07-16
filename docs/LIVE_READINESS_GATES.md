# Live Readiness Gates

## Current Verdict

LIVE is blocked. The production Engine decision loop, verified state service, simulation service, deployment, wallet, signer, nonce manager, and real SHADOW evidence do not exist.

Profitability remains unproven until the SHADOW evidence gates pass.

## Mandatory SHADOW Gates

All gates are conjunctive:

1. At least seven complete days of clean SHADOW operation; fourteen days is preferred before any capital proposal.
2. No unresolved Feed sequence regression, decoder corruption, Recorder/JetStream loss, PostgreSQL integrity failure, or replay mismatch.
3. Every decision is reproducible from persisted code/config/strategy/policy versions and block-pinned evidence.
4. A statistically meaningful independent sample exists after clustering by block, route, pool set, market event, and short time window.
5. Median net PnL is positive and cluster-bootstrap confidence bounds are reviewed.
6. Conservative aggregate PnL is positive.
7. Severe outcomes remain inside separately approved, configurable risk limits.
8. Positive results are not dominated by one or two opportunities, one protocol, or one token pair.
9. Net PnL remains positive under measured gas, slippage, state drift, and inclusion latency sensitivity.
10. Contract/fork simulation success is consistently high and every failure is classified.
11. RPC latency, timeout, retry, archive availability, stale-read, and disagreement behavior are stable.
12. Rejection reasons are complete, bounded, and explainable; candidates are not silently dropped.
13. Dashboard, metrics, and persisted evidence agree and remain explicitly SHADOW-labeled.
14. Contract bytecode and allowlists are reviewed and match simulation evidence.
15. All safety checks fail closed under dependency outage and recovery tests.

## Seven-Day Review

Review daily integrity, candidate funnel, independent sample growth, simulation rate, three scenario aggregates, latency decay, concentration, drawdown, RPC quality, rejection distribution, and replay equality. Seven days can only establish whether more SHADOW observation is justified; it cannot authorize LIVE automatically.

## Fourteen-Day Review

Repeat the seven-day review with held-out days, compare weekday/hour stability, examine cluster-bootstrap intervals, verify no late integrity corrections, and require explicit security, contract, infrastructure, and risk sign-off. Any material code, strategy, policy, route, contract, or cost-model change restarts the evidence window unless reviewers document why evidence remains comparable.

## Capital Protection Design

Before a canary, all limits must be configuration-validated, persisted with decisions, exposed as bounded metrics, and enforced outside the UI:

- maximum notional per transaction;
- maximum daily exposure and daily loss;
- maximum consecutive failures;
- minimum Base and stress PnL;
- gas price cap;
- token, pool, protocol, and contract allowlists;
- maximum quote and simulation age;
- maximum provider disagreement;
- emergency pause and kill switch;
- manual arm/disarm with automatic SHADOW fallback;
- one transaction at a time;
- wallet balance floor and ceiling.

No capital amount is hardcoded by this repository foundation.

## Tiny-Capital Canary Plan

The canary is design-only and requires a separate branch and release, reviewed deployment, dedicated low-balance wallet, manual approval and arm, strict configurable notional/daily-loss/gas caps, one transaction at a time, automatic disarm, complete audit logs, immediate SHADOW fallback, and receipt/realized-PnL reconciliation. It cannot begin until every SHADOW gate passes.

## Automatic Blockers

Any integrity failure, replay mismatch, ambiguous block, provider disagreement, stale state, simulation failure, contract-code mismatch, unknown token behavior, missing risk evidence, or unavailable persistence makes strategy and SHADOW readiness zero. No dashboard control may override this state.
