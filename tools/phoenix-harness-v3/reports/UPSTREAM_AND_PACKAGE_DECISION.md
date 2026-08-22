# UPSTREAM_AND_PACKAGE_DECISION.md

Phase 1 artifact of the V3 production-promotion mission.
Generated: 2026-08-22. Research: fresh web + npm registry + GitHub + local
pinned-checkout inspection (three independent research passes, read-only).

## A. Official DeepSeek Harness — compatibility matrix

Pinned: `@deepseek-ai/dsh` **0.1.0-rc.7**, npx checkout `1e7f6d9597241db0`.
Upstream today: repo `deepseek-ai/deepseek-harness` (public, branch `master`),
HEAD `b150a551b8d4` = tag **`dsh-v0.1.1-rc.2`** (2026-08-21). npm `latest` =
`next` = `0.1.1-rc.2`. Releases since our pin: rc.8 (8-19), 0.1.1-rc.1,
0.1.1-rc.2 (8-21).

| Surface | rc.7 (pinned) | current upstream | Compatible? |
|---|---|---|---|
| Preset composition schema (agent.cordis.yml rows) | as used | unchanged | YES |
| Plugin API (ctx injection, tools registration) | as used | unchanged | YES |
| Tool wire shape (object-root params) | as used | unchanged | YES |
| Compaction config keys (thresholdRatio/retainTokens/maxTokens/…) | as used | unchanged | YES |
| `ctx.agents` AgentRegistry (create/resume) | present | unchanged | YES |
| Headless runner (`dsh --profile headless`) | present | unchanged | YES |
| settings.yaml keys | as used | unchanged | YES |
| Session storage format | rc.7 SQLite format | **CHANGED in rc.8 (incompatible)** | **NO** |

**DECISION: STAY PINNED on 0.1.0-rc.7.** The only breaking change (SQLite
storage format, rc.8) buys us nothing (we run files-only telemetry sinks and
do not rely on the session database), and the upstream ships breaking preview
releases daily. Upgrade path is deferred until a measured rc.7 problem needs
a specific upstream fix; rollback remains available at all times.
Upstream note: Node 24 is documented as a failure point upstream; on THIS
machine Node v24.13.0 runs the harness + both presets without issue (this
session + the prior canary mount). The pin stays; Node is not changed
mid-mission.

### Built-in capabilities we use (no external substitute)

- AgentFactory / `ctx.agents` create+resume with per-agent preset mounts
  (different presets in one process: supported) — the eval runner backbone.
- `agent/pre-step` rejection seam ({kind:'reject'} zero-cost blocked turn),
  `agent/request-error` retry waterfall (provider retry policy).
- `dsh-goal-round-driver` (goal/changed + idle auto-trigger, maxGoalRounds).
- **Code Mode: `dsh-code-runtime-worker-thread`** (worker_threads, lossless
  JSON bindings, structured results, per-preset `tool-presentation {mode:code}`)
  — exists in rc.7; we enable it for safe read-only bindings (Phase 3F).
- `dsh-compaction-basic` + `dsh-token-meter` (4-chars/token heuristic) +
  `dsh-output-retention` + `dsh-compaction-tool-result-pruner`
  (8192/4096/1024) — all already configured in the V3 preset.
- Tool concurrency: `defineTool` object-root params, `isConcurrencySafe`,
  executionMode parallel/exclusive, maxParallelToolCalls 10.
- Headless profile (`dsh --profile headless`) — limited (no preset/session/
  JSON flags); used only as a fallback driver if in-process agents cannot
  isolate telemetry (see eval runner design).

## B. Community ecosystem — evaluated, adopted: NONE

Rule applied: adopt only what clearly pays for itself against the measured
waste (model round-trips) without supply-chain cost; prefer official
harness built-ins. Result: **zero packages adopted** — V3 stays a
zero-external-dependency ESM plugin.

| Candidate | Verdict | Why |
|---|---|---|
| MiniSearch 7.2.0 (lexical index) | REJECT | V3 knowledge graphs + `phoenix_symbol` + bounded grep already cover exact/fuzzy symbol retrieval with zero deps; fuzzy ranking does not move the measured cost driver |
| tree-sitter + grammars | REJECT | Native addon + MSVC toolchain on Windows; harness grep tooling already covers line-level search |
| ripgrep-as-library | REJECT | Harness ships rg-based grep; no library needed |
| node:sqlite FTS5 (Node 24) | NOT ADOPTED NOW | Verified working on this runtime (SQLite 3.50.4, ExperimentalWarning); no persistence requirement today — future option for a persistent index |
| @huggingface/tokenizers 0.1.3 (pure JS) | REJECT (conditional future) | Only route to an exact offline DeepSeek BPE (tiktoken/gpt-tokenizer are OpenAI-vocab; `deepseek-tokenizer` does not exist). Not required: gates use provider-reported usage, not offline counts |
| LLMLingua / LongLLMLingua | REJECT | Python+torch, stale; semantic compression stays out of Production unless quality gates demand it |
| prom-client | REJECT | Harness bundles session telemetry (+ OTel revival path); our JSONL sink covers the required metrics |
| OpenTelemetry JS SDK | REJECT | Redundant + a current (patched ≥0.217.0) HIGH advisory (CVE-2026-44902 / GHSA-q7rr-3cgh-j5r3, Prometheus exporter DoS) — avoid entirely |
| p-limit / p-retry / Bottleneck | REJECT | Harness provides retry policies + parallel tool execution; Bottleneck stale since 2024 |

Security posture: zero added dependencies ⇒ zero added transitive supply
chain, zero native binaries, Windows/Linux/Node-24 portable, no lockfile
churn. Every gate metric is computed from provider-reported token usage
recorded in our own telemetry sink.

## C. Compatibility commitments

- Harness: pinned rc.7 + checkout `1e7f6d9597241db0` (recorded in VERSION,
  README, docs/DEPENDENCIES.md, preset manifest).
- Node: ≥ v24.13.0 (machine-verified); CI runs the V3 suite on the runner's
  default Node with a DSH_HOME-free temp root (43/43 verified).
- Any future upgrade requires: a measured rc.7 problem fixed upstream, full
  V3 suite pass, composition preflight pass, real quality gates pass, and
  the V2 rollback preset available.

## D. Phase 1 conclusion

UPSTREAM_AND_PACKAGE_DECISION: PIN rc.7; ADOPT NO PACKAGES. The
capability seams we need (AgentFactory, Code Mode, rejection seam, retry
waterfall, compaction/pruner, headless fallback) all exist in the pinned
checkout and are used as-is.
