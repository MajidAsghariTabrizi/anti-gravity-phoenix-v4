# V3_SOURCE_INTEGRITY_REPORT

Phase 0 artifact of the V3 production-promotion mission.
Generated: 2026-08-22 (new clean session). All facts re-measured this session.

## 1. Protected baseline

| Item | Value |
|---|---|
| Remote | `https://github.com/MajidAsghariTabrizi/anti-gravity-phoenix-v4.git` |
| Protected main (fetched this session) | `f9ff8c95c37dfb56e789a2466cce1d3ea867f3c5` (Merge PR #286) |
| Local main before refresh | `9372bbf` (behind origin by 59) |
| Prior session branch | `fix/economic-dashboard-snapshot-timeout` @ `f3f73e7` (untouched) |
| Dedicated mission branch | `phoenix-v3-source-eval`, created from `origin/main` f9ff8c95 |
| Working-tree status at branch creation | `M AGENTS.md`, `M docs/DEPENDENCIES.md`, `?? tools/` (entire V3 source untracked) |

FACT: `git ls-tree origin/main -- tools/` is empty — **the protected main contains no
Phoenix Harness V3 tree**. The complete reviewed V3 source existed only as an
untracked working-tree directory. First action of this mission was to preserve it
by branching from protected main and committing it (see §6).

## 2. Four-way comparison

| Artifact | State | Verdict |
|---|---|---|
| Canonical V3 source `tools/phoenix-harness-v3/` | 84 files, tree hash `153a93686031551872358a39721a9fc42f13901103e322f1ace1c2c281f510d9` (per-file SHA-256 manifest: `reports/source-manifest.json`, regenerable via `src/preflight/source-manifest.mjs`) | Source of truth |
| Installed `phoenix-v3-canary` build (`~/.dsh/.agent-presets/phoenix-v3-canary/`) | 23 of 24 `src/` files byte-identical (SHA-256) to canonical; **missing `src/preflight/validate-composition.mjs`**; `.installed.json` sourceHash `da782acdfe260476` | STALE build — predates the final reviewed source (composition validator + 43/43 suite additions). Superseded; will be rebuilt from canonical source at promotion, never used as production input |
| V2 `phoenix` control preset (`~/.dsh/.agent-presets/phoenix/`) | 21 files captured with SHA-256 in §4; no `.installed.json` (predates manifest convention) | Untouched control; preserved verbatim for `phoenix-v2-rollback` at cutover |
| Protected main `origin/main` | No `tools/` tree | V3 is a pure addition, zero risk of overwriting tracked history |

README provenance note: `README.md` cites canary source hash `445b8d8357c7ebb6`
(final mission source state) while the installed manifest records
`da782acdfe260476` (install-time state). Both are historical; the canonical tree
hash above is the authoritative current identity.

## 3. Ignore-rule and provenance findings

1. `.agent-private/`, `.phoenix-harness/`, `AGENTS.local.md` are protected only by
   LOCAL `.git/info/exclude` entries — these do not travel with commits. This is
   correct for `.agent-private/` (machine-local by design) but unsafe for V3
   runtime artifacts on other machines.
2. FINDING: `benchmarks/frontier/runs/smoke-synthetic/telemetry/*.jsonl` (465 KB
   of synthetic session data) and the prepared run dir were VISIBLE to Git.
3. FIXED this session: committed `tools/phoenix-harness-v3/.gitignore` ignoring
   `.phoenix-harness/` and `benchmarks/frontier/runs/`. Run evidence and runtime
   artifacts can never ride a commit. Reproducible task definitions
   (`tasks/`, `fixtures/`) remain committed.

## 4. V2 control preset provenance (rollback input)

`~/.dsh/.agent-presets/phoenix/` — 21 files, captured 2026-08-22:

| File | SHA-256 (16) |
|---|---|
| agent.cordis.yml | C0ED28DB1886D9F3 |
| preset.yml | F68CC5CA8D953CB1 |
| plugins/dsh-phoenix-harness/lib/index.js | 18CB6771F182519A |
| plugins/dsh-phoenix-harness/lib/{checkpoint,context,guard,schema,sink,telemetry-view,tier}.js | 8AB4DB13/5990A36D/332A2FD6/511AEE25/2B279448/7E8017BD/95F4ADE4 |
| plugins/dsh-phoenix-harness/package.json | B8C9DD790AD74ED9 |
| plugins/dsh-phoenix-harness/test/* (7 files) | captured in cutover snapshot |
| skills/cordis-plugin-development/SKILL.md | 01811D3EE9C03A46 |
| skills/editing-cordis-compositions/SKILL.md | 3A9632BC8FAE5EFE |
| skills/phoenix-context/SKILL.md | 2249B1CDBBDC2746 |

Full manifest + settings backup will be frozen at cutover time (§Phase 6).

## 5. Security scans (this session)

- Secret-pattern scan of all committed-eligible V3 source (sk-*, AKIA*, PEM
  headers, ghp_*, api_key=, password=): **SECRET_SCAN_CLEAN**.
- Raw session histories, credentials, `.agent-private`, DSH credentials,
  machine-local telemetry: all excluded from the commit by the new `.gitignore`
  and pre-existing exclude rules. Verified against the final staged file list.

## 6. First preservation commit

`git add -A` dry-run = 85 files: 83 under `tools/phoenix-harness-v3/` (no
`.phoenix-harness/`, no `runs/`, no telemetry) + `AGENTS.md` (map pointer) +
`docs/DEPENDENCIES.md` (V3 pinning + cost proxy). Committed on
`phoenix-v3-source-eval` as the PR-1 base. `git diff --check` clean.

## 7. Baseline verification (re-run this session)

- `node --test tools/phoenix-harness-v3/tests/*.test.mjs` → **43/43 PASS**.
- `node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs verify` →
  **ALL CHECKS PASSED** (incl. tool-schema preflight and composition preflight
  against the installed harness boundary).
- `reports/gates.json`: synthetic smoke state, all 9 gates false — fail-closed,
  cannot unlock promotion (by design).

## 8. Phase 0 conclusion

V3_SOURCE_INTEGRITY: PASS. Source is now durably committed on a dedicated
branch off protected main, with regenerable checksum provenance, clean secret
scan, and committed ignore rules. Next: Phase 1 upstream/package research,
then Phase 2 automated live evaluation runner.
