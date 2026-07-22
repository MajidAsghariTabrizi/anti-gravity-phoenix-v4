# Security

Phoenix v4 is shadow-first and secrets-clean by default.

## Requirements

- No private keys in source.
- No API tokens in source.
- No credential values in examples.
- `.env` is ignored.
- `.env.example` uses placeholders only.
- Raw secret environment variables are never logged.
- LIVE execution requires explicit server-side gates.
- GitHub Actions never receives `SIGNER_PRIVATE_KEY`.

## Threat Review

| Threat | Control |
| --- | --- |
| GitHub Actions token overreach | Workflows default to `contents: read`; image publishing alone grants `packages: write`; deployment grants `actions: read` for artifact download. |
| Pull request secret exposure | CI pull-request workflows do not use production secrets. Deployment secrets are available only to an acknowledged manual job in the `production-shadow` environment. |
| Malicious dependency action | Third-party and official actions are pinned to full commit SHAs and documented in `docs/GITHUB_ACTIONS_DEPENDENCIES.md`. |
| Unpinned actions | Workflows must not use floating tags; Dependabot updates require review. |
| SSH key leakage | Deploy SSH key is a GitHub secret, written to `~/.ssh/id_ed25519` with mode `0600`, and never printed. Use a dedicated production deploy key. |
| Host key MITM | `PROD_KNOWN_HOSTS` is pre-verified out-of-band. Deployment uses `StrictHostKeyChecking=yes` and never trusts runtime `ssh-keyscan`. |
| GHCR credential leakage | GitHub publishes with `GITHUB_TOKEN`; production uses a separate least-privilege package pull credential entered through `docker login ghcr.io --password-stdin`. |
| Docker build secret leakage | No runtime secrets are passed as Docker build args. `.dockerignore` excludes env files, keys, runtime data, caches, and build outputs. |
| Runtime environment leakage | `/etc/phoenix/phoenix.env` must be `root:root` mode `0600`; validation reports variable names and categories, not values. |
| Production signer leakage | `live-executor` receives signer material only through a read-only file mount after every LIVE gate passes. Raw `SIGNER_PRIVATE_KEY` is local/test compatibility only and must not exist in GitHub secrets. |
| Fixture accidentally used in production | `feed-ingestor` fails startup if `PHOENIX_ENV=production` and `PHOENIX_FEED_FIXTURE` is set. |
| LIVE accidentally enabled | Production Compose forces `PHOENIX_MODE=SHADOW` and `LIVE_EXECUTION=false`; release-live workflow cannot edit env or restart LIVE. |
| Mutable image deployment | Build workflow emits `sha-<full git sha>` tags and digests; deploy scripts reject missing digests and never consume `latest`. |
| Mutable release assets | A deterministic exact-SHA bundle records every allowed path, mode, size, and digest; archive and extracted-tree verification reject symlinks, traversal, extras, and drift before the scoped release-context installer promotes the asset marker. |
| Automatic production rollout | Image publication cannot trigger deployment. Manual dispatch requires current-main release evidence, prepared rollback evidence, an explicit acknowledgement, and the production environment gate. |
| Protected service recreation | Deploy and rollback fingerprint relay, feed-ingestor, NATS, PostgreSQL, and Recorder IDs. Candidate protected-image digest changes fail before SSH; optional services start individually with `--no-deps`. |
| Authorized protected maintenance | A separate environment-gated workflow is pinned to the reviewed v3/v2 SHAs and build runs. It permits only Feed Ingestor and Recorder replacement, one service at a time, while fixed identities, mounts, protected-storage metadata, JetStream resources, database integrity, and zero-execution evidence fail closed. A bounded transient unit prevents SSH loss from becoming a rollback signal. |
| Migration tampering | Migration runner stores SHA-256 checksums and fails if an already-applied migration changes. |
| NATS network exposure | NATS is only on the Docker network and not published to the host. |
| PostgreSQL exposure | PostgreSQL is only on the Docker network and uses production env credentials, not local defaults. |
| Dashboard exposure | Dashboard binds to `127.0.0.1` in production. Treat operator tunneling/proxying as a separate access-control decision. |
| Unsafe rollback | Rollback restores exact digest-backed previous release through the deploy-context-only installer and reports success only after health and protected-storage continuity pass. General bootstrap is forbidden in release and rollback paths. |

## Contract And Hot Path Controls

- The executor is not a generic arbitrary-call wallet.
- Approved flash provider, assets, pool factories, and pools are enforced.
- Flash callback verifies provider, initiator, asset, amount, and active context.
- V3 callbacks are rejected unless they match active execution context and approved pool configuration.
- Hot path has zero external RPC reads.
- Read RPC goes through budgets, caches, and validation.
- Unsupported origins and incomplete state are measured and rejected.

## Security Tooling

Run:

```bash
./scripts/secret-scan.sh
powershell -ExecutionPolicy Bypass -File .\scripts\secret-scan.ps1
cargo audit
govulncheck ./...
slither contracts/src/PhoenixExecutor.sol
```

Unavailable tools must be documented in the verification report.
