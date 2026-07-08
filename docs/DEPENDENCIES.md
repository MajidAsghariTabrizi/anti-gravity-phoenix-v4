# Dependencies and Protocol Sources

This file records protocol-critical source references and versions. Do not add addresses or ABI assumptions elsewhere without updating this file.

## Arbitrum Nitro Feed Relay

- Official source: Offchain Labs Nitro GitHub repository.
- Verified release page: `https://github.com/OffchainLabs/nitro/releases/tag/v3.11.2`.
- Verified Docker image from the release notes: `offchainlabs/nitro-node:v3.11.2-3599aca`.
- Release date shown by GitHub: 2026-07-06.
- Chain id: `42161` for Arbitrum One.
- Support policy source: Nitro repository README currently lists supported versions and notes the current minor support policy.
- Feed input flags verified from official Nitro `broadcastclient.ConfigAddOptions`: `node.feed.input.url`, `node.feed.input.secondary-url`, `node.feed.input.require-chain-id`, `node.feed.input.require-feed-version`, reconnect backoff, timeout, and compression options.
- Feed output flags verified from official Nitro `wsbroadcastserver.BroadcasterConfigAddOptions`: `node.feed.output.enable`, `node.feed.output.addr`, `node.feed.output.port`, client version requirements, compression, and backlog settings. Default output port in source is `9642`.
- Local Compose uses one relay ingress and exposes it only inside the Docker network.

The live relay command still requires a Linux host and operator validation against current Arbitrum node flags. Phoenix does not let individual services connect independently to the upstream public feed.

## Uniswap V3 on Arbitrum One

Official source: Uniswap developer deployment page for Arbitrum V3 deployments.

Verified entries used by Phoenix configuration:

- Chain id: `42161`.
- Factory: `0x1F98431c8aD98523631AE4a59f267346ea31F984`.
- SwapRouter02: `0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45`.
- UniversalRouter: `0xa51afafe0263b40edaef0df8781ea9aa03e381a3`.
- QuoterV2, parity tests only: `0x61fFE014bA17989E743c5F6cB21bF9697530B21e`.
- WETH: `0x82aF49447D8a07e3bd95BD0d56f35241523fBab1`.

The Uniswap page states the listed deployments are current and warns integrators to confirm per-chain mappings. Phoenix validates configured addresses with `eth_getCode` through `rpc-gateway` at startup when credentials exist.

## SushiSwap V3 on Arbitrum One

Official source inspected: Sushi docs and `llms-full.txt`, which identifies the public `sushi` / `sushi/evm` SDK entrypoints and references V3 factory/init-code constants.

Current blocker: the inspected docs did not expose Arbitrum-specific Sushi V3 contract values directly in a stable table. Phoenix therefore ships the Sushi registry as required configuration fields and refuses startup validation when they are unset. Do not fill these values from memory or third-party blogs.

Required next verification:

1. Install or inspect the official `sushi` package source.
2. Confirm Sushi V3 factory, router/RouteProcessor entrypoint, quoter/parity target if any, and init code hash for chain `42161`.
3. Record package version, source file, and addresses here.

## Aave V3 Flash Liquidity

Phoenix includes Aave V3 `flashLoanSimple` interfaces only. No Arbitrum provider address is hardcoded. The flash provider is a registry value validated through the cold RPC gateway before LIVE mode can be enabled.

## Message Encoding

`proto/phoenix.proto` is the canonical typed message schema. The Go ingestor currently publishes canonical JSON matching that schema because `protoc` and generated Protobuf toolchains are not available in this workspace. This is an implementation constraint, not a protocol guess. The schema is ready for generated Protobuf bindings in the deployment toolchain.
