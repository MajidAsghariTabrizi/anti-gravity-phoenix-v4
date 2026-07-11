# Runbook

Production default:

- `PHOENIX_MODE=SHADOW`
- `LIVE_EXECUTION=false`
- no GitHub-held signer key
- release by immutable manifest

Production operations use `/opt/phoenix/deploy/deploy-release.sh <sha>`, `/opt/phoenix/deploy/rollback-release.sh`, and `/opt/phoenix/deploy/production-healthcheck.sh`.

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
- Expect feed-ingestor and recorder readiness to fail. Production Compose also prevents the engine from starting before NATS is healthy, but the engine `/readyz` endpoint only reports engine-owned runtime initialization.
- Core NATS has no acknowledgement or replay log. Preserve outage timestamps and reconcile database coverage before treating Recorder history as complete.
- Restore NATS before restarting dependent services.

### RPC gateway degraded

- Inspect provider budget and circuit breaker metrics.
- Reduce cold-path pressure.
- Hot path must not add direct public RPC reads.

### All RPC providers circuit-open

- Keep SHADOW.
- Reconcile provider credentials and quotas.
- Do not bypass `rpc-gateway` from engine or dashboard.

### PostgreSQL unavailable

- Check `pg_isready`, disk, and volume permissions.
- Recorder readiness fails and the current in-memory message is retried with backpressure. A Recorder crash during the outage loses that Core NATS delivery.
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

- Check `/opt/phoenix/data`, Docker image cache, logs, and PostgreSQL volume.
- Preserve critical database and release artifacts.
- Prune only reviewed caches/images.

### Prometheus unavailable

- Dashboard metrics may degrade.
- Health gate fails until Prometheus readiness recovers.
- Inspect `/opt/phoenix/data/prometheus`.

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
