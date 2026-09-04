# Contributing to Phoenix (Anti-Gravity Phoenix v4)

Thank you for your interest in Phoenix.

Phoenix is an **experimental, fail-closed financial-infrastructure project** that combines AI-assisted engineering, real-time blockchain state, conservative transaction economics, and protected execution. It is **not** a normal open-source application: it submits transactions to a public chain, holds capital under strict control, and is currently in `FULL_LIVE_NO_ALPHA` (live-capable, no opportunity yet cleared the conservative profitability gate).

Contributions are welcome, but the project is governed by strict safety, scope, and authority rules.

---

## Where to start

| You want to… | Read this first |
|---|---|
| Understand the architecture | [`README.md`](README.md) |
| Read the engineering research | [`docs/engineering-notes/README.md`](docs/engineering-notes/README.md) |
| Inspect a specific design decision | The relevant file under [`docs/engineering-notes/`](docs/engineering-notes/) |
| Run the system locally | [`README.md` § Local Development](README.md) and `.env.example` |
| Audit the production safety model | [`docs/SECURITY.md`](docs/SECURITY.md), [`docs/RELEASE_AND_ROLLBACK.md`](docs/RELEASE_AND_ROLLBACK.md) |
| Propose a public engineering question | [`docs/engineering-notes/discussions/`](docs/engineering-notes/discussions/) (when Discussions are enabled) |

---

## Accepted contribution kinds

The project accepts the following **only**:

1. **Documentation improvements** — typo fixes, broken-link fixes, factual corrections, expanded context, additional cross-references.
2. **Engineering-note additions or updates** — evidence-backed write-ups of specific decisions, failure modes, or research questions, in the same four-category format as the existing notes (Architecture / Implementation / Observation / Profitability).
3. **Reproducibility tasks** — additional deterministic fixtures, integration test scaffolding that runs against the local Compose stack, and audit-script enhancements.
4. **Bounded benchmark tasks** — clearly bounded performance or economic-modeling measurements on local fixtures only.
5. **Public engineering questions** — opened via the repository's GitHub Discussions feature (when enabled) for the categories laid out in `docs/engineering-notes/discussions/`.

The project **does not** accept:

- New opportunity strategies that are not in the currently supported lanes (Aave V3 liquidation, Atlas auction solver, origin-aware V3 DEX arbitrage/backrun research). Strategies explicitly excluded from v4.0: liquidation, sandwich, frontrun, triangular, CEX, ML-based, Timeboost, Curve, Camelot, and broad blind-scanning.
- Changes to the economic-control or live-execution state machine without a corresponding ADR.
- Production secret material of any kind (private keys, signer bytes, authenticated RPC URLs, API keys, passwords, database credentials).
- Production SQL mutations, arm/disarm actions, or release-controller commands. Production mutation is owner-authorized only.
- Changes to `LIVE_EXECUTION`, `FULL_LIVE_NO_ALPHA`, or other production-safety flags.
- Mass-outreach, astroturfing, or manufactured engagement.

---

## Engineering-note contribution format

Engineering notes follow the existing six:

1. Lead with the **engineering question** that the note answers, in a way a developer would actually search.
2. State the **Status** at the top — categories from {Architecture, Implementation, Observation, Profitability} — and disclose whether the note claims verified performance.
3. Cite **specific Phoenix source files** for the design being discussed. Vague claims without code references will be returned for revision.
4. Acknowledge **what is and is not proven**. Phoenix does not claim profitable live execution; the system is `FULL_LIVE_NO_ALPHA`.
5. Cross-reference **related engineering notes** and the **corresponding discussion draft** (if one exists in `docs/engineering-notes/discussions/`).
6. Preserve the `FULL_LIVE_NO_ALPHA` disclaimer where the note touches live execution.

---

## Pull request flow

1. **Branch from current protected `main`.** Do not branch from a merged feature branch — that base is not protected.
2. Keep the scope **small and single-purpose**. Multi-purpose PRs will be returned for splitting.
3. Before pushing:
   - run `make verify`
   - run `git diff --check`
   - run the secret-scan script: `pwsh -ExecutionPolicy Bypass -File .\scripts\secret-scan.ps1`
   - confirm no production secret material is included
4. Open a **PR** targeting `main`. Required CI checks (Phoenix CI workflow, hygiene, language-specific test jobs) will run on the exact head commit.
5. Wait for all required checks before requesting merge.

Force-push and history rewrites on the feature branch are discouraged after the first review. Force-push to `main` is never permitted.

---

## Security disclosures

If you have found a security issue that affects Phoenix or the public contracts:

- **Do not open a public Issue** for the disclosure.
- Email the address listed in [`docs/SECURITY.md`](docs/SECURITY.md), or use GitHub's private vulnerability-reporting flow once the repository's security policy is fully published.

The project follows a coordinated disclosure model for any production-affecting issue.

---

## Community standards

This project adopts the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you agree to its terms.

The community surfaces are:

- **GitHub Discussions** (when enabled) — engineering questions, calibration discussions, release lifecycle debate.
- **GitHub Issues** — concrete engineering tasks with clear acceptance criteria, reproducibility tasks, and bounded benchmark tasks.

External community platforms (Reddit, Hacker News, X, LinkedIn, other repositories' issues) are not official Phoenix surfaces and are not used for project announcements.

---

## Scope reminder

This is financial infrastructure that submits real transactions.

When in doubt, do not assume a change is safe. The conservative answer is to **fail closed** and let the maintainer explicitly authorize any production-relevant modification.

Thank you for contributing to Phoenix.