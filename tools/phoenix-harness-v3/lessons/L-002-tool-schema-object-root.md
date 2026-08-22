# Lesson L-002 — Tool schema object-root validation

- **Incident**: fresh session's first request failed pre-model: `Invalid schema for function phoenix_checkpoint: schema must be a JSON Schema of type object, got type null. INVALID_REQUEST`.
- **Root cause**: `ctx.tools.register` validates ONLY `output.schema`; raw parameter spec maps passed through verbatim to the DeepSeek serializer, which requires the object-root wire shape produced by `defineTool()`.
- **Evidence**: `.phoenix-harness/checkpoints/harness-v2.json`; `.phoenix-harness/reports/07-post-restart-schema-repair.md`.
- **Rule**: every tool parameter map MUST be compiled to an object-root JSON Schema before registration; `required:false` is illegal at the provider boundary; every schema is preflighted against the installed harness boundary before a preset ships.
- **Regression test**: `tests/tool-schema.test.mjs` + `src/preflight/tool-schemas.mjs` (walks every registered phoenix_* tool and validates the exact adapter wire shape).
