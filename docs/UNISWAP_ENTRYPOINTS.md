# Reviewed Uniswap Entrypoints

Phoenix classifies only the following official Arbitrum One destinations. Configuration may select a subset, but any other destination remains `possible_aggregator` and is never decoded generically.

| Router kind | Arbitrum address | Pinned source |
| --- | --- | --- |
| `legacy_swap_router` | `0xe592427a0aece92de3edee1f18e0157c05861564` | Uniswap `v3-periphery` tag `v1.0.0`, commit `464a8a49611272f7349c970e0fadb7ec1d3c1086` |
| `swap_router02` | `0x68b3465833fb72a70ecdf485e0e4c7bd8665fc45` | Uniswap `swap-router-contracts` tag `v1.1.0`, commit `8fe4f086cee7c08f0bdb6ebe20c9ab615921c65f` |
| `universal_router` | `0xa51afafe0263b40edaef0df8781ea9aa03e381a3` | Uniswap `universal-router` commit `b122a8d2a5d5cdd616252e62cc5c112cb99b432d` |

Primary deployment metadata is the [official Arbitrum deployment table](https://developers.uniswap.org/docs/protocols/v3/deployments/v3-arbitrum-deployments). Legacy SwapRouter source is pinned to [v3-periphery v1.0.0](https://github.com/Uniswap/v3-periphery/tree/v1.0.0). SwapRouter02 source is pinned to [swap-router-contracts v1.1.0](https://github.com/Uniswap/swap-router-contracts/tree/v1.1.0).

The Universal Router address is recorded as `UniversalRouterV2` in [official deployment commit b122a8d](https://github.com/Uniswap/universal-router/tree/b122a8d2a5d5cdd616252e62cc5c112cb99b432d). Its exact-match [verified Arbitrum source](https://arbiscan.io/address/0xa51afafe0263b40edaef0df8781ea9aa03e381a3#code) uses Solidity `0.8.26`, Cancun EVM, and 44,444,444 optimizer runs. The deployed `UniversalRouter.sol`, `Commands.sol`, and `Dispatcher.sol` match that official commit. This pins `COMMAND_TYPE_MASK = 0x3f`, `V3_SWAP_EXACT_IN = 0x00`, `V3_SWAP_EXACT_OUT = 0x01`, and `EXECUTE_SUB_PLAN = 0x21`.

## Selector And Layout Map

Selectors are derived from these ABI types in tests rather than trusted as standalone comments.

| Router | Selector | Proven ABI layout | Phoenix behavior |
| --- | --- | --- | --- |
| Legacy SwapRouter | `0x414bf389` | `exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))` | Supported; deadline is slot 4 and amount-in is slot 5 |
| Legacy SwapRouter | `0xc04b8d59` | `exactInput((bytes,address,uint256,uint256,uint256))` | Supported with bounded packed V3 paths |
| Legacy SwapRouter | `0xac9650d8` | `multicall(bytes[])` | Supported only for one V3 exact-in plus reviewed payment, refund, or permit companions |
| Legacy SwapRouter | `0xdb3e2198` | `exactOutputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))` | Explicitly unsupported exact-output |
| SwapRouter02 | `0x04e45aaf` | `exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))` | Supported; amount-in is slot 4 |
| UniversalRouterV2 | `0x24856bc3` | `execute(bytes,bytes[])` | One non-optional `V3_SWAP_EXACT_IN` only |
| UniversalRouterV2 | `0x3593564c` | `execute(bytes,bytes[],uint256)` | One non-optional `V3_SWAP_EXACT_IN` only |

Universal Router V3 exact-in command input is exactly `(address,uint256,uint256,bytes,bool)`. V3 exact-output, V2/V4 swaps, multiple swaps, optional swaps, unknown commands, and nested sub-plans remain fail-closed.

## Decoder Bounds

- Maximum outer calldata: 256 KiB.
- Maximum wrapper commands: 16.
- Maximum nested bytes: 128 KiB.
- Maximum packed V3 path: 8 hops and 204 bytes.
- ABI decode must round-trip to the identical canonical encoding; malformed, overlapping, truncated, and trailing dynamic data is rejected.
- Pool identity is `min(tokenA,tokenB):max(tokenA,tokenB):fee`; swap direction remains in `swap_path` and `exact_in`.
- Classification evidence stores only bounded enum kinds and counts. It never stores raw calldata.
