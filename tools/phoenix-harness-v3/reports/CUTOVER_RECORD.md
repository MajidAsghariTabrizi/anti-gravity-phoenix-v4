# Phoenix Harness V3 — Production Cutover Record

**Date**: 2026-08-23 (UTC)
**Authority**: Direct owner instruction (current session). Extended evaluation stopped by owner for cost/time; unexecuted final-stage eval slots recorded `WAIVED_BY_OWNER_FOR_COST_AND_TIME` (never pass) in `benchmarks/frontier/runs/debug-1/OWNER_WAIVER.json`.

## Final state (DSH home `C:\Users\ma.asghari\.dsh`)

| Key | Value |
|---|---|
| `phoenix` | **V3 Production** — fresh install from canonical source `tools/phoenix-harness-v3/` (sourceHash `9a73f144af591190`), cutover manifest in `.installed.json` |
| `phoenix-v2-rollback` | **V2 frozen** — byte copy of the previous V2 `phoenix` preset + `.rollback-freeze.json` (composition SHA-256 verified identical to `phoenix.bak-2026-08-23-092249`) |
| `phoenix-v3-canary` | fresh canary build (kept) |
| `phoenix.bak-2026-08-23-092249` | timestamped V2 backup |
| `settings.yaml` `agent-presets.default` | `phoenix` (unchanged — stable key) |
| `settings.yaml` model route | `deepseek-official` / `deepseek-v4-pro` (unchanged, effort max) |

## Verification performed (owner-approved scope only)

1. Provenance/git: cutover ran from branch `phoenix-v3-eval-fixes` (commit `10f8565`); protected main `2c4ccaa` untouched by the cutover (machine-state change only).
2. Composition preflight: **0 failures** — canonical + installed build pass the harness health check, strict compaction configs, plugin resolution, V2 control-row parity, documented deltas only.
3. V2 rollback existence: `phoenix-v2-rollback` byte-identical to the V2 backup (SHA-256 checked).
4. Post-cutover smoke (one tiny run): brand-new session on preset `phoenix` → boot OK, exit 0, reason completed, final "OK", model `deepseek-official/deepseek-v4-pro` effort `max`.
5. Default-persistence proof: `agent-presets.default = phoenix` (settings, unchanged) + `phoenix` boots a fresh V3 session (proven above); preset registry in the real home lists `phoenix, phoenix-v2-rollback, phoenix-v3-canary`. An explicit `--preset default` boot is not a valid preset id and fails closed at the driver (expected — the default mapping lives in settings, not the registry).

## Evidence basis for promotion

- Debug-stage A/B (15 tasks × 2 arms, real live runs, 15-min budgets): control 10 ok/5 killed vs candidate 11 ok/3 killed/1 transport record; candidate completed `release` where control could not; symmetric budget effects elsewhere.
- Final-stage partial (45-min budgets, stopped by owner): control bug-fix 3/3, safety-adversarial 3/3, wait-suspension r1 ok, rollback-recovery r0 ok — 45-min budgets unlock debug-killed tasks (budget artifact, not quality).
- Review (checker + anonymized judges, 15 verdicts): no critical candidate failures (bug-fix candidate_pass after rubric-aligned checker fix; safety-adversarial candidate_pass; pr-ci-delivery/release both_fail at 15-min budgets); quality overall not lower on judged tasks.
- Protected CI: PR #287 merged to protected main (`5e7971a3`) with all 12 checks green at exact head; exact-main CI re-run `32579796703` **12/12 green**; docs ledger PR #288 merged (`2c4ccaa`).
- `reports/gates.json` was **not** written — `gates --final` was never run (owner stop). Promotion is owner-directed on the evidence above; waived slots are recorded, never marked pass.

## Rollback

To restore V2 for new sessions: copy `phoenix-v2-rollback` back over `phoenix` (or point `agent-presets.default` at `phoenix-v2-rollback`). The frozen copy and timestamped backup are byte-identical (verified).
