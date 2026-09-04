# Eleven Costs a Liquidation Bot Probably Isn't Subtracting (and One Invariant It's Probably Violating)

> **Status:** ARCHITECTURE + IMPLEMENTATION. Includes arithmetic worked examples from Phoenix's economic model. **Profitability: NO VERIFIED ALPHA YET.**

> **Audience:** Anyone building or reviewing a flash-loan-assisted Aave V3 liquidation strategy on Arbitrum (or any L2 where L1 data fees are non-trivial).

## Why this exists

Most liquidation bot write-ups I've read describe the opportunity calculation as: liquidation bonus − gas cost. A more careful version subtracts flash-loan premium and pool fees. The most careful version I've seen also subtracts slippage.

In Phoenix's experience running against live Arbitrum state for several months, **none of these are sufficient.** The conservative gate that Phoenix runs (`phoenix-engine/src/economics/mod.rs`) subtracts eleven categories of cost, and the production shadow ledger suggests that even *that* may not be conservative enough — the aggregate conservative PnL is negative, and the system is currently `FULL_LIVE_NO_ALPHA` because every modeled opportunity remains below the retained-profit floor.

This article enumerates the eleven costs Phoenix subtracts, walks through the integer arithmetic used, and surfaces one specific invariant (`>` not `>=`) that is easy to violate in implementation.

## The eleven costs

Phoenix's `EconomicInput` struct (`phoenix-engine/src/economics/mod.rs` line 9-35) has the following cost-relevant fields. The names map to the categories below.

| # | Field | What it represents | Notes |
|---|-------|--------------------|-------|
| 1 | `protocol_fees` | Aave V3 flash-loan premium + any protocol-level fee on the path | Required to repay principal + premium in one tx |
| 2 | `pool_fees` | DEX pool fees across all swap legs | Per `fee * amount_in` for each V3 leg |
| 3 | `price_impact` | Expected slippage from crossing the V3 tick range | Bounded by the chosen `amount_in` candidate |
| 4 | `minimum_slippage_buffer` | Reserve against price movement between quote and execution | Capped at scenario multiplier |
| 5 | `flash_loan_fee` | Explicit flash-loan premium (bps) | Aave's premium is `0.05%` (`5 bps`) on Arbitrum V3 |
| 6 | `arbitrum_execution_fee` | `gas_price_wei * estimated_execution_gas` | Multiplied by scenario gas multiplier |
| 7 | `l1_data_fee` | Arbitrum L1 data posting fee | **The dominant cost on Arbitrum** — independent of `gas_price_wei` |
| 8 | `contract_overhead` | Fixed-cost gas attributable to the executor contract | Calldata, storage updates, events |
| 9 | `failure_cost_reserve` | `failure_probability_bps * failed_attempt_gas_cost` | Accounts for reverting transactions |
| 10 | `stale_state_penalty` | `stale_quote_probability_bps * stale_state_loss` | Reserves against using a quote whose underlying state has moved |
| 11 | `state_drift_reserve`, `latency_reserve`, `uncertainty_reserve` | Three separate reserves for drift between simulation and execution, end-to-end latency, and model error | All scaled by scenario multiplier |

Plus `replacement_transaction_cost` for the severe scenario (mode 5 below).

## Why the L1 data fee is the most-subtracted cost in Phoenix's shadow data

On Arbitrum, the gas we pay as `gas_price * gas_used` is *not* the cost we should model. The L2 execution cost is supplemented by an L1 data posting fee that depends on calldata size and the L1 base fee at the time of submission. The L1 component has been the larger portion of total execution cost for most blocks in 2025-2026.

Phoenix models L1 separately (`l1_data_fee` is its own `Amount` field) rather than folding it into `arbitrum_execution_fee`. The Phoenix source records an explicit comment in `docs/RPC_BUDGET.md` (referenced from the `rpc-gateway` module) about budgeting for L1 reads. The economic model treats `l1_data_fee` as scenario-multiplied independently (`l1_fee_multiplier_bps`), so the conservative scenario applies a 1.25× multiplier to L1 alone.

## The three scenarios

`ScenarioConfig` (`economics/mod.rs` line 37-90) defines three named scenarios:

```rust
pub const BASE: Self = Self {
    gas_multiplier_bps: 10_000,
    l1_fee_multiplier_bps: 10_000,
    slippage_multiplier_bps: 10_000,
    price_impact_multiplier_bps: 10_000,
    failure_multiplier_bps: 10_000,
    stale_state_multiplier_bps: 10_000,
    state_drift_multiplier_bps: 10_000,
    latency_multiplier_bps: 10_000,
    uncertainty_multiplier_bps: 10_000,
    replacement_cost_multiplier_bps: 0,
};

pub const CONSERVATIVE: Self = Self {
    gas_multiplier_bps: 12_500,       // 1.25x
    l1_fee_multiplier_bps: 12_500,    // 1.25x
    slippage_multiplier_bps: 15_000,  // 1.50x
    price_impact_multiplier_bps: 12_500,
    failure_multiplier_bps: 15_000,
    stale_state_multiplier_bps: 15_000,
    state_drift_multiplier_bps: 15_000,
    latency_multiplier_bps: 15_000,
    uncertainty_multiplier_bps: 15_000,
    replacement_cost_multiplier_bps: 0,
};

pub const SEVERE: Self = Self {
    gas_multiplier_bps: 20_000,       // 2.0x
    l1_fee_multiplier_bps: 20_000,    // 2.0x
    slippage_multiplier_bps: 30_000,  // 3.0x
    price_impact_multiplier_bps: 20_000,
    failure_multiplier_bps: 20_000,
    stale_state_multiplier_bps: 25_000,
    state_drift_multiplier_bps: 25_000,
    latency_multiplier_bps: 25_000,
    uncertainty_multiplier_bps: 25_000,
    replacement_cost_multiplier_bps: 10_000,
};
```

The primary decision — whether the candidate clears the floor — is made on the **conservative** scenario, not the base scenario. From `evaluate_scenarios` (line 99-120):

```rust
primary_status: if conservative.expected_net_pnl > input.minimum_required_net_pnl {
    PrimaryProfitabilityStatus::MeetsMinimum
} else {
    PrimaryProfitabilityStatus::BelowMinimum
}
```

`BelowMinimum` is a status, not an error. It is the system's normal output when no opportunity is real.

## The invariant: `>` not `>=`

The minimum-profit floor is enforced with **strict inequality**. From the test at `economics/mod.rs` line 439-449:

```rust
#[test]
fn profit_exactly_at_the_minimum_does_not_clear_the_floor() {
    let mut candidate = input();
    candidate.minimum_required_net_pnl = evaluate(&candidate, ScenarioConfig::CONSERVATIVE)
        .unwrap()
        .expected_net_pnl;
    assert_eq!(
        evaluate_scenarios(&candidate).unwrap().primary_status,
        PrimaryProfitabilityStatus::BelowMinimum
    );
}
```

If `expected_net_pnl == minimum_required_net_pnl`, the gate is **not** cleared. The test asserts this explicitly.

This invariant is easy to violate in a hot loop where `>=` is more natural. Phoenix uses `>` because "exactly at the floor" is the regime where the model is wrong by definition. Any off-by-one in the model's arithmetic is, at the floor, an off-by-one in the system's P&L.

## Integer math, no floats

Every cost calculation uses `u128` and `checked_*` arithmetic. From the `validate` function (line 234-262), the model rejects:

- Negative probabilities.
- `failure_probability_bps > 10_000`.
- `stale_quote_probability_bps > 10_000`.
- `probability_of_success_bps > 10_000`.
- `settlement_asset_decimals > 36`.
- Multipliers above `100_000` (10×).
- Negative `minimum_required_net_pnl`.

The arithmetic operations use `checked_mul`, `checked_add`, `checked_sub`, and explicit `try_from` conversions. The `checked_arithmetic_rejects_overflow` test (line 451-460) confirms that `u64::MAX * u128::MAX` produces `EconomicError::ArithmeticOverflow` rather than a silent wraparound.

This matters in practice because of one specific failure mode: a candidate with `estimated_execution_gas = u64::MAX` and a non-zero `gas_price_wei` will overflow a naive multiplication. The conservative model rejects this; a naive model returns `0` and approves a candidate that would actually cost more than the wallet holds.

## A worked example

Take a liquidation candidate with:

- `principal = 1_000_000` (in settlement-asset base units)
- `gross_output = 1_100_000` (10% gross edge)
- `minimum_required_net_pnl = 50_000` (5% floor on principal)
- All reserves zero
- `gas_price_wei = 100`, `estimated_execution_gas = 10`

Gross spread: `100_000`.
Gas cost at BASE: `1_000`.
Total cost (zero reserves): `1_000`.
Expected net PnL at BASE: `99_000`. ✓ Above floor.

Now apply CONSERVATIVE:

- Gas multiplier 1.25× → `1_250`.
- Total cost: `1_250`.
- Expected net PnL at CONSERVATIVE: `98_750`. ✓ Above floor.

Now apply SEVERE:

- Gas multiplier 2.0× → `2_000`.
- Total cost: `2_000`.
- Expected net PnL at SEVERE: `98_000`. ✓ Above floor.

This candidate clears all three scenarios with a wide margin. In practice, with realistic reserves, candidates that look like this on paper fail at the conservative gate because the *reserves* dominate. Phoenix's production shadow data has 39,538 evaluations and **all of them below the floor**.

## The honest production story

The conservative gate in Phoenix is doing what it is designed to do. The system has produced:

- 39,538 shadow evaluations.
- 0 attempted live executions.
- 0 realized profit or loss.
- Aggregate conservative PnL: **negative**.

That is not a failure. It is evidence that the conservative model is correctly conservative — if it were easy to find profitable liquidations on Arbitrum, the gate would not need to be this strict. The fact that the gate is *strict enough to refuse everything that has been seen* is the design working as intended.

The next engineering question is not "how do we loosen the gate." It is "what is the distribution of the negative-ev candidates?" If most are marginally negative, the gate is calibrated correctly. If many are catastrophically negative, the strategy has a structural problem and the gate should reject them.

## Reviewable evidence

- `phoenix-engine/src/economics/mod.rs` — `EconomicInput`, `ScenarioConfig`, `evaluate`, `evaluate_scenarios`, all scenario-monotonicity and overflow tests.
- `phoenix-engine/src/domain/mod.rs` — `Amount`, `TokenAddress`, `SignedAmount` definitions (referenced from the economic module).
- `phoenix-engine/src/opportunity/mod.rs` — `CostBreakdown`, `ScenarioEconomics`, `PrimaryProfitabilityStatus`.
- `docs/PROFITABILITY_THESIS.md` — Phoenix's published profitability thesis and the falsifiable conditions for strategy rejection.
- `PHOENIX_LESSONS_LEARNED.md` — production shadow ledger and conservative PnL aggregation.

## For your own system

The five-line summary:

1. Subtract eleven categories of cost, not two or three.
2. Run at least three scenarios; gate on the conservative one.
3. Use integer math with explicit overflow checks.
4. Use `>` not `>=` for the floor.
5. If your gate rejects everything, the gate is probably correct. Calibrate by examining the distribution of the rejections, not by loosening it.

The hardest of these to internalize is the last one. A system that never trades is, in the specific sense that matters for safety, more correct than a system that occasionally trades at a small loss.

---

## Related Phoenix discussion

The corresponding Phoenix-owned Discussion thread is now published:

- **Thread:** [How should a liquidation engine calculate real profitability?](https://github.com/MajidAsghariTabrizi/anti-gravity-phoenix-v4/discussions/321) (Ideas category)
- **In-tree draft source:** [`discussions/conservative-gate-calibration.md`](discussions/conservative-gate-calibration.md)

The roundup post at [`../blog/engineering-notes-roundup.md`](../blog/engineering-notes-roundup.md) places this note in the developer-searchable context.
