#!/bin/sh
set -eu
# Recovery is scoped to this Compose project; existing Oracle workloads are untouched.
for service in api worker; do
  id=$(docker ps -aq --filter label=com.docker.compose.project=mazimi --filter label=com.docker.compose.service="$service")
  [ -n "$id" ] || continue
  state=$(docker inspect --format '{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' "$id")
  case "$state" in
    *unhealthy*|exited*|dead*) docker restart "$id" >/dev/null ;;
  esac
done
