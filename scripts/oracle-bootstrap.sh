#!/bin/bash
# =============================================================================
# Mazimi — Oracle Cloud Ubuntu host bootstrap
# Run once with: sudo bash scripts/oracle-bootstrap.sh
#
# Installs Docker from Docker's signed apt repository, applies the host
# firewall, and prepares /opt/mazimi. Application secrets are never generated
# or copied by this host-level script.
# =============================================================================
set -euo pipefail

[[ "${EUID}" -eq 0 ]] || { echo "Run with sudo/root." >&2; exit 1; }

# shellcheck disable=SC1091
source /etc/os-release
[[ "${ID:-}" == "ubuntu" ]] || { echo "Ubuntu is required." >&2; exit 1; }

APP_USER="${SUDO_USER:-ubuntu}"
INSTALL_DIR="${MAZIMI_DEPLOY_DIR:-${INSTALL_DIR:-/opt/mazimi}}"
REPO_URL="${REPO_URL:-https://github.com/woominsung1116/mazimi-app.git}"

echo "[bootstrap] Installing base packages"
apt-get update
apt-get install -y ca-certificates curl git openssl ufw

echo "[bootstrap] Configuring Docker's signed apt repository"
for package in docker.io docker-compose docker-compose-v2 docker-doc podman-docker containerd runc; do
    apt-get remove -y "$package" >/dev/null 2>&1 || true
done
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc

ARCHITECTURE="$(dpkg --print-architecture)"
CODENAME="${UBUNTU_CODENAME:-$VERSION_CODENAME}"
printf '%s\n' \
    'Types: deb' \
    'URIs: https://download.docker.com/linux/ubuntu' \
    "Suites: ${CODENAME}" \
    'Components: stable' \
    "Architectures: ${ARCHITECTURE}" \
    'Signed-By: /etc/apt/keyrings/docker.asc' \
    > /etc/apt/sources.list.d/docker.sources

apt-get update
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl enable --now docker
usermod -aG docker "$APP_USER"

echo "[bootstrap] Applying host firewall rules"
ufw allow OpenSSH
ufw allow 80/tcp
ufw allow 443/tcp
ufw allow 443/udp
ufw --force enable

if [[ ! -e "$INSTALL_DIR/.git" ]]; then
    if [[ -d "$INSTALL_DIR" ]] && [[ -n "$(ls -A "$INSTALL_DIR" 2>/dev/null)" ]]; then
        echo "[bootstrap:error] $INSTALL_DIR exists and is not an empty Git checkout" >&2
        exit 1
    fi
    mkdir -p "$(dirname "$INSTALL_DIR")"
    git clone "$REPO_URL" "$INSTALL_DIR"
fi
chown -R "$APP_USER":"$APP_USER" "$INSTALL_DIR"

docker version
docker compose version

echo
echo "[bootstrap] Host ready at $INSTALL_DIR"
echo "Log out and back in so $APP_USER receives Docker group membership, then:"
echo "  cd $INSTALL_DIR"
echo "  cp .env.production.example .env.production"
echo "  nano .env.production"
echo "  ./scripts/deploy.sh"
