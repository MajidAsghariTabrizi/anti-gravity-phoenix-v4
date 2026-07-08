# Runbook

## Start Shadow Stack

```bash
cp .env.example .env
docker compose up --build
```

## Common Failures

### Feed relay cannot connect

- Confirm outbound access from the VPS.
- Confirm the pinned Nitro image can start.
- Confirm the relay port is not exposed publicly.
- Keep engine in SHADOW mode.

### Sequence gaps increase

- Stop LIVE attempts.
- Keep recording feed events.
- Inspect `feed_sequence_gaps_total`.
- Reconcile local state through `rpc-gateway`.

### RPC budget exhausted

- Dashboard remains usable from PostgreSQL and metrics.
- Bootstrap/reconciliation slows down.
- Hot path should continue from local state where complete.

### Database unavailable

- Engine must not block the synchronous decision path.
- Recorder retries with bounded queue behavior.
- Dashboard shows degraded state.

### Executor paused

- LIVE submission gate must fail closed.
- Investigate owner action and latest security event.

## Recovery Order

1. Preserve logs, metrics, recorder output, and database.
2. Keep `LIVE_EXECUTION=false`.
3. Restore NATS/PostgreSQL/metrics.
4. Restore feed relay.
5. Reconcile state through `rpc-gateway`.
6. Resume SHADOW.
7. Reconsider LIVE only after the release gate passes.

