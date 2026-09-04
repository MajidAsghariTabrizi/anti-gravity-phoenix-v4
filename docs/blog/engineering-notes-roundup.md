# Engineering Notes: How Phoenix Approaches Six Recurring Problems in Blockchain Execution

Phoenix has published six engineering notes documenting specific design decisions and failure modes from its implementation. This post links to each note with the searchable context an engineer is most likely searching for. None of the notes claim profitable live trading. Phoenix is `FULL_LIVE_NO_ALPHA`: live-capable and fully gated, with no opportunity yet cleared the complete conservative profitability gate. The notes document the engineering, not the financial performance.

---

## 1. When `send_raw_transaction` returning a hash is a lie

**Engineering note:** [submission-unknown-failure-modes.md](../engineering-notes/submission-unknown-failure-modes.md)

When a JSON-RPC `eth_sendRawTransaction` returns a hash, most systems assume it is live. That assumption breaks in five specific ways: hash mismatch, persistence failure after RPC success, post-restart recovery, in-flight disappearance, and replacement by another transaction. Phoenix treats `submission_unknown` as a first-class state with all five paths leading to disarm. Search this note when investigating "transaction submitted unknown state," "submission hash mismatch," "nonce race," "MEV bot stuck transaction replacement," or designing a submission state machine.

**Code reference:** `live-executor/src/engine.rs` (`step`, `reconcile_active`, `SubmissionUnknown` enum variant) and `live-executor/src/model.rs::AttemptStatus`.

---

## 2. Eleven costs a liquidation bot probably isn't subtracting

**Engineering note:** [conservative-economic-gate.md](../engineering-notes/conservative-economic-gate.md)

Liquidation economic models that only subtract gas and the flash-loan premium will systematically overstate profit on Arbitrum. The Phoenix model subtracts eleven categories of cost (gas, L1 data fee, slippage, price impact, flash-loan premium, failure reserve, stale-state reserve, latency reserve, uncertainty reserve, ordering reserve, contract overhead) across three named scenarios (BASE, CONSERVATIVE, SEVERE), and gates the candidate on the conservative scenario with strict inequality (`>`) at the floor. Search this note when investigating "Aave liquidation profitability," "modeled vs realized PnL," "MEV gas L1 cost Arbitrum," "conservative economic gate design," or calibrating any liquidation economic model.

**Code reference:** `phoenix-engine/src/economics/mod.rs` (`ScenarioConfig`, `evaluate`, `evaluate_scenarios`, the `>`-only floor test).

---

## 3. When two RPC providers should agree, not just fail over

**Engineering note:** [dual-rpc-agreement-pattern.md](../engineering-notes/dual-rpc-agreement-pattern.md)

The standard "fast + fallback" RPC pattern optimizes for availability. For execution authority reads, it is the wrong pattern. Phoenix requires two independent providers to agree on critical finalized state — block number, block hash, account state, oracle prices, state root — before any execution authority exists. Disagreement produces a structured error (`GatewayError::ProviderDisagreement`) with `retryable()=false`. Search this note when investigating "RPC provider disagreement," "dual RPC pattern," "multi-provider blockchain read," "execution authority RPC," or evaluating whether failover is sufficient for your read.

**Code reference:** `rpc-gateway/src/runtime.rs` (`resolve_hunter_state`, `resolve_aave_primary_screen`, `GatewayError::ProviderDisagreement`) and `rpc-gateway/src/hunter_state.rs::ProviderStateAgreement::agreed()`.

---

## 4. The global revenue submission lock

**Engineering note:** [global-revenue-submission-lock.md](../engineering-notes/global-revenue-submission-lock.md)

Multiple revenue lanes sharing one wallet need a global submission lock to prevent nonce races. Application-level mutexes are insufficient. Phoenix implements the lock as a SQL-level singleton with check constraints and a partial unique index. The database enforces the invariant; application bugs cannot create two simultaneous submissions. Search this note when investigating "multi-strategy nonce race," "shared wallet nonce strategies," "SQL enforced singleton lock," "postgres partial unique index concurrency," or designing a multi-lane financial system with a shared signer.

**Code reference:** `live-executor/schema/006_atlas_aave_revenue_lanes.sql` (`global_revenue_submission_lock` singleton, `live_canary_one_global_revenue_submission` partial unique index) and `live-executor/src/store.rs::release_revenue_submission_lock`.

---

## 5. Why activation is not deployment

**Engineering note:** [protected-release-lifecycle.md](../engineering-notes/protected-release-lifecycle.md)

For most software, "merge → deploy → live" is correct. For money-touching software, it is wrong. Phoenix separates deployment from activation: the deploy script (`deploy-release.sh`) starts services in SHADOW mode by default and cannot enable LIVE. The release manifest binds images by digest, not by tag, and binds provenance to exact CI runs. Activation is a separate workflow that cannot be reached from the deploy pipeline. Search this note when investigating "deployment vs execution activation financial system," "immutable artifact digest pinning," "release manifest provenance," "rollback release script," or designing any CI/CD for money-touching code.

**Code reference:** [`docs/RELEASE_AND_ROLLBACK.md`](../RELEASE_AND_ROLLBACK.md) (15-step deploy, manifest schema, asset bundling) and the deploy/rollback shell scripts referenced from it.

---

## 6. Why "live" is not a sufficient state name

**Engineering note:** [naming-financial-system-states.md](../engineering-notes/naming-financial-system-states.md)

A financial system can be in many states: live-capable but unprofitable, profitable but undisarmable, fail-closed due to provider disagreement, fail-closed due to unknown submission, disarmed for daily-loss budget. Phoenix names these explicitly: `FULL_LIVE_NO_ALPHA`, `FIRST_POSITIVE_REALIZED_PNL`, `STEADY_POSITIVE_REALIZED_PNL`, `FAIL_CLOSED_DUE_TO_*`, `DISARMED_FOR_DAILY_LOSS_BUDGET`. The four-category distinction (Architecture / Implementation / Observation / Profitability) holds throughout. Search this note when investigating "financial system state machine," "production state vocabulary," "how to describe a financial infrastructure system state," or designing an operational taxonomy for any financial system.

**Code reference:** [`live-executor/src/engine.rs::ExecutionState`](../engineering-notes/naming-financial-system-states.md) and [`live-executor/src/economic_control.rs`](../AUTOMATED_ECONOMIC_CONTROL.md) (control state transitions).

---

## Discussion drafts ready to open

Three Phoenix-owned GitHub Discussion bodies are pre-written in [`docs/engineering-notes/discussions/`](../engineering-notes/discussions/):

- `submission-state-state-machine.md` — "When should a blockchain execution system fail closed?"
- `conservative-gate-calibration.md` — "How should a liquidation engine calculate real profitability?"
- `deploy-vs-activate.md` — "Should deployment and execution activation be the same action?"

Each leads with the engineering question, links the relevant Phoenix material, preserves the `FULL_LIVE_NO_ALPHA` disclaimer, and asks genuine technical questions of the community.

## Where to read next

- Engineering-notes index — [`docs/engineering-notes/README.md`](../engineering-notes/README.md)
- Documentation index — [`docs/README.md`](../README.md)
- Phoenix README — [`README.md`](../../README.md)
- Contributing — [`CONTRIBUTING.md`](../CONTRIBUTING.md)
- Code of conduct — [`CODE_OF_CONDUCT.md`](../../CODE_OF_CONDUCT.md)

---

## Honest production story

The notes above document engineering decisions that resulted in 39,538 shadow evaluations, 0 attempted live executions, and 0 realized PnL. The conservative economic gate has rejected every modeled opportunity. The aggregate conservative PnL is negative, which is what the conservative model is designed to produce when no opportunity is real. The fact that the gate is *strict enough to refuse everything that has been seen* is the design working as intended.

The interesting engineering questions are:

- Is the conservative gate correctly calibrated, or over-conservative for the current Arbitrum liquidation market?
- What is the distribution of the rejected candidates — marginally negative (gate calibrated) or catastrophically negative (strategy structural)?
- Is the submission-unknown state machine too restrictive for healthy operations, or appropriately restrictive?

These are the questions Phoenix is publishing these notes to invite discussion on.
