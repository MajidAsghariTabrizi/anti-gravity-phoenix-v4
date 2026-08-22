# Lesson L-007 — FAILED_PRE_MUTATION retry identity

- **Incident**: a failed pre-mutation check left the active state ambiguous; retrying without identity would have re-entered a half-applied mutation.
- **Root cause**: retries without recorded failure identity can double-apply or re-enter an uncertain state.
- **Evidence**: `docs/emergency-pause-and-rollback.md:8`; `docs/release-controller-architecture.md:38` (FAILED_PRE_MUTATION leaves the active state intact; one serialized controller).
- **Rule**: a failed pre-mutation check records identity and leaves state intact; retry re-validates from scratch; uncertain outcomes are NEVER blind-retried (invariant I-11). The V3 governor records `governor.stop` identity and blocks continuation after breach.
- **Regression test**: `tests/governor.test.mjs` (hard-stop rejection records identity; human steps remain possible).
