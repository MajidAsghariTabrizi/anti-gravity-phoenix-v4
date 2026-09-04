# Phoenix Documentation Index

Phoenix publishes documentation across three layers:

1. **Engineering notes** — evidence-backed write-ups of specific design decisions and failure modes, written for a general blockchain-engineering audience.
2. **Blog posts** — long-form summaries linking the engineering notes to developer-searchable questions.
3. **Internal technical specifications and runbooks** — architecture, safety, release, deployment, operations, and security documentation for Phoenix operators.

> Phoenix is `FULL_LIVE_NO_ALPHA`. The documentation describes engineering, not financial results.

---

## Engineering notes

- [Engineering-notes index](engineering-notes/README.md) — the master index for the six published engineering notes and their cross-references into Phoenix source.
- [Engineering notes directory](engineering-notes/) — all notes.
- [Discussion drafts](engineering-notes/discussions/) — three Phoenix-owned Discussion bodies, ready to open when GitHub Discussions are enabled.

## Blog

- [Blog index](blog/README.md)
- [Engineering-notes roundup](blog/engineering-notes-roundup.md)

## Architecture and design

- [SHADOW_SECONDARY_VERIFICATION.md](SHADOW_SECONDARY_VERIFICATION.md)
- [SHADOW_ECONOMICS.md](SHADOW_ECONOMICS.md)
- [HOT_PATH.md](HOT_PATH.md)
- [RPC_BUDGET.md](RPC_BUDGET.md)
- [OPPORTUNITY_FUNNEL.md](OPPORTUNITY_FUNNEL.md)
- [STRATEGY_SELECTION.md](STRATEGY_SELECTION.md)
- [UNISWAP_ENTRYPOINTS.md](UNISWAP_ENTRYPOINTS.md)
- [CONTRACT_RISK_REGISTER.md](CONTRACT_RISK_REGISTER.md)
- [release-controller-architecture.md](release-controller-architecture.md)

## Safety, controls, and money-path

- [AUTOMATED_ECONOMIC_CONTROL.md](AUTOMATED_ECONOMIC_CONTROL.md)
- [EXECUTION_READINESS.md](EXECUTION_READINESS.md)
- [LIVE_READINESS_GATES.md](LIVE_READINESS_GATES.md)
- [MONEY_PATH_SELECTIVE_PERSISTENCE_V1.md](MONEY_PATH_SELECTIVE_PERSISTENCE_V1.md)
- [PRELIVE_SHADOW_CONTROL_PLANE.md](PRELIVE_SHADOW_CONTROL_PLANE.md)
- [PRELIVE_MONEY_PATH_OBSERVABILITY.md](PRELIVE_MONEY_PATH_OBSERVABILITY.md)
- [PROFITABILITY_THESIS.md](PROFITABILITY_THESIS.md)
- [SHADOW_PROFITABILITY_REPORTS.md](SHADOW_PROFITABILITY_REPORTS.md)
- [SHADOW_POSITIVE_ROUTE_EVIDENCE.md](SHADOW_POSITIVE_ROUTE_EVIDENCE.md)
- [SHADOW_ROUTE_DISCOVERY.md](SHADOW_ROUTE_DISCOVERY.md)

## Release, deployment, and operations

- [RELEASE_AND_ROLLBACK.md](RELEASE_AND_ROLLBACK.md)
- [DEPLOYMENT.md](DEPLOYMENT.md)
- [CI_CD.md](CI_CD.md)
- [production-live-runbook.md](production-live-runbook.md)
- [release-operations.md](release-operations.md)
- [emergency-pause-and-rollback.md](emergency-pause-and-rollback.md)
- [RUNBOOK.md](RUNBOOK.md)
- [PRODUCTION_BOOTSTRAP.md](PRODUCTION_BOOTSTRAP.md)
- [live-canary-executor-v1.md](live-canary-executor-v1.md)
- [AUTONOMOUS_LIVE_OPERATIONS.md](AUTONOMOUS_LIVE_OPERATIONS.md)
- [BENCHMARKS.md](BENCHMARKS.md)
- [PRELIVE_DASHBOARD.md](PRELIVE_DASHBOARD.md)

## Network, ingest, and observability

- [NITRO_FEED_INTEGRATION.md](NITRO_FEED_INTEGRATION.md)
- [RECORDER_DURABLE_DELIVERY.md](RECORDER_DURABLE_DELIVERY.md)
- [FORK_SANDBOX.md](FORK_SANDBOX.md)
- [ENGINE_DEPENDENCY_EXHAUSTION.md](ENGINE_DEPENDENCY_EXHAUSTION.md)
- [LIMITATIONS.md](LIMITATIONS.md)
- [DEPENDENCIES.md](DEPENDENCIES.md)

## Contracts

- [live-canary-executor-v1.md](live-canary-executor-v1.md)
- [AUTONOMOUS_HUNTER_CONTRACTS_V1.md](AUTONOMOUS_HUNTER_CONTRACTS_V1.md)
- [CONTRACT_RISK_REGISTER.md](CONTRACT_RISK_REGISTER.md)

## Releases, incidents, and audits

- [PHOENIX_PRELIVE_SHADOW_V5_RELEASE.md](PHOENIX_PRELIVE_SHADOW_V5_RELEASE.md)
- [release-incident-history.md](release-incident-history.md)
- [OLD_SYSTEM_AUDIT.md](OLD_SYSTEM_AUDIT.md)
- [incidents/](incidents/) — bounded incident write-ups
- [evidence/](evidence/) — bounded evidence snapshots
- [audits/](audits/) — bounded external-style audits
- [architecture/](architecture/) — bounded architectural baselines

## GitHub setup, secrets, and process

- [GITHUB_SETUP.md](GITHUB_SETUP.md)
- [GITHUB_ACTIONS_DEPENDENCIES.md](GITHUB_ACTIONS_DEPENDENCIES.md)
- [GIT_AND_SECRET_HYGIENE.md](GIT_AND_SECRET_HYGIENE.md)
- [GIT_FLOW.md](GIT_FLOW.md)

## Security

- [SECURITY.md](SECURITY.md)

---

## Decision records

- [docs/adr/](adr/) — bounded ADRs (Architecture Decision Records) captured for specific decisions

---

## How to navigate this documentation

If you are new to Phoenix:

1. Start with the [README](../README.md).
2. Read the [engineering-notes index](engineering-notes/README.md) for the recurring engineering research.
3. Read the [roundup post](blog/engineering-notes-roundup.md) for the developer-searchable framing.
4. Use the architecture, safety, release, and contracts links above for technical specification-level depth.
5. Use the engineering-notes cross-references into the source — every note cites specific Phoenix source files for the design being discussed.
6. Want to contribute? Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first.

## Contributing

For contribution guidelines, accepted contribution scope, and engineering-note format requirements, see [`CONTRIBUTING.md`](CONTRIBUTING.md).
