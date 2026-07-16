# Runbook

Engine dependency retry exhaustion is handled by the bounded SHADOW quarantine contract in
[`ENGINE_DEPENDENCY_EXHAUSTION.md`](ENGINE_DEPENDENCY_EXHAUSTION.md). Treat
`dependency_exhausted` as an unresolved dependency outcome, never as recovery or profitability.

Production default:

- `PHOENIX_MODE=SHADOW`
- `LIVE_EXECUTION=false`
- no GitHub-held signer key
- release by immutable manifest

Production operations use `/opt/phoenix/deploy/deploy-release.sh <sha>`, `/opt/phoenix/deploy/rollback-release.sh`, and `/opt/phoenix/deploy/production-healthcheck.sh`. Bounded read-only profitability reporting is documented in [`SHADOW_PROFITABILITY_REPORTS.md`](SHADOW_PROFITABILITY_REPORTS.md). Bounded official-router route discovery is documented in [`SHADOW_ROUTE_DISCOVERY.md`](SHADOW_ROUTE_DISCOVERY.md). The combined technical and business evidence contract is documented in [`PRELIVE_MONEY_PATH_OBSERVABILITY.md`](PRELIVE_MONEY_PATH_OBSERVABILITY.md).

## Money-Path Evidence

Generate a bounded 24-hour report without changing service state:

```sh
/opt/phoenix/deploy/prelive-money-path-report.sh --format text --window-hours 24
```

The command fails closed when a production scrape, PostgreSQL, Prometheus, the digest-pinned release
context, or any SHADOW safety invariant is unavailable. Treat a failed report as missing evidence,
not as a zero observation. Preserve the JSON form with the release SHA when collecting soak evidence.

## Bounded Engine Canary

Set `SHADOW_ENGINE_CANARY_INPUT_LIMIT` only for a manually reviewed SHADOW smoke run:

```sh
SHADOW_ENGINE_CANARY_INPUT_LIMIT=500 ./scripts/shadow-engine-live-smoke.sh
```

The default value is `0`, which leaves the existing full smoke workflow unchanged. A positive limit selects isolated canary mode: the durable Nitro relay, NATS, PostgreSQL, Feed Ingestor, and Recorder must already be healthy, and their readiness, JetStream stream/consumer, and PostgreSQL connectivity are checked before any optional runtime starts. Migrations must be run separately before the canary. The canary never starts, recreates, stops, or modifies those core services, the Shadow Dispatcher, Prometheus, or Dashboard; monitoring is not required. It starts only RPC Gateway and Phoenix Engine with explicit `--no-deps` service names.

The isolated path records protected container identity, image, creation/start timestamps, and restart count before startup and verifies them again after cleanup. A missing or unhealthy dependency fails closed without starting RPC Gateway or Engine, and a protected identity change fails the canary. Failure does not alter the durable Feed path.

The canary watches `phoenix_engine_inputs_processed_total` immediately before Engine startup and stops the Engine automatically when the requested persisted-input threshold is observed. Post-stop accounting uses the larger of that metric and new persisted classifications; the run fails if the accounted total exceeds the requested limit by more than the fixed 64-message Engine pull batch.

After stopping, the script waits up to `SHADOW_ENGINE_CANARY_SETTLE_TIMEOUT_SECONDS` (default 180 seconds) for ACK-pending to remain at zero. It verifies that `PHOENIX_ENGINE_INPUT` and durable consumer `PHOENIX_ENGINE_SHADOW` still exist, leaves the Engine stopped, and leaves any pending messages replayable. The canary path requires the same `PHOENIX_MODE=SHADOW`, `LIVE_EXECUTION=false`, and blank signer, executor, and wallet settings as the full smoke.

## Incidents

### Nitro feed relay disconnected

- Keep `LIVE_EXECUTION=false`.
- Check `nitro-feed-relay` health and outbound connectivity.
- Preserve relay logs and release SHA.
- Do not switch feed-ingestor to fixtures in production.

### Feed sequence gap

- Stop LIVE attempts.
- Inspect `feed_sequence_gaps_total`.
- Preserve feed logs and recorder output.
- Reconcile local state through `rpc-gateway`.

### High feed decode errors

- Inspect `feed_decode_failures_total`, `feed_unsupported_messages_total`, and malformed payload categories.
- Keep SHADOW only.
- Compare against pinned Nitro release semantics before changing parser logic.

### NATS unavailable

- Check NATS health endpoint and Docker network.
- Confirm `/healthz?js-enabled-only=true`, stream `PHOENIX_FEED_TX`, durable consumer `PHOENIX_RECORDER`, and Docker volume `phoenix-nats-jetstream` still exist.
- Expect feed-ingestor and Recorder readiness to fail. Do not delete or recreate the volume as a recovery shortcut.
- Inspect `recorder_consumer_pending_messages`, `recorder_consumer_ack_pending`, publish acknowledgement failures, and disk capacity. Restore service before the 24-hour stream age limit.
- Restore NATS before restarting dependent services.

### RPC gateway degraded

- Inspect provider budget and circuit breaker metrics.
- Reduce cold-path pressure.
- Hot path must not add direct public RPC reads.

### Independent verification failures

- Keep SHADOW and inspect bounded `independent_verification_status` evidence.
- Treat `provider_unavailable` as an availability or budget incident and
  `integrity_failure` as a response-contract incident; do not relabel either as
  agreement.
- For `disagreed`, compare logical provider inventory and the persisted primary
  and secondary block, block-hash, route-hash, and state-hash evidence.
- A same-provider result or block/route mismatch is invalid evidence. Do not
  bypass the gateway or relax the Engine/database checks.

### All RPC providers circuit-open

- Keep SHADOW.
- Reconcile provider credentials and quotas.
- Do not bypass `rpc-gateway` from engine or dashboard.

### PostgreSQL unavailable

- Check `pg_isready`, disk, and volume permissions.
- Recorder readiness fails. The bounded in-flight batch remains unacknowledged and sends work-in-progress signals while PostgreSQL retries; a Recorder restart replays it from JetStream.
- Hot decision path must not block on database recovery.

### Migration failure

- Deployment fails closed.
- Preserve migration-runner logs.
- Do not mark the release current.
- Fix migration forward; there is no automatic down migration.

### Phoenix engine unhealthy

- Check `/readyz`.
- In production, not-ready means engine-owned runtime initialization failed or is still initializing. The response body is sanitized and should be one of the engine readiness detail constants.
- Do not override health gates to force a deploy.

### State incomplete spike

- Inspect pool completeness metrics and `STATE_INCOMPLETE` miss reasons.
- Reconcile pool state through `rpc-gateway`.
- Reject candidates rather than extrapolating unknown ticks.

### Supported origins drop unexpectedly

- Inspect protocol registry changes and router configuration.
- Confirm no ABI/address assumptions changed without documentation.

### Zero profitable opportunities

- Compare origin volume, state completeness, optimizer candidates, gas model, and uncertainty reserve.
- Do not treat zero opportunities as a failure by itself.

### Deployment failure

- `deploy-release.sh` invokes rollback automatically.
- Preserve failed release manifest, logs, and health output.
- Do not write `current-release` manually for a failed release.

### Automatic rollback

- Verify `ROLLBACK_OK`.
- Confirm `current-release` points to the restored SHA.
- Open an incident note with failed and restored release SHAs.

### Rollback failure

- Treat as critical.
- Preserve diagnostics.
- Keep services in SHADOW.
- Manually inspect `previous-release`, manifests, GHCR access, and Compose health.

### GHCR pull failure

- Check production host GHCR login and package permissions.
- Verify manifest digest references exist.
- Do not fall back to `latest`.

### Production disk pressure

- Check `/opt/phoenix/data`, Docker image cache, logs, PostgreSQL, and `docker volume inspect phoenix-nats-jetstream`.
- If JetStream reaches its bound, `DiscardNew` rejects publication and feed readiness must fail. Do not purge unacknowledged data to force readiness.
- Preserve critical database and release artifacts.
- Prune only reviewed caches/images.

### Prometheus unavailable

- Dashboard metrics may degrade.
- Health gate fails until Prometheus readiness recovers.
- Inspect `/opt/phoenix/data/prometheus`.
- The bounded money-path report must fail; do not replace missing samples with zeros.

### Dashboard unavailable

- Check Streamlit health on loopback.
- Dashboard failure must not cause hot-path RPC reads or engine restarts.

## Recovery Order

1. Preserve logs, metrics, recorder output, release manifests, and database.
2. Keep `LIVE_EXECUTION=false`.
3. Restore Docker, NATS, PostgreSQL, and internal networking.
4. Restore Nitro relay.
5. Restore feed-ingestor readiness.
6. Restore rpc-gateway and state reconciliation.
7. Restore engine/recorder readiness.
8. Resume SHADOW.
9. Reconsider LIVE only after every release gate passes.
