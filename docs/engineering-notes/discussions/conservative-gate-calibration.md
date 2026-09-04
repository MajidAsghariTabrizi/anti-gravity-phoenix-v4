# How should a liquidation engine calculate real profitability?

> **Live Discussion thread:** [#321 — How should a liquidation engine calculate real profitability?](https://github.com/MajidAsghariTabrizi/anti-gravity-phoenix-v4/discussions/321) (Ideas category)

> **Status:** Phoenix is `FULL_LIVE_NO_ALPHA`. This is a discussion of economic modeling, not of realized returns.
>
> **Related Phoenix engineering note:**
> - [`conservative-economic-gate.md`](../conservative-economic-gate.md)

## Background

Public write-ups on liquidation-bot economics usually describe the opportunity calculation as gross value minus gas, minus flash-loan premium. More careful versions subtract slippage. Phoenix subtracts eleven categories of cost across three scenarios (BASE, CONSERVATIVE, SEVERE), gates on the conservative scenario, and uses strict inequality (`>`) at the floor.

The conservative gate in Phoenix has produced 39,538 shadow evaluations and 0 cleared candidates. The aggregate conservative PnL is negative, which the model is designed to produce when no opportunity is real. The honest production story is that the conservative model is correctly conservative.

## Questions for the discussion

1. Which costs does your liquidation economic model actually subtract? Which ones are silently omitted because they're hard to estimate (e.g., L1 data fee on Arbitrum, or the replacement-transaction cost after a failed submission)?
2. How many scenarios do you run? Phoenix runs three: BASE, CONSERVATIVE, SEVERE, with multipliers on per cost category. What other scenario taxonomies are useful?
3. When the conservative gate rejects everything, how do you tell whether the gate is over-conservative or the strategy is structurally unprofitable? Phoenix looks at the *distribution* of the conservative PnL values. What do others look at?
4. What's the right behavior when the gate rejects everything for days? Phoenix does not loosen the gate. Is that the right answer?
5. Has anyone run a controlled experiment where the conservative gate was deliberately loosened for shadow purposes, with the result compared to the actual realized PnL had the gate been at the looser setting?

## What this is not

This is not a claim that Phoenix has executed profitable liquidations. It has not. The system is `FULL_LIVE_NO_ALPHA`. The conservative gate has rejected every modeled opportunity. The honest position is that the gate is doing its job, and the engineering question is how to tell whether the gate is calibrated correctly.

The next engineering step for Phoenix is to examine the distribution of the rejected candidates. Are they marginally negative, or catastrophically negative? The answer determines whether the strategy needs adjustment or the calibration needs adjustment. The conservative gate itself should not be the variable.

What positions have others taken?