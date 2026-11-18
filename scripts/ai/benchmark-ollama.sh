#!/usr/bin/env bash
# Benchmark AI fact extraction (A-09) on the shared Oracle ARM host.
#
# NOT RUN BY CI OR AGENTS. A person runs it on the server after reading it.
# It starts a temporary, CPU- and memory-capped Ollama container bound to
# loopback only, pulls the candidate models, runs `extraction-eval` against a
# labeled fixture, samples Ollama CPU/RSS, and checks after every model that
# the host's other services stayed healthy (Besu chain node, Postgres, the
# Mazimi API). Any failed health check stops the run and rolls back.
#
# Usage:
#   OLLAMA_IMAGE=ollama/ollama:<pinned tag that supports the models> \
#   EVAL_BIN=/opt/mazimi/bin/extraction-eval \
#   FIXTURE=/opt/mazimi/label-set.json \
#   scripts/ai/benchmark-ollama.sh --i-understand-this-runs-on-the-shared-host
#
#   scripts/ai/benchmark-ollama.sh --rollback   # remove the bench container
#
# Build EVAL_BIN for linux/aarch64 elsewhere (it needs no database):
#   cargo build --release -p mazimi-worker --bin extraction-eval
# FIXTURE must be the human-verified label set (see docs/mazimi-ai-extraction.md);
# the checked-in SAMPLE is only a harness check.
#
# Release gates checked from the results (docs/mazimi-ai-extraction.md §7):
#   overall precision >= 0.95, recall >= 0.60, evidence validity >= 0.98,
#   0 provider failures, p95 latency <= 180 s per program,
#   Ollama peak memory below MEMORY, Besu/Postgres/API healthy throughout.
#
# Threads: Ollama sizes its thread pool from the HOST's cores and ignores the
# container cpuset. On the 4-OCPU host a 2-3 core cpuset without an explicit
# thread count ran ~45x slower (qwen3.5:4b 0.19 -> 8.7 tok/s with
# num_thread=3) and every program hit the timeout. NUM_THREADS (default: the
# number of CPUs in CPUSET) is passed as --num-thread, which extraction-eval
# sends as options.num_thread on every request.
set -euo pipefail

CONTAINER="${CONTAINER:-mazimi-ollama-bench}"
VOLUME="${VOLUME:-mazimi-ollama-bench-models}"
OLLAMA_IMAGE="${OLLAMA_IMAGE:-}"
CPUSET="${CPUSET:-2,3}"           # 2 of 4 OCPUs; keep 0-1 for Besu/Postgres/API
                                  # (on a dedicated host CPUSET=1,2,3 CPUS=3 is fine)
CPUS="${CPUS:-2}"
MEMORY="${MEMORY:-8g}"
HOST_PORT="${HOST_PORT:-11435}"   # loopback only; not the production 11434
MODELS="${MODELS:-qwen3.5:4b qwen3.5:9b-q4_K_M}"
EVAL_BIN="${EVAL_BIN:-./target/release/extraction-eval}"
FIXTURE="${FIXTURE:-}"
TIMEOUT_SECS="${TIMEOUT_SECS:-180}"
BESU_CONTAINER="${BESU_CONTAINER-besu}"      # set to "" when the host runs no Besu
PG_CONTAINER="${PG_CONTAINER-mazimi-db-1}"   # set to "" when Postgres is managed elsewhere
API_READY_URL="${API_READY_URL:-http://127.0.0.1:8080/ready}"
MIN_FREE_DISK_GB="${MIN_FREE_DISK_GB:-20}"
RESULTS="${RESULTS:-./benchmark-results/$(date -u +%Y%m%dT%H%M%SZ)}"
SAMPLER_PID=""

cpuset_count() {
  # Number of CPUs in a cpuset list such as "2,3", "1-3" or "0,2-3".
  local total=0 part lo hi parts
  IFS=',' read -ra parts <<< "$1"
  for part in "${parts[@]}"; do
    if [[ "$part" =~ ^([0-9]+)-([0-9]+)$ ]]; then
      lo="${BASH_REMATCH[1]}"; hi="${BASH_REMATCH[2]}"
      (( hi >= lo )) || return 1
      total=$(( total + hi - lo + 1 ))
    elif [[ "$part" =~ ^[0-9]+$ ]]; then
      total=$(( total + 1 ))
    else
      return 1
    fi
  done
  echo "$total"
}
NUM_THREADS="${NUM_THREADS:-$(cpuset_count "$CPUSET" || echo invalid)}"

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
die() { log "ERROR: $*"; exit 1; }

rollback() {
  # Removes only what this script created. Models stay in $VOLUME unless
  # ROLLBACK_REMOVE_MODELS=1, so a re-run does not re-download them.
  if [[ -n "$SAMPLER_PID" ]]; then kill "$SAMPLER_PID" 2>/dev/null || true; fi
  # Only a container this script labelled is ever removed, whatever
  # CONTAINER is set to (it must never touch Besu or Postgres).
  if docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER"; then
    if [[ "$(docker inspect -f '{{index .Config.Labels "mazimi.bench"}}' "$CONTAINER")" == 1 ]]; then
      log "rollback: removing container $CONTAINER"
      docker rm -f "$CONTAINER" >/dev/null
    else
      log "rollback: $CONTAINER is not a benchmark container; left untouched"
    fi
  fi
  if [[ "${ROLLBACK_REMOVE_MODELS:-0}" == "1" ]] && docker volume ls --format '{{.Name}}' | grep -qx "$VOLUME"; then
    log "rollback: removing model volume $VOLUME"
    docker volume rm "$VOLUME" >/dev/null
  fi
}

health() {
  # Prints one line per service and returns non-zero if any is unhealthy.
  local ok=0 state
  # An empty BESU_CONTAINER / PG_CONTAINER means the host does not run that
  # service (e.g. the database is managed elsewhere); its check is skipped.
  if [[ -z "$BESU_CONTAINER" ]]; then
    echo "besu=skipped"
  elif docker ps -a --format '{{.Names}}' | grep -qx "$BESU_CONTAINER"; then
    state="$(docker inspect -f '{{.State.Status}}{{if .State.Health}}/{{.State.Health.Status}}{{end}}' "$BESU_CONTAINER")"
    [[ "$state" == running || "$state" == running/healthy ]] || ok=1
    echo "besu=$state"
  else
    echo "besu=absent(${BESU_CONTAINER})"; ok=1
  fi
  if [[ -z "$PG_CONTAINER" ]]; then
    echo "postgres=skipped"
  elif docker exec "$PG_CONTAINER" pg_isready -q -U wello 2>/dev/null; then echo "postgres=ready"; else echo "postgres=NOT_READY"; ok=1; fi
  if curl --fail --silent --max-time 4 "$API_READY_URL" >/dev/null; then echo "api=ready"; else echo "api=NOT_READY"; ok=1; fi
  return "$ok"
}

api_ms() {
  # Readiness round trip in ms. A failed request still reports its elapsed
  # time; health() is what checks the status.
  curl --silent --output /dev/null --max-time 4 --write-out '%{time_total}' "$API_READY_URL" 2>/dev/null \
    | awk '{printf "%d", $1 * 1000}' || echo 99999
}

p95() { sort -n | awk '{v[NR]=$1} END {if (NR) {i=int(NR*0.95+0.999); print v[i]} else print "-"}'; }

sample() {
  # 2-second samples: Ollama CPU/memory, host load and free memory.
  local out="$1"
  echo "ts,ollama_cpu,ollama_mem,load1,host_mem_avail_mb,api_ready_ms" > "$out"
  while true; do
    local stats load avail api_ms
    stats="$(docker stats --no-stream --format '{{.CPUPerc}},{{.MemUsage}}' "$CONTAINER" 2>/dev/null || echo ',')"
    load="$(cut -d' ' -f1 /proc/loadavg)"
    avail="$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)"
    api_ms="$(api_ms)"
    echo "$(date -u +%s),${stats%%/*},$load,$avail,$api_ms" >> "$out"
    sleep 2
  done
}

case "${1:-}" in
  --rollback) rollback; exit 0 ;;
  --i-understand-this-runs-on-the-shared-host) ;;
  *) sed -n '2,34p' "$0"; exit 2 ;;
esac

[[ "$NUM_THREADS" =~ ^[0-9]+$ ]] && (( NUM_THREADS >= 1 && NUM_THREADS <= 64 )) \
  || die "NUM_THREADS must be 1..64 (CPUSET '$CPUSET' could not be counted)"
[[ -n "$OLLAMA_IMAGE" ]] || die "set OLLAMA_IMAGE to a pinned ollama/ollama tag (no :latest) that supports the models"
[[ "$OLLAMA_IMAGE" != *:latest && "$OLLAMA_IMAGE" == *:* ]] || die "OLLAMA_IMAGE must be pinned to a version tag"
[[ -x "$EVAL_BIN" ]] || die "EVAL_BIN $EVAL_BIN is not executable (build extraction-eval for aarch64)"
[[ -f "$FIXTURE" ]] || die "FIXTURE must point to the labeled JSON fixture"
command -v jq >/dev/null || die "jq is required"
free_gb="$(df -BG --output=avail / | tail -1 | tr -dc 0-9)"
(( free_gb >= MIN_FREE_DISK_GB )) || die "only ${free_gb}G free on /; need ${MIN_FREE_DISK_GB}G (models ~3-7G each + 8G headroom)"

mkdir -p "$RESULTS"
# Any exit before the end (die, set -e failure, signal) rolls back.
finished=0
on_exit() {
  local rc=$?
  if [[ $finished != 1 ]]; then log "stopped (exit $rc)"; rollback; fi
}
trap on_exit EXIT
trap 'exit 130' INT TERM
log "baseline health"
health | tee "$RESULTS/health-before.txt" || die "host is not healthy before the benchmark; not starting"

log "baseline API readiness latency (60 s)"
for _ in $(seq 1 30); do api_ms; echo; sleep 2; done > "$RESULTS/api-baseline-ms.txt"
api_baseline_p95="$(p95 < "$RESULTS/api-baseline-ms.txt")"

log "starting $CONTAINER ($OLLAMA_IMAGE, cpuset=$CPUSET, cpus=$CPUS, num_thread=$NUM_THREADS, memory=$MEMORY, 127.0.0.1:$HOST_PORT)"
docker run -d --name "$CONTAINER" --label mazimi.bench=1 \
  --cpuset-cpus "$CPUSET" --cpus "$CPUS" \
  --memory "$MEMORY" --memory-swap "$MEMORY" --oom-score-adj 500 \
  --pids-limit 256 --security-opt no-new-privileges:true \
  --log-opt max-size=10m --log-opt max-file=3 \
  -e OLLAMA_NUM_PARALLEL=1 -e OLLAMA_MAX_LOADED_MODELS=1 -e OLLAMA_KEEP_ALIVE=5m \
  -p "127.0.0.1:${HOST_PORT}:11434" \
  -v "${VOLUME}:/root/.ollama" \
  "$OLLAMA_IMAGE" >/dev/null
for _ in $(seq 1 30); do
  curl --silent --fail "http://127.0.0.1:${HOST_PORT}/api/version" >/dev/null && break
  sleep 2
done
curl --silent --fail "http://127.0.0.1:${HOST_PORT}/api/version" | tee "$RESULTS/ollama-version.json"; echo

summary="$RESULTS/summary.md"
{
  echo "# Ollama extraction benchmark $(date -u +%FT%TZ)"
  echo
  echo "image=$OLLAMA_IMAGE cpuset=$CPUSET cpus=$CPUS num_thread=$NUM_THREADS memory=$MEMORY fixture=$(basename "$FIXTURE")"
  echo
  echo "API readiness p95 before Ollama: ${api_baseline_p95} ms (gate: during-run p95 <= 1.1x)"
  echo
  echo "| model | digest | precision | recall | evidence validity | failures | p50 ms | p95 ms | peak mem | API p95 ms | health after |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|"
} > "$summary"

for model in $MODELS; do
  slug="${model//[:\/]/_}"
  log "pulling $model"
  docker exec "$CONTAINER" ollama pull "$model" | tail -1
  digest="$(curl --silent "http://127.0.0.1:${HOST_PORT}/api/tags" | jq -r --arg m "$model" '.models[] | select(.name==$m) | .digest' | cut -c1-12)"
  # Tags are mutable: pin the digest you benchmarked, e.g. EXPECT_DIGEST_qwen3_5_4b=abc123def456
  expect_var="EXPECT_DIGEST_${slug//[.-]/_}"
  if [[ -n "${!expect_var:-}" && "${!expect_var}" != "$digest" ]]; then
    die "$model digest $digest differs from $expect_var=${!expect_var}"
  fi

  log "evaluating $model (cold start included in the first program)"
  sample "$RESULTS/$slug-samples.csv" & SAMPLER_PID=$!
  set +e
  "$EVAL_BIN" --provider ollama --model "$model" \
    --url "http://127.0.0.1:${HOST_PORT}" --allow-any-port \
    --timeout-seconds "$TIMEOUT_SECS" --num-thread "$NUM_THREADS" --fixture "$FIXTURE" \
    --out "$RESULTS/$slug-report.json" | tee "$RESULTS/$slug-eval.txt"
  eval_status=$?
  set -e
  kill "$SAMPLER_PID" 2>/dev/null || true; SAMPLER_PID=""

  peak="$(cut -d, -f3 "$RESULTS/$slug-samples.csv" | tail -n +2 | sort -h | tail -1)"
  api_p95="$(cut -d, -f6 "$RESULTS/$slug-samples.csv" | tail -n +2 | p95)"
  if health > "$RESULTS/$slug-health.txt"; then after=healthy; else after=UNHEALTHY; fi
  jq -r --arg m "$model" --arg d "$digest" --arg p "$peak" --arg a "$api_p95" --arg h "$after" \
    '"| \($m) | \($d) | \(.overall.precision) | \(.overall.recall) | \(.evidence_validity_rate) | \(.provider_failures) | \(.latency_ms.p50) | \(.latency_ms.p95) | \($p) | \($a) | \($h) |"' \
    "$RESULTS/$slug-report.json" >> "$summary" || echo "| $model | $digest | eval exit $eval_status | | | | | | $peak | $api_p95 | $after |" >> "$summary"

  # Unload before the next model so only one is ever resident.
  curl --silent "http://127.0.0.1:${HOST_PORT}/api/generate" \
    -d "$(jq -cn --arg m "$model" '{model: $m, keep_alive: 0}')" >/dev/null || true
  if [[ "$after" != healthy ]]; then
    cat "$RESULTS/$slug-health.txt"
    die "a host service degraded during $model; see $RESULTS (rolled back)"
  fi
done

log "health after all models"
health | tee "$RESULTS/health-after.txt"
finished=1
rollback
log "done: $summary"
cat "$summary"

# ── Rollback (manual) ─────────────────────────────────────────────────────
# The benchmark changes nothing in Mazimi: no compose service, no env, no
# database write. To undo it completely:
#   scripts/ai/benchmark-ollama.sh --rollback                 # container
#   ROLLBACK_REMOVE_MODELS=1 scripts/ai/benchmark-ollama.sh --rollback   # + models
#   docker image rm "$OLLAMA_IMAGE"                           # image (~1G)
# If Besu or Postgres degraded: the EXIT trap already removed the container;
# confirm `docker ps` shows them healthy, check `dmesg | grep -i oom`, and
# do not retry with a larger MEMORY/CPUSET on this host.
