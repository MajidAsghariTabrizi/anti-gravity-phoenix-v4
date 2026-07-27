# Release Controller Architecture

## Trust boundaries

GitHub validates protected-main CI and builds digest-pinned assets. The
environment-scoped Ed25519 key can authenticate only as `phoenix-deploy`.
`authorized_keys` and sshd force `/usr/local/sbin/phoenix-release-transport`; PTY,
agent/X11/TCP forwarding, tunnels, user rc, passwords, and arbitrary commands are
disabled. Sudo permits only the digest-pinned root-owned gateway. The gateway is
Linux-only, protocol-versioned, argument-allowlisted, serialized with `flock`, and
contains no `eval` or general shell transport.

## Bounded receive protocol

Protocol `phoenix-release.v1` receives one gzip tar on stdin. Exactly eight regular
files are permitted: strict request JSON, release/rollback manifest and provenance,
release-assets manifest/checksums, and the SHA-bound release archive. Absolute
paths, separators, traversal, links, extra members, oversized files/counts/totals,
unstable identities, and checksum/provenance mismatches fail before extraction or
runtime mutation.

## Durable state

The strict Python schema records release/rollback/source-CI/build/deploy identities,
phase timestamps, mutation boundary, control state, pointers, image expectations
and observations, release digests, Engine identity/restart/integrity baselines,
owner transaction identity, failure evidence, and rollback result.

The canonical success phases are `REQUESTED`, `SOURCE_CI_VERIFIED`,
`BUILD_VERIFIED`, `HOST_PREFLIGHT_STARTED`, `HOST_PREFLIGHT_OK`,
`ACTIVE_CONTEXT_RECONCILED`, `ROLLBACK_VERIFIED`, `CANDIDATE_INSTALLED`,
`CANDIDATE_LIVE_RENDER_VERIFIED`, `MIGRATIONS_APPLIED`, `LIVE_MODE_INSTALLED`,
`RPC_GATEWAY_HEALTHY`, `ENGINE_HEALTHY`, `ENGINE_BURN_IN_STARTED`,
`ENGINE_BURN_IN_PASSED`, `AUTONOMOUS_ACTIVATED`, `EXECUTOR_UNPAUSE_STARTED`,
`EXECUTOR_UNPAUSED`, `LIVE_EXECUTOR_STARTED`, `POST_LIVE_VERIFYING`,
`POST_LIVE_VERIFIED`, and `COMPLETED`.

Failures use `FAILED_PRE_MUTATION`, `FAILED_POST_MUTATION`, `ROLLBACK_STARTED`,
`ROLLED_BACK`, and `ROLLBACK_FAILED`. Each transition first proves its postcondition;
out-of-order transitions are rejected.

## Active-context reconciliation

`reconcile-active-context` compares environment and pointers with immutable
manifest, rendered Compose, configured images, running container references and
image IDs. Metadata-only drift is rebuilt atomically with no container or contract
mutation. A real mismatch writes nothing and reports service, expected/configured/
actual image, image ID, and container ID.

Python files run through explicit `/usr/bin/python3 -I -B` entrypoints that establish
their package path safely. Shell files run through `/bin/sh`, so extracted release
assets work from `noexec` filesystems.
