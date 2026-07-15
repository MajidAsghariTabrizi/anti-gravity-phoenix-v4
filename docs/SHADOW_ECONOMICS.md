# Shadow Economics

## Unit Discipline

All values for one opportunity must be expressed as integers in the same settlement-asset unit before aggregation. PnL is signed; costs and reserves are unsigned. Gas and L1 fees must be converted into that asset using block-pinned evidence. The current model is valid only when configured fee values already use the route's settlement unit; it does not provide a cross-asset conversion service.

Reports derive the settlement asset from the first token in the canonical round-trip token path and never aggregate financial values across settlement assets.

## Cost Equation

The implemented model computes:

`gross_spread = gross_output - principal`

`gross_profit = gross_spread - protocol_fees - dex_fees - price_impact`

`total_cost = protocol_fees + dex_fees + price_impact + slippage_reserve + flash_loan_premium + arbitrum_execution_fee + l1_data_fee + contract_overhead + failed_attempt_reserve + stale_state_reserve + ordering_reserve + state_drift_reserve + latency_reserve + uncertainty_reserve`

`expected_net_pnl = gross_spread - total_cost`

Where:

- Arbitrum execution fee is `estimated_execution_gas * gas_price_wei` after the scenario multiplier.
- Failure reserve is failed-attempt gas cost multiplied by the scenario-adjusted failure probability.
- Stale-state reserve is probability-weighted stale loss only.
- State-drift and latency reserves are explicit components.
- Severe ordering reserve includes the configured replacement-transaction cost.
- Expected value is expected net PnL multiplied by probability of success. It is evidence, not an acceptance shortcut.
- Every addition, subtraction, multiplication, and unsigned-to-signed conversion in the canonical model is checked. Overflow fails the evaluation.

## Scenario Policy

Multipliers are basis points and are versioned code defaults. They are starting stress policy, not verified Arbitrum market constants. Production SHADOW configuration must replace them with measured distributions and persist the config version.

| Component | Base | Conservative | Severe |
|---|---:|---:|---:|
| Gas | 1.00x | 1.25x | 2.00x |
| L1 data fee | 1.00x | 1.25x | 2.00x |
| Slippage | 1.00x | 1.50x | 3.00x |
| Price impact | 1.00x | 1.25x | 2.00x |
| Failure probability | 1.00x | 1.50x | 2.00x, capped at 100% |
| Stale-state probability/cost | 1.00x | 1.50x | 2.50x |
| State drift | 1.00x | 1.50x | 2.50x |
| Latency reserve | 1.00x | 1.50x | 2.50x |
| Uncertainty reserve | 1.00x | 1.50x | 2.50x |
| Replacement transaction | 0.00x | 0.00x | 1.00x |

An opportunity is not SHADOW-accepted solely because Base is positive. The policy independently requires Base, Conservative, and Severe net PnL to exceed configured signed thresholds, plus complete integrity, freshness, allowlist, liquidity, simulation, contract, confidence, and risk-budget evidence.

## Deterministic Replay

Replay consumes NDJSON evidence with explicit sequence, block/hash, state/response hashes, provider ID, timestamps, economics, simulation classification, and integrity flags. It orders by `(observed_block, source_sequence, case_id)`, reads no wall clock, and performs no live RPC.

The clustered evidence report includes sample and independent block/route counts, accepted/rejected counts, simulation success, mean/median/P25/P75/P95/worst PnL, drawdown, positive rate, largest contribution, protocol/token concentration, hourly/daily bucket counts, three scenario aggregates, ordered in/out sample medians, fixed-seed cluster bootstrap mean confidence bounds, and isolated gas/slippage/latency sensitivities.

The committed eleven-case fixture is test coverage only. It is synthetic and must never be included in a profitability claim.

Replay output labels its financial values `SHADOW expected` or `counterfactual` and `not realized`. Neither replay nor a fork simulation is realized revenue.

## Canonical Persistence

Migration `007_canonical_profitability_truth.sql` adds one canonical fact per persisted SHADOW decision. Complete rows include identity, route and block evidence, every cost component, all three PnL scenarios, model and policy versions, verification state, and immutable non-execution flags. Database and application checks enforce the cost identities, scenario ordering, round-trip path shape, provider-state agreement semantics, and:

```text
shadow_only=true
execution_eligible=false
execution_request_created=false
```

Migration `009_profit_triggered_secondary_verification.sql` adds explicit
profit-triggered independent-verification evidence. New complete rows persist
the route hash, terminal status and lifecycle, and same-block/same-route
secondary proof when present. Existing rows remain historical nulls rather
than receiving inferred evidence.

Rows created from older decisions and candidate classifications are retained as `incomplete`. Missing financial fields remain `NULL`; they are never filled with zeros or fixture values. See [`SHADOW_PROFITABILITY_REPORTS.md`](SHADOW_PROFITABILITY_REPORTS.md) for the bounded read-only report.

## Fail-Closed Unknowns

Reject when a required fee, conversion, liquidity boundary, block context, contract code hash, simulation result, token behavior, provider agreement, or state freshness input is missing or ambiguous. Do not substitute zero for an unknown cost in production evidence.
