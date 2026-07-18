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

The manifest contains six immutable images: feed-ingestor, phoenix-engine,
rpc-gateway, recorder, fork-sandbox, and dashboard. The production renderer
uses the five production image entries; fork-sandbox remains isolated and is
published for controlled fork evidence only.

Nitro Relay, NATS, PostgreSQL, and Prometheus are pinned directly in
`compose.prod.yml` by multi-platform manifest digest. Phoenix-owned image
digests come only from the release manifest.

## Release Assets

`Build Phoenix Images` also creates:

- `phoenix-release-assets-<sha>.tar.gz`
- `release-assets-manifest.json`
- `release-assets-checksums.txt`

The bundle is deterministic for the same inputs, contains no environment file
or credential material, and is bounded to 512 files, 8 MiB per file, and 64
MiB of payload. The strict `phoenix.release-assets.v1` manifest records each
relative path, mode, size, and SHA-256 digest. Archive members must be regular
files beneath the exact release root; symlinks, traversal, extra files,
non-canonical JSON, checksum drift, and extracted-tree drift fail closed.

`install-release-assets.sh` verifies the archive before extraction, promotes
it under `/opt/phoenix/releases/<sha>`, verifies the immutable extracted tree,
and invokes the narrowly scoped release-context installer with that exact SHA
and tree. The installer updates canonical files under `/opt/phoenix/deploy`
and promotes `/opt/phoenix/deploy/release-assets.sha` only after the production
environment and Docker tooling validate. It never invokes host provisioning or
changes persistent-data ownership or permissions.

## Canonical Production Context

The only repository-supported host context is:

```text
compose:         /opt/phoenix/deploy/compose.prod.yml
operator env:    /etc/phoenix/phoenix.env
release env:     /opt/phoenix/deploy/current-release.env
release pointer: /opt/phoenix/deploy/current-release
release state:   /opt/phoenix/deploy/current-release.json
asset marker:    /opt/phoenix/deploy/release-assets.sha
release source:  /opt/phoenix/releases/<release-sha>/
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
2. Requires the installed release-assets marker to match that SHA.
3. Loads `/opt/phoenix/deploy/manifests/<sha>.json`.
4. Validates manifest SHA, tags, and digests.
5. Writes a candidate per-release digest env under `manifests/`.
6. Validates `/etc/phoenix/phoenix.env` and the canonical render before runtime mutation.
7. Captures healthy relay, feed-ingestor, NATS, PostgreSQL, and Recorder container IDs.
8. Saves the current release as `previous-release`.
9. Pulls exact digest-backed images without recreating services.
10. Runs the migration runner with `--no-deps`.
11. Starts Prometheus, RPC Gateway, Shadow Dispatcher, Phoenix Engine, and Dashboard one at a time with `--no-deps` and bounded health waits.
12. Verifies every protected container ID is unchanged.
13. Runs `production-healthcheck.sh` against the candidate release env.
14. Compares manifest, render, checksums, route hash, and running images.
15. Atomically replaces each active state file, with `current-release` promoted
    last as the activation pointer, only after every gate passes.

After candidate preflight, any failed deployment or interrupt exits through
`rollback-release.sh`, reports the bounded failure, and removes transient
candidate state.

## Rollback

`rollback-release.sh` reads `previous-release`, validates that manifest, and
integrity-checks and restores the immutable release-assets tree for that exact
SHA before restoring the five optional SHADOW services one at a time. It
fingerprints the same protected container IDs before and after, uses bounded
health waits, and reports rollback success only after health and
release-context validation pass. Deployment is blocked before asset
installation unless the active rollback pointer, asset marker, and immutable
tree all agree.

Database migrations are forward-only. Rollbacks require backward-compatible migrations until a dedicated manual data rollback plan exists.

## Protected-Service Maintenance

Normal `deploy-shadow` behavior remains unchanged: a Feed Ingestor or Recorder
digest difference fails before SSH. The separate
`deploy-prelive-protected-maintenance.yml` workflow is pinned to
`phoenix-prelive-shadow-v4` and `phoenix-prelive-shadow-v3`.

Its pre-SSH gate verifies the exact tag targets and build runs, both complete
asset bundles and checksum files, immutable image references and OCI revision
labels, byte-identical NATS/route/migration contracts, semantically equivalent
protected Compose contracts, exact SHADOW renders, blank execution
configuration, and the reviewed allowlist:

```text
feed-ingestor
recorder
```

Remote preflight requires the v2 pointer, asset marker, manifest, state, and
immutable release tree to agree. It also requires all optional services to be
stopped and all five protected services to be healthy. Redacted evidence
captures container, mount, network, JetStream, migration, database,
progress, restart, OOM, disk, and non-execution state.

The maintenance sequence is:

1. Stop Feed Ingestor so no new application publications enter JetStream.
2. Wait for the Recorder durable consumer to reach zero pending and
   ACK-pending messages.
3. Replace only Recorder with the exact v3 digest and wait for health.
4. Replace only Feed Ingestor with the exact v3 digest and wait for health.
5. Require Feed sequence, JetStream publication, Recorder persistence, and
   PostgreSQL feed-event progress with no sequence regression or unbounded
   redelivery.
6. Install the exact v3 release-assets bundle and atomically promote its
   release environment, state, maintenance context, and pointer. Optional
   containers remain stopped.

Any failure after mutation automatically restores only Feed Ingestor and
Recorder with exact v2 digests, restores the immutable v2 release context, and
repeats the same continuity, progress, and zero-execution checks. PostgreSQL,
NATS, Nitro Relay, networks, volumes, streams, and consumers are never
recreated by this path. Every snapshot also hashes stable PostgreSQL
owner/group/mode evidence and NATS volume metadata; any missing or changed
storage identity blocks release promotion.

The workflow starts maintenance as a bounded transient systemd oneshot unit.
The SSH session only launches and polls the unit. An SSH reset or HUP cannot
signal the maintenance process and therefore cannot trigger rollback.
Internal maintenance failures still trigger automatic rollback. The workflow
accepts completion only after the unit result, exit status, log, and bounded
host evidence bundle are retrieved.

## Host Migration

Before the first release using this contract, install the merged exact-SHA
bundle with `install-release-assets.sh`. A direct bootstrap from a trusted
checkout remains available for initial host preparation, but deployment is
blocked until the scoped release-context installer receives the exact release
SHA and promotes the asset marker. Bootstrap must not be used as a release or
rollback restore operation.
Do not copy generated release state into a source checkout. Existing manifests
remain compatible; their digest env is regenerated with an added
`PHOENIX_RELEASE_SHA` field. Existing plain `current-release` and
`previous-release` SHA pointers remain supported. The new JSON state is created
at the first successful deploy or rollback.

If a host still reports `/opt/phoenix/app/compose.prod.yml`, stop before any
runtime mutation. Reinstall the canonical assets and validate the existing
project/container migration explicitly; do not let a second Compose project
duplicate protected data-plane services.

## Known Current Gates

Linux VPS validation must still prove the Nitro relay adapter against the real
Arbitrum sequencer feed. A production deploy must fail health and rollback if
relay/feed readiness cannot be established, and it must never silently consume
fixtures.

This pre-live milestone changes feed-ingestor and Recorder while classifying
both as protected data-plane services. The manual deployment workflow compares
their candidate and rollback digests before SSH and blocks any difference.
Deploying those changed images therefore requires a separate, explicitly
authorized maintenance gate that reconciles protected-service continuity; this
milestone does not silently recreate them.
