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
| Pull request secret exposure | CI pull-request workflows do not use production secrets. Deployment secrets are only used by the workflow-run deployment from `main`. |
| Malicious dependency action | Third-party and official actions are pinned to full commit SHAs and documented in `docs/GITHUB_ACTIONS_DEPENDENCIES.md`. |
| Unpinned actions | Workflows must not use floating tags; Dependabot updates require review. |
| SSH key leakage | Deploy SSH key is a GitHub secret, written to `~/.ssh/id_ed25519` with mode `0600`, and never printed. Use a dedicated production deploy key. |
| Host key MITM | `PROD_KNOWN_HOSTS` is pre-verified out-of-band. Deployment uses `StrictHostKeyChecking=yes` and never trusts runtime `ssh-keyscan`. |
| GHCR credential leakage | GitHub publishes with `GITHUB_TOKEN`; production uses a separate least-privilege package pull credential entered through `docker login ghcr.io --password-stdin`. |
| Docker build secret leakage | No runtime secrets are passed as Docker build args. `.dockerignore` excludes env files, keys, runtime data, caches, and build outputs. |
| Runtime environment leakage | `/etc/phoenix/phoenix.env` must be `root:root` mode `0600`; validation reports variable names and categories, not values. |
| Production signer leakage | `SIGNER_PRIVATE_KEY` is host-only and LIVE-only. It is not required for SHADOW startup and must not exist in GitHub secrets. |
| Fixture accidentally used in production | `feed-ingestor` fails startup if `PHOENIX_ENV=production` and `PHOENIX_FEED_FIXTURE` is set. |
| LIVE accidentally enabled | Production Compose forces `PHOENIX_MODE=SHADOW` and `LIVE_EXECUTION=false`; release-live workflow cannot edit env or restart LIVE. |
| Mutable image deployment | Build workflow emits `sha-<full git sha>` tags and digests; deploy scripts reject missing digests and never consume `latest`. |
| Migration tampering | Migration runner stores SHA-256 checksums and fails if an already-applied migration changes. |
| NATS network exposure | NATS is only on the Docker network and not published to the host. |
| PostgreSQL exposure | PostgreSQL is only on the Docker network and uses production env credentials, not local defaults. |
| Dashboard exposure | Dashboard binds to `127.0.0.1` in production. Treat operator tunneling/proxying as a separate access-control decision. |
| Unsafe rollback | Rollback restores exact digest-backed previous release and reports success only after health passes. |

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
