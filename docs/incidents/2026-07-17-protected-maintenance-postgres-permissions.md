# Protected-Maintenance PostgreSQL Permission Incident

Date: 2026-07-17

Status: Recovered; candidate v3 was not promoted.

## Timeline

- During the manually authorized protected-maintenance attempt, mutation had
  begun when the SSH transport reset.
- The SSH-attached process received an unexpected exit and initiated automatic
  rollback.
- Rollback called the immutable v2 `bootstrap-production.sh` as a release
  context restore operation.
- At 13:02:06 UTC PostgreSQL began reporting permission-denied errors for files
  including relation FSM files, `global/pg_filenode.map`, `postmaster.pid`, and
  `global/pg_control`.
- At 13:05:02 UTC PostgreSQL entered PANIC and the container restarted once.
- WAL crash recovery completed successfully. PostgreSQL reported ready at
  13:05:04 UTC.
- Runtime returned to exact v2. Feed, Recorder, NATS, PostgreSQL, and the Nitro
  relay were healthy after recovery.

## Root Cause

`restore_release_context` invoked:

```text
<rollback-release-tree>/scripts/bootstrap-production.sh <rollback-sha>
```

The general bootstrap script ran `install -d` with Phoenix ownership and mode
against `/opt/phoenix/data/postgres`. That path is the live bind source for
PostgreSQL PGDATA. General host provisioning was incorrectly reused for a
release-context restore, so rollback changed live persistent-data metadata.

The maintenance process was also attached to SSH. Transport loss therefore
became an unexpected process exit and incorrectly initiated rollback even
though no internal maintenance failure had occurred.

## Impact

- PostgreSQL lost access to live data files and entered PANIC.
- The PostgreSQL container restarted once and performed WAL crash recovery.
- The protected data plane was unavailable during the failure and recovery
  interval.
- Candidate v3 was not promoted.
- No volume was deleted, no database was truncated, and no migration ran.
- `execution_attempts`, `executions`, and `realized_pnl` remained zero.

## Recovery

PostgreSQL completed WAL crash recovery and became ready. The runtime returned
to exact v2, and Feed, Recorder, NATS, PostgreSQL, and Nitro relay health were
confirmed. No production evidence or runtime directory was deleted as part of
this repository correction.

## Missing Guardrail

- First-host provisioning and release-context installation were not separate
  operations.
- Bootstrap could mutate an existing non-empty PostgreSQL directory.
- Release and rollback paths were allowed to invoke general bootstrap.
- Protected continuity evidence covered container and mount identity but not
  stable PostgreSQL owner/group/mode evidence or NATS volume metadata.
- The maintenance process lifetime was coupled to the SSH transport.

## Permanent Corrective Actions

- Use `provision-production-host.sh` only for first-host directory creation.
  Existing persistent directories are never chowned or chmodded, and unsafe
  non-empty PostgreSQL ownership fails closed.
- Use `install-production-release-context.sh` for release install and rollback.
  It only updates canonical deploy scripts, configuration, and release-context
  files.
- Remove every release and rollback call to `bootstrap-production.sh`.
- Hash protected PostgreSQL metadata and NATS volume metadata into every
  maintenance snapshot. Missing or changed evidence blocks promotion.
- Run protected maintenance through a bounded transient systemd oneshot unit.
  SSH only starts, polls, and retrieves evidence from the unit.
- Preserve automatic rollback for actual internal failures while ensuring SSH
  disconnect or HUP alone cannot trigger rollback.
- Require regression tests and exact-tip CI before any future maintenance is
  considered. This incident correction does not authorize a rerun.
