# Deployment

Target first deployment: one Linux VPS.

Minimum sizing is not measured in this workspace. Recommended first shadow host:

- 4 vCPU
- 8 GB RAM
- 80 GB SSD
- Docker Engine and Compose plugin
- outbound access to Arbitrum feed and configured RPC providers

Do not expose internal services publicly:

- Postgres: internal only
- NATS: internal only
- Nitro relay feed: internal only
- RPC gateway: internal only

Explicitly exposed by default:

- Dashboard on localhost or operator-controlled interface
- Prometheus metrics on localhost/operator network

## VPS Bootstrap

```bash
git clone <repo-url>
cd anti-gravity-phoenix-v4
cp .env.example .env
vi .env
docker compose up --build -d
```

Before LIVE:

```bash
make verify
make contract-test
PHOENIX_MODE=SIMULATE ARBITRUM_RPC_URL=<url> make integration
```

