#!/usr/bin/env bash
# Every compose file must parse and resolve, alone and with each optional
# overlay, the way the production host runs it:
#   docker compose -f compose.yml -f infra/docker/<overlay>.yml --profile '*' config
# Overlays that require a pinned image (${VAR:?...}) get a placeholder pin here,
# so only syntax and structure are checked, not the operator's pin. An overlay
# that is invalid YAML (planner, 29a) stops the deploy before any swap; this
# catches it in CI instead.
set -euo pipefail
cd "$(dirname "$0")/.."

env_file=$(mktemp)
trap 'rm -f "$env_file"' EXIT
# Placeholder pins for the overlays' required variables.
cat > "$env_file" <<'EOF'
PLANNER_OLLAMA_IMAGE=ollama/ollama:0.0.0-check
OLLAMA_IMAGE=ollama/ollama:0.0.0-check
EOF

compose() {
  docker compose --project-directory . --env-file "$env_file" "$@" --profile '*' config -q 2>&1 \
    | grep -v 'variable is not set. Defaulting to a blank string' || true
}

fail=0
check() {
  local label=$1; shift
  local out
  out=$(compose "$@")
  if docker compose --project-directory . --env-file "$env_file" "$@" --profile '*' config -q >/dev/null 2>&1; then
    echo "ok: $label"
  else
    echo "FAIL: $label"
    [ -n "$out" ] && echo "$out" | sed 's/^/  /'
    fail=1
  fi
}

check compose.yml -f compose.yml
for overlay in infra/docker/compose.*.yml; do
  check "compose.yml + $overlay" -f compose.yml -f "$overlay"
done
exit $fail
