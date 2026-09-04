# When should a blockchain execution system fail closed?

> **Status:** Phoenix is `FULL_LIVE_NO_ALPHA`. This is a discussion of design, not of results.
>
> **Related Phoenix engineering notes:**
> - [`submission-unknown-failure-modes.md`](../submission-unknown-failure-modes.md)
> - [`dual-rpc-agreement-pattern.md`](../dual-rpc-agreement-pattern.md)

## Background

A common pattern in MEV bot and liquidation-bot postmortems is the failure mode where the system kept issuing new execution authority when its prior submission was already in an unknown state. The system believed the prior transaction had been dropped. The transaction had not been dropped. The result is a nonce collision, a duplicate submission, or a submission that invalidates another.

Phoenix treats `submission_unknown` as a first-class state. The five paths that lead to it are documented in the linked engineering note, and in every case the executor disarms and refuses new execution authority until a human investigates.

## Questions for the discussion

1. What failure modes have you seen in production where a system continued issuing execution authority after a prior submission became unaccounted for?
2. Where do you draw the line between "this is an automatic recovery" and "this requires a human"? Phoenix draws the line at `submission_unknown`. Where do others draw it?
4. For systems with multiple revenue lanes competing for one signer — how is the global submission lock structured in your implementation? Application mutex, database lock, both?
5. For systems that monitor submission state across process restarts — how is the post-restart state decided, and what evidence is required before new execution authority is granted?

## What this is not

This is not a claim that Phoenix has executed transactions profitably. It has not. The system is `FULL_LIVE_NO_ALPHA`. Phoenix has run 39,538 shadow evaluations and 0 attempted live executions, with aggregate conservative PnL negative. The conservative gate is doing what it is designed to do.

What this is, is an invitation to discuss the design space around submission uncertainty. The positions Phoenix takes are:

- "I do not know what happened to this submission" must close execution authority.
- The lock is held in the database, not the application. Check constraints and partial unique indexes, not application mutexes.
- Auto-recovery on `submission_unknown` is forbidden by design. Recovery requires human investigation.
- Five specific failure modes produce this state, and the executor's response is the same in all five.

What positions have others taken?