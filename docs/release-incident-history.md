# Release Incident History and Permanent Invariants

The automated platform converts prior failures into enforced contracts:

1. Event-scoped Engine integrity quarantines only the delivery; process-scoped
   integrity remains fatal.
2. RPC URLs and weights must both contain exactly two entries; providers must be
   distinct and weights positive.
3. Environment is reloaded after atomic mode changes; stale SHADOW shell values
   cannot select a healthcheck branch.
4. LIVE healthcheck explicitly uses the LIVE overlay.
5. `current-release` and `release-assets.sha` are one coherent identity.
6. Pointer metadata is root-owned with the Phoenix group; failures report path and
   expected/actual UID, GID, mode, and link count.
7. Stale LIVE state with actual SHADOW runtime is metadata-reconciled only after
   running-image proof.
8. Rollback compatibility and required files are checked before mutation.
9. Context tooling is version-matched to its immutable release tree.
10. Extracted shell and Python assets use explicit interpreters on `noexec`.
11. Isolated Python entrypoints establish safe sibling-package imports.
12. The production controller is Linux-only; PowerShell parsing, redraw watchers,
    local path shortening, and laptop temp cleanup are outside the control plane.
13. Non-interactive GitHub identity replaces SSH passphrases and sudo passwords.
14. Normal production has no required-reviewer environment gate; protected main,
    exact CI, provenance, gateway preflight, and rollback remain mandatory.
15. Candidate LIVE Compose is rendered against a temporary LIVE copy, never the
    real SHADOW operator environment.
16. Errors preserve exact structured expected/actual evidence.
17. Root-owned durable phases permit safe resume after interruption.
18. Source CI, build, receive, preflight, deploy, owner operation, health, evidence,
    and rollback are one serialized controller.

The deterministic controller tests also cover concurrent serialization, completed
idempotency, stale main, missing/expired artifacts, secret-safe evidence, restricted
SSH/sudo, docs-only skips, and one state machine for hotfixes, features, and
migration releases.
