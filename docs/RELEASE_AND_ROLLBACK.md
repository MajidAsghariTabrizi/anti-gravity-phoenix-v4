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

## Deploy

`deploy-release.sh <release_sha>`:

1. Validates the 40-character SHA.
2. Loads `/opt/phoenix/deploy/manifests/<sha>.json`.
3. Validates manifest SHA, tags, and digests.
4. Writes the exact image refs to `current-release.env`.
5. Saves the current release as `previous-release`.
6. Validates `/etc/phoenix/phoenix.env`.
7. Renders production Compose.
8. Pulls exact digest-backed images.
9. Runs the migration runner.
10. Starts or updates SHADOW services.
11. Runs `production-healthcheck.sh`.
12. Writes `current-release` atomically only after health passes.

If deployment fails, it invokes `rollback-release.sh` and preserves failed deployment diagnostics.

## Rollback

`rollback-release.sh` reads `previous-release`, validates that manifest, restores exact previous image refs, updates services, and runs the production health check. It reports rollback success only after health passes.

Database migrations are forward-only. Rollbacks require backward-compatible migrations until a dedicated manual data rollback plan exists.

## Known Current Gate

Linux VPS validation must still prove the Nitro relay adapter against the real Arbitrum sequencer feed. A production deploy must fail health and rollback if relay/feed readiness cannot be established, and it must never silently consume fixtures.
