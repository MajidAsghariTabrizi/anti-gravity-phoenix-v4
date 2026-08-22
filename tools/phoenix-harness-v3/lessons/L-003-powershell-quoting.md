# Lesson L-003 — PowerShell quoting failures

- **Incident**: V2 sessions spent hundreds of model rounds composing PowerShell/ssh/jq/heredoc quoting, with recurring quoting mistakes (467 pwsh calls in the main V2 build session alone; V2 telemetry corpus).
- **Root cause**: shell string construction by a token model is inherently error-prone and unverifiable; quoting is orchestration noise, not engineering work.
- **Evidence**: V2 telemetry corpus (`.phoenix-harness/telemetry/session-b642d6f2-*.jsonl` — `pwsh` 467 calls / 376K result chars); Phase 0 report `tools/phoenix-harness-v3/reports/00-phase0-forensics.md`.
- **Rule**: native tools spawn processes with ARGUMENT ARRAYS only (`execFile`); the model never composes shell quoting; all repeatable shell work is behind typed tools with allowlists.
- **Regression test**: `tests/native-tools.test.mjs` (structural: no template-built shell strings in tool sources; refusals verified per probe).
