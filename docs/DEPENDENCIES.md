# Dependencies and Protocol Sources

This file records protocol-critical source references and versions. Do not add addresses or ABI assumptions elsewhere without updating this file.

## Arbitrum Nitro Feed Relay

- Official source: Offchain Labs Nitro GitHub repository.
- Verified release page: `https://github.com/OffchainLabs/nitro/releases/tag/v3.11.2`.
- Verified Docker image from the release notes: `offchainlabs/nitro-node:v3.11.2-3599aca`.
- Production manifest-list digest: `sha256:ebc985e3b105980734630744981e1542001c22d74cba57509fe0d5ed8bb84c14`.
- Release date shown by GitHub: 2026-07-06.
- Chain id: `42161` for Arbitrum One.
- Support policy source: Nitro repository README currently lists supported versions and notes the current minor support policy.
- Feed input flags verified from official Nitro `broadcastclient.ConfigAddOptions`: `node.feed.input.url`, `node.feed.input.secondary-url`, `node.feed.input.require-chain-id`, `node.feed.input.require-feed-version`, reconnect backoff, timeout, and compression options.
- Feed output flags verified from official Nitro `wsbroadcastserver.BroadcasterConfigAddOptions`: `node.feed.output.enable`, `node.feed.output.addr`, `node.feed.output.port`, client version requirements, compression, and backlog settings. Default output port in source is `9642`.
- Feed envelope structures verified from official Nitro `broadcaster/message/message.go`, `arbos/arbostypes/messagewithmeta.go`, and `arbos/arbostypes/incomingmessage.go`.
- Feed WebSocket protocol headers and versions verified from official Nitro `wsbroadcastserver/wsbroadcastserver.go`.
- OffchainLabs/go-ethereum submodule commit used by Nitro `v3.11.2`: `f3a977ddf30b138da2fe673ac5cbff2bc6dd4c88`.
- Transaction type identifiers verified from that submodule: standard `0x00`, `0x01`, `0x02`, `0x03`, `0x04`; Arbitrum `0x64`, `0x65`, `0x66`, `0x68`, `0x69`, `0x6a`, `0x78`.
- Arbitrum unsigned transaction payload type `0x65` is the only transaction payload currently decoded by Phoenix.
- Local Compose uses one relay ingress and exposes it only inside the Docker network.

The live relay command still requires a Linux host and operator validation against current Arbitrum node flags. Phoenix does not let individual services connect independently to the upstream public feed.

Current feed-ingestor status: Nitro relay mode is implemented for first runtime verification with a version-pinned WebSocket/envelope adapter and Arbitrum unsigned transaction payload support. Production relay mode remains blocked by startup guard until the adapter is live-verified against the real Arbitrum feed and unsupported payload coverage is resolved. See `docs/NITRO_FEED_INTEGRATION.md`.

## Production Infrastructure Images

The production Compose contract pins multi-platform manifest digests rather
than resolving mutable tags at deploy time:

- NATS `2.10-alpine`: `sha256:b83efabe3e7def1e0a4a31ec6e078999bb17c80363f881df35edc70fcb6bb927`
- PostgreSQL `16-alpine`: `sha256:57c72fd2a128e416c7fcc499958864df5301e940bca0a56f58fddf30ffc07777`
- Prometheus `v2.53.0`: `sha256:075b1ba2c4ebb04bc3a6ab86c06ec8d8099f8fda1c96ef6d104d9bb1def1d8bc`

Changing one of these digests is a reviewed dependency update. Phoenix-owned
images remain bound to the exact merged Git SHA and registry digest through
`phoenix.release.v1`.

## Uniswap V3 on Arbitrum One

Official source: Uniswap developer deployment page for Arbitrum V3 deployments.

Verified entries used by Phoenix configuration:

- Chain id: `42161`.
- Factory: `0x1F98431c8aD98523631AE4a59f267346ea31F984`.
- SwapRouter: `0xE592427A0AEce92De3Edee1F18E0157C05861564`.
- SwapRouter02: `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45`.
- UniversalRouter: `0xa51afafe0263b40edaef0df8781ea9aa03e381a3`.
- QuoterV2, parity tests only: `0x61fFE014bA17989E743c5F6cB21bF9697530B21e`.
- WETH: `0x82aF49447D8a07e3bd95BD0d56f35241523fBab1`.

The Uniswap page states the listed deployments are current and warns integrators to confirm per-chain mappings. Phoenix validates configured addresses with `eth_getCode` through `rpc-gateway` at startup when credentials exist.

Engine origin decoding is pinned separately for each reviewed entrypoint. See `docs/UNISWAP_ENTRYPOINTS.md`; no ABI layout is shared between SwapRouter and SwapRouter02.

Autonomous LIVE state reads use the official Uniswap V3 Core interfaces and
the deployed pool bytecode contract:

- `IUniswapV3PoolImmutables`: factory, token pair, fee, and tick spacing.
- `IUniswapV3PoolState`: `slot0`, active liquidity, `tickBitmap`, and `ticks`.
- Factory `getPool(token0, token1, fee)` is checked against every reviewed
  pool before state is accepted.
- Source: `https://github.com/Uniswap/v3-core/tree/main/contracts/interfaces`.
- Pool implementation source:
  `https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol`.
- Canonical pool-address derivation is pinned to Uniswap V3 Periphery
  `PoolAddress.sol`: ordered `token0`/`token1`, `uint24 fee`,
  `keccak256(abi.encode(token0, token1, fee))`, factory, and pool init-code
  hash
  `e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54`.
  Source:
  `https://github.com/Uniswap/v3-periphery/blob/main/contracts/libraries/PoolAddress.sol`.

Pinned-block reads are batched through Multicall3 `aggregate3` at the canonical
deployment `0xcA11bde05977b3631167028862bE2a173976CA11`. Each inner result must
succeed and decode under its exact ABI; partial batches are rejected. Source:
`https://github.com/mds1/multicall3`.

## Exact Transaction-Boundary State Evidence

Source identity enrichment uses the provider `debug_traceTransaction` method
with the built-in `prestateTracer`. Prestate mode supplies the accounts and
storage needed to execute the exact mined transaction; `diffMode: true`
supplies the changes caused by that transaction. Phoenix binds both responses
to the canonical transaction, receipt, block hash, transaction index, ordered
Swap logs, decoded command, token/fee path, and CREATE2-derived pool addresses,
then applies only the returned account diff to the matching prestate. A current
head read, an end-of-block read, or an unrelated transaction trace is never
substituted for this boundary.

The trace is evidence rather than a cryptographic state proof. Missing trace
support, pruned historical state, timeout, budget exhaustion, oversized output,
partial touched-pool state, and provider-integrity failure remain distinct
`incomplete` results and are never promoted to exact post-initiating state.
Source:
`https://geth.ethereum.org/docs/developers/evm-tracing/built-in-tracers`.

## SushiSwap V3 on Arbitrum One

Official source inspected: Sushi docs and `llms-full.txt`, which identifies the public `sushi` / `sushi/evm` SDK entrypoints and references V3 factory/init-code constants.

Current blocker: the inspected docs did not expose Arbitrum-specific Sushi V3 contract values directly in a stable table. Phoenix therefore ships the Sushi registry as required configuration fields and refuses startup validation when they are unset. Do not fill these values from memory or third-party blogs.

Required next verification:

1. Install or inspect the official `sushi` package source.
2. Confirm Sushi V3 factory, router/RouteProcessor entrypoint, quoter/parity target if any, and init code hash for chain `42161`.
3. Record package version, source file, and addresses here.

## Aave V3 Flash Liquidity

Phoenix includes Aave V3 `flashLoanSimple` interfaces only. No Arbitrum provider address is hardcoded. The flash provider is a registry value validated through the cold RPC gateway before LIVE mode can be enabled.

## Aave V3 Arbitrum Borrower Evidence

The offline Atlas borrower index is source-bound to the following official
repositories, but it does not claim that the current source head is the exact
implementation behind the deployed Arbitrum Pool proxy:

- Aave address book commit
  `a1770e87fd61db02a7725cd9eed3b1d07c3980af`, file
  `src/AaveV3Arbitrum.sol` in
  `https://github.com/bgd-labs/aave-address-book`.
- Aave v3-origin commit
  `fd1fbd9150426ca8ace9cee45b4acf912ae84f5b` in
  `https://github.com/aave-dao/aave-v3-origin`.
- Event declarations: `src/contracts/interfaces/IPool.sol`.
- Reserve configuration bit layout:
  `src/contracts/protocol/libraries/configuration/ReserveConfiguration.sol`.
- Health-factor and liquidation validation:
  `src/contracts/protocol/libraries/logic/ValidationLogic.sol`.
- Close factor, seize, fee and dust rounding:
  `src/contracts/protocol/libraries/logic/LiquidationLogic.sol`.

The tracked market fixture intentionally leaves the Pool implementation,
implementation code hash, reserve configuration, reserve indexes and eMode
configuration incomplete. Those values must be resolved at one canonical
checkpoint and the deployed implementation must be mapped to reviewed source
before an inventory can become `complete`. Candidate constants copied from the
current v3-origin source are labeled `review_candidate_*` and are never used by
the evaluator. Exact constants are deployment-bound evidence fields.

The indexer is offline and signerless. It accepts only a hash-bound, contiguous
block/hash transcript and exact scaled token movements. Raw ERC-20 `Transfer`
amounts are not silently treated as scaled Aave balances. A missing archive
range, missing scaled movement, checkpoint user-configuration bitmap, reorg,
or incomplete reserve evidence keeps coverage incomplete.

## Arbitrum Transaction Cost Components

LIVE submission quotes use the official Nitro `NodeInterface` virtual contract
at `0x00000000000000000000000000000000000000C8`. The
`gasEstimateComponents(address,bool,bytes)` result binds total gas, the L1 gas
component, L2 base fee, and the ArbOS L1 base-fee estimate to the same
transaction calldata. Source:
`https://github.com/OffchainLabs/nitro-contracts/blob/main/src/node-interface/NodeInterface.sol`.

## Message Encoding

`proto/phoenix.proto` is the canonical typed message schema. The Go ingestor currently publishes canonical JSON matching that schema because `protoc` and generated Protobuf toolchains are not available in this workspace. This is an implementation constraint, not a protocol guess. The schema is ready for generated Protobuf bindings in the deployment toolchain.

## PostgreSQL Migration Runner

- Runner language: Go.
- PostgreSQL driver: `github.com/lib/pq v1.10.9`.
- Migration source: ordered SQL files in `migrations/`.
- Production execution: `/usr/local/bin/migration-runner` bundled into the feed-ingestor image and invoked by `scripts/deploy-release.sh`.

The runner records `schema_migrations`, migration version, SHA-256 checksum, and `applied_at`; it uses a PostgreSQL advisory lock and fails on checksum drift.

## GitHub Actions

Workflow actions are pinned to full commit SHAs in `.github/workflows/`. The dependency ledger and update process are in `docs/GITHUB_ACTIONS_DEPENDENCIES.md`.
