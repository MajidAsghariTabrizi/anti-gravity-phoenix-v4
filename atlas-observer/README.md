# Phoenix Atlas SVR read-only observer

This component implements only Phase 1 of the Phoenix Atlas mission. It connects
to Chainlink's official Atlas Searcher Gateway, subscribes to
`userOperations`, validates the Arbitrum chain, Atlas, and DappControl
identities, classifies the current official Arbitrum Aave-SVR aggregators, and
writes a bounded append-only auction ledger.

It contains no signer, private-key input, bond operation, solver contract,
SolverOp builder, bid method, or transaction submission path.

The observation stops after 72 hours or 500 unique auctions, whichever occurs
first. Unknown post-auction and economic fields remain explicit JSON `null`
until they are reconciled from public chain evidence.

`atlas-reconciler` accepts a bounded, sanitized public JSON-RPC transcript on
standard input. It verifies the exact Arbitrum and Atlas identities, decodes
the Atlas v1.6.4 `metacall`, `SolverTxResult`, and `MetacallResult` evidence,
matches the canonical user-operation hash, and decodes Aave V3
`LiquidationCall` events emitted by the reviewed Arbitrum Pool. Results are
written to a separate append-only `reconciliation.ndjson` file; the raw auction
ledger is never rewritten. Mature auctions with no public settlement keep all
unknown post-auction values null.

The transcript intentionally contains no RPC URL or credential. The
reconciler has no network, signer, bid, bond, or submission capability.

Official identities used by this observer were verified on 2026-07-31 against:

- Chainlink SVR Searcher Onboarding: Atlas, contracts v1.6.4;
- Chainlink's current Arbitrum Aave-SVR aggregator table;
- Aave's current `AaveV3Arbitrum` address book;
- Arbitrum chain ID 42161 and non-empty onchain bytecode.

Run locally with an isolated directory:

```sh
go run ./cmd/atlas-observer --ledger-dir /absolute/private/path
```
