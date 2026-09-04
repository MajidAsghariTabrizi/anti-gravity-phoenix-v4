# Five Failure Modes Where `send_raw_transaction` Returning a Hash Is a Lie

> **Status:** ARCHITECTURE + IMPLEMENTATION. Observations from production shadow data referenced where relevant. **Profitability: NO VERIFIED ALPHA YET** (system is `FULL_LIVE_NO_ALPHA`).

> **Audience:** Engineers building or auditing any blockchain execution system that submits transactions and tracks outcomes. Applies to MEV searchers, liquidation systems, bridge relayers, and keeper networks.

## The premise most systems get wrong

When a JSON-RPC `eth_sendRawTransaction` call returns a transaction hash, the natural assumption is that the transaction is "live" — it has either been accepted by the sequencer or by a peer and is on its way to inclusion. From there, the usual logic is to wait for a receipt, and if the receipt is slow, *replace* the transaction with a higher-fee one.

The natural logic is wrong in five specific ways. Phoenix's `live-executor` crate handles each of them with a distinct code path, and in every case the system's response is to **disarm** rather than to retry.

## The five modes

### 1. The hash that returns doesn't match the hash we signed

`eth_sendRawTransaction` returns the transaction hash derived from `(nonce, gasPrice, gasLimit, to, value, data, chainId)`. If the provider returns a hash that does not match the hash of what was actually signed, something between our signer and the provider altered the transaction. The most common cause is a provider-side "speed-up" or gas-estimation override. The honest answer is that we no longer know what we submitted.

**Phoenix behavior** — `live-executor/src/engine.rs` line 242-251: the executor compares `returned_hash` against `signed.tx_hash()`. On mismatch, it calls `mark_submission_unknown(request.id, "submission_hash_mismatch", now)` and `disarm("submission_hash_mismatch")`, returning `ExecutionState::SubmissionUnknown { request_id, nonce }`. The executor will not issue any new execution authority until a human investigates.

### 2. The provider returns success, but the persistence layer fails

The RPC call succeeded. The transaction hash is in our hands. We try to write it to the store — and the write fails. This is the moment where most systems fall back to "in-memory state" and continue. The next iteration of the executor step will try to resume the active attempt, and the active attempt will not be findable in the store. The system will think the attempt never happened. It will start a new one with the same nonce. Two transactions, one nonce.

**Phoenix behavior** — `live-executor/src/engine.rs` line 253-264: on `mark_pending` failure, the executor calls `mark_submission_unknown(request.id, "hash_persistence_failure", now)` and `disarm("hash_persistence_failure")`, returning `EngineError::HashPersistence`. This path is harder to detect because the RPC has already been charged. The next restart will discover the active attempt, see that its `tx_hash` is missing, and again treat the submission as unknown.

### 3. The process restarts between nonce allocation and submission confirmation

The executor allocates a nonce, signs the transaction, calls `send_raw_transaction`, and then the process is killed (OOM, deploy, crash). When it comes back up, it queries `pending_nonce` for the wallet and finds a number. If that number is greater than the allocated nonce, it knows a transaction was submitted by *something* with that nonce. It does not know whether that transaction is the one Phoenix signed, or whether some other process raced ahead. It also doesn't know if the transaction is still in the mempool or already mined.

**Phoenix behavior** — `live-executor/src/engine.rs` line 297-311: on restart with `AttemptStatus::NonceAllocated` (no `tx_hash`), the executor calls `mark_submission_unknown(active.request.id, "restart_after_nonce_allocation", now)` and `disarm("submission_unknown")`. The recovery is not to assume "the transaction was probably dropped, so just send another." It is to treat the nonce as owned by an unknown submission and stop.

### 5. The transaction was sent but is neither in the mempool nor mined

The RPC returned a hash. The transaction is not visible in `eth_getTransactionByHash`. The transaction is not in the receipt history. The wallet's `pending_nonce` is unchanged from before the call. The transaction is, as far as we can tell, gone.

This can happen because of:
- Provider-side caching of a hash that never made it to the mempool
- Sequencer-level rejection after accepting (pre-D2-2024 Arbitrum had rare cases of this)
- Network partition between the provider and the sequencer

The naive response is to replace the transaction with the same nonce and higher gas. This is the wrong response: if the original transaction *is* in flight at a peer somewhere, replacing it cancels it. We pay for two transactions where we should have paid for one. Worse, if the original *was* mined and we're looking at a stale RPC view, the replacement is a duplicate — same nonce, different hash, and one of them is now in the wrong state.

**Phoenix behavior** — `live-executor/src/engine.rs` line 343-378: the reconciliation path checks `transaction_known` and compares `pending_nonce` to the allocated nonce. If the transaction is not known and `pending_nonce > nonce`, the executor calls `mark_terminal(active.request.id, AttemptStatus::Replaced, ...)` and `disarm("transaction_replaced")`. The replacement may be Phoenix's own resubmission, or it may be a searcher's. Either way, the executor disarms — the integrity assumption that "we own this nonce" has been violated.

### 5. The hash was returned but the executor cannot persist it *and* the process restarts

The combination of modes 2 and 3: persistence failed, the RPC said success, and the process restarts. The next step sees an attempt with `AttemptStatus::Claimed` (because `mark_pending` was never called) but with no `tx_hash`.

**Phoenix behavior** — `live-executor/src/engine.rs` line 282-296: this is actually treated as `restart_before_nonce_allocation` (less dangerous than the nonce-allocated case because no nonce has been burned). The attempt is marked `Failed` with reason `restart_before_nonce_allocation`, the executor disarms, and the operator must investigate before re-arming.

## Why "fail closed" is the only safe answer in all five cases

In every one of these modes, the system's "live" state has diverged from what the executor believes about it. Continuing to issue new execution authority — whether by retrying, replacing, or just claiming a fresh nonce — risks one of:

- **Nonce corruption** — submitting a transaction with a nonce that is owned by an in-flight unknown transaction.
- **Capital loss** — paying gas for a transaction that has already been paid for.
- **Conflicting transactions** — the `global_revenue_submission_lock` is a single-row table; if Phoenix is convinced no submission is active when one actually is, the lock does not protect against the second submission.

The Phoenix position is that "I do not know what happened to this submission" must close execution authority. The cost is operational downtime. The benefit is that the operator can investigate without races compounding.

## How the state is stored

`SubmissionUnknown` is a first-class enum variant in `live-executor/src/model.rs::AttemptStatus` (line 857). It is a valid value in the `execution_attempts.status` column check constraint — `live-executor/src/store.rs` line 20:

```sql
CHECK (status IN ('claimed', 'nonce_allocated', 'submission_unknown', 'pending', 'timed_out'))
```

It is a value the dashboards query for. `autonomous_live_control_main.rs` lines 2659-2661 produce a `unresolved_submissions` count from exactly this status:

```sql
SELECT count(*) FROM live_canary.execution_attempts
WHERE status IN ('submission_unknown','pending','timed_out')
```

The dashboard does not treat unresolved submissions as a recoverable warning. It is a release-blocker for any operation that would issue new execution authority.

## How this connects to the global submission lock

The lock `live_canary.global_revenue_submission_lock` (`schema/006_atlas_aave_revenue_lanes.sql` line 121-129) holds `active_lane` and `active_identity` while a lane is mid-submission. The check constraint enforces `(active_lane IS NULL) = (active_identity IS NULL)` and `(active_lane IS NULL) = (acquired_at IS NULL)`. The partial unique index `live_canary_one_global_revenue_submission` (line 158-160) physically prevents two atlas solver rows from being in `claimed/signed/submitted/submission_unknown` simultaneously:

```sql
CREATE UNIQUE INDEX live_canary_one_global_revenue_submission
ON live_canary.atlas_solver_requests ((true))
WHERE status IN ('claimed', 'signed', 'submitted', 'submission_unknown');
```

The lock is only meaningful when the system that holds it can prove it knows what it submitted. `SubmissionUnknown` is the state in which the system explicitly does *not* know, and the lock — by design — does not get cleared automatically when entering `SubmissionUnknown`. The lock is cleared only by reconciliation or by a human.

## What this is not

This is not a claim that Phoenix has executed transactions profitably. It is not. The system is `FULL_LIVE_NO_ALPHA`. The current production shadow ledger shows 39,538 shadow evaluations and 0 attempted executions; aggregate conservative PnL is **negative**, which is what the conservative gate is supposed to produce when no opportunity is real.

What this *is* is a description of how Phoenix handles the case where a submission cannot be accounted for. The position is that "we don't know" is more expensive than "we paused." Most public discussion of MEV bot failures focuses on gas estimation, slippage, or frontrunning. The submission-uncertainty class of failure is less discussed and, in the Phoenix team's experience, equally dangerous.

## Reviewable evidence

- `live-executor/src/engine.rs` — `step`, `reconcile_active`, `complete_receipt`, all `mark_submission_unknown` call sites.
- `live-executor/src/model.rs` line 853-880 — `AttemptStatus` enum and string mapping.
- `live-executor/src/store.rs` line 20, 51-91 — store-level submission state transitions.
- `live-executor/schema/006_atlas_aave_revenue_lanes.sql` line 121-160 — submission lock and partial unique index.
- `live-executor/tests/postgres_nonce_recovery.rs` — integration tests covering recovery from each of the five modes.
- `live-executor/tests/engine_state_machine.rs` line 1158-1174 — `ExecutionState::SubmissionUnknown` recovery tests.

## For your own system

If you are building or auditing a similar system, the question to ask is not "do I retry on hash mismatch" but "do I have a state machine that includes `submission_unknown` as a terminal-until-investigated state, and is that state machine enforced by the database schema, not just by application code?" The application layer can be bypassed. The check constraint cannot.

---

## Related Phoenix discussion draft

When GitHub Discussions are enabled on this repository, the corresponding Phoenix-owned discussion thread is pre-written at [`discussions/submission-state-state-machine.md`](discussions/submission-state-state-machine.md) — *"When should a blockchain execution system fail closed?"* The roundup post at [`../blog/engineering-notes-roundup.md`](../blog/engineering-notes-roundup.md) places this note in the developer-searchable context.