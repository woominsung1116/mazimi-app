#!/bin/bash
# =============================================================================
# Mazimi — host disk cleanup (images and build cache only)
# Usage: ./scripts/host-cleanup.sh            # dry run: log what would go
#        ./scripts/host-cleanup.sh --apply    # remove it
#
# What it removes:
#   - rollback image tags (pre-*) of the app images beyond the newest
#     HOST_CLEANUP_KEEP_TAGS (default 4) per image, newest by image creation
#     time; a tag whose image is used by any container (running or stopped)
#     or is also the image's :latest is always kept. Only the tag is removed
#     (docker image rm without --force); the image layers go only when no
#     other tag references them.
#   - dangling build cache older than 7 days (docker builder prune, never
#     --all).
# What it never touches: volumes (named or anonymous), running or stopped
# containers, images in use, base images, networks. No `system prune`,
# `volume prune` or `image prune -a` is ever run.
# =============================================================================
set -euo pipefail

APPLY=false
case "${1:-}" in
    --apply) APPLY=true ;;
    ""|--dry-run) ;;
    *) echo "Usage: $0 [--dry-run|--apply]" >&2; exit 2 ;;
esac

KEEP_TAGS="${HOST_CLEANUP_KEEP_TAGS:-4}"
REPOS="${HOST_CLEANUP_REPOS:-mazimi-api mazimi-worker mazimi-web}"
CACHE_UNTIL="${HOST_CLEANUP_CACHE_UNTIL:-168h}"
[[ "$KEEP_TAGS" =~ ^[1-9][0-9]*$ ]] || { echo "HOST_CLEANUP_KEEP_TAGS must be a positive integer" >&2; exit 2; }
[[ "$CACHE_UNTIL" =~ ^[0-9]+h$ ]] || { echo "HOST_CLEANUP_CACHE_UNTIL must look like 168h" >&2; exit 2; }

MODE=$([[ "$APPLY" == true ]] && echo apply || echo dry-run)
log() { echo "[host-cleanup $(date -u '+%Y-%m-%dT%H:%M:%SZ') $MODE] $*"; }

command -v docker >/dev/null 2>&1 || { log "docker not found; nothing to do"; exit 0; }

free_gb() { df -Pk / | awk 'NR==2 {printf "%.1f", $4/1048576}'; }
log "free disk before: $(free_gb) GB"

# Full image IDs referenced by any container, running or stopped.
IN_USE="$(docker ps -aq | xargs -r docker inspect --format '{{.Image}}' 2>/dev/null | sort -u || true)"

removed=0
for repo in $REPOS; do
    latest_id="$(docker image inspect --format '{{.Id}}' "${repo}:latest" 2>/dev/null || true)"
    # "<created> <tag>" for pre-* tags, newest first.
    tags="$(
        docker image ls "$repo" --format '{{.CreatedAt}}|{{.Tag}}' 2>/dev/null \
            | awk -F'|' '$2 ~ /^pre-/' | sort -r | cut -d'|' -f2 || true
    )"
    count=0
    while IFS= read -r tag; do
        [[ -n "$tag" ]] || continue
        count=$((count + 1))
        ref="${repo}:${tag}"
        if (( count <= KEEP_TAGS )); then
            log "keep  $ref (newest $KEEP_TAGS)"
            continue
        fi
        id="$(docker image inspect --format '{{.Id}}' "$ref" 2>/dev/null || true)"
        if [[ -z "$id" ]]; then
            continue
        fi
        if grep -qxF "$id" <<<"$IN_USE"; then
            log "keep  $ref (image used by a container)"
            continue
        fi
        if [[ -n "$latest_id" && "$id" == "$latest_id" ]]; then
            log "keep  $ref (same image as ${repo}:latest)"
            continue
        fi
        if [[ "$APPLY" == true ]]; then
            if docker image rm "$ref" >/dev/null; then
                log "removed tag $ref"
                removed=$((removed + 1))
            else
                log "could not remove $ref; left in place"
            fi
        else
            log "would remove tag $ref"
            removed=$((removed + 1))
        fi
    done <<<"$tags"
done
log "rollback tags $([[ "$APPLY" == true ]] && echo removed || echo to remove): $removed"

if [[ "$APPLY" == true ]]; then
    reclaimed="$(docker builder prune --force --filter "until=${CACHE_UNTIL}" 2>/dev/null | tail -n 1 || true)"
    log "build cache older than ${CACHE_UNTIL} pruned: ${reclaimed:-nothing}"
else
    log "would run: docker builder prune --force --filter until=${CACHE_UNTIL} (dangling cache only)"
    docker system df 2>/dev/null | awk 'NR==1 || /Build Cache/' | while read -r line; do log "  $line"; done
fi

log "free disk after: $(free_gb) GB"
