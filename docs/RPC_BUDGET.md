# RPC Budget

Phoenix treats public RPC as cold infrastructure. Search latency does not depend on free RPC limits because the hot loop consumes ordered feed events, local state, pool graph data, local V3 math, and cached gas/flash-premium profiles.

All read RPC access is centralized in `rpc-gateway`.

## Priorities

- `P0`: post-execution receipt and reconciliation
- `P1`: pool state reconciliation
- `P2`: startup state bootstrap
- `P3`: metadata and registry refresh
- `P4`: dashboard or offline analytics

## Controls

- weighted provider pool
- health scoring
- circuit breaker
- exponential backoff with jitter
- per-provider token bucket
- global budget
- priority admission
- request deduplication
- single-flight coalescing
- short and long TTL caches
- bounded retries
- request deadlines

Dashboard traffic never talks to public RPC and is admitted only as internal database/metrics traffic.

