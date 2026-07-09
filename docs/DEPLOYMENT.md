# Deployment

Target first deployment: one Ubuntu 24.04 LTS x86_64 VPS.

Minimum sizing is not measured in this workspace. Recommended first shadow host:

- 4 vCPU
- 8 GB RAM
- 80 GB SSD
- Docker Engine and Compose plugin
- outbound access to Arbitrum feed and configured RPC providers

Production host layout:

```text
/opt/phoenix/
    deploy/
        compose.prod.yml
        current-release
        previous-release
        manifests/
    data/
        postgres/
        prometheus/
        feed/
    logs/

/etc/phoenix/
    phoenix.env
```

The production host pulls prebuilt GHCR images. It does not build Phoenix application source.

Do not expose internal services publicly:

- Postgres: internal Docker network only
- NATS: internal Docker network only
- Nitro relay feed: internal Docker network only
- RPC gateway: internal Docker network only

Explicit loopback bindings:

- Dashboard on `127.0.0.1:8501`
- Prometheus on `127.0.0.1:9090`

Production bootstrap, GHCR authentication, environment validation, release, and rollback are documented in:

- `docs/PRODUCTION_BOOTSTRAP.md`
- `docs/RELEASE_AND_ROLLBACK.md`
- `docs/CI_CD.md`

Current blocker: real Nitro feed relay ingestion is implemented for first runtime verification but not live-verified. Production feed startup fails intentionally until `docs/NITRO_FEED_INTEGRATION.md` is resolved with real-feed evidence.
