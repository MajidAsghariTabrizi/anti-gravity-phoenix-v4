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

## Atlas/Aave Candidate-Level Current-State Authority

The read-only borrower archive is bound to Arbitrum One chain ID 42161, the
official Aave V3 Arbitrum Pool
0x794a61358D6845594F94dc1DB02A252b5b4814aD, and the Pool Borrow topic
0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0.
Address identities are pinned in
fixtures/atlas-borrowers/arbitrum-market-20260801.json to
aave-dao/aave-address-book commit
a1770e87fd61db02a7725cd9eed3b1d07c3980af. Liquidation math source is
aave-dao/aave-v3-origin commit
fd1fbd9150426ca8ace9cee45b4acf912ae84f5b.

At that pinned Origin revision, `LiquidationLogic` reads the variable-debt
token balance for the repay path. Phoenix therefore sizes WETH repayment from
variable debt only. A borrower with nonzero legacy stable WETH debt is marked
`unsupported_stable_weth_debt` and fails closed borrower-locally; stable debt
is never added to repay capacity, and it does not abort screening of unrelated
borrowers in the same batch.

The live WETH-debt liquidation universe is deliberately limited to reviewed
collateral paths. WETH collateral uses the executor's deterministic identity
route (no DEX legs). The current Production executor already approves WETH,
so the route needs no additional owner action there. The corrected same-asset
collateral accounting must nevertheless be deployed as a new, code-hash-bound
executor before that route can create a Production Candidate; a replacement
executor starts with an empty allowlist and must receive the normal reviewed
WETH approval during its paused bootstrap. The existing fork-simulation gate
keeps the route fail-closed against the older executor. Native USDC may still
be reconstructed and quoted as fail-closed evidence, but the deployed
executor's asset allowlist is authoritative. The reviewed WBTC/WETH 0.05%
Uniswap V3 pool is `0x2f5e87c9312fa29aed5c179e456625d79015299c`;
WBTC remains disabled until an
owner explicitly calls `setAsset(WBTC, true)` and `approvePool` for that pool,
the canonical Uniswap V3 factory, WBTC token0, WETH token1, fee 500, and enabled
status. Phoenix never sends those owner calls from the hunter or release path.

The current Aave fork endpoint calls the executor's direct
`executeAaveLiquidation` wrapper. It does not recreate Atlas' complete
`metacall`/`atlasSolverCall` context, including the execution environment and
Atlas shortfall reconciliation. That evidence may authorize the direct Aave
lane only. Auction-triggered Atlas artifacts remain fail-closed, and the Atlas
signing boundary requires a distinct callback-path evidence mode that the
current schema deliberately cannot persist. Enabling Atlas Aave submission
therefore requires a future full callback/metacall simulator, an explicit
schema migration, and the normal reviewed release process.

scripts/export_aave_borrow_discovery.py builds hash-bound, resumable discovery
chunks. The resulting archive and borrower addresses are discovery-only input.
They grant no candidate, signer, bond, bid, submission, capital, or execution
authority. Historical completeness and independent historical replay are not
candidate gates, and Phoenix does not claim that a primary-only archive is
independently complete.

In explicit `discovery-only-current-state` mode,
scripts/export_aave_checkpoint.py processes the seed in hash-bound resumable
batches. A primary provider performs only the cheap current-debt bitmap screen;
two independently configured providers must then agree at one exact finalized
block on the block hash, Pool implementation and code, reserve and isolation
configuration, oracle source code, prices and round timestamps, retained user
configuration, balances, debt, protocol Health Factor, and eMode state. The
incremental post-archive Borrow tail remains discovery-only: the primary slot
collects the bounded range and every discovered Borrow identity must be
reproduced by the second slot from an exact canonical block-hash log query. This
does not claim independent tail completeness and grants no candidate authority.
The SSH bridge binds distinct protected provider references without emitting
provider URLs. Each batch and cursor is root-only, immutable, and hash-bound.

For broad economic triage, scripts/atlas_aave_economic_prefilter.py performs
one batched, exact-finalized-block `getUserAccountData` call per discovery
address through the authenticated NOWNodes primary. It persists only derived
account values and classifies no-debt, debt-safe, watch, urgent, liquidatable,
and incomplete rows. Its atomic cursor, monitoring queue, and retained cohort
are discovery-only and grant neither candidate nor execution authority. Only
urgent or liquidatable survivors may be passed through the hash-bound
`--screen-cohort-file` input to the existing two-provider exact validator. That
narrow current-state input never relaxes historical archive validation and
does not claim post-archive tail completeness.

`scripts/atlas_aave_candidate_exact_validator.py` is the bounded next gate for
that retained cohort. It first refreshes at most two borrowers through
individual, one-attempt calls on both reviewed providers at one exact finalized
block. Stale rows stop before any reserve query. For an exact HF below one, the
validator resolves only reserve IDs set in the agreed user bitmap (maximum 20)
and records a sanitized `Pool.getConfiguration`/
`AaveProtocolDataProvider` compatibility matrix. The direct Pool bitmap is the
normal configuration source. The source-bound ProtocolDataProvider field set
may be selected only for the reviewed NOWNodes `rpc_error:3` case when Slot 0's
direct bitmap succeeds and both providers agree on the complete configuration,
pause, liquidation-fee, silo and debt-ceiling fields. The V3 Origin binding is
`fd1fbd9150426ca8ace9cee45b4acf912ae84f5b`; at that revision the legacy
borrowable-in-isolation, silo and debt-ceiling bitmap fields are removed, while
the compatibility getters return the reviewed disabled/zero values. Raw RPC
results and provider URLs are never persisted. Complete state can grant only
Candidate authority; execution authority is always false.

Oracle validation follows the pinned Aave V3 `AaveOracle.getAssetPrice`
semantics: both providers must agree on the configured source, source bytecode,
the positive source `latestAnswer()`, and the exact AaveOracle price, and those
two integers must match without decimal normalization. Full AggregatorV3 round
metadata is validated when both providers expose it. A reviewed source-level
revert of optional `latestRoundData()` selects the explicit
`aave_v3_latest_answer` policy instead; zero sources, nonpositive answers,
hidden fallback use, provider disagreement, or unexpected provider errors
remain fail-closed. The bounded capability matrix and all Candidate artifacts
retain public Oracle source addresses and provider-reference hashes, but never
Provider endpoints, credentials, request headers, or raw responses.

Current-state candidate reconstruction uses the fixed provider identities
`production-nownodes-arbitrum` and `production-slot-0`. NOWNodes is the
operational primary and is configured only with the public endpoint
`https://arbitrum.nownodes.io/`; its `api-key` credential is supplied as an
HTTP header from the protected runtime file and is never placed in a URL,
environment file, release package, manifest, log, or evidence artifact. The
credential file is root-owned and group-readable only by the verified
`65532:65532` rpc-gateway runtime. Provider Slot 0 remains the independently
operated proof-capable peer.

Both providers must agree on direct exact-block code, storage, and calls.
Provider Slot 0 must additionally return a valid EIP-1186 account/storage proof
whose account path is verified against the finalized block `stateRoot` and
whose storage path is verified against the proved account storage root. The
reviewed NOWNodes response to `eth_getProof` is an exact HTTP 405 capability
limitation, represented as
`secondary_proof_supported=false`,
`secondary_cryptographic_proof=false`, and
`direct_state_independent_agreement=true`; no other status or error class is
accepted. This limitation grants no execution authority and never substitutes
for direct independent agreement or the Slot 0 cryptographic proof.

Bounded borrower scans hash-bind per-provider request and transport counts
before advancing the cursor. The included NOWNodes request budget is 1,000,000
requests, with a 250,000 reserve, a 500,000 warning threshold, and a 700,000
broad-scan stop threshold. A batch that fails provider agreement, proof
validation, budget validation, or any write-precondition leaves the committed
cursor unchanged.

scripts/atlas_borrower_index.py independently derives Health Factor from the
agreed integer state and rejects disagreement with the Pool's exact
`getUserAccountData` result. It retains liquidatable and bounded near-threshold
buckets, then performs integer-only health-factor, close-factor, repay, seize,
protocol-fee, flash-premium, unwind, and full-cost scenario economics.
scripts/atlas_aave_provider_agreement.py binds every
provider scope to the exact inventory. scripts/atlas_aave_fork_package.py may
then emit only a READY_FOR_EXTERNAL_FORK evidence package; it creates no Fork
request and grants no signer, bond, bid, submission, or capital authority.

Candidate-level authority does not require
`PHOENIX_ATLAS_ARCHIVE_SECONDARY_RPC_URL`. It uses the two already configured,
distinct Production gateway provider slots for current finalized state only.
Provider URL values remain protected and must never be committed, printed,
included in evidence, or exposed through the SSH bridge.

## Atlas v1.6.4 Solver Interface

The active solver implementation is bound to the official Arbitrum Atlas
v1.6.4 deployment and does not infer signatures or bid semantics from auction
payloads. The reviewed sources are:

- Chainlink SVR Atlas searcher onboarding at commit
  `064e168227f40d98069163d0c9c11cab243cfacb`;
- FastLane Atlas tag `atlas-v1.6.4`, commit
  `083dccd05a2c92e0e9cae90ac404504f741bc493`;
- Atlas Go SDK commit `25e48369da286cf80f966a72411268a66527c101`;
- Atlas deployment configuration commit
  `8f3c6871485503cf4867c880f5a069d45ed59804`;
- Atlas operations relay commit
  `f99339726acba7c0272e1525583c3988bc2c9b03`.

The exact Arbitrum bindings are Atlas
`0x8ad1aE9D97C79aA68A0a151E83ff3942f68F86C1`, DappControl
`0xe15BBa987C002ecc3586e81244517877D294d291`, and AtlasVerification
`0xAC116AbB948E26B023c9C4815ab001845Fbf54fF`. Solver operations use the
`AtlasVerification` / `1.6.4` EIP-712 domain on chain 42161. The Searcher
Gateway subscription is `solver_subscribe(["userOperations"])` and submission
is `solver_submitSolverOperation`. Oracle gas, auction deadline, solver gas
limit, exact user-operation hash, solver contract, bid token, and maximum bid
are all request bindings. Solver registration or bonding never grants route
authority and cannot relax the retained-profit floor.

The legacy `historical-authority` mode remains fail-closed for an operator who
separately chooses to prove the full historical archive. That optional mode
still requires an independently validated archive, exact deployment boundary,
and a protected secondary archive provider; its evidence is not substituted
for candidate-level current-state reconstruction.

Optional historical capability preflight:

    python scripts/probe_aave_archive_provider.py \
      --provider-env PHOENIX_ATLAS_ARCHIVE_SECONDARY_RPC_URL \
      --provider-id owner-independent-archive \
      --from-block <verified-first-boundary> \
      --to-block <bounded-test-end>

Final manifest validation must also pass
`scripts/verify_aave_borrow_archive.py --require-deployment-boundary`, which
requires canonical headers and an exact Pool code transition from `0x` at the
prior block to deployed bytecode at the archive start block.

The primary and secondary operators must be independently identified. A
credentialless endpoint that merely agrees on returned inclusion but cannot
prove the complete requested range does not satisfy archive authority.

## Atlas Auction Shadow Evaluation

The Atlas/SVR shadow classification (revenue-capture program workstream B)
evaluates auction economics without ever producing a bid, bond, or submission.
Recorded assumptions and limitations:

- The SVR auction stream (`solver_subscribe(["userOperations"])`) carries the
  PartialUserOperation (user-operation hash, Atlas target, gas, max fee,
  deadline, dapp control, oracle hints) and the auction identity only. It
  contains no borrower data, so an auction is coupled to a borrower purely by
  asset identity: an auction is attached to a selected liquidation when its
  oracle asset matches the liquidation's debt asset (or collateral asset for
  the registry lookup).
- The rpc-gateway `atlas_mode` re-simulation exercises the DIRECT
  `executeAaveLiquidation` wrapper with Atlas parameters (bid amount and
  deadline). It is NOT the real `atlasSolverCall` callback, so
  `SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED` evidence is the closest
  available proxy for the solver settlement path, not a proof of the callback
  execution itself. Overriding the simulation price base with the auction's
  median-price hint requires gateway price-base request support and is a
  recorded follow-up, not an assumption of current behavior.
- Auction bounds are validated before any economics: deadline block > 0,
  `SolverGasLimit` within `(0, MaximumGasLimit]`, and `OracleGasPriceWei`
  within `(0, MaximumFeePerGasWei]` and `(0, MaximumPriorityFeeWei]`.
- Shadow bid economics use the same exact/fork simulation the direct lane
  would consume: solver exposure is
  `max(SolverGasLimit × OracleGasPriceWei, direct execution cost)`; the
  zero-bid conservative net applies `EconomicReserveBPS`; the maximum bid is
  `zeroBidConservative − retainedProfitFloor − 1` capped at
  `MaximumAtlasBidWei`; the selected shadow bid is half the maximum.
- Shadow outcomes are persisted ONLY to `live_canary.atlas_auction_shadow`.
  `atlas_solver_requests` and `execution_requests` are never materialized by
  the shadow path, and the direct lane's record semantics are unchanged
  (`TerminalOutcome` stays `candidate`; `AtlasCandidate` is never set).

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
