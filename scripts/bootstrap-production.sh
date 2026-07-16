#!/usr/bin/env sh
set -eu

release_sha=${1:-}
if [ -n "$release_sha" ]; then
  case "$release_sha" in
    *[!0-9a-f]*) echo "RELEASE_ASSET_INVALID: release SHA must be 40 lowercase hex characters"; exit 1 ;;
  esac
  [ "${#release_sha}" -eq 40 ] || { echo "RELEASE_ASSET_INVALID: release SHA must be 40 lowercase hex characters"; exit 1; }
fi

if [ "$(id -u)" -ne 0 ]; then
  echo "bootstrap-production.sh must run as root"
  exit 1
fi

if [ "$(uname -s)" != "Linux" ]; then
  echo "unsupported OS: Linux is required"
  exit 1
fi

arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) ;;
  *) echo "unsupported architecture: $arch"; exit 1 ;;
esac

if [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  [ "${ID:-}" = "ubuntu" ] || echo "WARNING: Ubuntu 24.04 LTS is the supported target"
  [ "${VERSION_ID:-}" = "24.04" ] || echo "WARNING: Ubuntu 24.04 LTS is the supported target"
fi

if ! command -v docker >/dev/null 2>&1 || ! docker compose version >/dev/null 2>&1; then
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io docker-compose-v2
fi

if ! id phoenix >/dev/null 2>&1; then
  useradd --system --home-dir /opt/phoenix --create-home --shell /usr/sbin/nologin phoenix
fi

install -d -m 0750 -o phoenix -g phoenix /opt/phoenix
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/manifests
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/.runtime
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data/postgres
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data/prometheus
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data/feed
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/logs
install -d -m 0755 -o phoenix -g phoenix /opt/phoenix/evidence
install -d -m 0755 -o phoenix -g phoenix /opt/phoenix/evidence/dashboard
install -d -m 0750 -o root -g root /etc/phoenix

if [ ! -f /etc/phoenix/phoenix.env ]; then
  echo "MISSING_ENV_FILE: create /etc/phoenix/phoenix.env as root:root 0600 before starting production"
  exit 1
fi

owner="$(stat -c '%U:%G' /etc/phoenix/phoenix.env)"
mode="$(stat -c '%a' /etc/phoenix/phoenix.env)"
[ "$owner" = "root:root" ] || { echo "ENV_INVALID: /etc/phoenix/phoenix.env must be root:root"; exit 1; }
[ "$mode" = "600" ] || { echo "ENV_INVALID: /etc/phoenix/phoenix.env must be mode 600"; exit 1; }

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "$script_dir/.." && pwd)"

install -m 0640 -o phoenix -g phoenix "$repo_root/compose.prod.yml" /opt/phoenix/deploy/compose.prod.yml
install -m 0644 -o phoenix -g phoenix "$repo_root/deploy/nats-server.conf" /opt/phoenix/deploy/nats-server.conf
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/prometheus
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/sql
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/schemas
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/routes
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/deploy/contracts
install -m 0640 -o phoenix -g phoenix "$repo_root/prometheus/prometheus.yml" /opt/phoenix/deploy/prometheus/prometheus.yml
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/scripts/sql/shadow-profitability-report.sql" \
  /opt/phoenix/deploy/sql/shadow-profitability-report.sql
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/scripts/sql/shadow-route-discovery-enrichment.sql" \
  /opt/phoenix/deploy/sql/shadow-route-discovery-enrichment.sql
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/scripts/sql/prelive-money-path-report.sql" \
  /opt/phoenix/deploy/sql/prelive-money-path-report.sql
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/scripts/sql/prelive-dashboard-source.sql" \
  /opt/phoenix/deploy/sql/prelive-dashboard-source.sql
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/schemas/prelive-money-path-summary.schema.json" \
  /opt/phoenix/deploy/schemas/prelive-money-path-summary.schema.json
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/schemas/prelive-shadow-control-evidence.schema.json" \
  /opt/phoenix/deploy/schemas/prelive-shadow-control-evidence.schema.json
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/schemas/phoenix-release-assets.schema.json" \
  /opt/phoenix/deploy/schemas/phoenix-release-assets.schema.json
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/dashboard/snapshot_model.py" \
  /opt/phoenix/deploy/snapshot_model.py
install -m 0640 -o phoenix -g phoenix \
  "$repo_root/fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json" \
  /opt/phoenix/deploy/routes/arbitrum_uniswap_v3_pool_proofs.json
for script in \
  production_context.py \
  render-production-compose.sh \
  verify-compose-route-registry.py \
  validate-production-release-context.sh \
  validate-production-env.sh \
  production-healthcheck.sh \
  shadow-engine-isolated-canary.sh \
  shadow-positive-route-evidence.sh \
  shadow-profitability-report.sh \
  shadow_profitability_report.py \
  shadow-route-discovery.sh \
  shadow_route_discovery.py \
  prelive-money-path-report.sh \
  prelive_money_path_report.py \
  prelive-protected-maintenance.sh \
  prelive_protected_maintenance.py \
  prelive_dashboard_snapshot.py \
  prelive_dashboard_live.py \
  prelive_shadow_control.py \
  prelive-shadow-control.sh \
  release_assets.py \
  install-release-assets.sh \
  verify_dashboard_compose.py \
  rollback-release.sh \
  deploy-release.sh
do
  install -m 0750 -o phoenix -g phoenix "$repo_root/scripts/$script" "/opt/phoenix/deploy/$script"
done

if [ -n "$release_sha" ]; then
  [ -f "$repo_root/release-assets-manifest.json" ] || { echo "RELEASE_ASSET_INVALID: bundled manifest is missing"; exit 1; }
  [ -f "$repo_root/contracts/PhoenixExecutor.compiled.json" ] || { echo "RELEASE_ASSET_INVALID: compiled contract artifact is missing"; exit 1; }
  install -m 0640 -o phoenix -g phoenix \
    "$repo_root/release-assets-manifest.json" \
    /opt/phoenix/deploy/release-assets-manifest.json
  install -m 0640 -o phoenix -g phoenix \
    "$repo_root/contracts/PhoenixExecutor.compiled.json" \
    /opt/phoenix/deploy/contracts/PhoenixExecutor.compiled.json
fi

"/opt/phoenix/deploy/validate-production-env.sh" /etc/phoenix/phoenix.env
docker version >/dev/null
docker compose version >/dev/null

if [ -n "$release_sha" ]; then
  marker=$(mktemp /opt/phoenix/deploy/.release-assets.XXXXXX) || { echo "RELEASE_ASSET_INVALID: marker staging failed"; exit 1; }
  printf '%s\n' "$release_sha" >"$marker"
  chown phoenix:phoenix "$marker"
  chmod 0640 "$marker"
  mv "$marker" /opt/phoenix/deploy/release-assets.sha
fi

echo "BOOTSTRAP_OK: production assets installed under /opt/phoenix/deploy"
echo "FIREWALL_EXPECTATION: expose SSH only as intended; dashboard and Prometheus bind to 127.0.0.1"
echo "SHADOW_DEFAULT: PHOENIX_MODE=SHADOW and LIVE_EXECUTION=false are required"
