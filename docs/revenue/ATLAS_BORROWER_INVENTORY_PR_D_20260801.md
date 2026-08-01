# Atlas Phase 2 Borrower-Risk Inventory — PR D

## Result

The deterministic borrower index, current-checkpoint importer and
future-auction evaluator are implemented as read-only Python tools. The
implementation is signerless, bondless, bidless, Solver-free and incapable of
Production writes.

The completed Borrow archive is bound to 242/242 verified ranges from block
7,736,400 through 489,813,224, 1,547,315 canonical Borrow logs, 186,426
historically discovered borrowers and content SHA-256
`0f204e03a26ee26c1b9e295ab13e946f15a0102a6929c6d4c00f9c3b893f24a4`.
The current exporter adds a bounded canonical Borrow tail, screens the entire
union with exact Aave configuration debt bits, and retains only debt-bearing
addresses for full state reads.

Current real borrower coverage remains **incomplete** because the configured
reviewed secondary provider returns HTTP 403 for exact current state reads.
The exporter fails before emitting a checkpoint. No current borrower count,
health factor, liquidation pair or PnL is asserted.

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

The historical archive dependency is complete. The shortest remaining blocker
is one reviewed independent provider that can serve exact-block `eth_call` for
the selected current finalized block. The already-configured secondary agrees
on chain and block headers but returns HTTP 403 for exact state reads. It is not
replaced or silently downgraded.

Once that endpoint can serve the existing bounded request set, the exporter
will independently agree reserve configuration/index/eMode data and every
retained borrower configuration/balance used by economics. The broad screen
and bounded Borrow tail may use the primary provider; the artifact records that
scope explicitly. For any non-zero auction, checkpoint-bound flash and reviewed
unwind quotes plus integer gas/L1 attribution remain required before residual
PnL exists.

## Commands and tests

```text
python -m py_compile scripts/atlas_borrower_index.py scripts/tests/test_atlas_borrower_index.py
python -m unittest scripts.tests.test_atlas_borrower_index scripts.tests.test_export_aave_borrow_discovery scripts.tests.test_export_aave_checkpoint -v
python scripts/atlas_borrower_index.py validate-market --input fixtures/atlas-borrowers/arbitrum-market-20260801.json
python scripts/atlas_borrower_index.py build --market fixtures/atlas-borrowers/arbitrum-market-20260801.json --transcript fixtures/atlas-borrowers/archive-transcript-unavailable-20260801.json --output <local-output.json>
python scripts/export_aave_checkpoint.py --container <reviewed-provider-container> --discovery <immutable-borrow-discovery.json> > <private-checkpoint.json>
python scripts/atlas_borrower_index.py import-checkpoint --market fixtures/atlas-borrowers/arbitrum-market-20260801.json --checkpoint <private-checkpoint.json> --output <private-inventory.json>
```

The focused suite covers replay idempotency, reorg rejection, snapshot hash
binding, borrower/feed indexing, scaled/mirror semantics, stable-debt support,
HF before/after, exact quote-bound residual economics, the zero-delta fast path,
and incomplete-coverage preservation.

## Authority boundary

The importer and evaluator read local JSON and write local derived reports. The
checkpoint exporter performs only bounded read-only RPC using provider URLs
loaded without printing them. None of these tools has a database writer,
signer, bond, bid, Solver, transaction-submission, contract, route-policy or
Production mutation path.
