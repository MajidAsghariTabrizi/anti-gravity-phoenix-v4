# Lesson L-010 — No-op goal rounds

- **Incident**: rounds that produce zero change (goal-state polls, waiting on CI/jobs) burn full-context model calls — measured: 69 job_output polling rounds (5.9% of requests, ~1M billed-eq tokens) in the V2 build session.
- **Root cause**: the model polls instead of suspending; each poll bills the entire context.
- **Evidence**: Phase 0 forensics (`tools/phoenix-harness-v3/reports/00-phase0-forensics.md`, `.phoenix-harness/telemetry/`); harness seam verified in `dsh-agent-loop` — `agent/pre-step` `{kind:'reject'}` ends the turn BLOCKED with zero model calls.
- **Rule**: waits suspend the model INSIDE native tools (phoenix_ci_watch blocks until state change or deadline); bookkeeping-only rounds during an active wait, and goal-round continuations after a hard-stop breach, are REJECTED by the governor at pre-step (zero-cost blocked turns). Waits never generate polling rounds.
- **Regression test**: `tests/governor.test.mjs` (reject on bookkeeping streak during wait; hard-stop rejection; waitForState resolve/deadline semantics).
