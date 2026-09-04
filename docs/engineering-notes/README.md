# Engineering Notes

Technical write-ups produced from Phoenix's implementation, documenting specific engineering decisions, trade-offs, and failure modes that are relevant across the blockchain infrastructure ecosystem.

Each note is evidence-backed by verified Phoenix source files. The four-category distinction — **Architecture**, **Implementation**, **Observation**, **Profitability** — holds throughout. Where a note cites production data, it says so explicitly. Where it does not, it says so as well.

> Phoenix is `FULL_LIVE_NO_ALPHA`: live-capable and fully gated, with no opportunity yet cleared the complete conservative profitability gate. The notes below document the engineering, not the financial performance.

---

## Notes

| # | Note | Problem addressed | Key Phoenix edge |
|---|------|-------------------|------------------|
| 1 | [Submission Unknown Failure Modes](submission-unknown-failure-modes.md) | `send_raw_transaction` returns a hash — is the transaction actually live? | `SubmissionUnknown` as a first-class state with five failure-mode paths |
| 2 | [Conservative Economic Gate](conservative-economic-gate.md) | Which execution costs does a liquidation strategy actually need to subtract? | Eleven cost categories, three scenarios, strict-inequality floor |
| 3 | [When Two RPC Providers Should Agree, Not Just Fail Over](dual-rpc-agreement-pattern.md) | When is provider failover the wrong RPC pattern? | Two-provider agreement-or-fail-closed for execution authority reads |
| 4 | [Global Revenue Submission Lock](global-revenue-submission-lock.md) | How do multiple strategies share one wallet nonce safely? | SQL-enforced singleton lock with partial unique index |
| 5 | [Protected Release Lifecycle](protected-release-lifecycle.md) | Should code deployment be the same action as enabling live execution? | 15-step deploy script with activation separate from deployment |
| 6 | [Why "Live" Is Not a Sufficient State Name](naming-financial-system-states.md) | Why "live" is not a sufficient description of a financial system's posture | Explicit state vocabulary with four-category provenance |

---

## Cross-references into Phoenix source

The notes above are written for a general blockchain-engineering audience. The internal Phoenix docs below provide the technical specification-level detail behind each decision:

- **Economic modeling:** [`SHADOW_ECONOMICS.md`](../SHADOW_ECONOMICS.md), [`PROFITABILITY_THESIS.md`](../PROFITABILITY_THESIS.md)
- **RPC evidence:** [`RPC_BUDGET.md`](../RPC_BUDGET.md), [`SHADOW_SECONDARY_VERIFICATION.md`](../SHADOW_SECONDARY_VERIFICATION.md), [`HOT_PATH.md`](../HOT_PATH.md)
- **Execution lifecycle:** [`AUTOMATED_ECONOMIC_CONTROL.md`](../AUTOMATED_ECONOMIC_CONTROL.md), [`LIVE_READINESS_GATES.md`](../LIVE_READINESS_GATES.md)
- **Release and deployment:** [`RELEASE_AND_ROLLBACK.md`](../RELEASE_AND_ROLLBACK.md), [`DEPLOYMENT.md`](../DEPLOYMENT.md), [`CI_CD.md`](../CI_CD.md)
- **Money-path philosophy:** [`MONEY_PATH_SELECTIVE_PERSISTENCE_V1.md`](../MONEY_PATH_SELECTIVE_PERSISTENCE_V1.md)

---

## Discussion drafts

Three Phoenix-owned Discussion bodies are pre-written in [`discussions/`](discussions/) and ready to open when the repository's GitHub Discussions feature is enabled:

| Discussion title | Related engineering note |
|---|---|
| *When should a blockchain execution system fail closed?* | [`submission-unknown-failure-modes.md`](submission-unknown-failure-modes.md), [`dual-rpc-agreement-pattern.md`](dual-rpc-agreement-pattern.md) |
| *How should a liquidation engine calculate real profitability?* | [`conservative-economic-gate.md`](conservative-economic-gate.md) |
| *Should deployment and execution activation be the same action?* | [`protected-release-lifecycle.md`](protected-release-lifecycle.md) |

The discussion drafts preserve the same `FULL_LIVE_NO_ALPHA` disclaimer and ask genuine technical questions of the community. They are not comments on other repositories' issues.

---

## Why these exist

The notes are Phoenix-owned technical content produced to contribute to ongoing public engineering discussions. They are not marketing, not outreach, and not comments on other repositories' issues.

Contributing guidelines, accepted contribution scope, and engineering-note format requirements are at [`docs/CONTRIBUTING.md`](../CONTRIBUTING.md).

The strongest recurring public conversations these address:

- "What should a trading engine do when a submission is unknown?" → Note 1
- "How should a liquidation bot calculate real profitability?" → Note 2
- "When should an MEV system require two providers instead of one?" → Note 3
- "How do you safely prevent nonce races across multiple strategies?" → Note 4
- "Should deployment be the same action as activating live execution?" → Note 5
- "What states can a financial system actually be in?" → Note 6

The target flywheel:

> Technical problem → Engineering note → Search discovery → Developer reads the technical explanation → Developer inspects the Phoenix source → Stars / forks / discussion / contributors → Future integrations

A round-up post linking each note to the developer-searchable context is at [`../blog/engineering-notes-roundup.md`](../blog/engineering-notes-roundup.md).
