# Phoenix Release Operations

Phoenix production releases are driven by `.github/workflows/phoenix-release-controller.yml`.
The previous laptop, PowerShell, artifact-download, SCP, and general interactive-SSH
process is deprecated and unsupported.

## Normal release

A reviewed merge to protected `main` starts Phoenix CI. When all 12 required jobs
pass, `workflow_run` invokes the controller. The controller proves that the CI
repository, branch, event, attempt, SHA, and job set are exact; rejects a superseded
main tip; skips docs-only changes; reuses a non-expired exact-SHA immutable build
when present; otherwise calls `build-images.yml` through `workflow_call`; creates a
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
resume <release-sha>
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
repeated. Candidate installation can resume. An interruption after money-path
mutation is never allowed to blindly repeat an owner transaction; it fails closed
for state/receipt reconciliation or rollback.

## Key rotation and protocol upgrades

Generate a dedicated Ed25519 key directly for GitHub Actions, install only its
public half for `phoenix-deploy`, replace the environment-scoped
`PROD_SSH_PRIVATE_KEY`, prove `status`, then remove the old public key. Never print
either private key.

Gateway upgrades are reviewed release assets. Install
`scripts/install-phoenix-release-platform.sh` from an exact merged commit, verify
root ownership/modes, `sshd -t`, `sudo -l -U phoenix-deploy`, and protocol
`phoenix-release.v1` before enabling the controller.

## Migration policy

Migrations require identity, checksum, lock, idempotency, compatibility evidence,
and expand-before-contract sequencing. A release that makes the active rollback
schema incompatible is rejected before runtime mutation unless the reviewed
manifest contains an explicit forward-fix policy.
