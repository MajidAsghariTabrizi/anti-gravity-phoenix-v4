# CI/CD

Phoenix uses five GitHub workflows:

- `Phoenix CI`: pull requests to `main`, pushes to `main`, and manual dispatch.
- `Build Phoenix Images`: explicitly confirmed manual dispatch only.
- `Deploy Shadow Production`: acknowledged manual dispatch for an already validated release and rollback pair.
- `Deploy PRE-LIVE Protected Maintenance`: acknowledged manual dispatch for the separately reviewed v3 Feed Ingestor and Recorder maintenance.
- `Live Readiness Report`: manual-only report generation for a proposed LIVE release.

## CI Checks

The exact PR check names are:

- `hygiene`
- `go`
- `rust-phoenix`
- `rust-rpc-gateway`
- `rust-recorder`
- `rust-replay`
- `rust-fork-sandbox`
- `solidity`
- `python-dashboard`
- `docker-validation`
- `integration-fixtures`
- `jetstream-integration`

These jobs are intentionally split so a failing crate or surface is visible in GitHub required checks.

## CI Rules

Core checks do not use `continue-on-error`. CI has minimum permissions, job timeouts, and concurrency cancellation for superseded PR branch runs.

The repository `CODEOWNERS` file identifies reviewers for release, workflow,
contract, executor, and deployment-gateway control surfaces. `CODEOWNERS` does
not enforce review by itself. The `main` branch ruleset must be configured
manually with the exact GitHub setting **Require review from Code Owners** in
addition to the required status checks.

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
- `ghcr.io/majidasgharitabrizi/fork-sandbox`
- `ghcr.io/majidasgharitabrizi/live-executor`
- `ghcr.io/majidasgharitabrizi/dashboard`

The manual dispatch requires the exact current `main` SHA, the exact successful
`Phoenix CI` push run ID and attempt for that SHA, a bounded release intent, and
the confirmation
`PUBLISH_IMMUTABLE_PHOENIX_IMAGES`. Ordinary main pushes and pull requests
cannot publish. `packages: write` is scoped only to the seven publishing jobs.

The canonical component, build, protected-image, production-Compose, and CI-job
contracts are defined in `release-components.json`. Publication preflight loads
its matrix from that registry and rejects a CI run unless it is the completed,
successful `.github/workflows/ci.yml` `push` run on `main` for the exact release
SHA, exact run ID, and exact attempt. The normalized job evidence and its
deterministic SHA-256 fingerprint are embedded in release provenance.

Example future publication dispatch (after the exact `main` push CI is green):

```sh
gh workflow run build-images.yml \
  --repo MajidAsghariTabrizi/anti-gravity-phoenix-v4 \
  --ref main \
  -f release_sha=<exact-main-sha> \
  -f ci_run_id=<successful-main-push-ci-run-id> \
  -f ci_run_attempt=<successful-main-push-ci-run-attempt> \
  -f release_intent=PHOENIX_PRELIVE_SHADOW_V5 \
  -f confirm_publish=PUBLISH_IMMUTABLE_PHOENIX_IMAGES
```

Images use `sha-<full git sha>` tags and OCI labels for source, revision,
created timestamp, and image title. The release manifest records the exact
repositories, tags, and digests. A same-run provenance sidecar binds all
fragments and release assets to one successful workflow run and SHA.
Production deployment consumes the manifest, not `latest`.

The same workflow builds `phoenix-release-assets-<sha>.tar.gz`, a deterministic bounded bundle containing the canonical Compose context, migrations, report and control schemas, deployment/control scripts, route proofs, Dashboard snapshot model, and compiled PhoenixExecutor artifact. `release-assets-manifest.json` records every path, mode, size, and SHA-256 digest. The release manifest is withheld unless all seven image builds and the asset bundle succeed. Canonical validation also requires the final manifest job and complete run to succeed; run `29683234024` is explicitly quarantined as an incomplete non-release build.

Replay is an offline CLI and is not published as a permanent production daemon image.

## Deployment

`Deploy Shadow Production` never runs automatically. A manual dispatch must provide the exact current `main` SHA, its successful image-build run, the exact active rollback SHA, the rollback image-build run, and the acknowledgement `DEPLOY_PRELIVE_SHADOW`. The `production-shadow` environment gate applies before secrets are available.

Before SSH, the workflow verifies the checkout is current `main`, reconciles
the candidate's embedded source-CI evidence against the GitHub API, verifies
both manifests are strict and digest-pinned, integrity checks the release asset
archive and extracted tree, and requires protected `feed-ingestor` and
`recorder` image identities to match the active rollback release. Before
candidate asset installation, the host's active pointer, asset marker, and
integrity-checked immutable rollback tree must also agree. A protected-image
change fails closed and requires a separately authorized maintenance gate;
this workflow will not recreate protected data-plane services.

After those checks, the workflow uploads only the candidate and rollback
manifest/provenance pair plus the three candidate release-assets files to a
deterministic run-bound stage. Its only privileged remote command is
`sudo -n /usr/local/sbin/phoenix-shadow-deploy-gateway`. The root-owned gateway
locks and snapshots those inputs, repeats canonical validation, verifies the
active rollback contract and SHADOW controls, and launches a bounded detached
systemd oneshot. No script from `/tmp` is executed. Deploy and rollback start
only `prometheus`, `rpc-gateway`, `shadow-dispatcher`, `phoenix-engine`, and
`dashboard`, one at a time with bounded health waits. Relay, feed-ingestor,
NATS, PostgreSQL, and Recorder container IDs must remain unchanged.

The workflow polls sanitized gateway evidence. A successful stage is removed
only after terminal evidence is retrieved; failed stage and root evidence are
retained for recovery review.

The workflow uses strict host key checking. It requires only:

- `PROD_HOST`
- `PROD_PORT`
- `PROD_USER`
- `PROD_SSH_PRIVATE_KEY`
- `PROD_KNOWN_HOSTS`

It never receives `SIGNER_PRIVATE_KEY`.

## Protected Maintenance

`Deploy PRE-LIVE Protected Maintenance` is separate from normal deployment and
does not weaken its protected-image refusal. It accepts `release_sha`,
`build_run_id`, `rollback_sha`, `rollback_build_run_id`, and the exact
acknowledgement `DEPLOY_PRELIVE_PROTECTED_MAINTENANCE`. The reviewed v3 and v2
SHAs, build runs, and immutable tag targets are pinned in the workflow.

Before SSH material is installed, the workflow verifies both successful build
runs, downloads and verifies both complete release-asset bundles, validates the
exact protected allowlist, renders both release contracts in SHADOW with blank
execution configuration, pulls every digest-pinned image, and checks each OCI
revision label against its source SHA.

On the host, optional services must already be stopped. Feed Ingestor is
quiesced first so Recorder can drain `PHOENIX_RECORDER` to zero pending and
ACK-pending messages. Recorder is then replaced and health-checked before Feed
Ingestor is replaced and health-checked. PostgreSQL, NATS, Nitro Relay,
networks, mounts, streams, durable consumers, PostgreSQL owner/group/mode
evidence, and NATS volume metadata must retain their identities.
Only after live Feed and Recorder progress is proven does the gate install and
promote the exact v3 release context. Optional containers remain stopped and
unchanged for the later controlled SHADOW startup.

Any failed mutation invokes the same bounded sequence with exact v2
Feed Ingestor and Recorder digests, restores the v2 immutable release context,
and re-runs health, identity, progress, and zero-execution checks. Evidence is
retained under `/opt/phoenix/evidence/protected-maintenance`.

The remote operation runs in a bounded transient systemd oneshot unit with a
stable GitHub run identity. SSH launches and polls the unit but does not own its
lifetime, so transport loss cannot send HUP or initiate rollback. Actual unit
or maintenance failure still invokes automatic rollback. Completion requires
the exact unit status, exit file, result line, log, and bounded evidence archive
to be retrieved. Incomplete evidence fails the workflow. This workflow has no
automatic trigger and must not be dispatched as part of repository release
preparation.

## LIVE Gate

`Live Readiness Report` is `workflow_dispatch` only. It validates the exact acknowledgement `I_UNDERSTAND_THIS_CAN_SEND_REAL_TRANSACTIONS`, release SHA shape, and executor address shape. It does not enable LIVE, does not edit `/etc/phoenix/phoenix.env`, does not restart services, and does not receive signer material.
