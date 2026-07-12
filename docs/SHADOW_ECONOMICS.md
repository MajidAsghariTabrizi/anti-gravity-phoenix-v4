# Shadow Economics

## Unit Discipline

All values for one opportunity must be expressed in the same settlement asset unit before aggregation. PnL is signed. Gas and L1 fees must be converted into that asset using block-pinned evidence; the current foundation stores the inputs but does not implement that conversion service.

## Cost Equation

The implemented model computes:

`gross_spread = gross_output - principal`

`expected_net_pnl = gross_spread - protocol_fees - pool_fees - price_impact - slippage_buffer - flash_loan_fee - arbitrum_execution_fee - l1_data_fee - contract_overhead - failure_cost_reserve - stale_state_penalty - uncertainty_reserve`

Where:

- Arbitrum execution fee is `estimated_execution_gas * gas_price_wei` after the scenario multiplier.
- Failure reserve is failed-attempt gas cost multiplied by the scenario-adjusted failure probability.
- Stale-state penalty combines probability-weighted stale loss, state-drift reserve, and latency reserve.
- Severe contract overhead also includes the configured replacement-transaction cost.
- Expected value is expected net PnL multiplied by probability of success. It is evidence, not an acceptance shortcut.

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

## Fail-Closed Unknowns

Reject when a required fee, conversion, liquidity boundary, block context, contract code hash, simulation result, token behavior, provider agreement, or state freshness input is missing or ambiguous. Do not substitute zero for an unknown cost in production evidence.
