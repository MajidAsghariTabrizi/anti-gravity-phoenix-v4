# Contract Risk Register

No contract is deployed by this work. Severity reflects potential capital impact, not an assertion of exploitability.

| ID | Severity | Area | Evidence | Required treatment before canary | Status |
|---|---|---|---|---|---|
| C-01 | High | Intermediate balances | Each leg supplies configured `amountIn`; the contract does not bind it to the prior leg's actual output. Pre-existing intermediate balances may be consumed while the final flash-asset profit guard still passes. | Bind each leg to actual prior output or prove an equivalent invariant; add fork/fuzz tests with donated balances. | Open |
| C-02 | High | Deployment truth | No verified Arbitrum deployment, bytecode hash, constructor configuration, or allowlist exists. | Reviewed deployment in a separate release; persist and verify code/config hashes in every simulation. | Open |
| C-03 | High | Token behavior | Low-level transfer/approve supports optional boolean returns but does not make fee-on-transfer, rebasing, callback, or approval semantics safe. | Strict verified token allowlist plus adversarial token tests. | Open |
| C-04 | Medium | Price limit | Swaps use extreme V3 sqrt-price limits; protection relies on `minAmountOut`. | Define and test per-leg price-limit policy in addition to minimum output. | Open |
| C-05 | Medium | Fork parity | Tests use local mocks only. | Pinned deterministic Arbitrum fork tests for Aave callback, V3 callback, fees, approvals, and profit settlement. | Open |
| C-06 | Medium | Invariants/fuzzing | No invariant or broad fuzz tests are present. | Add balance conservation, no-loss, authorization, callback, deadline, approval, and atomic-revert invariants. | Open |
| C-07 | Medium | Governance | Owner can change searchers, flash provider, factories, pools, pause, and rescue assets. Custody is unspecified. | Multisig/timelock and emergency procedure review appropriate to canary scope. | Open |
| C-08 | Medium | Searcher authorization | Authorized searchers can submit any route that passes on-chain allowlists and guards. | Signer-side canonical opportunity binding, replay protection, policy/version checks, and audit log. | Open |
| C-09 | Medium | Flash provider update | Owner can change the provider immediately; active execution interactions have not been adversarially tested. | Operational pause-before-change rule and state-machine tests. | Open |
| C-10 | Low | Approval lifecycle | Repayment approval is reset then set to exact repayment, but residual allowance behavior across non-standard tokens is unverified. | Standard-token allowlist and allowance invariants. | Open |
| C-11 | Low | Rescue centralization | Owner-only rescue can move arbitrary token balances. | Document custody, destination allowlist or governance control, and audit events. | Open |
| C-12 | Low | Upgradeability | Contract is not upgradeable, reducing proxy risk but requiring a new deployment for fixes. | Versioned deployment/runbook and explicit deprecation procedure. | Open |

## Existing Positive Controls

The contract checks owner/searcher authorization, pause, asset/factory/pool allowlists, factory-derived pool identity, token/fee/direction route consistency, deadline, authenticated flash and V3 callbacks, per-leg minimum output, flash repayment, and minimum final profit. Top-level execution is non-reentrant and a failure atomically reverts the transaction.

## Release Blocker

Open High risks, missing fork/invariant evidence, or an unverified deployment block any tiny-capital canary. A positive local mock test or SHADOW PnL result cannot waive a contract risk.
