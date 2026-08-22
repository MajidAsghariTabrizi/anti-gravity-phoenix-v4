# PHOENIX HARNESS V3 — PHASE 0 FORENSICS REPORT

Captured 2026-08-21. Evidence: machine-local telemetry only — the mission's
named inputs `session(6).jsonl`, `session(7).jsonl`,
`amount-2026-07-23_2026-08-21.csv`, `cost-2026-07-23_2026-08-21.csv` **do not
exist on this machine** (exhaustive search: Downloads, Desktop, Documents,
repo, `~/.dsh`, npx checkout). Machine-local equivalents were used and are
cited per claim:

| Named input | Machine-local equivalent used |
|---|---|
| session(6).jsonl (legacy cordis baseline) | `.phoenix-harness/telemetry/baseline/session-4a21bf00.json` + `-metrics.json` (captured 2026-08-20, pre-change) |
| session(7).jsonl (Phoenix V2 baseline) | live V2 telemetry corpus `.phoenix-harness/telemetry/session-*.jsonl` (6 files, 1263 requests) |
| amount/cost CSVs | unavailable → token-based cost proxies (documented assumption, §4) |

## 1. Quantified corpus

### 1.1 LEGACY CORDIS CONTROL (long autonomous session 4a21bf00)

```text
model requests           ≈ 640 (+51 TRANSPORT retries, 8% of requests)
average input/request    ≈ 441,000 tokens (cache-read dominated)
max input/request        ≈ ~700K
total input              821,751 uncached + 282,314,880 cache-read
output (incl. reasoning) 490,656 tokens
compactions              1 (default policy: 0.8 threshold × 1M window)
tools/request            ≈ 1.0 (heavy read/pwsh/grep single-call rounds)
```

### 1.2 PHOENIX V2 CONTROL (main build session b642d6f2, 24 turns)

```text
model requests           1167
avg request input        144,940 tokens (median 147K, p95 197K, max 201K)
total input              3,525,583 uncached + 165,619,072 cache-read
output                   705,555 (+ reasoning 300,583)
failures                 20 (all TRANSPORT)
finishes                 tool-calls 1111 / stop 34 / error 20 / max-tokens 2
tools per request        1.03
tool-result volume       2.02M chars / 1197 results (max 50K = spill cap)
job_output polling       69 calls (model rounds spent waiting)
```

Corpus-wide (6 sessions): 1263 requests, 1274 tool results, tools/request 1.01.

## 2. Hypothesis verification — BOTH HALVES CONFIRMED

**H1: context inflation is materially improved — TRUE.**

| Metric | Legacy cordis | Phoenix V2 | Δ |
|---|---|---|---|
| avg request input | 441K | 145K | **−67%** |
| max request input | ~700K | 201K | **−71%** |
| compactions/640 steps | 1 (late) | proactive @20% + 64K tail | policy-level |

**H2: model-call granularity and orchestration are now the dominant waste — TRUE.**

- **1.03 tool calls per model request**: every tool call costs a full model
  round, billing the whole ~145K-token prompt. Per-tool-call billed cost =
  20.1M billed-eq / 1197 results ≈ **16.8K billed-tokens per tool call**.
- **69 polling rounds** (`job_output`) — 5.9% of all model requests spent
  waiting with zero work, each billing the full context.
- **34 `stop` rounds** with no tool call — pure reasoning turns billed at
  full context each.
- **Retry replay** remains proportional to surface (20 TRANSPORT failures);
  bounded by policy but still billed.
- Repetition of identical tool calls measured **0** in V2 telemetry —
  *measurement limitation*: V2 `tool.result` records omit arguments, so
  fingerprints cannot be computed. V3 telemetry includes argument
  fingerprints (see Phase 6 governor).
- No-op goal rounds: absent from the corpus (goal tooling saw little use in
  the V2 build session) but the failure mode is structural — see Phase 6.

## 3. Root causes (ranked, V2 state)

1. **One model round per tool call** — 1.03 tools/request wastes a full
   prompt billing per tool call; parallel batching and deterministic
   workflows can raise this to ~4.
2. **Orchestration rounds** — polling/waiting burns model rounds that
   produce zero artifacts (69 polls ≈ 5.9% of requests ≈ 1M billed-eq).
3. **145K average working surface** — improved from 441K but still far
   above the 30–70K the layered compiler targets; each round multiplies it.
4. **Uniform max-effort reasoning** — 1137/1167 steps `standard`/`critical`
   all at max effort; mechanical steps overpay on output.
5. **No argument-level loop evidence** — telemetry cannot prove repeat
   waste (limitation, fixed in V3).

## 4. Cost accounting method (documented assumptions)

The mission's USD CSVs are absent. All cost work uses token-based proxies:

- `billed-input-equivalent = uncached input + 0.1 × cache-read`
  (assumption: cache-hit billed at 0.1× input price, DeepSeek-style).
- `cost-index = billed-input-equivalent + output` (assumption: output at
  input price).

| Session | billed-eq | cost-index |
|---|---|---|
| legacy cordis (640 req) | 29.1M | 29.5M |
| Phoenix V2 main (1167 req) | 20.1M | 20.8M |
| Phoenix V2 per tool call | 16.8K | 17.4K |

**V3 projection basis** (H): tools/request → ~4 (parallel batches), waits
suspend the model, working surface → 30–70K ⇒ ~350 rounds × ~5K billed-eq
≈ 1.8M cost-index — an **~88–91% cost-index reduction** on comparable
long tasks. This is the design target, not a claim yet; Phase 10 measures.

## 5. Bottleneck proof

**PROVEN: model-call granularity and orchestration are the dominant waste.**
Context inflation was the dominant waste in legacy cordis (441K avg input)
and is materially improved in V2 (−67%). What remains is: every tool call
billed at full context + waiting rounds + stop rounds. V3's levers are the
mission/context compilers (Phases 3–4), deterministic wait tools (Phase 5),
and the round/budget governor (Phase 6).

Raw data: `.phoenix-harness/telemetry/`, `tools/phoenix-harness-v3/reports/phase0-forensics.json`.
Analyzer: `tools/phoenix-harness-v3/src/forensics/analyze-telemetry.mjs`.
