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

The script validates exact route JSON rendering, requires `PHOENIX_MODE=SHADOW` and `LIVE_EXECUTION=false`, snapshots all protected services, and verifies Feed, Recorder, NATS, PostgreSQL, and relay health. It stops only `rpc-gateway` and `phoenix-engine`, captures `RUN_STARTED_AT_UTC` from the PostgreSQL server clock while both are down, and then starts only those services with `--no-deps`. It performs no pull or build.

Every success query requires `classified_at` or `completed_at` to be at or after that run baseline. Candidate discovery also requires a positive candidate count, a configured route fingerprint paired with its evaluation, a current final processing attempt, and the exact source-event identity and transaction hash. A source sequence alone is insufficient because one Nitro sequence can contain multiple transactions. Historical rows cannot be used as a fallback; a run with no matching current evidence returns `POSITIVE_ROUTE_EVIDENCE_NOT_FOUND`.

The first matching current-run candidate is replayed through the production decoder in the running Engine container. Its persisted report selects the newest attempt for the same source-event identity and final classification with `ORDER BY completed_at DESC, id DESC`. The report names the classification table's primary key `source_event_identity`. It does not emit `classification_id` or `classification_record_id`, because `shadow_engine_classifications` has no separate numeric record ID. The attempt row is reported separately as `processing_attempt_id`, `delivery_attempt`, and `processing_attempt_completed_at`.

Before either optional service starts, the workflow reads effective `RPC_STATE_REQUESTS_PER_MINUTE` from the rendered Compose JSON. Values below `12` fail with a dedicated marker. The workflow only validates this budget; it never edits the supplied environment files or changes the controlled upstream call rate of `1` call per second and burst of `4`.

ACK-pending and replayable pending counts are reported before and after without requiring zero and without changing the stream or durable consumer.

## Persisted Report Semantics

`PERSISTED_RUNTIME_EVIDENCE` is one validated JSON object. It contains the source identity, sequence and transaction hash; final classification and rejection reason; candidate and matched-route data; newest current-run attempt tuple; persisted and block-pinned state evidence; provider and verification status; and the literal safety fields `shadow_only: true` and `execution_request_created: false`. Optional RPC response, independent-provider, agreement, and skip fields are emitted only when their persisted semantics apply.

`primary_only` is valid when the primary provider supplied block-pinned state but no independent request was needed. In the `no_profitable_candidate` path, persisted `primary_screen_rejected: true` and `secondary_skipped: true` produce `independent_verification_status: not_requested` and `independent_verification_skip_reason: primary_screen_no_profitable_candidate`. No provider agreement is claimed. Genuine `agreed`, `disagreed`, and `secondary_unavailable` outcomes remain distinct; integrity failures are rejected rather than relabeled as `primary_only`.

## Illustrative Verified Outcome

A successful SHADOW run on the VPS observed Universal Router transaction `0x276b6775bc2b9d27f0d615d5000fce9232e8015f93cddd5d7e70ce0d3dffbbaa` in source sequence `461219428`, decoded `V3_SWAP_EXACT_IN`, and matched `arb1-weth-usdc-uni500-uni3000-canary-v2`. The current classification was `candidate_rejected` with one candidate and `no_profitable_candidate` at block `483792695` (`0xfdb4b9a0a59ecf4c675b725390d41cb2820fe59a89caa0b7359b47eb644dda45`). Primary provider `publicnode` returned state hash `1397b50a50d7b6128075572a6c730d731e0a5512c2463999cb509b7c989aa013`; verification was `primary_only`, so independent verification was not requested. No execution request was created.

During that bounded run, JetStream ACK-pending changed from `106` to `3` and replayable pending changed from `45382` to `44148`; Feed and Recorder stayed ready and shutdown was graceful. This is an illustrative successful outcome, not a fixture, a deployment instruction, or a claim that later runs have the same result.

## Terminal Results

- `POSITIVE_ROUTE_EVIDENCE_FOUND`: a real stored official-router transaction produced a configured-route candidate and a validated current-run persisted evidence object.
- `POSITIVE_ROUTE_EVIDENCE_NOT_FOUND`: the time box or complete bounded offline scan ended without that evidence.

A found result may still report `primary_only`, `secondary_unavailable`, or a rejected SHADOW decision. Those outcomes prove the minimum decoder/route candidate path but do not claim complete independent verification, profitability, production readiness, or execution eligibility. The workflow remains SHADOW-only throughout.
