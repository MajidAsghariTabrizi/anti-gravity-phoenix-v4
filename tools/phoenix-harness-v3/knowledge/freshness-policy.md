# Phoenix Temporal State Freshness Policy

Every dynamic fact carries a freshness class. A fact is evidence only while
its freshness class is satisfied; otherwise it is historical context and
must be refreshed before any decision.

## Classes and maximum ages

| Class | Max age for decision use | Refresh action |
|---|---|---|
| `prod-live` (container health, lane armed state, locks, attempts, submissions, release gateway, controller state, provider readiness) | 5 minutes | `phoenix_production_snapshot` |
| `chain` (executor pause state, on-chain balances, reserves, finality) | 15 minutes | on-chain read via bounded remote/evidence path |
| `release` (active release SHA, image identity, autorelease flag) | before any release/ops action, regardless of age | `phoenix_release_verify` |
| `ci` (PR checks, exact-main CI runs) | 5 minutes during active work | `phoenix_ci_watch` |
| `ledger` (Ground-Truth ledgers, economic ledgers) | dated historical facts; never current truth | re-open ledger, stamp as-of date |
| `market` (auction/opportunity statistics) | 1 hour for prioritization | `phoenix_opportunity_replay` / SVR report |
| `repo` (git branch/SHA/status) | before push/PR actions | `phoenix_repo_snapshot` |

## Rules

1. A fact from class `prod-live`/`chain`/`release`/`ci` that is older than
   its class age is `STALE` and must not support a mutation or money-path
   decision. It may support hypotheses only.
2. Ledger and historical documents are never current truth — every cited
   figure carries its as-of date (see `knowledge/business-twin.md`).
3. `phoenix_evidence` records a freshness stamp per claim and refuses
   stale-evidence registration for decision claims.
4. The governor injects a staleness warning when a registered wait/decision
   relies on evidence older than its class age.
5. Unknown values remain unknown/null; never substitute stale or modeled
   values for measured ones.
