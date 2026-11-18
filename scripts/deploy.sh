#!/bin/bash
# =============================================================================
# Mazimi — One-Command Production Deployment
# Usage: ./scripts/deploy.sh
# Prerequisites: Docker, Docker Compose v2, domain DNS A record pointing to VPS
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$REPO_ROOT/.env.production"
ENV_EXAMPLE="$REPO_ROOT/.env.production.example"
COMPOSE="docker compose --env-file $ENV_FILE -f $REPO_ROOT/compose.yml"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log()  { echo -e "${CYAN}[deploy]${NC} $*"; }
ok()   { echo -e "${GREEN}[ok]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
die()  { echo -e "${RED}[error]${NC} $*" >&2; exit 1; }

# -----------------------------------------------------------------------------
# 1. Check prerequisites
# -----------------------------------------------------------------------------
log "Checking prerequisites..."

command -v docker >/dev/null 2>&1 || die "Docker is not installed. See https://docs.docker.com/engine/install/"
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 plugin not found. Run: apt install docker-compose-plugin"
command -v openssl >/dev/null 2>&1 || die "openssl is required for secret generation."

DOCKER_VERSION=$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo "unknown")
ok "Docker $DOCKER_VERSION detected"

# -----------------------------------------------------------------------------
# 2. Copy .env.production.example → .env.production (if not exists)
# -----------------------------------------------------------------------------
if [[ ! -f "$ENV_FILE" ]]; then
    log "Creating .env.production from example..."
    cp "$ENV_EXAMPLE" "$ENV_FILE"
    warn ".env.production created. Please review and fill in real values before continuing."
    warn "  Edit: $ENV_FILE"
    echo ""
    read -rp "Press Enter after you have configured .env.production (Ctrl+C to abort): "
fi

ok ".env.production found"

# Source env file for validation
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

# -----------------------------------------------------------------------------
# 3. Generate secrets if placeholders remain
# -----------------------------------------------------------------------------
log "Checking secrets..."

_replace_secret() {
    local key="$1"
    local length="${2:-48}"
    local encoding="${3:-base64}"
    local current
    current=$(grep "^${key}=" "$ENV_FILE" | cut -d'=' -f2- | tr -d '"' || true)
    if [[ -z "$current" || "$current" == *"CHANGE_ME"* ]]; then
        local secret
        case "$encoding" in
            base64) secret=$(openssl rand -base64 "$length" | tr -d '\n') ;;
            hex) secret=$(openssl rand -hex "$length") ;;
            *) die "Unsupported secret encoding: $encoding" ;;
        esac
        if grep -q "^${key}=" "$ENV_FILE"; then
            # Use | as delimiter to avoid issues with / in base64.
            sed -i.bak "s|^${key}=.*|${key}=${secret}|" "$ENV_FILE"
        else
            printf '\n%s=%s\n' "$key" "$secret" >> "$ENV_FILE"
        fi
        ok "Generated ${key}"
    else
        ok "${key} already set"
    fi
}

_replace_secret "JWT_SECRET" 48
# URI-safe hex avoids accidental parsing errors in DATABASE_URL.
_replace_secret "DB_PASSWORD" 32 hex
_replace_secret "OFFICIAL_PAGE_MCP_TOKEN" 48 hex

# Remove sed backup files
rm -f "$ENV_FILE.bak"
chmod 600 "$ENV_FILE"

# Re-source after secret generation
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

DATABASE_MODE="${DATABASE_MODE:-auto}"
case "$DATABASE_MODE" in
    auto)
        if [[ "$DATABASE_URL" == *"@db:"* ]]; then
            DATABASE_MODE="local"
        else
            DATABASE_MODE="external"
        fi
        ;;
    local|external) ;;
    *) die "DATABASE_MODE must be one of: auto, local, external" ;;
esac

if [[ "$DATABASE_MODE" == "local" ]]; then
    if [[ -z "${DB_PASSWORD:-}" || "$DB_PASSWORD" == *"CHANGE_ME"* ]]; then
        die "DB_PASSWORD is required when DATABASE_MODE=local"
    fi
    # Keep the generated compose password and SQLx DSN in lock-step. The
    # example intentionally contains a placeholder so a first deployment can
    # be completed without hand-copying a generated secret.
    LOCAL_DATABASE_URL="postgres://wello:${DB_PASSWORD}@db:5432/wello"
    if grep -q '^DATABASE_URL=' "$ENV_FILE"; then
        sed -i.bak "s|^DATABASE_URL=.*|DATABASE_URL=${LOCAL_DATABASE_URL}|" "$ENV_FILE"
    else
        printf '\nDATABASE_URL=%s\n' "$LOCAL_DATABASE_URL" >> "$ENV_FILE"
    fi
    rm -f "$ENV_FILE.bak"
    DATABASE_URL="$LOCAL_DATABASE_URL"
    ok "Database mode: local compose PostgreSQL"
else
    ok "Database mode: external PostgreSQL"
fi

# Validate required non-empty variables after local DATABASE_URL synthesis.
REQUIRED_VARS=(DOMAIN DATABASE_URL JWT_SECRET)
for var in "${REQUIRED_VARS[@]}"; do
    val="${!var:-}"
    if [[ -z "$val" || "$val" == *"CHANGE_ME"* ]]; then
        die "Required variable ${var} is not set or still has placeholder value. Edit $ENV_FILE"
    fi
done

ok "All required secrets validated"

if [[ ! "$DOMAIN" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]]; then
    die "DOMAIN must be a bare hostname without a scheme, path, port, or trailing slash"
fi

# Long-running SQLx services require the Supabase direct or session-pooler
# connection. The transaction pooler does not support prepared statements.
if [[ "$DATABASE_URL" == *".pooler.supabase.com:6543/"* ]]; then
    die "DATABASE_URL uses Supabase transaction pooling (port 6543). Use the direct or session pooler on port 5432."
fi

# -----------------------------------------------------------------------------
# 3b. Host disk: install the daily cleanup timer, clean, refuse a low-disk build
# -----------------------------------------------------------------------------
# A host build adds roughly 3 GB; a full disk took the API down once. The
# cleanup only removes old rollback tags and stale build cache (never volumes
# or images in use). DEPLOY_MIN_FREE_GB (default 20) is the floor.
if [[ $EUID -eq 0 ]] && command -v systemctl >/dev/null 2>&1 && [[ -d /etc/systemd/system ]]; then
    for unit in mazimi-host-cleanup.service mazimi-host-cleanup.timer; do
        # Units name the default checkout /opt/mazimi; point them at this one.
        sed "s#/opt/mazimi#$REPO_ROOT#g" "$REPO_ROOT/scripts/systemd/$unit" > "/etc/systemd/system/$unit"
        chmod 0644 "/etc/systemd/system/$unit"
    done
    systemctl daemon-reload
    systemctl enable --now mazimi-host-cleanup.timer >/dev/null
    ok "Host cleanup timer installed and enabled"
else
    warn "Not root or no systemd: host cleanup timer not installed"
fi

log "Cleaning old rollback image tags and stale build cache..."
bash "$REPO_ROOT/scripts/host-cleanup.sh" --apply || warn "Host cleanup reported an error; continuing to the disk check"
MIN_FREE_GB="${DEPLOY_MIN_FREE_GB:-20}"
[[ "$MIN_FREE_GB" =~ ^[0-9]+$ ]] || die "DEPLOY_MIN_FREE_GB must be a whole number of GB"
FREE_KB=$(df -Pk / | awk 'NR==2 {print $4}')
if (( FREE_KB < MIN_FREE_GB * 1048576 )); then
    die "Only $((FREE_KB / 1048576)) GB free after cleanup (minimum ${MIN_FREE_GB} GB). Free space before building."
fi
ok "$((FREE_KB / 1048576)) GB free (minimum ${MIN_FREE_GB} GB)"

# -----------------------------------------------------------------------------
# 4. Pull/build Docker images
# -----------------------------------------------------------------------------
log "Building Docker images (this may take a few minutes on first run)..."
$COMPOSE build --parallel

ok "Images built"

# -----------------------------------------------------------------------------
# 5. Start infrastructure and run migrations
# -----------------------------------------------------------------------------
if [[ "$DATABASE_MODE" == "local" ]]; then
    log "Starting local database..."
    $COMPOSE up -d db

    log "Waiting for local database to be healthy..."
    WAIT=0
    MAX_WAIT=60
    until $COMPOSE exec -T db pg_isready -U wello -q; do
        WAIT=$((WAIT + 2))
        if [[ $WAIT -ge $MAX_WAIT ]]; then
            die "Database did not become healthy within ${MAX_WAIT}s. Check logs: $COMPOSE logs db"
        fi
        sleep 2
    done
    ok "Local database is ready"
else
    log "Using external database; local PostgreSQL will not be started"
fi

if [[ "${PRE_DEPLOY_BACKUP:-true}" == "true" ]]; then
    log "Creating a pre-migration database backup..."
    "$REPO_ROOT/scripts/backup.sh"
    ok "Pre-migration backup complete"
else
    warn "PRE_DEPLOY_BACKUP=false — skipping the pre-migration backup"
fi

log "Running SQLx migrations..."
$COMPOSE run --rm --no-deps api api-migrate
ok "Migrations applied"

# -----------------------------------------------------------------------------
# 6. Start all services
# -----------------------------------------------------------------------------
log "Starting all services..."
SERVICES=(api worker web caddy)
if [[ "$DATABASE_MODE" == "local" ]]; then
    SERVICES=(db "${SERVICES[@]}")
fi
$COMPOSE up -d "${SERVICES[@]}"

ok "All services started"

# -----------------------------------------------------------------------------
# 7. Health check
# -----------------------------------------------------------------------------
log "Running health checks (up to 90s)..."

_wait_healthy() {
    local service="$1"
    local max="${2:-90}"
    local waited=0
    local health=""
    local container_id=""
    container_id=$($COMPOSE ps -q "$service")
    if [[ -z "$container_id" ]]; then
        warn "No container found for ${service}"
        return 1
    fi
    until [[ "$health" == "healthy" ]]; do
        health=$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$container_id" 2>/dev/null || true)
        if [[ "$health" == "exited" || "$health" == "dead" ]]; then
            warn "${service} stopped before becoming healthy"
            return 1
        fi
        waited=$((waited + 3))
        if [[ $waited -ge $max ]]; then
            warn "Health check timed out for ${service}. Run: $COMPOSE logs ${service}"
            return 1
        fi
        sleep 3
    done
    ok "${service} is healthy"
}

for service in api worker web; do
    if ! _wait_healthy "$service" 120; then
        $COMPOSE logs --tail=100 "$service" || true
        die "Deployment aborted because ${service} is not healthy"
    fi
done

# Caddy exposes 80/443; check HTTP redirect
if curl -sf --max-time 5 "http://${DOMAIN}/" -o /dev/null -w "%{http_code}" 2>/dev/null | grep -qE "^(200|301|302)$"; then
    ok "Caddy is routing traffic"
else
    warn "Could not reach http://${DOMAIN}/ — DNS may not have propagated yet"
fi

# -----------------------------------------------------------------------------
# 8. Print success message
# -----------------------------------------------------------------------------
echo ""
echo -e "${BOLD}${GREEN}============================================================${NC}"
echo -e "${BOLD}${GREEN}  Mazimi deployed successfully!${NC}"
echo -e "${BOLD}${GREEN}============================================================${NC}"
echo ""
echo -e "  Web app  : ${CYAN}https://${DOMAIN}${NC}"
echo -e "  API      : ${CYAN}https://${DOMAIN}/api${NC}"
echo -e "  Health   : ${CYAN}https://${DOMAIN}/api/health${NC}"
echo ""
echo -e "  Useful commands:"
echo -e "    make logs     — tail all service logs"
echo -e "    make status   — check container health"
echo -e "    make backup   — backup the database"
echo -e "    make restart  — rolling restart"
echo ""
echo -e "  SSL certificate is provisioned automatically by Caddy."
echo -e "  First HTTPS request may take 10-30s while Let's Encrypt issues the cert."
echo ""
