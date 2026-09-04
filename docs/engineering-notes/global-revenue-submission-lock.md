# The Global Revenue Submission Lock: When Multiple Strategies Share a Nonce

> **Status:** ARCHITECTURE + IMPLEMENTATION. Includes the SQL schema for the lock and the application-level invariant. **Profitability: NO VERIFIED ALPHA YET.**

> **Audience:** Engineers operating financial-infrastructure backends with multiple revenue strategies or lanes competing for a shared signing key.

## The shared-nonce problem

A wallet has one nonce. Two strategies that share the wallet have one nonce between them. If both strategies decide to submit a transaction in the same block, the second transaction will be invalid by construction (duplicate nonce), or it will overwrite the first in the mempool (same nonce, higher fee). Either outcome is wrong for at least one of the strategies.

The simplest answer is "don't run two strategies." This works for a one-strategy system. It does not work for a system that wants to evaluate multiple revenue lanes (Aave liquidations, Atlas auctions, DEX backruns) with the same capital base. The natural reflex is to add a mutex. The interesting question is *where* the mutex lives.

Phoenix implements the answer as a SQL-level single-row table with a check constraint and a partial unique index. The lock is enforced by the database, not by application code. Application bugs cannot create two simultaneous submissions.

## The schema

From `live-executor/schema/006_atlas_aave_revenue_lanes.sql` line 121-160:

```sql
CREATE TABLE IF NOT EXISTS live_canary.global_revenue_submission_lock (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    active_lane TEXT CHECK (active_lane IS NULL OR active_lane IN ('phoenix_dex', 'atlas_solver', 'aave_liquidation')),
    active_identity TEXT CHECK (active_identity IS NULL OR length(active_identity) BETWEEN 1 AND 256),
    acquired_at TIMESTAMPTZ,
    control_epoch BIGINT NOT NULL DEFAULT 0 CHECK (control_epoch >= 0),
    CHECK ((active_lane IS NULL) = (active_identity IS NULL)),
    CHECK ((active_lane IS NULL) = (acquired_at IS NULL))
);

INSERT INTO live_canary.global_revenue_submission_lock(singleton)
VALUES (true)
ON CONFLICT (singleton) DO NOTHING;

CREATE UNIQUE INDEX IF NOT EXISTS live_canary_one_global_revenue_submission
ON live_canary.atlas_solver_requests ((true))
WHERE status IN ('claimed', 'signed', 'submitted', 'submission_unknown');
```

The schema enforces four things at the database layer:

1. **The lock is a singleton.** The `singleton BOOLEAN PRIMARY KEY` constraint means at most one row can exist. `INSERT ... ON CONFLICT DO NOTHING` seeds it once. There is no `INSERT` path to create a second lock.

2. **The active lane is from a known set.** The check constraint `active_lane IN ('phoenix_dex', 'atlas_solver', 'aave_liquidation')` means a typo in a lane identifier causes the insert to fail rather than producing an ungoverned lane.

3. **The lock state is self-consistent.** The two check constraints `(active_lane IS NULL) = (active_identity IS NULL)` and `(active_lane IS NULL) = (acquired_at IS NULL)` mean the lock is either fully empty (all three fields NULL) or fully populated (all three fields set). It cannot be in a half-populated state.

4. **Only one atlas request can be in-flight at a time.** The partial unique index `live_canary_one_global_revenue_submission` on `atlas_solver_requests` enforces that at most one row can be in the status set `('claimed', 'signed', 'submitted', 'submission_unknown')`. The `(true)` index key is a constant; the index is partial on the status filter. PostgreSQL allows partial unique indexes, which is what makes this a single-row constraint without naming a specific column.

The fourth point is the most interesting. It says: "regardless of what the application code does, the database will not allow two atlas solver requests to be simultaneously in active states." A bug that races the lock acquisition at the application layer cannot produce two concurrent submissions, because the database will reject the second INSERT.

## How the lock is acquired and released

The acquisition path is in `live-executor/src/store.rs` and `live-executor/src/revenue.rs`. The `release_revenue_submission_lock` function in `store.rs` line 1962 updates the lock back to NULL when the active attempt reaches a terminal state:

```rust
async fn release_revenue_submission_lock(
    transaction: &mut sqlx::PgConnection,
    request_id: Uuid,
) -> Result<(), StoreError> {
    "UPDATE live_canary.global_revenue_submission_lock
        SET active_lane = NULL, active_identity = NULL, acquired_at = NULL
        WHERE active_identity = $1 OR ..."
}
```

The release happens only when the attempt's terminal state is reached. The terminal states that release the lock are the ones where the wallet nonce is known to be advanced or known to be safe to advance:

- `Confirmed` — the transaction is mined; the nonce is spent.
- `Reverted` — the transaction was mined but reverted; the nonce is still spent.
- `Failed` — pre-submission failure; no nonce was burned.
- `Replaced` — the transaction was replaced (by Phoenix or by someone else); the lock is closed because the integrity assumption has been broken.
- `TimedOut` — the transaction is past its receipt timeout; the lock is closed and the operator must investigate.

The terminal state that does **not** release the lock automatically is `SubmissionUnknown`. From `engine.rs` line 312-319:

```rust
AttemptStatus::SubmissionUnknown => {
    let nonce = active.nonce.ok_or(EngineError::ActiveAttemptInvariant)?;
    self.store.disarm("submission_unknown").await?;
    return Ok(ExecutionState::SubmissionUnknown {
        request_id: active.request.id,
        nonce,
    });
}
```

The system disarms but does not clear the lock. The lock remains held by the lane that owns the unknown submission. A human operator must investigate, determine what actually happened to the nonce, and clear it with a privileged operation that has its own audit trail.

## The pattern is database-enforced, not application-enforced

This is the most important architectural property of the lock. Application-level mutexes are necessary but not sufficient. A bug in the application can:

- Forget to acquire the lock.
- Forget to release the lock.
- Release the lock before the submission is reconciled.
- Race two acquisitions of the lock.

A database-enforced lock survives all four bugs. The `singleton` primary key prevents creation. The check constraints prevent partial state. The partial unique index prevents two in-flight atlas rows. The release function requires a terminal state with an explicit identity check.

If the application tries to insert a second atlas row while one is in `submitted`, the database returns a unique-violation error. The application cannot ignore it. The lock is the database, not the code.

## What this is not

This is not a claim that Phoenix has executed multiple revenue lanes successfully. It has not. The system is `FULL_LIVE_NO_ALPHA`. The lock schema is in place, the code paths that read and write the lock exist, and the integration tests in `live-executor/tests/postgres_nonce_recovery.rs` cover the lock acquisition, the failure paths, and the rotation drain. None of this has been exercised against real revenue, because no revenue has cleared the conservative gate.

What this *is* is a description of the lock's structure and the reasoning behind the database-enforced design. The position is that the lock should be impossible to violate, not just unlikely.

## Reviewable evidence

- `live-executor/schema/006_atlas_aave_revenue_lanes.sql` line 121-160 — singleton table, check constraints, partial unique index.
- `live-executor/src/store.rs` — `release_revenue_submission_lock`, lock acquisition and release paths.
- `live-executor/src/revenue.rs` — atlas-specific lock semantics.
- `live-executor/src/engine.rs` — `SubmissionUnknown` handling, disarm-on-unknown.
- `live-executor/src/store.rs` line 132-145 — `submission_lock_free` check.
- `live-executor/tests/postgres_nonce_recovery.rs` — integration tests covering lock rotation, drain, and concurrent races.
- `autonomous-live-e2e/tests/autonomous_live_e2e.rs` line 1482 — end-to-end test of the lock against simulated concurrent lanes.

## For your own system

If you are building a multi-lane or multi-strategy financial system with a shared signer:

1. **Encode the lock in the database.** Application-level mutexes are insufficient. A schema-enforced singleton with check constraints is the only design that survives application bugs.
2. **Use partial unique indexes for "at most one active row" constraints.** PostgreSQL supports partial unique indexes natively. They are the right tool for "at most one atlas request in flight" type invariants.
3. **Distinguish "lock released" from "submission reconciled."** A lock release should require a terminal attempt state. Reconciliation should be a separate audit log, not the lock release itself.
4. **Do not auto-release on `SubmissionUnknown`.** Unknown state means the nonce is not safe to reuse. The lock should hold until a human investigates.
5. **Cover the lock in integration tests.** The lock is a database-level invariant; test it with concurrent inserts and verify only one succeeds.