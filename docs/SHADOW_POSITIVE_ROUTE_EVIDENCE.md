# SHADOW Positive-Route Evidence

This workflow proves the reviewed positive path without enabling transaction execution or changing durable JetStream state. A profitable decision is not required. The minimum positive result is a real PostgreSQL `feed_events` transaction that production decoding maps to the configured two-pool route and that reaches persisted primary-state evidence.

## Current Evidence Outcome

No real positive fixture is committed on this branch. The repository feed fixtures use placeholder transaction hashes and synthetic calldata, have no real source-block provenance, and are retained only as deterministic tests. The workstation PostgreSQL listener was not accessible with either the configured deployment credentials or the repository's explicit CI test credentials, so VPS rows could not be exported during implementation.

This is `POSITIVE_ROUTE_EVIDENCE_NOT_FOUND`, not a decoder failure. No router or exact-output guardrail was loosened to manufacture a result.

## Read-Only Discovery

The `shadow-positive-route-evidence` binary is shipped beside the Phoenix Engine service binary. It reads `feed_events` in a read-only PostgreSQL transaction, restricts database discovery to the three reviewed official routers, validates each payload through the Recorder and Engine input contracts, and then uses the production `OriginDetector` and `RouteRegistry`.

Run a bounded future-traffic scan inside the already-running SHADOW Engine container:

```sh
docker compose --env-file /etc/phoenix/phoenix.env \
  --env-file deploy/current-release.env \
  -f compose.prod.yml exec -T phoenix-engine \
  /usr/local/bin/shadow-positive-route-evidence scan-postgres \
  --dsn-env POSTGRES_DSN \
  --route-registry-env ENGINE_ROUTE_REGISTRY_JSON \
  --limit 100000
```

The output contains bounded summaries and aggregate router, selector, and result counts. It never prints the PostgreSQL DSN, RPC URLs, raw transactions, or complete calldata. If the eligible row count exceeds the requested limit, the command fails instead of returning an incomplete no-evidence conclusion.

## PostgreSQL Export And Offline Replay

An explicit export preserves real normalized payloads and provenance for offline replay. The output file is created with mode `0600` on Unix and must not already exist:

```sh
docker compose --env-file /etc/phoenix/phoenix.env \
  --env-file deploy/current-release.env \
  -f compose.prod.yml exec -T phoenix-engine \
  /usr/local/bin/shadow-positive-route-evidence scan-postgres \
  --dsn-env POSTGRES_DSN \
  --route-registry-env ENGINE_ROUTE_REGISTRY_JSON \
  --limit 100000 \
  --export-jsonl /tmp/phoenix-positive-route-evidence.jsonl
```

Each JSONL row records `postgresql.feed_events`, the feed-event ID, persisted timestamp, optional source-block metadata, and the exact normalized payload. Copy the file to a restricted operator-controlled location, then replay it from a source checkout:

```sh
cargo run --locked --manifest-path phoenix-engine/Cargo.toml \
  --bin shadow-positive-route-evidence -- replay-jsonl \
  --input /restricted/phoenix-positive-route-evidence.jsonl \
  --route-registry-file fixtures/routes/weth_usdc_uniswap_v3.json \
  --limit 100000
```

Only a matched candidate with PostgreSQL feed-event provenance can increment `production_candidate_count` or return `POSITIVE_ROUTE_EVIDENCE_FOUND`. Synthetic tests can exercise the same decoder and route path but remain non-production evidence.

## Time-Boxed SHADOW Run

The host workflow defaults to 900 seconds and accepts an explicit timeout:

```sh
PHOENIX_ENV_FILE=/etc/phoenix/phoenix.env \
PHOENIX_RELEASE_ENV=deploy/current-release.env \
sh scripts/shadow-positive-route-evidence.sh --timeout-seconds 900
```

The script validates exact route JSON rendering, requires `PHOENIX_MODE=SHADOW` and `LIVE_EXECUTION=false`, snapshots all protected services, and verifies Feed, Recorder, NATS, PostgreSQL, and relay health. It starts only `rpc-gateway` and `phoenix-engine` with `--no-deps`, performs no pull or build, and stops only those two services.

The first persisted candidate with primary-state evidence is replayed through the production decoder in the running Engine container. The report includes the route candidate, block-pinned RPC evidence, independent verification status when attempted, classification identity, processing-attempt identity, source sequence, and persisted timestamp. ACK-pending and replayable pending counts are reported before and after without requiring zero and without changing the stream or consumer.

## Terminal Results

- `POSITIVE_ROUTE_EVIDENCE_FOUND`: a real stored official-router transaction produced a configured-route candidate and persisted primary-state evidence.
- `POSITIVE_ROUTE_EVIDENCE_NOT_FOUND`: the time box or complete bounded offline scan ended without that evidence.

A found result may still report `primary_only`, `secondary_unavailable`, or a rejected SHADOW decision. Those outcomes prove the minimum decoder/route candidate path but do not claim complete independent verification or profitability.
