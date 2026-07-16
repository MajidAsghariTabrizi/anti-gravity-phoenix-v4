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
    releases/
        <release-sha>/
            release-assets-manifest.json
    deploy/
        compose.prod.yml
        current-release
        previous-release
        release-assets.sha
        manifests/
    data/
        postgres/
        prometheus/
        feed/
        # Docker volume phoenix-nats-jetstream stores JetStream data.
    evidence/
        dashboard/
            latest-dashboard.json
            # Bounded redacted report artifacts only.
    logs/

/etc/phoenix/
    phoenix.env
```

The production host pulls prebuilt GHCR images. It does not build Phoenix application source. Canonical host scripts, schemas, route proofs, and the compiled contract artifact come only from the verified exact-SHA release-assets bundle.

Do not expose internal services publicly:

- Postgres: internal Docker network only
- NATS: internal Docker network only
- Nitro relay feed: internal Docker network only
- RPC gateway: internal Docker network only

Explicit loopback bindings:

- Dashboard on `127.0.0.1:8501`
- Prometheus on `127.0.0.1:9090`

The Dashboard bind-mounts only `/opt/phoenix/evidence/dashboard` read-only. It
does not receive `/etc/phoenix/phoenix.env`, a database connection string,
Prometheus access, or the Docker socket. Access it through an SSH tunnel.

Production bootstrap, GHCR authentication, environment validation, release, and rollback are documented in:

- `docs/PRODUCTION_BOOTSTRAP.md`
- `docs/RELEASE_AND_ROLLBACK.md`
- `docs/CI_CD.md`

Current live-evidence gap: real Nitro feed relay ingestion is implemented for first SHADOW runtime verification but not live-verified. A Linux VPS validation run must still prove the pinned relay can observe and decode real Arbitrum feed messages before any production-readiness or LIVE claim.

Current deployment gate: feed-ingestor and Recorder changed in this milestone
but remain protected against recreation by normal deployment. The normal
workflow still blocks before SSH when either candidate digest differs from the
active rollback manifest. The separate protected-maintenance workflow is the
only repository-supported path for the reviewed v3/v2 pair. It updates
Recorder and Feed Ingestor one at a time while optional services remain stopped,
then promotes the v3 release context for later controlled SHADOW startup. It
has not been dispatched by repository preparation.

Recorder durability configuration and single-node storage limits are documented in `docs/RECORDER_DURABLE_DELIVERY.md`.
