# Atlas Phase 2 Borrower-Risk Inventory — PR D

## Result

The deterministic borrower index and future-auction evaluator are implemented
as an offline, read-only Python tool. The implementation is signerless,
bondless, bidless, Solver-free and incapable of Production writes.

Current real borrower coverage remains **incomplete**. No reviewed archive
transcript was available to this PR, so the tracked evidence correctly yields
zero *indexed* borrowers and zero evidence events. Those are not claims that
Aave has zero borrowers. No current borrower count, health factor,
liquidation pair, or PnL is asserted.

## Official Arbitrum identities

The hash-bound market fixture is
`fixtures/atlas-borrowers/arbitrum-market-20260801.json` with content SHA-256
`19ac2d2fac94147a232f2b53827642e01a76cdaae86a181b1065d1e647545d60`.
It is pinned to the official Aave address-book commit
`a1770e87fd61db02a7725cd9eed3b1d07c3980af`.

| Identity | Address |
|---|---|
| PoolAddressesProvider | `0xa97684ead0e402dc232d5a977953df7ecbab3cdb` |
| Pool | `0x794a61358d6845594f94dc1db02a252b5b4814ad` |
| Oracle | `0xb56c2f0b653b2e0b10c9b928c8580ac5df02c7c7` |
| ProtocolDataProvider | `0x243aa95cac2a25651eda86e80bee66114413c43b` |

Mandatory feed coverage is present for ARB, WETH/ETH, WBTC/BTC, AAVE and
LINK. Native USDC is included as a common debt/unwind asset. Each reserve binds
the official underlying, aToken, variable-debt token and Aave oracle source.
The current address book has no stable-debt token for these reserves; the
indexer nevertheless supports legacy stable-debt Mint/Burn/Transfer semantics
when an exact market snapshot supplies such an identity.

## Canonical borrower construction

The replay model starts only at a reviewed zero-state boundary. It validates a
contiguous canonical block-number/hash/parent-hash chain and strictly ordered
event identities. Replaying the same evidence is deterministic and produces
the same snapshot hash; a conflicting event identity or a reorg fails closed.

Pool `Supply`, `Withdraw`, `Borrow`, `Repay` and `LiquidationCall` events are
borrower-discovery and consistency evidence. Accounting comes from exact:

- aToken Mint, Burn and Transfer scaled movements;
- variable-debt Mint, Burn and Transfer scaled movements;
- stable-debt Mint, Burn and Transfer balance-adjusted movements;
- reserve configuration and reserve index updates;
- collateral enable/disable events;
- user eMode changes; and
- end-checkpoint `getUserConfiguration` bitmap snapshots.

Mint/Burn `Transfer` mirror logs are explicitly marked `mirror` and cannot
double count primary accounting. A primary token movement without its exact
scaled amount is rejected. Every borrower result persists address, positions,
collateral flags, debt type, eMode, feed dependencies, last block/hash,
evidence hashes, actual and derived account-configuration bitmaps, and explicit
completeness reasons.

## Future auction evaluation

An auction binds both the affected reserve and its exact feed identity.

- Equal pre/post prices return `ZERO_DELTA_NO_RISK_CHANGE` with zero borrower
  evaluations, zero crossings and zero pairs. This is the path for the 19
  already-reviewed Atlas settlements, including the exact LINK settlement.
- A non-zero delta on an incomplete inventory returns
  `INCOMPLETE_INVENTORY`; it does not invent affected borrowers or PnL.
- A non-zero delta on a complete inventory loads only addresses indexed to the
  affected feed, calculates integer HF before/after, distinguishes newly from
  already liquidatable accounts, and enumerates every debt/collateral pair.

The pair evaluator uses deployment-bound close-factor constants, reserve
configuration, eMode thresholds/bonuses, reserve active/paused flags,
liquidation grace periods, Aave floor/ceil seize math, protocol-fee math and
the minimum-leftover dust rule. Current v3-origin constants are retained only
as non-executable review candidates until the Arbitrum Pool implementation and
code hash are source-mapped.

Full-cost PnL is emitted only when the exact pair quote:

- is bound to the inventory checkpoint block and hash;
- names the exact debt/collateral assets;
- binds a flash provider, capacity and premium;
- binds the exact seized collateral input and reviewed unwind venue;
- provides expected, conservative and severe debt-token outputs; and
- attributes DEX fee/impact, gas plus Arbitrum L1 data, ordering cost and
  failure reserve for each scenario.

The maximum rational Atlas bid is
`max(0, conservative residual - retained-profit floor)`. Missing or mismatched
quotes produce no PnL and no bid value.

## Exact remaining external dependency

Completion needs one sanitized, hash-bound Arbitrum archive evidence package:

1. independent agreement on a reviewed zero-state start and checkpoint block
   number/hash;
2. a complete `eth_getBlockByNumber` header chain and `eth_getLogs` range for
   the Pool and every reviewed aToken/variable-debt/stable-debt token from that
   start through the checkpoint;
3. trace/state-diff evidence sufficient to derive exact scaled aToken transfer
   movements (raw ERC-20 amounts are insufficient), or an equivalently reviewed
   exact scaled-balance bootstrap supported by a future explicit snapshot
   input;
4. checkpoint reserve configuration/index/eMode reads and a
   `getUserConfiguration` bitmap for every discovered borrower;
5. deployed Pool proxy implementation, implementation code hash and exact
   source-version mapping; and
6. a second provider confirming the checkpoint hash and bound state reads.

For any non-zero auction, a second package of checkpoint-bound flash and
reviewed unwind quotes plus integer gas/L1 attribution is then required before
residual PnL exists.

## Commands and tests

```text
python -m py_compile scripts/atlas_borrower_index.py scripts/tests/test_atlas_borrower_index.py
python -m unittest scripts.tests.test_atlas_borrower_index -v
python scripts/atlas_borrower_index.py validate-market --input fixtures/atlas-borrowers/arbitrum-market-20260801.json
python scripts/atlas_borrower_index.py build --market fixtures/atlas-borrowers/arbitrum-market-20260801.json --transcript fixtures/atlas-borrowers/archive-transcript-unavailable-20260801.json --output <local-output.json>
```

The focused suite covers replay idempotency, reorg rejection, snapshot hash
binding, borrower/feed indexing, scaled/mirror semantics, stable-debt support,
HF before/after, exact quote-bound residual economics, the zero-delta fast path,
and incomplete-coverage preservation.

## Authority boundary

The CLI reads local JSON and writes a local derived JSON report. It has no RPC,
SSH, database, GitHub, signer, bond, bid, Solver, transaction-submission,
contract, route-policy or Production integration.
