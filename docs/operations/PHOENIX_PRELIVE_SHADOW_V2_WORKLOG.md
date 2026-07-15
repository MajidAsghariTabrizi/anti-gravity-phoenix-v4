# Phoenix Pre-Live SHADOW v2 Worklog

## Phase 0

- phase: Phase 0 - Repository, release, and architecture truth; immutable starting SHA `4cf4375452bffd9b3e10b635ab687353dec6cab8`
- branch: `chore/prelive-baseline-audit`
- commit SHA: `956bfb13af9db06f64041a0dc968afb64077b0c2`
- files changed: `docs/architecture/PRELIVE_SHADOW_V2_BASELINE.md`
- tests run: `git diff --check`; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `sh -n scripts/*.sh`; `sh ./scripts/shadow-engine-canary-control-tests.sh`; `sh ./scripts/shadow-engine-isolated-canary-tests.sh`; `sh ./scripts/shadow-positive-route-evidence-tests.sh`
- test result: PASS - all available Phase 0 gates completed successfully
- known blocker: repository production intent uses `/opt/phoenix/deploy` while supplied runtime evidence reports `/opt/phoenix/app`; no route-registry hash exists; deploy and rollback can recreate protected services
- next gate: Phase 1 must prove one production renderer, release context, route hash, immutable image contract, generated-file hygiene, and protected-service-safe operations

## Phase 1

- phase: Phase 1 - Production Compose, release, and route-registry truth
- branch: `fix/production-compose-route-registry-truth`
- commit SHA: `f244ccecbd2dde33d7ea470d112a78e6feb45304` (implementation), `c25333eb4950649dd53bb1e5a3eaf22e9988e4ea` (operations documentation)
- files changed: `.env.example`; `.github/workflows/ci.yml`; `.gitignore`; `compose.prod.yml`; `docs/DEPENDENCIES.md`; `docs/RELEASE_AND_ROLLBACK.md`; `docs/SHADOW_POSITIVE_ROUTE_EVIDENCE.md`; `scripts/bootstrap-production.sh`; `scripts/deploy-release.sh`; `scripts/forbidden-file-check.ps1`; `scripts/forbidden-file-check.sh`; `scripts/production-compose-context-tests.sh`; `scripts/production_context.py`; `scripts/render-production-compose.sh`; `scripts/rollback-release.sh`; `scripts/shadow-positive-route-evidence-tests.sh`; `scripts/shadow-positive-route-evidence.sh`; `scripts/validate-production-env-tests.sh`; `scripts/validate-production-env.sh`; `scripts/validate-production-release-context.sh`
- tests run: `python -m py_compile scripts/production_context.py`; `sh -n scripts/*.sh`; `sh ./scripts/production-compose-context-tests.sh`; `sh ./scripts/validate-production-env-tests.sh`; `sh ./scripts/shadow-engine-canary-control-tests.sh`; `sh ./scripts/shadow-engine-isolated-canary-tests.sh`; `sh ./scripts/shadow-positive-route-evidence-tests.sh`; PowerShell and POSIX secret scans; PowerShell and POSIX forbidden-file scans; `git diff --check`
- test result: PASS - all locally available deterministic Phase 1 gates completed successfully; no runtime service was started
- known blocker: Docker is unavailable locally, so Docker-backed Compose route preservation and the complete Linux CI matrix must run in GitHub Actions
- next gate: push the exact branch tip, require every `ci.yml` job to pass, then merge Phase 1 into the cumulative integration branch without deploying
