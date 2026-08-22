# Phoenix Harness V3 — Phoenix Intelligence Operating System

Canonical source for the third-generation Phoenix-native DeepSeek Harness
configuration. The `~/.dsh/.agent-presets/phoenix-v3-*` presets are INSTALLED
BUILDS of this directory; this directory is the only source of truth.

Pinned runtime: `@deepseek-ai/dsh ^0.1.0-rc.7` (npx checkout
`1e7f6d9597241db0`), Node ≥ v24.13.0. See `VERSION`.

## Status

- V2 CONTROL: `~/.dsh/.agent-presets/phoenix` — **do not modify**. Healthy
  baseline captured in `.phoenix-harness/reports/06-harness-v2-final.md`.
- V3 CANARY: `phoenix-v3-canary` preset (installed build of this source).
  Installed at 2026-08-21, source hash `445b8d8357c7ebb6`.
- V3 PRODUCTION: `phoenix-v3-production` (promoted copy; gate-enforced).
  NOT promoted — promotion refuses until every gate in `reports/gates.json`
  passes (frontier eval runs are operator-run in separate harness sessions).
- Verified today: 43/43 unit+regression tests (incl. promote→rollback
  end-to-end: settings pointer switch + verbatim restore); preflight 20/20
  tool schemas against the installed harness boundary; composition preflight
  17/17 (harness's own parser + discovery health check on canonical and
  installed builds, strict compaction config schemas, V2-control parity
  oracle); bench: V3 compaction policy −82.4% cache-read vs legacy (−47% vs
  V2), avg surface 62.7K, peak 89.2K (targets: normal 30–70K, P95 ≤96K,
  hard ≤160K — all IN TARGET).

## One-command operations

```powershell
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs status
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs install canary --yes
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs verify
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs bench
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs eval prepare|compare|gates
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs promote --yes   # refuses unless every gate passes
node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs rollback
```

Mount-validation (inside a harness session that has `tool-cordis`, e.g. a
session on the shipped `cordis` preset): probe
`agentPresets.standingKeyFor('phoenix-v3-canary')`.

## Operator runbook: frontier eval → promotion (Phase 10 → 11)

The live frontier eval runs in SEPARATE harness sessions (a session cannot
switch its own preset). All 10 task briefs are pre-built:

```powershell
node tools/phoenix-harness-v3/src/eval/eval-runner.mjs prepare
# -> benchmarks/frontier/runs/<iso>/<task>/brief.md + manifest.json
```

1. **Mount-validate** the canary preset in a cordis session:
   `agentPresets.standingKeyFor('phoenix-v3-canary')`.
2. For each task: run it once in a session on the **V2 control** preset
   (`phoenix`) and once on **`phoenix-v3-canary`**; record session ids and
   copy each session's `.phoenix-harness/telemetry/session-*.jsonl` into the
   run directory; fill `runs.json` (per-task rubricPass/safetyViolations/
   evidenceOk/resumeOk/restartOk/rollbackOk flags + telemetry paths).
3. Re-plant the bug-fix fixture between runs:
   `Copy-Item benchmarks/frontier/fixtures/amount-math/buggy_amount.mjs.planted benchmarks/frontier/fixtures/amount-math/buggy_amount.mjs -Force`.
4. Reviewers emit proof-carrying certificates (only MissionSpec + evidence +
   results, never the transcript): `src/eval/evaluator.js` makeCertificate /
   verifyCertificate.
5. `node .../eval-runner.mjs compare runs.json` → `reports/eval-compare.json`
6. `node .../eval-runner.mjs gates` → `reports/gates.json` (synthetic rows
   can never pass — fail-closed).
7. `node .../phoenix-harness-v3.mjs promote --yes` — refuses unless every
   gate is true; on success installs `phoenix-v3-production`, backs up
   `settings.yaml` as `settings.yaml.phx-v3-bak`, and sets the default
   preset for new sessions.
8. `node .../phoenix-harness-v3.mjs rollback` restores the backed-up
   settings pointer; the V2 `phoenix` preset is never touched.

## Layout

```text
bin/phoenix-harness-v3.mjs   control CLI (install/verify/promote/rollback/bench/eval)
src/                         canonical plugin source (zero-dep ESM; installed into the preset)
  plugin.js                  preset plugin entry (tools + governor + compilers)
  schema.js                  parameter-schema compiler (harness wire shape)
  sink.js                    telemetry sink (jsonl + fingerprints)
  mission.js                 MissionSpec compiler (typed, durable)
  governor.js                round/budget governor (waits, budgets, no-op detection)
  context.js                 layered context retrieval (knowledge + domain packs)
  tools-native/*.mjs         15 typed fail-closed Phoenix tools (repo/symbol/test/remote/prod-snapshot/sql-readonly/release-verify/ci-watch/pr-flow/release-preflight/release-dispatch/ground-truth/business-funnel/opportunity-replay/evidence)
  wait.js                    deterministic wait core (suspend model, wake on state change)
  preflight/tool-schemas.mjs wire-shape preflight for every tool schema
  bench/bench-compaction.mjs compaction benchmark (V3 policy vs measured baselines)
  eval/eval-runner.mjs       frontier eval runner (prepare/compare/gates)
presets/phoenix-v3/          V3 preset composition (agent.cordis.yml, preset.yml)
knowledge/                   knowledge system (kernel, ontologies, graphs, registries, freshness)
lessons/                     incident/lesson registry (evidence + regression test each)
benchmarks/frontier/         reproducible benchmark tasks + rubrics + fixtures
reports/                     phase reports + gates.json (promotion evidence)
tests/                       unit + regression tests for every module
```

## Knowledge system (Phase 2)

`knowledge/` holds the Phoenix Intelligence knowledge base; AGENTS.md stays a
map. Dynamic/private runtime state is never committed here — freshness policy
in `knowledge/freshness-policy.md`.

## Financial authority

Building this harness never modifies Phoenix financial execution authority,
lane states, or the business runtime. Native tools are fail-closed and
read-only by default; the only mutation-capable tool (`phoenix_release_dispatch`)
requires an owner-approved current-session mission flag and invokes only the
canonical release commands.
