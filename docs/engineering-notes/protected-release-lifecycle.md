# Protected Release Lifecycle: Why Activation Is Not Deployment

> **Status:** ARCHITECTURE + IMPLEMENTATION. Documents Phoenix's actual release model from `docs/RELEASE_AND_ROLLBACK.md` and the deploy script. **Profitability: NO VERIFIED ALPHA YET.**

> **Audience:** Engineers running financial infrastructure in production where a bad deploy can cause capital loss. Applies to MEV systems, liquidation systems, oracle publishers, keeper networks, and bridge relayers.

## The default mental model is wrong

The default CI/CD mental model for a trading system is:

1. Code is reviewed.
2. Tests pass.
3. Merge.
5. Deploy.
6. The system is now live.

This is correct for most software. It is wrong for software that can lose money. The mental model that *is* correct is:

1. Code is reviewed.
2. Tests pass.
3. Merge.
4. Build an immutable artifact.
5. Record provenance: what was in the artifact, what was in the build environment, what tests had passed at the time.
6. Preflight: can the artifact run on the target host?
7. Rehearse: can the artifact actually execute against current state?
8. Burn-in: does the artifact survive a sustained period of correct operation?
9. **Then, and only then**, activate execution authority.

Steps 4-8 are not part of "deploy." They are part of "activate." Phoenix's release controller (`docs/RELEASE_AND_ROLLBACK.md`) implements this separation as a non-bypassable sequence.

## What "protected" means in Phoenix

The Phoenix release controller has four distinct invariants that distinguish it from a "deploy on merge" pipeline:

### 1. Immutable artifacts by digest

Every Phoenix container image is published by digest, not by tag. From `docs/RELEASE_AND_ROLLBACK.md`:

```text
The manifest contains seven immutable images: feed-ingestor, phoenix-engine,
rpc-gateway, recorder, fork-sandbox, live-executor, and dashboard.
```

The `release-manifest.json` schema is `phoenix.release.v1`. It records for each image the `repository`, the immutable `tag` (`sha-<full git sha>`), and the `digest` (`sha256:...`). Production `render-production-compose.sh` validates that every Compose image reference in the rendered config is a digest reference. Mutable tags are rejected at the render step.

The implication: there is no way for a tag to be moved to a different image content. If the SHA is `abc...`, the digest of the image at that SHA is fixed for the lifetime of the registry.

### 2. Provenance binding

`release-provenance.json` is created in the same workflow run that creates the manifest. It binds:

- All seven image fragments by digest.
- The immutable release assets archive.
- The release manifest.
- The exact source SHA.
- The release intent (the reason this release was authorized).
- The build run ID.
- The exact-main CI run ID and attempt.

Canonical validation additionally requires the completed GitHub run and every required job to be successful. A run that failed partway through is quarantined as `NON_CANONICAL_INCOMPLETE_BUILD` (the docs cite `Run 29683234024` as a concrete example). None of its partial images or artifacts are release evidence.

This means: a release cannot be constructed from artifacts that come from an incomplete or failed build. The provenance cannot be forged from a successful build's manifest because the build's exact CI run is part of the binding.

### 3. Bounded release assets

The release asset bundle is deterministic for the same inputs and is bounded:

- At most 512 files.
- 8 MiB per file.
- 64 MiB total payload.
- No environment file.
- No credential material.

The strict `phoenix.release-assets.v1` manifest records each relative path, mode, size, and SHA-256 digest. The validator fails closed on symlinks, traversal, extra files, non-canonical JSON, checksum drift, or extracted-tree drift.

The bundle is signed indirectly through the release manifest and provenance. It is installed by `install-release-assets.sh`, which verifies the archive before extraction and promotes it under `/opt/phoenix/releases/<sha>`. The installer does not invoke host provisioning. It does not change persistent-data ownership or permissions.

### 5. The 15-step deploy script with explicit rollback

`deploy-release.sh <release_sha>` is the deploy entry point. It is *not* called by the production workflow directly. The workflow calls a constrained gateway that validates and installs the immutable candidate tree, then calls `deploy-release.sh` from the verified tree inside a bounded systemd oneshot. The oneshot boundary is critical: a deploy cannot survive an SSH session interruption, and the gateway records sanitized rollback evidence to `/var/lib/phoenix-shadow-deploy`.

The 15 steps (paraphrased from `docs/RELEASE_AND_ROLLBACK.md`):

1. Validate the 40-character SHA.
2. Require the installed release-assets marker to match that SHA.
3. Load `/opt/phoenix/deploy/manifests/<sha>.json`.
4. Validate manifest SHA, tags, and digests.
5. Write a candidate per-release digest env under `manifests/`.
6. Validate `/etc/phoenix/phoenix.env` and the canonical render before any runtime mutation.
7. Capture healthy relay, feed-ingestor, NATS, PostgreSQL, and Recorder container IDs.
8. Save the current release as `previous-release`.
9. Pull exact digest-backed images without recreating services.
10. Run the migration runner with `--no-deps`.
11. Start Prometheus, RPC Gateway, Shadow Dispatcher, Phoenix Engine, and Dashboard one at a time with `--no-deps` and bounded health waits.
12. Verify every protected container ID is unchanged.
13. Run `production-healthcheck.sh` against the candidate release env.
14. Compare manifest, render, checksums, route hash, and running images.
15. Atomically replace each active state file, with `current-release` promoted last as the activation pointer, only after every gate passes.

If any step fails, the script exits through `rollback-release.sh`. The rollback script:

- Reads `previous-release`.
- Validates that manifest.
- Integrity-checks and restores the immutable release-assets tree for that exact SHA.
- Restores the five optional SHADOW services one at a time.
- Fingerprints the same protected container IDs before and after.
- Uses bounded health waits.
- Reports rollback success only after health and release-context validation pass.

Deployment is blocked before asset installation unless the active rollback pointer, asset marker, and immutable tree all agree.

## The activation gate: separate from deploy

This is the most important part of the model and the easiest to miss: **deployment is not activation.** The deploy script starts services in SHADOW mode by default. The runtime mode is governed by `LIVE_EXECUTION=false` in the operator env, which is the canonical production default.

To activate LIVE execution, a separate readiness report is required. From `docs/PLANS.md`:

> Phase 21: implemented manual readiness-report workflow that cannot enable LIVE or receive signer material.

This means: the deploy script cannot enable LIVE mode. The release controller cannot enable LIVE mode. Only an operator who has separately authenticated against the readiness workflow can change the activation state. The signer material is not loaded by the deploy process; it is loaded by a separate, manually-acknowledged path that happens after the deploy.

A bad deploy cannot result in a bad live trade. The deployer cannot enable execution; the activator cannot deploy code.

## Why this matters

The most common production incident in financial infrastructure is not "the code had a bug." It is "the wrong version of the code reached production" or "the code ran before the environment was ready." Phoenix's release model addresses both:

- Wrong version: digest references and provenance binding make it impossible to deploy a version that does not match its manifest. The manifest cannot be constructed from a failed build.
- Code ran before ready: the activation gate separates deploy from LIVE. A deploy succeeds when SHADOW is healthy. LIVE happens when an operator authorizes it.

The cost is operational overhead: more runs to do, more places to validate. The benefit is that the failure modes that hurt are gated by the parts of the system that can survive the failure.

## What this is not

This is not a claim that Phoenix has run a profitable production release. Phoenix has run SHADOW releases extensively. The most recent production audit captured 16 Docker containers, 4.35 GB of PostgreSQL state, 0 attempted live executions, and 0 realized PnL. The system is `FULL_LIVE_NO_ALPHA`.

What this *is* is a description of how Phoenix ensures that a *bad* release cannot produce a *bad* execution. The release model does not guarantee profitability. It guarantees that the cost of a bad release is bounded: a rollback is a release-shaped operation, not a debug incident.

## Reviewable evidence

- `docs/RELEASE_AND_ROLLBACK.md` — full release lifecycle, deploy and rollback scripts, manifest schema, asset bundling.
- `docs/CI_CD.md` — GitHub Actions workflows, image publishing, exact-main CI.
- `docs/PRODUCTION_BOOTSTRAP.md` — first-host setup, asset installation, canonical context.
- `release-components.json` at repo root — observed release component bindings.
- `compose.prod.yml` — production render with digest-pinned images.
- `scripts/deploy-release.sh` and `scripts/rollback-release.sh` (referenced from `RELEASE_AND_ROLLBACK.md`).

## For your own system

If you are building or auditing a financial-system release pipeline, the questions to ask:

1. **Are your deployable artifacts referenced by digest or by tag?** Mutable tags can be moved. Digests cannot.
2. **Can your CI produce a manifest without a successful build?** If yes, the manifest does not enforce build success.
3. **Can your deploy process enable live execution?** If yes, the deploy process has authority it should not have.
4. **Can your rollback process produce a known-good state?** If rollback is itself a release-shaped operation, it should have its own manifest and provenance.
5. **Is the activation authority separate from the deployment authority?** If both are in the same role, a single compromise can deploy and activate.

The hardest to internalize is the last. The natural mental model is "the deployer is the activator." That model conflates two different kinds of authority and produces systems where a single bad event can take a system from "in development" to "losing money" in one step.