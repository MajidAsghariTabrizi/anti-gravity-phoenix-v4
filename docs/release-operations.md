# Phoenix Release Operations

Phoenix production releases are driven by `.github/workflows/phoenix-release-controller.yml`.
The previous laptop, PowerShell, artifact-download, SCP, and general interactive-SSH
process is deprecated and unsupported.

## Normal release

A reviewed merge to protected `main` starts Phoenix CI. When all 12 required jobs
pass, `workflow_run` invokes the controller. The controller proves that the CI
repository, branch, event, attempt, SHA, and job set are exact; rejects a superseded
main tip; skips docs-only changes; reuses a non-expired exact-SHA immutable build
when present — including when a prior run's deploy phase failed closed after its
immutable-build jobs all succeeded (the pre-mutation retry path, which requires
the stored package identity to match exactly); otherwise calls
`build-images.yml` through `workflow_call`; creates a
GitHub Deployment; and sends a bounded release package to `phoenix-deploy`.

`PHOENIX_AUTORELEASE_ENABLED=true` is required. Concurrency group
`phoenix-production-release` serializes releases without cancelling an active
release. A ten-minute schedule resumes eligible interrupted work.

## Status and evidence

The permanent SSH key is forced through `phoenix-release-transport`; it accepts
only:

```text
status
history
plan <release-sha>
readiness <release-sha>
resume <release-sha>
retry-pre-mutation <release-sha>
retry-rolled-back <release-sha>
rollback <release-sha>
emergency-pause
evidence <release-sha>
reconcile-active-context
```

The GitHub workflow normally invokes these commands. Operators use the same
protocol only for break-glass diagnosis. Evidence is bounded JSON and a
`phoenix-release-evidence-<sha>` Actions artifact.

## Resume and idempotency

State lives at `/var/lib/phoenix-release/releases/<sha>/state.json`, is root-owned,
atomically replaced, and records every postcondition. Completed phases are not
repeated. The controller runs one aggregated, read-only Production readiness
report before image building. The immutable package is extracted to a root-only
temporary directory and rehearsed against the candidate Compose render, live
schema in a read-only transaction, isolated monitor output, control status, and
health contracts before the mutation boundary.

The durable release sequence is:

```text
BUILD_VERIFIED
  -> HOST_PREFLIGHT_OK
  -> ACTIVE_CONTEXT_RECONCILED
  -> ROLLBACK_VERIFIED
  -> CANDIDATE_REHEARSED
  -> CANDIDATE_INSTALLED
  -> DISARMED_EVIDENCE_STARTED
  -> COMPLETED
```

A pre-mutation failure or a fully successful pre-activation rollback may create
a new numbered attempt only when the original SHA, image manifest, package
digest, rollback identity, fail-closed controls, and absence of owner
transactions still match. The failed state is archived immutably. No image is
rebuilt and no fake commit is required. An interruption after an owner
transaction is never retried.

## Key rotation and protocol upgrades

Generate a dedicated Ed25519 key directly for GitHub Actions, install only its
public half for `phoenix-deploy`, replace the environment-scoped
`PROD_SSH_PRIVATE_KEY`, prove `status`, then remove the old public key. Never print
either private key.

Gateway upgrades are reviewed release assets. Install the exact merged platform
while preserving the existing deploy key:

```text
sudo /bin/sh scripts/install-phoenix-release-platform.sh \
  --release-sha <exact-merged-sha> \
  --reuse-existing-deploy-key --reuse-existing-rpc-provider-secret
```

RPC credential reuse is valid only after the approved persistent file passes
the complete metadata and bounded-content contract. A missing file requires
the controller's explicit stdin first-install mode; the credential is never an
argument, URL, environment-file entry, manifest field, or evidence field.

The installer writes
`/usr/local/libexec/phoenix-release/platform-manifest.json`. Every root platform
and deploy-context safety file is hash-bound to that release SHA. Resume blocks
if the installed platform, deploy context, ownership, or modes drift. Verify
`release_platform.py verify`, `sshd -t`, `sudo -l -U phoenix-deploy`, and
protocol `phoenix-release.v1` before enabling the controller.

## Migration policy

Migrations require identity, checksum, lock, idempotency, compatibility evidence,
and expand-before-contract sequencing. A release that makes the active rollback
schema incompatible is rejected before runtime mutation unless the reviewed
manifest contains an explicit forward-fix policy.
