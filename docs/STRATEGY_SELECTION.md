# Strategy Selection

## Method

Scores are unweighted from 1 (least favorable) to 5 (most favorable). For costs and risks, 5 means lower cost or risk. Scores reflect repository implementation on this branch, not theoretical market return.

| Criterion | Two-pool V3 arbitrage | Triangular arbitrage | Liquidation | Cross-protocol discrepancy |
|---|---:|---:|---:|---:|
| Current code completeness | 5 | 2 | 1 | 2 |
| Atomic executability | 5 | 5 | 4 | 3 |
| Capital requirements | 4 | 3 | 4 | 3 |
| Flash liquidity availability | 4 | 3 | 4 | 3 |
| Contract readiness | 4 | 2 | 1 | 1 |
| RPC independence | 4 | 3 | 1 | 1 |
| Feed fit | 4 | 4 | 2 | 2 |
| State reconstruction difficulty | 4 | 2 | 1 | 1 |
| Competition intensity | 1 | 1 | 2 | 2 |
| Expected opportunity frequency | 4 | 2 | 3 | 3 |
| Opportunity half-life | 2 | 2 | 3 | 2 |
| Gas sensitivity | 3 | 2 | 3 | 2 |
| Price-impact sensitivity | 2 | 1 | 3 | 2 |
| Revert risk | 2 | 1 | 2 | 2 |
| Protocol integration count | 2 | 1 | 2 | 1 |
| Security surface | 3 | 1 | 2 | 2 |
| Deterministic simulation ability | 4 | 2 | 2 | 2 |
| SHADOW validation ability | 4 | 2 | 3 | 3 |
| Engineering effort | 3 | 1 | 1 | 1 |
| Probability of measurable net-positive evidence | 3 | 1 | 2 | 2 |
| **Total / 100** | **67** | **42** | **48** | **40** |

## Selection

Primary beachhead: **origin-aware two-pool V3 arbitrage**.

Secondary observational strategy: **none**. Adding a second detector now would split state, simulation, and evidence work before the primary path has economic truth.

Two-pool V3 arbitrage is selected because it is the only path with material repository support: exact-input origin decoding, touched-pool routing, two-leg graph representation, bounded sizing, a local fee-aware swap primitive, an atomic guarded Solidity executor, and SHADOW coordination. Selection does not imply that the strategy has an exploitable edge.

## Invalidating Assumptions

- The supported router command is frequent enough to create an independent sample.
- The affected-pool mapping corresponds to verified deployed pools.
- Relevant V3 state can be reconstructed completely at explicit blocks.
- Flash liquidity and fee inputs are available and verified.
- End-to-end detection and hypothetical inclusion latency are shorter than opportunity half-life.
- Contract and fork simulation match local arithmetic within documented tolerances.
- Competition does not consume nearly all positive severe-case opportunities.
- Token behavior is standard enough for the allowlist and executor assumptions.

## Abandonment Evidence

Abandon or materially revise this beachhead when fourteen-day evidence shows non-positive conservative aggregate PnL, catastrophic severe outcomes, unacceptable simulation failure, persistent provider disagreement, outlier-dominated return, insufficient independent samples, or negative PnL after measured latency. The strategy must also be abandoned if verified deployed protocol behavior conflicts with the local AMM or executor model.

## Missing Data

- real SHADOW origin frequency and coverage;
- verified route/pool registry and state checkpoints;
- observed gas and Arbitrum L1 data fees by execution shape;
- verified flash premium and available liquidity;
- contract-level and fork simulation outcomes;
- provider latency/disagreement distributions;
- opportunity lifetime and pre-inclusion movement;
- competitor/inclusion observations;
- independent clustered outcome count.
