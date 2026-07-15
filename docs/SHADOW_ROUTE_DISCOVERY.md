# SHADOW Route Discovery

## Purpose

`shadow-route-discovery.sh` produces a bounded, deterministic ranking of two-pool Arbitrum One routes from persisted production evidence. It is reporting only. It does not update `ENGINE_ROUTE_REGISTRY_JSON`, start or stop services, create an execution request, sign, submit, or claim realized profit.

The initial discovery boundary is deliberately narrow:

- chain `42161`
- the three reviewed official Uniswap router families already supported by the production origin decoder
- exact-input swaps only
- two distinct Uniswap V3-compatible pools over one canonical token pair
- two-token atomic cycles only
- pool addresses proven from the official factory, init-code hash, token pair, and fee through Ethereum CREATE2

Unknown aggregators, exact-output commands, unverified pools, triangular routes, and LIVE behavior remain unsupported.

## Production Command

Run from the bootstrapped deployment assets while PostgreSQL and Phoenix Engine are already running:

```sh
sudo /opt/phoenix/deploy/shadow-route-discovery.sh \
  --format text \
  --limit 10000 \
  --evidence-limit 10000 \
  --top 10
```

Use `--format json` for a machine-readable review artifact. The workflow validates `/etc/phoenix/phoenix.env`, the active digest-pinned release environment, and the exact rendered production Compose context before reading data. Temporary decoder and enrichment files are private and removed on exit. No fixture path is available to the production workflow.

The decoder scan runs inside the deployed Engine image, reads `feed_events` in a read-only transaction, and applies the production Recorder input contract, Engine input contract, origin detector, and official-router decoders. The SQL enrichment runs in a repeatable-read, read-only transaction. If either eligible source exceeds its configured bound, the report fails instead of truncating silently.

## Ranking Contract

Every candidate receives 20 named, equally weighted components on a 10,000-basis-point scale:

1. transaction count
2. swap count
3. unique blocks
4. router distribution
5. fee-tier diversity
6. directional flow
7. volume proxy
8. liquidity proxy
9. pool-impact frequency
10. candidate frequency
11. RPC-evaluation availability
12. expected net PnL
13. near-profitable frequency
14. RPC cost proxy
15. provider failure rate
16. state freshness
17. competition proxy
18. decoder confidence
19. data completeness
20. feed-gap overlap

Integer arithmetic is used throughout. Missing evidence scores zero and is labeled `unavailable`; it is never imputed. Financial comparisons are partitioned by settlement asset, volume by token pair and input asset, and liquidity by token pair. A multi-hop transaction input is not credited as per-hop volume because downstream hop amounts are not persisted by the decoder.

Settlement asset and pool-leg order are part of route identity: WETH-settled and USDC-settled cycles are never mixed, `500 -> 3000` and `3000 -> 500` are ranked separately, and profitability evidence can score only the exact ordered cycle it evaluated. The deterministic tie break is total score descending, transaction count descending, then route ID ascending. Output includes the top ten routes, component details, quality warnings, unsupported or unsafe reasons, canonical token and pool paths, verified pool addresses, and suggested registry JSON when a settlement-specific reviewed economics template exists. The committed template covers WETH settlement only; other settlement assets remain ineligible until independently reviewed unit-correct economics are added.

## Activation Gate

At most three top-ranked suggestions can be marked `shadow_activation_eligible`, and only when all fail-closed checks pass:

- at least 20 distinct route transactions
- every route transaction comes from trusted PostgreSQL feed-event provenance
- at least five distinct persisted source blocks
- both pool addresses pass the pinned official CREATE2 proof
- a reviewed economics template exists
- both latest pool liquidity checkpoints are positive and no older than the latest ranked profitability fact
- complete canonical profitability facts and primary RPC evidence exist
- the worst observed severe expected PnL remains positive
- no provider disagreement is observed
- provider failures are at most 10 percent
- p90 detection-to-evaluation delay stays within the reviewed quote-age policy
- data completeness is at least 70 percent

Eligibility is still only a recommendation for human SHADOW review. The report always emits `mode=SHADOW`, `live_execution=false`, `execution_eligible=false`, `execution_request_created=false`, `production_registry_mutated=false`, `financial_basis=SHADOW expected`, and `realization_status=not realized`.

## Pool Proofs

The committed proof registry is `fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json`. It pins the official `Uniswap/v3-periphery` source commit, factory, pool init-code hash, reviewed WETH/USDC pool addresses, and the existing conservative SHADOW economics template. The analyzer independently recomputes every pool address with Ethereum Keccak-256 and CREATE2 before it can render a suggestion.

Adding a pool requires a separately reviewed proof update and tests. Discovery history alone cannot establish a pool address.

## Evidence Limits

Persisted source block identity is reported only when present in `origin_transactions.metadata`; it is not reconstructed. Feed-gap overlap is currently not persisted and therefore appears as `unavailable_not_persisted`. Liquidity is a latest-checkpoint proxy, RPC cost is a quality-record count rather than provider billing, competition is same-block candidate overlap, and expected PnL is modeled SHADOW evidence rather than realized capital PnL.

Synthetic fixtures under `fixtures/reports` validate ranking and failure behavior only. They are marked untrusted and produce zero selected routes. They are not production route evidence.
