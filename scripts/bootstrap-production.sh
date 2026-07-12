#!/usr/bin/env sh
set -eu

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
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data/postgres
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data/prometheus
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/data/feed
install -d -m 0750 -o phoenix -g phoenix /opt/phoenix/logs
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
install -m 0640 -o phoenix -g phoenix "$repo_root/prometheus/prometheus.yml" /opt/phoenix/deploy/prometheus/prometheus.yml
for script in validate-production-env.sh production-healthcheck.sh rollback-release.sh deploy-release.sh; do
  install -m 0750 -o phoenix -g phoenix "$repo_root/scripts/$script" "/opt/phoenix/deploy/$script"
done

"/opt/phoenix/deploy/validate-production-env.sh" /etc/phoenix/phoenix.env
docker version >/dev/null
docker compose version >/dev/null

echo "BOOTSTRAP_OK: production assets installed under /opt/phoenix/deploy"
echo "FIREWALL_EXPECTATION: expose SSH only as intended; dashboard and Prometheus bind to 127.0.0.1"
echo "SHADOW_DEFAULT: PHOENIX_MODE=SHADOW and LIVE_EXECUTION=false are required"
