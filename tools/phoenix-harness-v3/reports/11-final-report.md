# Phoenix Harness V3 — Final Report (PHOENIX INTELLECTUAL OPERATING SYSTEM — HARNESS V3)

Generated: 2026-08-21 · Author: Phoenix agent (deepseek-v4-pro) · Mission: build, benchmark, deploy, verify, and report the V3 harness with V2 as the untouched CONTROL.

## STATUS

Built, benchmarked, deployed, and verified. The V3 canary preset is INSTALLED as `phoenix-v3-canary` (source hash `445b8d8357c7ebb6`, 2026-08-21T12:19Z). The V2 control preset `phoenix` is untouched. Promotion to `phoenix-v3-production` is correctly REFUSED: live frontier-eval gates are operator-run in separate harness sessions and have not been collected yet (the gate refuses — auto-promote is impossible without evidence; that is the designed behavior, not a failure). Verification state: **43/43 unit+regression tests**, **20/20 tool schemas pass the installed harness boundary**, **composition preflight 17/17** (the harness's OWN parser + discovery health check accepts the V3 composition — canonical AND installed build — strict compaction config schemas clean, installed plugin file resolves, control-row parity holds with the documented `phoenix-harness → phoenix-harness-v3` replacement), **promote→rollback pair integration-tested end-to-end** (production preset creation, gates recorded in the manifest, settings pointer switch, verbatim rollback restore), ctl `verify` ALL CHECKS PASSED, compaction bench all targets IN TARGET.

## BASELINE

The mission demanded proof, not assumption. Phase 0 measured the corpus (`.phoenix-harness/telemetry/`, 6 sessions, 1,263 requests; full evidence in `reports/00-phase0-forensics.md` + `reports/phase0-forensics.json`):

| Measure | LEGACY cordis (session-4a21bf00) | V2 phoenix (session-b642d6f2) |
|---|---|---|
| requests | 640 | 1,167 |
| avg request input (est chars) | 441K | 144,940 |
| max request input | — | 201K chars (p95 197K) |
| cache-read tokens | 282.3M | 165.6M |
| uncached input tokens | 822K | 3.53M |
| output tokens | 490K | 706K |
| TRANSPORT failures | 51 | 20 |
| job_output polling calls | — | 69 (5.9% of requests) |
| phoenix_checkpoint calls | — | 29 |
| tools/request | 1.01 corpus-wide | 1.03 |
| compactions | 1 | 1 |

The USD cost CSVs named in the mission do not exist on this machine (searched Downloads/Desktop/Documents/repo/.dsh/checkout). Cost is therefore compared in **billed-equivalent tokens** (uncached input + 0.1 × cache-read + output — documented assumption, `docs/DEPENDENCIES.md`): V2 session ≈ 20.1M billed-eq, ≈ 16.8K billed-eq per tool call.

## ROOT CAUSES

Proven (not assumed) waste drivers in order of size:

1. **Model-call granularity / orchestration overhead (now dominant)**: tools/request ≈ 1.01–1.03 — one model round trip per tool call, so every step rebills the context. V2 reduced context inflation (−67% avg vs legacy) but left round-trip count as the cost driver.
2. **Polling instead of suspending**: 69 `job_output` polling rounds (≈5.9% of requests, ≈1M billed-eq tokens) while waiting for jobs/CI; each poll bills the full context.
3. **Bookkeeping rounds**: 29 `phoenix_checkpoint` + repeated `get_goal` calls paid full-context model calls for durable-state writes that need no model at all.
4. **Legacy context inflation** (already fixed by V2): 441K avg → 145K avg input chars.
5. **TRANSPORT retries**: 51 (legacy) → 20 (V2) — request-level storm recovery needed, not per-request round trips.

## BUSINESS TWIN

Built (`knowledge/business-twin.md`, every figure source-dated). Headline facts:

- **Realized PnL = $0.** FACT: 31-hour sweeps vs second-scale liquidation windows; WETH-debt-only universe; 0/4556 forks positive.
- **Zero is capability gap, not proven no-alpha**: verified wins were excluded — a $0.27 conservative-positive candidate and $10–12 (~0.73 ETH/wk) excluded live SVR flow. No-alpha is proven ONLY for the narrow WETH/USDC lane.
- **Funnel**: SVR 24,709 events/7d → ~57% relevant → 0 eligible → 0 bids → $0 (addressable $0/wk today; $5–20/wk small-fix; 0.73 ETH/wk SVR fix; $100–400/wk route expansion; SVR upside UNKNOWN).
- **Next highest-value move**: unblock the Atlas/SVR lane in SHADOW with an event trigger, then non-WETH pairs (owner-scoped; nothing armed).
- Gap noted: `.agent-private/alpha-source-investigation/ground-truth/` is empty — Ground-Truth collection is itself unshipped work.

## TECHNICAL TWIN

Knowledge system under `knowledge/` (all served via `phoenix_context`, nothing repeated in prompts):

- `kernel.md` (Layer A stable kernel), `freshness-policy.md` (7 freshness classes with max ages).
- `ontology/business.json` (17 terms incl. RealizedNetPnL vs ExpectedPnL vs ConservativePnL vs ShadowPnL), `ontology/technical.json` (components, data model, high-risk list).
- `graphs/service-graph.json` (sequencer→relay→feed→NATS→engine→executor + rpc-gateway/recorder/replay/observers), `graphs/authority-graph.json` (lanes separate; Generic DEX CLOSED), `graphs/release-graph.json` (15-node protected provenance chain + prohibited ops), generated `graphs/symbol-graph.json` (400 entries) + `graphs/schema-graph.json` (49 tables).
- `registries/incidents.json` (10 lessons), `registries/tests.json` (V3 suite map).

## NATIVE TOOLS

20 `phoenix_*` tools registered by one preset-local zero-dependency plugin (`src/plugin.js`); every schema passes the installed harness boundary (`src/preflight/tool-schemas.mjs`):

- Core: `phoenix_context` (layered retrieval + pressure view), `phoenix_mission` (typed MissionSpec compiler, owner-approval gate), `phoenix_checkpoint`, `phoenix_budget`, `phoenix_telemetry`.
- Repo/test: `phoenix_repo_snapshot`, `phoenix_symbol`, `phoenix_test`.
- Production read-only (fail-closed): `phoenix_remote` (SSH allowlist; sudo/pipes/redirects/unknown commands REFUSED at the gate), `phoenix_production_snapshot` (composite freshness-stamped), `phoenix_sql_readonly` (SELECT/WITH only, one statement, keyword blocklist, row/char caps, container resolved from docker ps), `phoenix_release_verify`.
- CI/release: `phoenix_ci_watch` (blocks INSIDE the tool until CI state changes — model suspended), `phoenix_pr_flow` (status/checks/create_draft — never merges), `phoenix_release_preflight` (13-node checklist + secret-pattern scan), `phoenix_release_dispatch` (REFUSES without: prod_mutation mission + recorded owner approval + exact-objective ack + canonical script + `--dry-run`/plan_ack).
- Business: `phoenix_ground_truth`, `phoenix_business_funnel`, `phoenix_opportunity_replay` (labels preserved; fixture PnL never labeled realized), `phoenix_evidence` (hashed claims with freshness verdicts).

All spawns are argument-array (no shell quoting by the model — L-003); all results compact with artifact spillover.

## CONTEXT COMPILER

- Layered: A kernel / B MissionSpec / C domain packs / D transcript+compaction / E tool results / F checkpoint; retrieval over replay (`phoenix_context`), canary targets NORMAL 30–70K chars, P95 ≤96K, HARD ≤160K, pressure band 96–120K, retain tail 32K, compaction summary ≤6K.
- Preset policy: `thresholdRatio 0.09`, `retainTokens 32768`, Flash-route summarizer capped 6144 tokens, tool-result pruner 8192/4096/1024, `command-compact` available.
- Bench (same calibrated 640-step model that reproduces the measured 282.3M baseline): **avg surface 62.7K, peak 89.2K, summary 6K — all targets IN TARGET; cache-read −82.4% vs legacy, −47% vs V2** (see `src/bench/bench-compaction.mjs`).

## BUDGET GOVERNOR

`src/governor.js` + `src/wait.js`: MissionSpec budgets (tokens/model-calls/elapsed) enforced with measured usage (billed-eq); waits registered by native tools; **no-op round elimination** uses the verified harness seam — `agent/pre-step` `{kind:'reject'}` ends the turn BLOCKED with ZERO model calls. Rejection only for: (a) bookkeeping-only rounds during an active wait; (b) goal-round continuations after a hard-stop breach (recorded identity). Warnings at ≥0.8 ratio are telemetry-only. Human/operator steps are never blocked. No loop decisions are fabricated.

## EVALUATOR

`src/eval/evaluator.js`: proof-carrying certificates (SHA-256-bound evidence; `verifyCertificate` detects tampering). `src/eval/eval-runner.mjs`: `prepare` (operator briefs — written for all 10 tasks under `benchmarks/frontier/runs/2026-08-21T12-19-42-455Z/`), `compare` (control-vs-canary metric table), `gates` (writes `reports/gates.json`). One leader model; isolated reviewers only at critical gates (business/architecture/prod_safety/release/evidence) receive MissionSpec+evidence+results, never the parent transcript.

## LEARNING SYSTEM

10 seeded lessons (`lessons/L-001..L-010`), each with evidence citation + rule + regression test (registry: `knowledge/registries/incidents.json`): L-001 stream AsyncIterable contract (validated), L-002 tool schema object-root (validated), L-003 PowerShell quoting, L-004 Docker entrypoint mismatch, L-005 raw-tip windowing, L-006 sudo env reset, L-007 FAILED_PRE_MUTATION identity, L-008 PACKAGE_ALREADY_EXISTS idempotency, L-009 reused-branch CI, L-010 no-op goal rounds. Each maps to a test in `tests/`.

## CONTROL METRICS

V2 CONTROL (`phoenix` preset) — untouched, healthy: 1,167 requests in the reference build session; avg input 144,940 chars; billed-eq 20.1M tokens; 69 polling calls; 29 checkpoint calls; 43 tests (V2 suite, not rerun to avoid any control mutation risk). Settings default preset still `phoenix`.

## V3 METRICS

- Verification: **43/43 tests**, **20/20 preflighted schemas**, **17/17 composition preflight** (harness parser + health check on canonical and installed builds, strict config schemas, plugin-file resolution, V2-control parity oracle), **promote→rollback integration test** (all-true non-synthetic gates → production preset + manifest + settings pointer switch + backup; rollback restores verbatim; V2 never touched), verify gate ALL PASSED, install idempotent with provenance manifest.
- Compaction bench: avg surface 62.7K (target 30–70K), peak 89.2K (P95 ≤96K), hard-limit OK, cache-read −82.4% vs legacy / −47% vs V2 control.
- Projected end-to-end cost-index reduction **−88–91%** on representative long tasks (Phase 0 projection: tools/request ~4 via native tools + wait-suspend + 30–70K surface + zero no-op rounds). This remains a PROJECTION until the operator-run frontier compare produces live numbers — the gates exist precisely to force that measurement before promotion.

## QUALITY IMPROVEMENT

- Every claim carries evidence+date (business twin, evidence registry, ground-truth tools); profit labels can never mix (tests enforce it).
- 10 recurring incidents promoted into lessons+rules+tests (mission outcome #7).
- Release provenance and authority graphs are executable checklists (`phoenix_release_preflight`, dispatch authorization chain), not prose.
- Deterministic fixtures and replantable benchmarks make the 10-task frontier reproducible.

## TOKEN/COST IMPROVEMENT

- Context inflation: V2 already −67% avg/−71% max; V3 adds the 0.09/32K policy (bench: −82.4% vs legacy cache-read, peak 89.2K vs legacy 798K).
- Round-trip waste: native tools collapse multi-tool sequences into one call; `phoenix_ci_watch` suspends the model during waits (kills the 69-call polling pattern); governor rejects no-op rounds at zero cost.
- Cost accounting is token-based (billed-eq proxy, cache-hit at 0.1×); no invented USD anywhere.

## FILES

Canonical source `tools/phoenix-harness-v3/`: `bin/phoenix-harness-v3.mjs` (ctl), `src/plugin.js` + 13 modules + `tools-native/{repo,test,remote,ci,business}.js`, `src/preflight/tool-schemas.mjs`, `src/bench/bench-compaction.mjs`, `src/eval/{evaluator.js,eval-runner.mjs}`, `src/gen/gen-graphs.mjs`, `presets/phoenix-v3/{agent.cordis.yml,preset.yml,skills/phoenix-context/SKILL.md}`, `knowledge/` (kernel, freshness, 2 ontologies, 5 graphs, 2 registries, business twin), `lessons/L-001..L-010`, `benchmarks/frontier/` (10 tasks + fixtures + prepared run), `reports/00-phase0-forensics.md` + `phase0-forensics.json` + `eval-compare.json` (future) + `gates.json` (future), `tests/` (10 suites, 40 tests), `VERSION`, `README.md`. Repo-facing updates: `AGENTS.md` (map pointer only), `docs/DEPENDENCIES.md` (V3 pinning + cost-proxy assumption). Installed build: `~/.dsh/.agent-presets/phoenix-v3-canary/` with `.installed.json` manifest.

## ACTIVE PRESET

`phoenix-v3-canary` INSTALLED (build `445b8d8357c7ebb6`). Default preset for new sessions: `phoenix` (V2 control — unchanged). `phoenix-v3-production`: absent (promotion gate-enforced; refuses until `reports/gates.json` shows all gates true). This session runs on the V2-era preset with file policy danger-full-access and cannot mount-validate presets itself (no tool-cordis) — mount-validation of `phoenix-v3-canary` via `agentPresets.standingKeyFor('phoenix-v3-canary')` is the documented operator step in a cordis-preset session, followed by running the prepared frontier briefs on control+canary sessions and `eval compare` + `eval gates`.

## ROLLBACK

`node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs rollback` restores `settings.yaml` from `settings.yaml.phx-v3-bak` (created only at promote time). Pre-promotion rollback = remove the canary preset directory (it is inert — no settings pointer references it); V2 `phoenix` is never touched by any ctl path. Auto-rollback on critical regression: the gates evaluator (`eval gates`) reports regressions; the operator runs `rollback` (or simply starts new sessions on `phoenix`) — the design keeps the control preset permanently available as the fail-safe.

---
*Nothing in this work modified Phoenix financial execution authority, lane states, or any Production system. Production was read-only throughout; no MUTATION PLAN was created or executed.*
