# Production Bootstrap

Target host:

- Ubuntu 24.04 LTS
- x86_64 / amd64
- single VPS
- deployment root `/opt/phoenix`
- runtime env file `/etc/phoenix/phoenix.env`

The production host pulls immutable GHCR images. It does not build Phoenix source and does not need Rust, Go, Foundry, or Python installed for application builds.

## Runtime Env File

Create `/etc/phoenix/phoenix.env` as `root:root` with mode `0600`.

Required SHADOW categories:

- Phoenix mode: `PHOENIX_ENV=production`, `PHOENIX_MODE=SHADOW`, `LIVE_EXECUTION=false`, `CHAIN_ID=42161`
- PostgreSQL credentials and DSN
- NATS URL
- Nitro relay source URL and parent-chain RPC URL
- RPC gateway provider URLs, weights, and global budget

Optional integration categories:

- Sushi V3 registry values after official verification
- Arbitrum fork/integration RPC URLs for trusted validation contexts

LIVE-only categories:

- `EXECUTOR_ADDRESS`
- `SIGNER_PRIVATE_KEY`

SHADOW startup must not require `SIGNER_PRIVATE_KEY`.

## Bootstrap

Run from a trusted Phoenix release checkout or deployment asset bundle:

```bash
sudo sh scripts/bootstrap-production.sh
```

The script validates Linux, Ubuntu compatibility, amd64 architecture, Docker Engine, Docker Compose plugin, production directories, `/etc/phoenix/phoenix.env` ownership and permissions, and required environment variable shape. It installs Compose, the bounded NATS JetStream server configuration, Prometheus, and deployment scripts into `/opt/phoenix/deploy`.

It does not request the Ethereum signer key. It does not start LIVE.

## GHCR Authentication

GitHub Actions publishes images with `GITHUB_TOKEN`. The production host uses a separate least-privilege package pull credential.

Authenticate without putting a token in shell history:

```bash
docker login ghcr.io --username <github-user> --password-stdin
```

Paste the read-only package token into stdin when prompted by your terminal workflow. Do not store the token in Git.

## Firewall

Internal services are not published publicly. Dashboard and Prometheus bind to `127.0.0.1` by default. Keep SSH access deliberate and do not let the bootstrap script close or mutate SSH firewall rules automatically.
