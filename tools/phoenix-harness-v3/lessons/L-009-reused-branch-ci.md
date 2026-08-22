# Lesson L-009 — Reused-branch CI failure

- **Incident**: reusing a branch tried to borrow CI evidence from a different head, invalidating the protected-CI gate.
- **Root cause**: CI evidence is bound to an exact head; a reused branch name does not carry its predecessor's runs.
- **Evidence**: `docs/release-controller-architecture.md` (stale main / exact-head CI); release-incident-history item 14 (exact CI mandatory).
- **Rule**: CI runs are bound to exact heads — every PR/merge re-runs at its own head; `phoenix_release_preflight` and `phoenix_ci_watch` pin the SHA being watched and never infer results from another head.
- **Regression test**: `tests/release-graph.test.mjs` (pr-ci/main-ci nodes require exact-head evidence in the provenance chain).
