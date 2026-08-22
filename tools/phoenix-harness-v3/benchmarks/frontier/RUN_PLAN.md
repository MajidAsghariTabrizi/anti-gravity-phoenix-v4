# Frontier Evaluation Run Plan (live, automated)

This plan defines how the automated runner (Phase 2) executes the real
V2-vs-V3 A/B evaluation (Phase 4) that gates promotion. It is the contract
for `phoenix-harness-v3 eval-live`. Synthetic data can never pass promotion
(unchanged fail-closed rule).

## 1. Principles

- CONTROL = the untouched `phoenix` (V2) preset. CANDIDATE = the V3 preset
  built from the exact canonical source under test (`phoenix-v3-canary`
  rebuilt fresh from `tools/phoenix-harness-v3/` before each campaign).
- Identical model/provider/effort for both arms: the effective
  `agent-default-model` from `settings.yaml` (`deepseek-official` /
  `deepseek-v4-pro`, reasoningEffort max) — read from effective config at
  campaign start, never guessed or hardcoded.
- Identical repository starting SHA per task (the campaign's pinned HEAD).
- Identical task prompt (the committed `tasks/*.json` `prompt` field, verbatim).
- Per-run isolation: separate temp worktree per (task, preset, run), planted
  fixtures re-applied, no shared session store, no cross-run memory.
- Reviewer blinding: one anonymized judge session per (task, campaign round)
  scores OUTPUT A vs OUTPUT B (random assignment, seed recorded) against the
  rubric; deterministic checkers are used where the rubric is mechanically
  verifiable (bug-fix, pr-ci-delivery, safety-adversarial, rollback-recovery).
- Every run records: session id, wall time, telemetry path (worktree-local
  `.phoenix-harness/telemetry/session-*.jsonl`), transcript path, exit state.

## 2. Task families and tasks

Mission-required family → task id (committed definitions under `tasks/`):

| # | Family | Task id | Deterministic? | Final runs/arm |
|---|---|---|---|---|
| 1 | Codebase orientation | codebase-orientation (new) | partial | 3 |
| 2 | Precise code investigation | code-investigation | no | 3 |
| 3 | Bug reproduction + minimal fix | bug-fix | yes (checker) | 3 |
| 4 | Schema/migration work | schema-migration | no | 3 |
| 5 | PR and CI delivery | pr-ci-delivery | yes (checker) | 1 debug + 1 final |
| 6 | Release-plan reasoning | release | no | 3 |
| 7 | Production incident diagnosis | incident-recovery | no | 3 |
| 8 | Ground-Truth/business analysis | ground-truth-analysis | no | 3 |
| 9 | Cross-domain prioritization | cross-domain-prioritization | no | 3 |
| 10 | Adversarial safety/refusal | safety-adversarial | yes (checker) | 3 |
| 11 | Long-context continuation after compaction | long-context (new) | no | 3 |
| 12 | Multi-tool batch / Code Mode | code-batch (new) | partial | 3 |
| 13 | Wait/CI suspension | wait-suspension (new) | yes (checker) | 3 |
| 14 | Rollback/recovery | rollback-recovery (new) | yes (checker) | 3 |

Rationale: tasks 5 and 13 create real CI runs and real draft PRs; they are
largely deterministic, so the "three final runs" requirement is applied where
nondeterminism matters (LLM-judged tasks), while deterministic tasks get one
debug run + one final run (and bug-fix/safety/wait/rollback get 3 because
they are cheap and safety-critical). Cleanup: draft PRs and eval branches are
closed/deleted by the runner after evidence capture (never merged, never
pushed to protected main).

## 3. New task definitions (added this mission)

- `codebase-orientation`: bounded orientation over the repo (components,
  test layout, CI layout, money-path entry) using retrieval tools only;
  rubric scores component-map completeness + evidence refs.
- `long-context`: a generated corpus (deterministic seed) is ingested
  (bounded reads), then `command-compact` is exercised mid-task, then the
  session must answer questions about EARLY details; rubric scores answer
  accuracy post-compaction + no summary corruption + identifier retention.
- `code-batch`: a safe read-only multi-step workflow (git state + symbol
  lookup + bounded grep + test summary) that must be completed via batched
  operations (Code Mode or parallel native composite calls); metric targets
  recorded (>= 3 operations per model request on this task).
- `wait-suspension`: deterministic wait exercise — start a background job,
  register a wait via the native wait tool until a marker file appears,
  report duration; rubric: zero polling calls in telemetry, wake correct.
- `rollback-recovery`: in an isolated DSH_HOME, install production from a
  fixture gates file, promote, verify the pointer, rollback, verify verbatim
  restore; rubric: all checks pass, V2 preset untouched.

## 4. Campaign stages

1. `eval-live prepare` — rebuild candidate preset from canonical source,
   pin SHA, plant fixtures, create worktree template.
2. Debug stage: 1 run per task per arm (sequential). Fixes only for
   harness/runner defects, never task-favoring changes. Debug results are
   recorded but excluded from gate statistics.
3. Final stage: run counts per §2. Statistics (median) computed over final
   runs only.
4. Review stage: anonymized judge sessions + deterministic checkers emit
   proof-carrying certificates (existing evaluator.js).
5. `eval compare` + `eval gates` — synthetic rows can never pass.

## 5. Cost model (billed-equivalent, documented assumption)

billed-eq = uncached input + 0.1 × cache-read + output. Estimated campaign:
~15 tasks × 2 arms × (1 + 3) runs ≈ 120 task sessions ≈ 11M billed-eq
tokens, plus ~45 judge sessions. USD CSVs still absent on this machine
(re-verified 2026-08-22): all cost claims remain token-based percentages.

## 6. Gate mapping (reports/gates.json)

Unchanged gates; live runs supply real per-run flags:
rubricPass (judge/checker), safetyViolations, evidenceOk, resumeOk,
restartOk, rollbackOk, noopRounds, plus telemetry-derived deltas:
costIndex <= -50%, cacheRead <= -60%, modelCalls <= -40% (candidate
reduction targets), polling rounds = 0, bookkeeping rounds <= -80%.
Context gates: P95 estInputChars <= 96K (char-based surface), no overflow.

## 7. Child mechanism (verified live 2026-08-22)

Each run is one child process (`src/eval/run-one.mjs`) booting the pinned
checkout with a composed host tree, driven by a mounted driver row
(`src/eval/driver-plugin.mjs`) — the same architecture dsh-headless ships
(the driver's row-scoped context is the only place the core registries
resolve). Facts established by live verification:

- `boot()` takes a FLAT patch list (`loadOptionalPatches` shape) with insert
  rows intact; passing `composeEntries` output double-applies patches and
  yields an empty tree.
- The root config file must live INSIDE the checkout's node_modules
  (`.phx-eval/root-<pid>.cordis.yml`): its directory becomes `ctx.baseUrl`,
  which preset mounts use to resolve bare package names.
- Model selection settles asynchronously after boot (settings user layer
  lands ~1s later); the driver waits for the selection to match the
  settings section (20s cap) before creating the agent, then fails closed.
- TRANSPORT (provider-side) failures and telemetry-stalled children (4 min
  without progress) trigger attempt retries (3 attempts max) — provider
  weather is not arm quality; every attempt still records telemetry.
- Killed runs keep their worktree telemetry via `collectAllTelemetryEvidence`
  so budget kills still feed gate statistics.
- Arm isolation: per-arm `DSH_HOME` (settings/.credentials/.agent-presets,
  never sessions), child cwd = per-run worktree (plugin/telemetry roots),
  candidate rebuilt from canonical source per campaign prepare.
