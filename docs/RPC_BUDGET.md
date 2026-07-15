# RPC Evidence Budget

Phoenix discovery is driven by ordered Nitro Feed transactions. Public RPC is not used to search for opportunities. `rpc-gateway` is a bounded, fail-closed evidence verifier for routes already matched locally by the Engine.

## Evidence Flow

1. The Engine matches a configured two-pool V3 route from a Feed transaction.
2. The Gateway coalesces the route and canonical block identity.
3. The primary provider returns one Multicall3 batch.
4. The Engine runs the existing integer V3 model, optimizer, and SHADOW economics locally.
5. A route clearly below the existing threshold stops without a secondary provider call.
6. A potentially policy-passing route requests one secondary Multicall3 at the exact primary block number and hash.
7. Only explicit provider agreement is recorded as agreement. The secondary must be a different logical provider and prove the same block number, block hash, and route configuration hash before state hashes are compared.
8. Unavailability, malformed output, block or route drift, and disagreement remain distinct fail-closed evidence. See `SHADOW_SECONDARY_VERIFICATION.md`.

The canonical Arbitrum Multicall3 deployment is `0xcA11bde05977b3631167028862bE2a173976CA11`. Each provider is checked once per process for Arbitrum chain ID `42161` and non-empty code at that address. A route's `token0`, `token1`, and `fee` values are validated on its first provider use and retained in a long-lived cache keyed by provider, route fingerprint, and pool configuration hash. A configuration change creates a new cache identity.

## Block Identity

The shared head tracker uses `eth_getBlockByNumber("latest", false)` and coalesces concurrent refreshes. Dynamic `eth_call` requests always use an explicit hexadecimal block number, never `latest`.

[EIP-1898](https://eips.ethereum.org/EIPS/eip-1898) block-hash parameters are not enabled because support has not been proven against every configured production provider. Until that provider matrix is verified with live tests, every Multicall result is followed by a bounded `eth_getBlockByNumber(number, false)` check. A number/hash mismatch is never treated as equivalent state. A same-number canonical hash change invalidates matching route-block and verification cache entries.

## Exact Call Counts

For a configured two-pool route:

| Path | Upstream calls | Multicall inner calls |
| --- | ---: | ---: |
| Route-block cache hit | 0 | 0 |
| Warm primary screen, rejected | 2 | 4 |
| Warm primary plus secondary agreement | 4 | 8 |
| First process-cold primary | 5 | 10 |
| First process-cold primary plus cold secondary | 9 | 20 |

The warm primary count is one Multicall plus one canonical block-hash verification. The warm secondary adds the same two calls. A stale shared head adds one coalesced head lookup, amortized across concurrent routes. Process-cold counts include one chain-ID check and one Multicall3 code check per newly used provider; the primary count also includes the first shared head lookup. Cold batches include six static metadata reads and four dynamic reads. Later batches contain only four dynamic reads.

The previous implementation performed 13 upstream requests per provider for two pools, or 26 requests for mandatory two-provider agreement. It repeated chain ID, block, static metadata, dynamic state, and final block reads for every candidate.

One Multicall JSON-RPC request is not necessarily one provider billing unit. Providers may charge compute units according to execution cost, payload size, or inner calls. Production capacity planning must use provider invoices and observed CU consumption, not request count alone.

## Independent Limits

- `RPC_STATE_REQUESTS_PER_MINUTE=12` limits incoming state HTTP requests.
- `RPC_UPSTREAM_CALLS_PER_SECOND=1` sets the sustained real transport-call rate.
- `RPC_UPSTREAM_CALL_BURST=4` sets the transport burst capacity.
- `RPC_PROVIDER_PROBE_INTERVAL_SECONDS=60` controls background provider probes.

The transport token is acquired immediately before every real `JsonRpcClient::call`, including chain checks, code checks, head reads, Multicalls, block verification, retries, failover, and probes. Provider sequences are serialized and begin only when enough tokens are available for the complete setup or Multicall-plus-hash-check step, preventing a one-token retry loop from repeatedly issuing partial evidence calls. If no token is available, no transport call occurs, the current retry chain stops, and the caller receives a bounded retryable budget result. Request admission and transport admission are deliberately independent.

HTTP 429 is handled separately from transport failure. `Retry-After` delta seconds and HTTP dates are parsed, clamped to 60 seconds, and applied as provider cooldown. The same provider is not retried immediately. Failover still requires a transport token. Provider URLs are never metric labels or error fields.

## Metrics

Gateway metrics:

- `rpc_state_requests_total`
- `rpc_state_request_budget_rejected_total`
- `rpc_upstream_calls_total`
- `rpc_upstream_call_budget_rejected_total`
- `rpc_multicall_requests_total`
- `rpc_multicall_inner_calls_total`
- `rpc_static_metadata_cache_hits_total`
- `rpc_route_block_cache_hits_total`
- `rpc_coalesced_requests_total`
- `rpc_secondary_verifications_total`
- `rpc_provider_rate_limited_total`
- `rpc_provider_cooldown_total`
- `rpc_probe_calls_total`
- `rpc_provider_disagreement_total`
- `rpc_gateway_readiness`

Engine screening metrics:

- `rpc_primary_screen_rejected_total`
- `rpc_secondary_skipped_total`

The only labels on transport calls are bounded `method`, `outcome`, and `provider_slot`. URLs, transactions, routes, pools, tokens, and opportunity identities are forbidden as metric labels.

## Future Work

An event-driven local pool-state mirror may later remove most verification reads. It remains optional future work and is not implemented by this change. Any mirror must preserve explicit canonical block identity, deterministic replay, and the same fail-closed SHADOW policy.
