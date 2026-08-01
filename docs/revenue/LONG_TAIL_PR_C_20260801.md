# PR C — Event-specific long-tail replay and LINK coverage

## Decision

All ten immutable events remain `STATE_INCOMPLETE`.  Their initiating notionals
are positive optimistic ceilings and therefore replay triggers, but none has an
exact post-initiating-transaction boundary.  Expected, conservative, and severe
PnL are intentionally absent.  Candidate count is zero and no route authority
is justified.

The exact blocker for every event is
`transaction_trace_method_unsupported`.  The minimum external evidence needed
is either:

1. reviewed historical `debug_traceTransaction` prestate and diff responses at
   the listed blocks, followed by independent-provider agreement; or
2. a sanitized canonical raw-transaction/receipt transcript plus reviewed
   archive access at the listed parent block, so the bounded replay can execute
   transaction indexes `0..initiating_index` and stop before the next canonical
   transaction.

The gate in `scripts/long_tail_event_replay.py` accepts only those two methods.
It rejects `latest`, `pending`, safe/finalized head tags, end-of-block state,
skipped or reordered transactions, replay past the target transaction, missing
initiating/alternative pool state, and provider disagreement.

## Immutable target results

| Surface | Transaction | Block / tx index / event index | Input | Parent | Result |
| --- | --- | --- | ---: | --- | --- |
| USDC/WETH/UNI | `0x089f78f2c6582bca6721a05efb6ea49b6cdcc5d7ac98e45ed3b2f1b4bff60c72` | `489692643 / 3 / 5` | `98210000` | `489692642` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x1f8ecad783494ababd17fb217133d7e331538238dcd743121c22db38e4106d39` | `489816451 / 3 / 3` | `1080000000000000000` | `489816450` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x2d8c3b1e585ba717c393d6efb62a8f6c807572fbd488c128ba5ce0410a40737e` | `489816587 / 4 / 10` | `1090000000000000000` | `489816586` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x671a7aae11248eb19f5984e33dd0016dc19c340a7d032acc85cdeb0c23e82702` | `489816656 / 11 / 24` | `1090000000000000000` | `489816655` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x1d30e4fb2437fe61e4809899d758585955d6c207cdfb1ab15c5ed155144207ff` | `489817265 / 1 / 3` | `1090000000000000000` | `489817264` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x64dd49ec80d7afdebd77eead05e5300535c07e7415fe1f4f9824803f00f07ac0` | `489817272 / 1 / 3` | `1090000000000000000` | `489817271` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x5081dcb64cdd283851338d51c3779766a73491112ef76c98221c63a7f8a65cd1` | `489817319 / 6 / 8` | `1090000000000000000` | `489817318` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0xc3e06072b384b9b34ec455922c199bba20b6edd4a30f5b088166d99d5cc205bd` | `489818133 / 5 / 6` | `1090000000000000000` | `489818132` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0x1afdd9348ffa0a48b9d495854ff21f8d5da0a87abfd32fd09587c417e599f7cd` | `489818180 / 3 / 17` | `1090000000000000000` | `489818179` | `STATE_INCOMPLETE` |
| AAVE/WETH/USDC | `0xb0043b0d52fccbf0ec7f84bca112ea98aaf90fa5bcc4504df75d9769e1717bf9` | `489818751 / 1 / 3` | `1090000000000000000` | `489818750` | `STATE_INCOMPLETE` |

The immutable fixture also binds every full block hash, parent hash, source
identity hash, command index, token path, fee path, pool path, and resolved pool
address.  Its canonical SHA-256 is
`a840cef45e68dbebb74c00528825a94513e45337fc94d55e3bca5c596be7ddc8`.

## LINK

### Atlas LINK

The one exact LINK settlement is auction
`94b5e867-122c-4dca-84b8-f106feaaef67`, transaction
`0x4976ddd7451654b757094ca8da733f064fd51a3c84195eedb66eb6c205fb861a`
at block `489762211`.  The parent and settlement price were both `814423817`,
so price delta, newly induced HF crossings, and public liquidations were all
zero.  It is not a liquidation opportunity.  LINK remains mandatory for future
non-zero-delta borrower-risk processing.

### DEX LINK/WETH

A bounded read-only scan at `2026-08-01T09:03:10.980248Z` covered all `288`
append-only source identities and found zero exact adjacent LINK/WETH hops.
The matcher requires the adjacent token pair, matching fee at that hop,
numeric command index, exact Uniswap V3 factory, and resolved pool address.  It
does not inspect recipient, calldata substring, raw substring, or final token.

Status is **LINK-D**: no exact LINK/WETH event observed; bounded sensor
continues.  Zero observations are not zero-opportunity proof.  A future exact
match must pass the optimistic upper-bound screen before state replay, and a
positive ceiling must evaluate independently bound Uniswap V3, SushiSwap V3,
Camelot, and Curve pools at the same transaction boundary before any Candidate
recommendation.

## Usage and tests

```text
python scripts/long_tail_event_replay.py \
  --allowlist fixtures/long-tail/immutable_events_20260801.json \
  --link-backfill fixtures/long-tail/link_backfill_20260801.json \
  --atlas-link fixtures/long-tail/atlas_link_zero_delta_20260801.json
python -m unittest discover -s scripts/tests -p test_long_tail_event_replay.py -v
```

The tool is analysis-only.  It has no signer, public broadcast, route policy,
execution request, Production mutation, or generic arbitrary-call path.
