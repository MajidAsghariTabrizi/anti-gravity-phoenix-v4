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

Initial host preparation may run from a trusted Phoenix release checkout:

```bash
sudo sh scripts/bootstrap-production.sh
```

Every deployable release must instead use the three artifacts emitted by the
successful exact-SHA image workflow:

```bash
sudo sh scripts/install-release-assets.sh \
  <release-sha> \
  phoenix-release-assets-<release-sha>.tar.gz \
  release-assets-manifest.json \
  release-assets-checksums.txt
```

The installer rejects unbounded, non-canonical, linked, traversing, or
checksum-mismatched content, retains the immutable source under
`/opt/phoenix/releases/<release-sha>`, and invokes the scoped
`install-production-release-context.sh` operation with the exact release SHA
and verified release tree. It never invokes general host provisioning.
Deployment remains blocked until
`/opt/phoenix/deploy/release-assets.sha` matches the candidate release.

The bootstrap script is only for first-host preparation. It validates Linux,
Ubuntu compatibility, amd64 architecture, Docker Engine, and Docker Compose,
then delegates persistent-directory creation to
`provision-production-host.sh`. PostgreSQL, NATS, Feed, and Recorder data are
never recursively chowned or chmodded. A non-empty PostgreSQL directory must
contain a regular `PG_VERSION`, must not be group- or world-writable, and must
have consistent ownership across the directory and critical PostgreSQL files;
unsafe ownership fails closed.

Prometheus is explicitly configured to run as numeric UID/GID `65534:65534`.
Provisioning rejects symlinks, hard-linked files, nested mounts, special files,
and non-directory paths under its dedicated
`/opt/phoenix/data/prometheus` tree before normalizing only that tree to the
runtime identity. Existing Prometheus file contents are preserved; directories
use mode `0750` and regular files use mode `0640`. The installed
`prometheus.yml` is mode `0644` so that the non-root runtime can read it.

Release-context installation separately updates only canonical files under
`/opt/phoenix/deploy`, validates `/etc/phoenix/phoenix.env`, and promotes the
asset marker last. It does not access PostgreSQL data, JetStream storage, or any
protected volume.

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
