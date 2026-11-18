#!/bin/bash
# =============================================================================
# Mazimi — Verified PostgreSQL Restore
# Usage: ./scripts/restore.sh backups/wello_YYYYMMDD_HHMMSS.sql.gz
#
# This is intentionally interactive because it replaces the configured target
# database. Automation must opt in with RESTORE_CONFIRM=RESTORE.
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$REPO_ROOT/.env.production"
COMPOSE="docker compose --env-file $ENV_FILE -f $REPO_ROOT/compose.yml"
RESET_SQL="$REPO_ROOT/scripts/restore-reset.sql"
BACKUP_FILE="${1:-}"

log() { printf '[restore] %s\n' "$*"; }
die() { printf '[restore:error] %s\n' "$*" >&2; exit 1; }

[[ -n "$BACKUP_FILE" ]] || die "Usage: $0 <backup.sql.gz>"
[[ -f "$BACKUP_FILE" ]] || die "Backup not found: $BACKUP_FILE"
[[ -f "$ENV_FILE" ]] || die ".env.production not found at $ENV_FILE"
[[ -f "$RESET_SQL" ]] || die "Restore reset SQL not found: $RESET_SQL"

set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

[[ -n "${DATABASE_URL:-}" ]] || die "DATABASE_URL is not configured"
command -v gzip >/dev/null 2>&1 || die "gzip is required"

log "Verifying gzip stream"
gzip -t "$BACKUP_FILE" || die "Backup gzip verification failed"

CHECKSUM_FILE="$BACKUP_FILE.sha256"
if [[ -f "$CHECKSUM_FILE" ]]; then
    BACKUP_DIR="$(cd "$(dirname "$BACKUP_FILE")" && pwd)"
    CHECKSUM_NAME="$(basename "$CHECKSUM_FILE")"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$BACKUP_DIR" && sha256sum -c "$CHECKSUM_NAME") \
            || die "Backup checksum verification failed"
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$BACKUP_DIR" && shasum -a 256 -c "$CHECKSUM_NAME") \
            || die "Backup checksum verification failed"
    else
        die "sha256sum or shasum is required to verify the checksum"
    fi
else
    die "Checksum file not found: $CHECKSUM_FILE"
fi

if [[ "${RESTORE_CONFIRM:-}" != "RESTORE" ]]; then
    printf '\nTarget DATABASE_URL host will be replaced using:\n  %s\n\n' "$(basename "$BACKUP_FILE")"
    read -r -p "Type RESTORE to continue: " confirmation
    [[ "$confirmation" == "RESTORE" ]] || die "Restore cancelled"
fi

log "Stopping write-capable application services"
$COMPOSE stop api worker web >/dev/null 2>&1 || true

if [[ "${SKIP_PRE_RESTORE_BACKUP:-false}" != "true" ]]; then
    log "Creating pre-restore safety backup"
    "$REPO_ROOT/scripts/backup.sh" "$REPO_ROOT/backups/pre-restore"
else
    log "SKIP_PRE_RESTORE_BACKUP=true — no safety backup will be created"
fi

log "Applying the current application schema before restoring data"
$COMPOSE run --rm --no-deps api api-migrate

log "Replacing application data in one transaction"
if ! {
    cat "$RESET_SQL"
    gunzip -c "$BACKUP_FILE"
} | $COMPOSE run --rm -T --no-deps db-tools psql \
        --dbname="$DATABASE_URL" \
        --set=ON_ERROR_STOP=on \
        --single-transaction; then
    die "Restore failed. Application services remain stopped; inspect PostgreSQL before restarting them."
fi

log "Starting application services"
$COMPOSE up -d api worker web caddy

log "Restore completed. Verify /api/ready and the admin operation queues before reopening traffic."
