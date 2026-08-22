# Phoenix Context Retrieval (phoenix-context, V3)

Phoenix Harness V3 keeps knowledge OUT of the model prompt and serves it on
demand: layered context, retrieval over replay. Load this skill when starting
Phoenix engineering or operations work, after resuming a session, or before
touching a new domain.

## When to use the retrieval tools

- `phoenix_context` — at mission start load the kernel
  (`action=load file=knowledge/kernel.md`) and the invariant registry
  (`action=invariants`); before deep work in a domain load the domain pack
  (`action=load file=domains/<id>.md`); to find where something lives use
  `action=search`; at phase boundaries run `action=budget` to check context
  pressure against targets (normal 30-70K, P95<=96K, hard<=160K).
- `phoenix_mission` — `get` at mission start and after resume; `update
  phase=<name>` at every phase boundary. The MissionSpec is the single
  mission source — never paste it into prompts.
- `phoenix_checkpoint` — `get` after resume; `update` at decision points.
  The checkpoint is the durable progress record (Layer F); the transcript
  is NOT the progress record.
- `phoenix_budget` / `phoenix_telemetry` — measured usage vs MissionSpec
  budgets and token/retry/loop behavior. Check at phase boundaries.

## Artifacts

| Path | Purpose |
|---|---|
| .phoenix-harness/*.json | context maps (architecture, domain, test, operations, invariants, repo inventory) |
| .phoenix-harness/domains/*.md | 13 compact domain packs |
| .phoenix-harness/checkpoints/ | per-session checkpoints + MissionSpecs |
| .phoenix-harness/telemetry/ | per-session JSONL telemetry (evidence, not context) |
| tools/phoenix-harness-v3/knowledge/kernel.md | stable kernel (Layer A) |
| tools/phoenix-harness-v3/knowledge/business-twin.md | business twin facts (every figure sourced + as-of dated) |
| tools/phoenix-harness-v3/knowledge/freshness-policy.md | staleness classes and max ages |
| tools/phoenix-harness-v3/knowledge/ontology/*.json | business/technical ontologies |
| tools/phoenix-harness-v3/knowledge/graphs/*.json | service/symbol/schema/authority/release graphs |
| tools/phoenix-harness-v3/knowledge/registries/*.json | invariants/tests/incident-lesson registries |

## Rules

- Load a domain pack instead of re-reading many files; then read only the
  ranges you need (locate symbol → focused read).
- Keep checkpoints compact and factual. Stale hypotheses and raw output do
  not belong there.
- Telemetry files are evidence, not context: summarize with
  `phoenix_telemetry` instead of reading the JSONL directly.
- Regenerate the inventory when the repository layout changes:
  `node .phoenix-harness/tools/gen-context-map.mjs`
- V3 forensics (Phase 0 corpus):
  `node tools/phoenix-harness-v3/src/forensics/analyze-telemetry.mjs <telemetryDir>`
- Compaction benchmark (V3 policy):
  `node tools/phoenix-harness-v3/src/bench/bench-compaction.mjs`
