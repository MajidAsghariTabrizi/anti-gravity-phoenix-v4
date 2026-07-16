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

## PostgreSQL Migration Runner

- Runner language: Go.
- PostgreSQL driver: `github.com/lib/pq v1.10.9`.
- Migration source: ordered SQL files in `migrations/`.
- Production execution: `/usr/local/bin/migration-runner` bundled into the feed-ingestor image and invoked by `scripts/deploy-release.sh`.

The runner records `schema_migrations`, migration version, SHA-256 checksum, and `applied_at`; it uses a PostgreSQL advisory lock and fails on checksum drift.

## GitHub Actions

Workflow actions are pinned to full commit SHAs in `.github/workflows/`. The dependency ledger and update process are in `docs/GITHUB_ACTIONS_DEPENDENCIES.md`.
