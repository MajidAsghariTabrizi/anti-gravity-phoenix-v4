# Git Flow

Phoenix uses GitHub Flow / trunk-based development.

`main` is the only deployable branch. A merge to `main` means deployable SHADOW production only. It never means LIVE trading, and it must never automatically set `LIVE_EXECUTION` to true.

## Branches

Use short-lived branches:

- `feat/*`
- `fix/*`
- `perf/*`
- `infra/*`
- `security/*`
- `refactor/*`
- `test/*`
- `docs/*`

Do not create a permanent `develop` branch.

## Commits

Use Conventional Commit style:

- `feat(feed):`
- `feat(engine):`
- `fix(state):`
- `perf(optimizer):`
- `infra(ci):`
- `infra(images):`
- `infra(deploy):`
- `infra(prod):`
- `security(executor):`
- `test(v3):`
- `docs(git):`

Prefer squash merge into `main` after the pull request checks pass. Keep local commits logical while developing; the PR merge commit should be the clean history unit on `main`.

## Pull Requests

Every PR into `main` must use `.github/pull_request_template.md`. The checklist is intentionally biased toward hot-path RPC safety, route immutability, profit accounting, protocol registry correctness, contract security, LIVE gating, and secret hygiene.

Required checks should be configured only after the workflow has run once and GitHub has created the check names listed in `docs/GITHUB_SETUP.md`.

## Line Endings

The repository uses `.gitattributes` to store production source, scripts, configuration, SQL, protobuf, JSON, and Markdown as LF in Git. PowerShell files are marked CRLF-friendly. Do not run broad line-ending rewrites unless the diff is intentionally reviewed.
