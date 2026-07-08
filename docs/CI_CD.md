# CI/CD

Phoenix uses four GitHub workflows:

- `Phoenix CI`: pull requests to `main` and manual dispatch.
- `Build Phoenix Images`: pushes to `main` and manual dispatch.
- `Deploy Shadow Production`: workflow-run deployment after a successful image build on `main`.
- `Live Readiness Report`: manual-only report generation for a proposed LIVE release.

## CI Checks

The exact PR check names are:

- `hygiene`
- `go`
- `rust-phoenix`
- `rust-rpc-gateway`
- `rust-recorder`
- `rust-replay`
- `solidity`
- `python-dashboard`
- `docker-validation`
- `integration-fixtures`

These jobs are intentionally split so a failing crate or surface is visible in GitHub required checks.

## CI Rules

Core checks do not use `continue-on-error`. CI has minimum permissions, job timeouts, and concurrency cancellation for superseded PR branch runs.

The hygiene job runs both secret scanners, both forbidden-file checks, `git diff --check`, and tracked-file validations for `.env`, private key material, runtime databases, feed recordings, replay output, and build output.

The Go job verifies `gofmt` without modifying source, then runs `go vet` and `go test` for feed-ingestor and migration-runner.

Rust jobs install `rustfmt` and `clippy` through `rustup` on the GitHub runner and check each crate separately.

Solidity runs Foundry formatting and tests. Fork tests must not run on untrusted pull request secrets.

Dashboard CI installs dashboard dependencies, compiles syntax, and runs an import smoke check without blockchain RPC access.

Docker validation checks local and production Compose rendering and builds every Phoenix-owned runtime Dockerfile. It does not push images.

Integration fixtures exercise deterministic profitable, non-profitable, unsupported-origin, incomplete-state, and duplicate-feed boundaries. The profitable engine test verifies dynamic amount sizing and proves the selected amount is not merely the first configured candidate.

## Image Publishing

`Build Phoenix Images` publishes:

- `ghcr.io/majidasgharitabrizi/feed-ingestor`
- `ghcr.io/majidasgharitabrizi/phoenix-engine`
- `ghcr.io/majidasgharitabrizi/rpc-gateway`
- `ghcr.io/majidasgharitabrizi/recorder`
- `ghcr.io/majidasgharitabrizi/dashboard`

Images use `sha-<full git sha>` tags and OCI labels for source, revision, created timestamp, and image title. The release manifest records the exact repositories, tags, and digests. Production deployment consumes that manifest, not `latest`.

Replay is an offline CLI and is not published as a permanent production daemon image.

## Deployment

`Deploy Shadow Production` downloads the exact release manifest from the image build workflow, validates it against the `workflow_run` SHA, copies it to `/opt/phoenix/deploy/manifests/<sha>.json`, and calls `/opt/phoenix/deploy/deploy-release.sh <sha>` over SSH.

The workflow uses strict host key checking. It requires only:

- `PROD_HOST`
- `PROD_PORT`
- `PROD_USER`
- `PROD_SSH_PRIVATE_KEY`
- `PROD_KNOWN_HOSTS`

It never receives `SIGNER_PRIVATE_KEY`.

## LIVE Gate

`Live Readiness Report` is `workflow_dispatch` only. It validates the exact acknowledgement `I_UNDERSTAND_THIS_CAN_SEND_REAL_TRANSACTIONS`, release SHA shape, and executor address shape. It does not enable LIVE, does not edit `/etc/phoenix/phoenix.env`, does not restart services, and does not receive signer material.
