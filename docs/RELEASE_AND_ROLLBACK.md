# Release And Rollback

## Release Manifest

`Build Phoenix Images` creates `release-manifest.json`:

```json
{
  "schema": "phoenix.release.v1",
  "release_sha": "0000000000000000000000000000000000000000",
  "created_at": "2026-07-08T00:00:00Z",
  "images": {
    "feed-ingestor": {
      "repository": "ghcr.io/majidasgharitabrizi/feed-ingestor",
      "tag": "sha-0000000000000000000000000000000000000000",
      "digest": "sha256:..."
    }
  }
}
```

Production deploys image references from this manifest only. Mutable tags are rejected.

Nitro Relay, NATS, PostgreSQL, and Prometheus are pinned directly in
`compose.prod.yml` by multi-platform manifest digest. Phoenix-owned image
digests come only from the release manifest.

## Canonical Production Context

The only repository-supported host context is:

```text
compose:         /opt/phoenix/deploy/compose.prod.yml
operator env:    /etc/phoenix/phoenix.env
release env:     /opt/phoenix/deploy/current-release.env
release pointer: /opt/phoenix/deploy/current-release
release state:   /opt/phoenix/deploy/current-release.json
```

There is no production override file. `/opt/phoenix/app/compose.prod.yml`, a
plain source-checkout render, and local `app-*` images are not valid production
contexts.

`render-production-compose.sh` requires every path explicitly. It writes the
secret-bearing rendered Compose JSON to a caller-selected mode-`0600` file and
prints only bounded release metadata. It validates exact digest images, the
Engine route string, canonical route IDs/fingerprints, route hash, chain 42161,
SHADOW mode, disabled LIVE execution, blank signer/wallet/executor values, and
an RPC state budget of at least 12. It never rewrites an input env file.

The route hash is:

```text
sha256(UTF-8 JSON with sorted object keys, compact separators, ASCII escaping)
```

Array order is preserved because route ranking order is meaningful.

`validate-production-release-context.sh` additionally compares the manifest,
release env, SHA pointer, checksummed state, a fresh render, and every running
container image. It can consume a bounded running-image JSON snapshot for tests
or inspect an existing runtime. Inspection never starts a service.

Transient deployment files live only under
`/opt/phoenix/deploy/.runtime`. The repository-local equivalent and generated
active release files are ignored; root-level `FETCH_HEAD` is explicitly
forbidden rather than ignored.

## Deploy

`deploy-release.sh <release_sha>`:

1. Validates the 40-character SHA.
2. Loads `/opt/phoenix/deploy/manifests/<sha>.json`.
3. Validates manifest SHA, tags, and digests.
4. Writes a candidate per-release digest env under `manifests/`.
5. Validates `/etc/phoenix/phoenix.env` and the canonical render before runtime mutation.
6. Saves the current release as `previous-release`.
7. Pulls exact digest-backed images.
8. Runs the migration runner.
9. Starts or updates SHADOW services without broad orphan removal.
10. Runs `production-healthcheck.sh` against the candidate release env.
11. Compares manifest, render, checksums, route hash, and running images.
12. Atomically replaces each active state file, with `current-release` promoted
    last as the activation pointer, only after every gate passes.

After candidate preflight, any failed deployment or interrupt exits through
`rollback-release.sh`, reports the bounded failure, and removes transient
candidate state.

## Rollback

`rollback-release.sh` reads `previous-release`, validates that manifest, restores exact previous image refs, updates services, and runs the production health check. It reports rollback success only after health passes.

Database migrations are forward-only. Rollbacks require backward-compatible migrations until a dedicated manual data rollback plan exists.

## Host Migration

Before the first release using this contract, install the merged exact-SHA
assets with `bootstrap-production.sh` or the reviewed release-asset workflow.
Do not copy generated release state into a source checkout. Existing manifests
remain compatible; their digest env is regenerated with an added
`PHOENIX_RELEASE_SHA` field. Existing plain `current-release` and
`previous-release` SHA pointers remain supported. The new JSON state is created
at the first successful deploy or rollback.

If a host still reports `/opt/phoenix/app/compose.prod.yml`, stop before any
runtime mutation. Reinstall the canonical assets and validate the existing
project/container migration explicitly; do not let a second Compose project
duplicate protected data-plane services.

## Known Current Gate

Linux VPS validation must still prove the Nitro relay adapter against the real Arbitrum sequencer feed. A production deploy must fail health and rollback if relay/feed readiness cannot be established, and it must never silently consume fixtures.
