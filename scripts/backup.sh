#!/bin/bash
# =============================================================================
# Mazimi — PostgreSQL Backup Script
# Usage: ./scripts/backup.sh [backup_dir]
#
# Crontab example (daily at 03:00):
#   0 3 * * * /opt/mazimi/scripts/backup.sh >> /var/log/mazimi-backup.log 2>&1
#
# Optional S3 upload: set USE_S3=true and configure S3_BUCKET + AWS credentials.
# Backups older than RETENTION_DAYS are automatically pruned.
# =============================================================================
set -euo pipefail
# Backups may contain personal application records.
umask 077

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$REPO_ROOT/.env.production"
COMPOSE="docker compose --env-file $ENV_FILE -f $REPO_ROOT/compose.yml"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${CYAN}[backup $(date '+%H:%M:%S')]${NC} $*"; }
ok()   { echo -e "${GREEN}[ok]${NC} $*"; }
warn() { echo -e "${YELLOW}[warn]${NC} $*"; }
die()  { echo -e "${RED}[error]${NC} $*" >&2; exit 1; }

# Load production env
if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
else
    die ".env.production not found at $ENV_FILE"
fi

# Resolve settings after loading the file so scheduled runs honor it too.
BACKUP_DIR="${1:-${BACKUP_DIR:-$REPO_ROOT/backups}}"
RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-7}"
USE_S3="${USE_S3:-false}"
S3_BUCKET="${S3_BUCKET:-}"
S3_ENDPOINT_URL="${S3_ENDPOINT_URL:-}"
[[ "$RETENTION_DAYS" =~ ^[1-9][0-9]*$ ]] || die "BACKUP_RETENTION_DAYS must be a positive integer"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
BACKUP_FILE="$BACKUP_DIR/wello_${TIMESTAMP}.sql.gz"
PARTIAL_FILE="$BACKUP_FILE.partial"
CHECKSUM_FILE="$BACKUP_FILE.sha256"

mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"
cleanup_partial() { rm -f "$PARTIAL_FILE"; }
trap cleanup_partial EXIT

log "Starting backup -> $BACKUP_FILE"

# The versioned SQLx migrations are the schema source of truth. Back up only
# application data in `public`; dumping the whole Supabase database would also
# capture provider-managed schemas and extensions that cannot be restored to a
# standard PostgreSQL server. `_sqlx_migrations` remains owned by the deployed
# application version and is intentionally not copied between environments.
$COMPOSE run --rm -T --no-deps db-tools pg_dump \
    --dbname="$DATABASE_URL" \
    --format=plain \
    --schema=public \
    --data-only \
    --exclude-table-data=public._sqlx_migrations \
    --no-owner \
    --no-acl \
    | gzip -9 > "$PARTIAL_FILE"

[[ -s "$PARTIAL_FILE" ]] || die "pg_dump produced an empty backup"
gzip -t "$PARTIAL_FILE" || die "backup compression verification failed"
mv "$PARTIAL_FILE" "$BACKUP_FILE"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$BACKUP_DIR" && sha256sum "$(basename "$BACKUP_FILE")") > "$CHECKSUM_FILE"
elif command -v shasum >/dev/null 2>&1; then
    (cd "$BACKUP_DIR" && shasum -a 256 "$(basename "$BACKUP_FILE")") > "$CHECKSUM_FILE"
else
    die "sha256sum or shasum is required to checksum backups"
fi

BACKUP_SIZE=$(du -sh "$BACKUP_FILE" | cut -f1)
ok "Backup written: $BACKUP_FILE ($BACKUP_SIZE)"

# -----------------------------------------------------------------------------
# Optional: upload to S3
# -----------------------------------------------------------------------------
if [[ "$USE_S3" == "true" ]]; then
    [[ "$S3_BUCKET" == s3://* ]] || die "S3_BUCKET must be an s3:// URL when USE_S3=true"
    log "Uploading to S3: $S3_BUCKET"
    if ! command -v aws >/dev/null 2>&1; then
        warn "aws CLI not found — skipping S3 upload"
    else
        AWS_ENDPOINT_ARGS=()
        if [[ -n "$S3_ENDPOINT_URL" ]]; then
            AWS_ENDPOINT_ARGS=(--endpoint-url "$S3_ENDPOINT_URL")
        fi
        aws "${AWS_ENDPOINT_ARGS[@]}" s3 cp "$BACKUP_FILE" "$S3_BUCKET/$(basename "$BACKUP_FILE")" \
            --storage-class STANDARD_IA
        aws "${AWS_ENDPOINT_ARGS[@]}" s3 cp "$CHECKSUM_FILE" "$S3_BUCKET/$(basename "$CHECKSUM_FILE")" \
            --storage-class STANDARD_IA
        ok "Uploaded to $S3_BUCKET"
    fi
fi

# -----------------------------------------------------------------------------
# Prune old backups
# -----------------------------------------------------------------------------
log "Pruning backups older than ${RETENTION_DAYS} days..."
PRUNED=0
while IFS= read -r old_file; do
    rm -f "$old_file"
    rm -f "$old_file.sha256"
    log "  Removed: $(basename "$old_file")"
    PRUNED=$((PRUNED + 1))
done < <(find "$BACKUP_DIR" -name "wello_*.sql.gz" -mtime "+${RETENTION_DAYS}" 2>/dev/null || true)

if [[ $PRUNED -eq 0 ]]; then
    ok "No old backups to prune"
else
    ok "Pruned $PRUNED old backup(s)"
fi

# -----------------------------------------------------------------------------
# List current backups
# -----------------------------------------------------------------------------
log "Current backups in $BACKUP_DIR:"
CURRENT_COUNT=0
while IFS= read -r current_backup; do
    current_size=$(du -h "$current_backup" | cut -f1)
    log "  $current_backup ($current_size)"
    CURRENT_COUNT=$((CURRENT_COUNT + 1))
done < <(find "$BACKUP_DIR" -maxdepth 1 -type f -name 'wello_*.sql.gz' -print | sort)

if [[ $CURRENT_COUNT -eq 0 ]]; then
    log "  (none)"
fi

echo ""
echo "To restore from this backup with checksum verification and a pre-restore snapshot:"
echo "  ./scripts/restore.sh $BACKUP_FILE"
echo ""
