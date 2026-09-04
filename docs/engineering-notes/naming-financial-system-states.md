# Why "Live" Is Not a Sufficient State Name for a Financial System

> **Status:** ARCHITECTURE + IMPLEMENTATION. Documents the production-state vocabulary Phoenix uses and the reasoning behind it. **Profitability: NO VERIFIED ALPHA YET.**

> **Audience:** Anyone running, building, or auditing a production financial-infrastructure system.

## The state most projects are afraid to name

There is a state that nearly every production financial-infrastructure system has been in, and that almost none of them name explicitly:

> The system is live-capable and armed. Production execution infrastructure exists. Safety gates are active. The system is capable of live operation. **No opportunity has yet cleared the complete conservative profitability and execution validation pipeline.**

Phoenix names this state `FULL_LIVE_NO_ALPHA`. It is documented in the README (`README.md` line 512-530) and in `docs/PROFITABILITY_THESIS.md`. It is also the system's current production state.

Naming this state explicitly is itself a design decision. Most systems leave it implicit, which produces two specific failure modes:

1. **Stakeholders assume profitability.** A reviewer who sees "live" without seeing "no alpha" assumes revenue. They relax the controls, increase the budget, or scale the system based on an assumption that has never been tested.

2. **Operators hide it.** A team whose system is "live but not profitable" treats the absence of alpha as a failure to be explained. They either lower the gate to manufacture activity, or they keep the state private and make decisions that are inconsistent with it.

Both failure modes are bad. Naming the state makes it visible, which makes it discussable, which makes the system's posture toward it a real one.

## The vocabulary

Phoenix uses a small set of named states. Two of them are documented in the README. The full set is:

### `FULL_LIVE_NO_ALPHA`

> Production execution infrastructure is live-capable. Safety gates are active. No opportunity has yet cleared the conservative profitability gate. Realized PnL is zero. There is no live capital at risk beyond what the safety gates permit.

The current Phoenix state.

The characteristics of `FULL_LIVE_NO_ALPHA`:

- The system is producing decisions (observation is active).
- The system is not producing realized PnL (the gate is preventing it).
- The system is producing economic truth (shadow data is recorded).
- The system is at risk of being misread as "live but unprofitable," which is a misread.

### `FIRST_POSITIVE_REALIZED_PNL`

> A real transaction has been submitted, confirmed, balance-reconciled, and has produced positive realized net PnL.

This is the first state in which the system has evidence of real capital gain through the conservative gate. Reaching it requires:

- A submission to have cleared the conservative gate.
- The submission to have been included in a block.
- The receipt to have been reconciled against the baseline.
- The balance diff to be positive after fees, L1 cost, and flash premium.
- The realized net PnL to be positive.

Each of these is a separate gate. None of them is implicit.

The transition from `FULL_LIVE_NO_ALPHA` to `FIRST_POSITIVE_REALIZED_PNL` is the moment when the system goes from "we have a hypothesis" to "we have evidence." Phoenix's design treats this as a meaningful transition, not as a routine event.

### `STEADY_POSITIVE_REALIZED_PNL`

> The system has sustained `FIRST_POSITIVE_REALIZED_PNL` across a meaningful number of trades.

This state is not yet reached by Phoenix. The conditions for it are not specified — they would require empirical evidence of distribution and drawdown behavior.

### `FAIL_CLOSED_DUE_TO_PROVIDER_DISAGREEMENT`

> Execution authority is currently closed because two independent RPC providers disagree on critical finalized state.

This is a transient state. The system is operating normally; the disagreement is a property of the providers, not the system. The system will re-acquire authority when the providers converge.

### `FAIL_CLOSED_DUE_TO_UNKNOWN_SUBMISSION`

> Execution authority is currently closed because a prior submission cannot be accounted for.

This is a *not*-transient state. The lock is held until a human investigates. The system does not automatically recover.

### `FAIL_CLOSED_DUE_TO_OBSERVED_LOSS`

> A trade executed, was reconciled, and produced a realized loss. The system has stopped further execution pending investigation.

This state is distinct from "the conservative gate rejected." It means a trade happened, and the trade lost money. The right response is investigation, not gate adjustment. Lowering the gate after a loss is a common failure mode; the state machine prevents it by making the post-loss state explicit.

### `DISARMED_FOR_DAILY_LOSS_BUDGET`

> Realized losses for the day exceed the configured maximum. The system has disarmed. Re-arming requires operator action.

This is a daily reset state. The budget is enforced at the executor layer (`live-executor/src/engine.rs` line 67-72):

```rust
let daily_loss = self.store.daily_loss_wei(now).await?;
if daily_loss >= self.config.limits.maximum_daily_loss_wei {
    self.store.disarm("daily_loss_budget").await?;
    return Ok(ExecutionState::Disarmed {
        reason: DisarmReason::DailyLossBudget,
    });
}
```

The budget is checked before claiming a new attempt and again before submission. Two checks because the state can change between the two.

## Why naming matters

The states above are not just labels. They map to specific code paths, specific disarms, and specific database transitions. The state machine is implemented in:

- `live-executor/src/engine.rs` — `ExecutionState` enum, terminal states, disarm reasons.
- `live-executor/src/economic_control.rs` — control state, daily loss, retention floor.
- `live-executor/src/model.rs` — `AttemptStatus`, `DisarmReason`.
- `live-executor/schema/*.sql` — durable state transitions in PostgreSQL.

When the system is in `FAIL_CLOSED_DUE_TO_PROVIDER_DISAGREEMENT`, the engine's `step` function returns early with no attempt claimed. When the system is in `DISARMED_FOR_DAILY_LOSS_BUDGET`, the engine refuses to claim and refuses to sign. The code does not assume "we'll just wait for the budget to reset tomorrow"; the disarm is operator-controlled.

When the system is in `FULL_LIVE_NO_ALPHA`, the conservative gate is the active filter. The dashboard reads this state correctly because the gate is producing `BelowMinimum` statuses, not errors.

When the system is in `FIRST_POSITIVE_REALIZED_PNL`, the dashboard shows the realized PnL row in PostgreSQL, the conservative PnL aggregate, and the loss-cause ledger. These are all separate columns because they answer different questions.

## The honest production data

Phoenix's `PHOENIX_LESSONS_LEARNED.md` documents the production shadow ledger:

- 39,538 shadow decisions over months.
- 0 live executions.
- 0 realized PnL.
- Aggregate conservative PnL: **negative**.
- Aggregate severe PnL: **negative**.

This is the data of a system in `FULL_LIVE_NO_ALPHA`. The conservative gate has rejected every modeled opportunity. The aggregate conservative PnL is negative, meaning the modeled opportunities, if executed at conservative assumptions, would have lost money.

The state is named. The data is published. The system's posture is honest.

## What this is not

This is not a claim that `FULL_LIVE_NO_ALPHA` is the desired steady state. It is the desired state for the duration of the system's evolution from "hypothesis" to "evidence." A system that permanently remains in `FULL_LIVE_NO_ALPHA` has not produced value.

Phoenix's goal is to reach `FIRST_POSITIVE_REALIZED_PNL` through the conservative gate. The conservative gate may be correctly conservative; it may also be over-conservative for the current state of the Arbitrum liquidation market. The next engineering question is which. The state name lets that question be asked without ambiguity.

## Reviewable evidence

- `README.md` line 512-530 — the explicit "current status" section.
- `docs/PROFITABILITY_THESIS.md` — falsifiable profitability hypothesis with specific rejection criteria.
- `PHOENIX_LESSONS_LEARNED.md` — production shadow ledger data.
- `live-executor/src/engine.rs::ExecutionState` — terminal and transient states.
- `live-executor/src/model.rs::AttemptStatus` — attempt-level states.
- `live-executor/src/economic_control.rs` — control state transitions.
- `live-canary-executor-v1.md` — production executor documentation.

## For your own system

If you are building or auditing a financial-infrastructure system, the questions to ask:

1. **Do you have a name for the state the system is in right now?** If "live" is the only name, the system is misnamed.
2. **Is the state machine implemented in the database or only in the application?** Application-level state machines can be bypassed. Schema-level ones cannot.
3. **Can the system transition between states automatically?** Some transitions should be automatic (re-acquiring authority after provider agreement). Some should require manual (re-arming after a realized loss). Distinguishing them is design.
4. **Is the system's current state honest?** If the system claims to be profitable and is not, the state is dishonest. If the system claims to be in development and is actually trading, the state is also dishonest. Honest naming is the prerequisite for honest operation.