# Lesson L-001 — llm/stream AsyncIterable contract

- **Incident**: V2 canary broke model streaming — `yield* (intermediate value) is not async iterable`.
- **Root cause**: the `llm/stream` listener was declared `async`, wrapping the returned generator in a Promise; the harness composition performs `yield* next()` on the listener return value.
- **Evidence**: `.phoenix-harness/reports/06-harness-v2-final.md`; authoritative installed examples `dsh-llm/lib/invariant.js`, `dsh-session-checkpoint-policy/lib/index.js`.
- **Rule**: `llm/stream` listeners return an `AsyncIterable` DIRECTLY — never a Promise. Non-async listener; `next()` called exactly once, synchronously, inside the returned generator; chunks re-yielded in order; downstream `return()` closed in `finally`; telemetry incapable of throwing.
- **Regression test**: `tools/phoenix-harness-v3/tests/stream-contract.test.mjs` (drives the real `apply()`; asserts `typeof result?.then === 'undefined'` and AsyncIterable, exactly-one-next, order, close-on-cancel).
