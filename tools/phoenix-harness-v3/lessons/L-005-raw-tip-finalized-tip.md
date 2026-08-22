# Lesson L-005 — Raw-tip vs finalized-tip windowing

- **Incident**: candidates evaluated on raw-tip state can disappear or reorder before finality, wasting the attempt or breaking exactness.
- **Root cause**: deciding on unfinalized state without a finality evidence window.
- **Evidence**: `docs/architecture/PRELIVE_SHADOW_V2_BASELINE.md` (tip windowing in feed/engine design); invariant I-06 (dual-provider agreement on finalized block before economics).
- **Rule**: decision state must be validated against finalized tips; the chain freshness class expires at 15 minutes and any money-path decision on stale chain state fails closed.
- **Regression test**: `tests/governor.test.mjs` + `knowledge/freshness-policy.md` (chain class age enforced by phoenix_evidence staleness verdicts).
