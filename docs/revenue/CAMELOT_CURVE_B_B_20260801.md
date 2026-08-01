# Camelot → Curve exact revenue proof — B-B

## Decision

**B-B — full-cost negative; do not implement an adapter and do not canary.**

At Arbitrum block `489927908` (`0x88bf46c24f3a633e9f1683358e93b8c449323ace2c1f0d8d83a36365f7beacee`), two independently configured reviewed RPC providers returned the same block, identities, state, code hashes, and exact seven-size quote ladder. Every Camelot WETH → USDC → Curve WETH round trip was gross-negative before flash premium, execution gas, L1 data fee, ordering cost, failure reserve, and the retained-profit floor.

The least-negative row was the minimum reviewed size:

| Field | Exact integer value |
|---|---:|
| WETH input | `100000000000000` wei |
| Camelot USDC output | `186626` |
| Curve WETH output | `99379294658882` wei |
| Gross PnL | `-620705341118` wei |
| Aave flash premium | `50000000000` wei |
| Expected net upper bound | `-670705341118` wei |
| Conservative net upper bound | `-683205341118` wei |
| Severe net upper bound | `-720705341118` wei |
| Retained-profit floor | `1000000000000` wei |

Because all omitted costs are non-negative, this is a monotone proof: actual full-cost PnL can only be lower. A fork transaction and atomic adapter are neither economically justified nor capable of reversing the B-B classification.

## Exact identity

The immutable fixture binds:

- Arbitrum One chain ID `42161`.
- WETH `0x82af49447d8a07e3bd95bd0d56f35241523fbab1`.
- USDC `0xaf88d065e77c8cc2239327c5edb3a432268e5831`.
- Camelot AMMv3 factory `0x1a3c9b1d2f0529d97f2afc5136cc23e58f1fd35b`.
- Camelot quoter `0x0fc73040b26e9bc8514fa028d998e73a254fa76e`.
- Camelot router `0x1f721e2e82f6676fce4ea07a5958cf098d339e18`.
- Camelot pool `0xb1026b8e7276e7ac75410f1fcbbe21796e8f7526`.
- Curve StableSwapNG factory/registry `0x9af14d26075f142eb3f292d5065eb3faa646167b`.
- Curve StableSwapNG implementation `0xf6841c27fe35ed7069189afd5b81513578afd7ff`.
- Curve pool `0x85bbd07ec4d0fc23c42b6ca4af266eaec65342fb` (`factory-stable-ng-319`).
- Aave V3 pool `0x794a61358d6845594f94dc1db02a252b5b4814ad` and its on-chain 5 bps simple-flash premium.

The fixture also persists every runtime code hash. Camelot's factory resolves the bound WETH/USDC pool, the pool reports the same factory and token order, and the Curve factory reports the bound pool coins and implementation.

Official primary sources:

- [Camelot Arbitrum One deployments](https://docs.camelot.exchange/contracts/arbitrum/one-mainnet)
- [Curve StableSwapNG repository](https://github.com/curvefi/stableswap-ng) at commit `2abe778f40206a6c0fd108a0a53ad3266cbedeee`
- [Curve official pool API](https://api.curve.finance/v1/getPools/arbitrum/factory-stable-ng)

## Cost boundary

The proof preserves existing Phoenix gates:

- size ladder `10^14` through `10^16` wei;
- 5 bps flash premium;
- 1.25× conservative cost multiplier;
- zero ordering-payment authority;
- retained-profit floor `10^12` wei.

Expected, conservative, and severe values in the fixture are deliberately optimistic upper bounds. They subtract the exact flash premium but assume zero execution gas, zero L1 data fee, zero ordering cost, and zero failure reserve. Even those upper bounds are negative. Measuring additional positive costs cannot change B-B to B-A.

## Security review

No adapter, calldata builder, executor integration, route policy, approval, signer path, submission path, or Production configuration was added. The snapshot exporter:

- permits only bounded read-only JSON-RPC methods;
- reads the reviewed provider set from the existing RPC gateway container;
- never prints provider URLs or environment values;
- requires two providers and aborts on identity, state, code-hash, or quote disagreement;
- rejects any identity outside the exact WETH/USDC Camelot and Curve path.

This PR is evidence-only and must not merge as an execution feature. The path should be reconsidered only after a fresh exact snapshot shows positive gross PnL before full-cost proof.

## Reproduction

Stream the exporter to the reviewed Phoenix host over the existing BatchMode SSH identity, then validate the committed sanitized evidence locally:

```text
python scripts/camelot_curve_economic_gate.py fixtures/revenue-proof/camelot_curve_b_b_489927908.json
```

Expected classification: `B-B` with best expected-net upper bound `-670705341118` wei.
