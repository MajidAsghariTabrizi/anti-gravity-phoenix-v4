# Lesson L-008 — PACKAGE_ALREADY_EXISTS recovery

- **Incident**: re-receiving an already-installed release artifact surfaced as an error instead of an idempotent identity check.
- **Root cause**: treating an already-exists state as failure instead of comparing identities.
- **Evidence**: `docs/release-controller-architecture.md` (completed idempotency; receive step verifies identity); release-incident-history item 5 (one coherent release identity).
- **Rule**: already-exists states are idempotent-recoverable — compare the identity (SHA), then refresh or skip; never fail blindly and never overwrite without provenance. The V3 installer is idempotent by construction (manifest refreshed, source hash recorded).
- **Regression test**: `tests/ctl.test.mjs` (double install succeeds with identical source hash; provenance manifest refreshed).
