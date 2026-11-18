#!/usr/bin/env bash
# Server-side automatic form templates, unattended (systemd: mazimi-form-auto.timer).
#
#   form-auto.sh run                         # finish runs whose review arrived, then a new run
#   form-auto.sh finish <day>/<run>          # verdicts + automatic import for one run
#   form-auto.sh rollback <resource uuid> <reason>   # unpublish one template now (and refuse its bytes)
#   form-auto.sh stop | start                # kill switch on / off: a file on persistent storage
#                                            # (survives reboots) plus disabling / enabling the timer
#   form-auto.sh pending                     # imports waiting for their post-check, and where their packets are
#   form-auto.sh postcheck <import id> ok|rollback <reason>   # answer one; rollback unpublishes it
#
# Trust layout. $ROOT/<day>/<run> (the run dir) is written by containers that parse agency
# files, so nothing in it is trusted: it is root-owned, its subdirectories and the few files
# steps write are created by this script before any container runs (a container cannot swap
# them for symlinks), and every mount source is checked to be a real directory under $ROOT.
# $ROOT/.state/<day>/<run> (the state dir, root-only) holds what decides an import: the
# worker's candidate rows, the converter output hashes, the packet key (nonce, template
# hashes), the deciding verdict, the judge's file and verdicts.py's manifest. No parsing step
# mounts any of it writable; the reviewer's verdict is dropped into state/verdict/.
#
# Which process may produce which file, and which files decide an import: the "Trust
# boundary" table in scripts/forms/auto/steps.sh. In short, the parse container's output only
# selects; the fidelity, xcheck and derive containers and the state dir decide.
#
# Every step that reads an agency file runs in its own digest-pinned container with no network,
# a read-only root, all capabilities dropped, no-new-privileges, pid and memory caps, a tmpfs
# /tmp, inputs read-only and only its own output directories writable; outputs are swept to
# regular, size-capped, valid files. The download step alone has egress and parses nothing.
# No container gets the Docker socket.
#
# Gates before a run: FORM_AUTO_ENABLED=true, no kill file, a healthy worker, >= 22 GB free
# (the server floor is 20 GB and is never lowered). After import the run dir is deleted; the
# state dir keeps the JSON verdicts. Runs waiting for a review are deleted the same way after
# FORM_AUTO_KEEP_DAYS.
#
# Review: while FORM_REVIEW_JUDGE is not judge, the independent reviewer decides. A run stops
# with review/batch.prompt.txt ready; its verdict goes to state/verdict/batch.verdict.json and
# the next run imports what passed with zero findings. The text judge runs in shadow next to
# it until its gate passes. There is no human approval step.
#
# Post-check: the first 20 automatic imports (api::form_auto_import::POST_CHECKS clean answers)
# are flagged post_check_pending. Their review packet is kept in state/postcheck/ (the run dir
# is still deleted) so the import can be checked after it went live; a failed post-check rolls
# the template back with the same rollback as below.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
ENV_FILE="${MAZIMI_ENV_FILE:-$REPO/.env.production}"
COMPOSE=(docker compose --env-file "$ENV_FILE" -f "$REPO/compose.yml")
# To stderr: also inside $(...) (real()), where stdout would become an argument.
LOG() { echo "[form-auto $(date -u +%FT%TZ)] $*" >&2; }
# shellcheck source=state.sh
. "$(dirname "$0")/state.sh"  # the ownership model of every file root writes

# A value from the env file: last KEY=value line, quotes stripped (the file is never sourced).
flag() {
  local v
  v="$(grep -E "^$1=" "$ENV_FILE" 2>/dev/null | tail -1 | cut -d= -f2- | sed -E 's/^"(.*)"$/\1/; s/^'"'"'(.*)'"'"'$/\1/')" || true
  printf '%s' "${v:-${2:-}}"
}
pinned() {  # a local image id or a digest reference, never a moving tag
  [[ "$1" =~ ^(sha256:[0-9a-f]{64}|[a-z0-9./_-]+@sha256:[0-9a-f]{64})$ ]] || { LOG "image '$1' is not pinned by digest"; exit 2; }
}

ROOT="$(flag FORM_AUTO_RUN_ROOT /var/lib/mazimi/form-auto)"
# On persistent storage: a kill switch must survive a reboot (the timer is Persistent=true, so
# a run missed while the host was down fires right after boot).
KILL="$(flag FORM_AUTO_KILL_FILE /var/lib/mazimi/form-auto-control/stop)"
KEEP_DAYS="$(flag FORM_AUTO_KEEP_DAYS 7)"
MODE="$(flag FORM_REVIEW_JUDGE shadow)"
IMPORT="$(flag FORM_AUTO_IMPORT_ENABLED false)"
FORMS_IMAGE="$(flag FORM_AUTO_FORMS_IMAGE)"
export HWP2PDF_IMAGE="$(flag FORM_AUTO_HWP2PDF_IMAGE)"
export KORDOC_IMAGE="$(flag FORM_AUTO_KORDOC_IMAGE)"
export CHECK_IMAGE="$FORMS_IMAGE" FORMS_USER  # convert.sh validates the IR with the job image's Python, not the host's
JUDGE_NET="mazimi-formjudge"
[[ "$ROOT" =~ ^/[A-Za-z0-9/_.-]+$ && "$KILL" == /* ]] || { LOG "bad FORM_AUTO_RUN_ROOT or FORM_AUTO_KILL_FILE"; exit 2; }
# The kill file sits in a directory of its own, so making it never touches anything shared.
[[ "$(basename "$(dirname "$KILL")")" == form-auto-control ]] \
  || { LOG "FORM_AUTO_KILL_FILE must be <dir>/form-auto-control/<name> (got $KILL)"; exit 2; }
kill_dir() {  # created only when missing; an existing directory is never re-moded, but it must
  # be one the forms user can read (root:1999, group r-x), or every import would see "killed"
  local d u g m
  d="$(dirname "$KILL")"
  [[ -d "$d" ]] || { state_dir "$d"; return 0; }
  read -r u g m < <(stat -c '%u %g %a' "$d" 2>/dev/null || stat -f '%u %g %Lp' "$d")
  if (( (8#$m & 8#050) != 8#050 )) || { [[ "$(id -u)" == 0 ]] && [[ "$u:$g" != "0:$FORMS_GID" ]]; }; then
    LOG "$d exists as $u:$g $m: it must be 0:$FORMS_GID with group read and traverse (fix it by hand; it is never re-moded)"
    exit 2
  fi
}
[[ "$KEEP_DAYS" =~ ^[0-9]+$ ]] && (( KEEP_DAYS >= 1 && KEEP_DAYS <= 30 )) || { LOG "FORM_AUTO_KEEP_DAYS must be 1-30"; exit 2; }
case "$MODE" in off|shadow|judge) ;; *) LOG "FORM_REVIEW_JUDGE must be off, shadow or judge"; exit 2 ;; esac

# A mount source must be a real directory (or regular file) under $ROOT: never a symlink a
# container planted, since bind mounts follow symlinks.
real() {
  local p="$1"
  [[ ! -L "$p" && ( -d "$p" || -f "$p" ) && "$(realpath -e "$p")" == "$p" && "$p" == "$ROOT"/* ]] \
    || { LOG "refusing mount source $p"; kill -TERM $$; exit 3; }  # stops the runner, not only this subshell
  printf '%s' "$p"
}
SANDBOX=(--rm --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 256 --memory 2g --memory-swap 2g
         --tmpfs "/tmp:size=256m,uid=$FORMS_UID,gid=$FORMS_GID" --user "$FORMS_USER")
job() {  # job <network> <timeout s> <docker args...> -- <command...>
  local net="$1" secs="$2"; shift 2
  local args=()
  while [[ "$1" != -- ]]; do args+=("$1"); shift; done; shift
  docker run "${SANDBOX[@]}" --network "$net" ${args[@]+"${args[@]}"} -e PY="python -I" -e FORM_REVIEW_JUDGE="$MODE" \
    --entrypoint timeout "$FORMS_IMAGE" "$secs" "$@"
}
worker() { "${COMPOSE[@]}" run --rm --no-deps -T --user "$FORMS_USER" "$@"; }  # the forms user, not the image's
sweep() {
  local d
  for d in "$@"; do
    [[ -d "$d" ]] || continue
    worker -v "$(real "$d"):/data/out" worker form-auto sweep /data/out >/dev/null || LOG "sweep failed for $d"
  done
  return 0
}
record() {  # record <state dir> <status> [reason]
  local at="/data/$(basename "$(dirname "$1")")/$(basename "$1")" imp=()
  [[ -f "$1/import.json" && ! -L "$1/import.json" ]] && imp=(-v "$1/import.json:$at/import.json:ro")
  [[ -f "$1/converters.json" && ! -L "$1/converters.json" ]] && imp+=(-v "$1/converters.json:$at/converters.json:ro")
  worker -v "$(real "$1/out"):$at:ro" ${imp[@]+"${imp[@]}"} worker form-auto record "$at" "$2" "${3:-}" >/dev/null || LOG "could not record $2"
}
# A run stopped by a signal (real() refusing a mount source, the systemd timeout, a stop) is
# a failed run: recorded, non-zero. Every other failure path records through ERR / finish.
CUR_S=""
on_term() {
  trap - TERM
  if [[ -n "$CUR_S" ]]; then record "$CUR_S" failed "stopped by a signal: refused mount source, timeout or stop"
  else worker worker form-auto record "/data/failed/$(date -u +%Y%m%d)" failed "stopped by a signal" >/dev/null 2>&1 || true; fi
  LOG "stopped by a signal: recorded as failed"
  exit 143
}
trap on_term TERM

mkdirs() { container_dirs "$@"; }     # directories a container writes (state.sh)
mkfiles() { container_files "$@"; }   # files a container fills (state.sh)

finish() {  # finish <day>/<run>
  local rel="$1" R S name ro
  R="$ROOT/$rel"; S="$ROOT/.state/$rel"; name="$(basename "$R")"
  [[ -d "$S" && ! -L "$S" ]] || { LOG "no state for $rel"; return 1; }
  CUR_S="$S"
  [[ -f "$S/import.json" ]] && { LOG "$rel already finished"; return 0; }
  if [[ -d "$R" ]]; then ro=(-v "$(real "$R"):/data/$name:ro"); else ro=(); fi
  rm -rf "$S/out"; mkdirs "$S/out"
  state_log "$S/verdicts.log" job none 300 ${ro[@]+"${ro[@]}"} -v "$(real "$S/keys"):/state/keys:ro" -v "$(real "$S/verdict"):/state/verdict:ro" \
      -v "$(real "$S/judge"):/state/judge:ro" -v "$(real "$S/out"):/state/out" \
      -- python -I /opt/repo/scripts/forms/auto/verdicts.py "/data/$name" --state /state \
    || { record "$S" failed "verdicts.py refused the run"; return 1; }
  if [[ "$IMPORT" == true && ! -e "$KILL" && -d "$R" ]]; then
    kill_dir
    # The importer re-checks nonce, hashes, the candidate rows and the converter record itself.
    # Mounted as /data/<day>/<run>: audit rows name the day too (run names repeat daily).
    state_log "$S/import.log" worker -v "$(real "$R"):/data/$rel:ro" -v "$(real "$S"):/state:ro" \
        -v "$(dirname "$KILL"):$(dirname "$KILL"):ro" api form-auto-import apply "/data/$rel" /state \
      || { record "$S" failed "import failed"; return 1; }
    tail -1 "$S/import.log" | state_put "$S/import.json"
    keep_packet "$R" "$S"
  elif [[ "$IMPORT" != true && ! -e "$KILL" && -d "$R" ]]; then
    # Import off: the importer still runs, as a dry run against the real database (every check,
    # every read-only precondition, the audit insert in a transaction rolled back).
    state_log "$S/import.log" worker -v "$(real "$R"):/data/$rel:ro" -v "$(real "$S"):/state:ro" \
        api form-auto-import check "/data/$rel" /state \
      || { record "$S" failed "import dry run failed"; return 1; }
    tail -1 "$S/import.log" | state_put "$S/import.json"
  else
    echo '{"forms": 0, "refused": 0, "skipped": "kill switch or run dir gone"}' | state_put "$S/import.json"
  fi
  record "$S" done
  [[ -d "$R" ]] && rm -rf --one-file-system "$R"  # the state dir keeps the JSON verdicts
  LOG "$rel finished: $(cat "$S/import.json")"
}

keep_packet() {  # keep_packet <run dir> <state dir>: the review packet of imports awaiting a post-check
  local R="$1" S="$2"
  grep -q '"post_check_pending":[1-9]' "$S/import.json" || return 0
  [[ -d "$R/review/batch" && ! -L "$R/review/batch" ]] || { LOG "no review packet to keep for the post-check"; return 0; }
  # Root-only on purpose (not the state model): read by the operator (Claude, as root), never
  # mounted into a container.
  rm -rf "$S/postcheck"; install -d -m 0700 "$S/postcheck"
  # Regular files only (the packet was swept); copied, owned by root, read-only.
  find "$R/review/batch" -maxdepth 1 -type f -size -8M -exec cp --no-dereference -t "$S/postcheck" {} +
  [[ -f "$R/review/batch.prompt.txt" && ! -L "$R/review/batch.prompt.txt" ]] && cp "$R/review/batch.prompt.txt" "$S/postcheck/"
  chmod -R a-w,go-rwx "$S/postcheck"; chmod u+wx "$S/postcheck"
  LOG "kept the review packet for the post-check: $S/postcheck (key: $S/keys/batch.key.json)"
}

new_run() {
  local day rel R S name dl
  day="$(date -u +%Y%m%d)"
  name="run-$(date -u +%H%M%S)"
  rel="$day/$name"; R="$ROOT/$rel"; S="$ROOT/.state/$rel"
  [[ -e "$R" || -e "$S" ]] && { LOG "$rel exists"; return 1; }
  install -d -m 0755 "$ROOT/$day" "$R"
  state_dir "$ROOT/.state/$day" "$S"
  mkdirs "$R"/{dl,derived,derived-check,h2o,ir,out,overlay,gates,groups,fidelity,review,rfill,rpdfium,rpoppler,rxcheck,rderive} "$S"/{cand,keys,judge,out}
  state_dir "$S/verdict"  # the reviewer's verdict is placed here by root: state_put, or install -m 0640 -g 1999
  mkfiles "$R"/{candidates.json,sources.json,conversions.json}
  CUR_S="$S"
  trap 'record "$S" failed "step failed (see the journal)"' ERR
  LOG "run $rel"
  worker -v "$(real "$S/cand"):/data/cand" worker form-auto candidates /data/cand/candidates.raw.json
  local RO=(-v "$(real "$R"):/data/$name:ro")
  job none 120 "${RO[@]}" -v "$(real "$S/cand"):/state/cand:ro" -v "$(real "$R/candidates.json"):/data/$name/candidates.json" \
    -- python -I /opt/repo/scripts/forms/auto/choose.py /state/cand/candidates.raw.json "/data/$name/candidates.json"
  # The only step with egress: robots.txt, 30 s per host, https, collector hash must match. It
  # hashes the bytes and parses nothing but the notice page's title text.
  job bridge 3600 "${RO[@]}" -v "$(real "$R/dl"):/data/$name/dl" -v "$(real "$R/sources.json"):/data/$name/sources.json" \
    -- python -I /opt/repo/scripts/forms/auto/fetch.py "/data/$name/candidates.json" "/data/$name/dl" "/data/$name/sources.json"
  dl="$R/dl"
  sweep "$dl"
  local hwp=()
  while IFS= read -r f; do hwp+=("$f"); done < <(cd "$dl" && find . -maxdepth 1 -type f \( -name '*.hwp' -o -name '*.hwpx' \) -printf '%f\n')
  if ((${#hwp[@]})); then
    "$REPO/scripts/forms/hwp/convert.sh" rhwp "$dl" "$R/derived" "${hwp[@]}"
    "$REPO/scripts/forms/hwp/convert.sh" rhwp "$dl" "$R/derived-check" "${hwp[@]}" >/dev/null
    # The reference engine and the IR do not decide alone, but their failures are counted and
    # reported (the run summary's converters.*), never swallowed.
    local h2o_failed kordoc_failed
    h2o_failed="$({ "$REPO/scripts/forms/hwp/convert.sh" h2o "$dl" "$R/h2o" "${hwp[@]}" >/dev/null; } 2>&1 | grep -c ' FAILED ' || true)"
    kordoc_failed="$({ "$REPO/scripts/forms/hwp/convert.sh" kordoc "$dl" "$R/ir" "${hwp[@]}" >/dev/null; } 2>&1 | grep -c ' FAILED ' || true)"
    (( h2o_failed + kordoc_failed == 0 )) || LOG "WARN converters failed: h2o $h2o_failed, kordoc $kordoc_failed of ${#hwp[@]} files"
    printf '{"files": %d, "h2o_failed": %d, "kordoc_failed": %d}\n' "${#hwp[@]}" "$h2o_failed" "$kordoc_failed" | state_put "$S/converters.json"
    sweep "$R/derived" "$R/derived-check" "$R/h2o" "$R/ir"
    # Reproducible rhwp output only, recorded before any parsing step runs: the importer
    # publishes a converted PDF only if it is byte-for-byte this file.
    local sums=""
    for f in "${hwp[@]}"; do
      p="${f%.*}.pdf"
      if [[ -f "$R/derived/$p" && ! -L "$R/derived/$p" ]] && cmp -s "$R/derived/$p" "$R/derived-check/$p"; then
        sums+="$p $(sha256sum "$R/derived/$p" | cut -d' ' -f1)"$'\n'
      else
        rm -f "$R/derived/$p"
      fi
    done
    printf '%s' "$sums" | state_put "$S/derived.sha256"
  fi
  # Trust boundary (steps.sh): each stage its own fresh container, writing only its own files.
  # The fidelity verdict comes from its own container, not the parse one.
  job none 1800 "${RO[@]}" -v "$(real "$R/fidelity"):/data/$name/fidelity" -v "$(real "$R/conversions.json"):/data/$name/conversions.json" \
    -- bash /opt/repo/scripts/forms/auto/steps.sh "/data/$name" "/data/$name/dl" fidelity
  sweep "$R/fidelity"
  # Parse (pdfium, the proposal) writes the template under review and the selection; nothing
  # it writes decides an import.
  local W=()
  for d in out overlay gates groups review; do W+=(-v "$(real "$R/$d"):/data/$name/$d"); done
  job none 1800 "${RO[@]}" "${W[@]}" -- bash /opt/repo/scripts/forms/auto/steps.sh "/data/$name" "/data/$name/dl" parse
  sweep "$R/out" "$R/overlay" "$R/gates" "$R/groups" "$R/review"
  # The reviewer's images and the gates, again from the template and the bound blank: the app
  # fill, pdfium, poppler, and the cross-check that needs both renderings to agree (it parses
  # no PDF); then the programme match and the slot check (derive: pdfplumber only).
  local st
  for st in fill:rfill render:rpdfium poppler:rpoppler xcheck:rxcheck derive:rderive; do
    job none 1800 "${RO[@]}" -v "$(real "$R/${st#*:}"):/data/$name/${st#*:}" \
      -- bash /opt/repo/scripts/forms/auto/steps.sh "/data/$name" "/data/$name/dl" "${st%%:*}"
    sweep "$R/${st#*:}"
  done
  # The packet reads JSON and images only, in a fresh container; its key goes to the state dir.
  job none 600 "${RO[@]}" -v "$(real "$R/review"):/data/$name/review" -v "$(real "$S/keys"):/state/keys" -e KEYS_DIR=/state/keys \
    -- bash /opt/repo/scripts/forms/auto/steps.sh "/data/$name" "/data/$name/dl" packet
  sweep "$R/review" "$S/keys"
  if [[ -f "$S/keys/batch.key.json" && "$MODE" != off ]]; then
    if judge_net; then
      job "$JUDGE_NET" 1800 "${RO[@]}" -v "$(real "$S/keys"):/state/keys:ro" -v "$(real "$S/judge"):/state/judge" \
        -e FORM_JUDGE_URL=http://ollama:11434 -e FORM_JUDGE_MODEL="$(flag FORM_JUDGE_MODEL qwen3:1.7b)" \
        -e FORM_JUDGE_NUM_THREADS="$(flag FORM_JUDGE_NUM_THREADS 2)" \
        -- python -I /opt/repo/scripts/forms/auto/judge.py batch "/data/$name" --keys /state/keys --out /state/judge \
        || LOG "judge failed: no judge verdict for this run"
      judge_net_detach
      sweep "$S/judge"
    else
      LOG "judge unavailable: no judge verdict for this run"
    fi
  fi
  trap - ERR
  if [[ ! -f "$S/keys/batch.key.json" ]]; then
    LOG "nothing to review: no template reached the review step"
    finish "$rel"
  elif [[ "$MODE" == judge ]]; then
    finish "$rel"
  else
    record "$S" done "awaiting the reviewer"
    LOG "awaiting the reviewer: $R/review/batch.prompt.txt -> $S/verdict/batch.verdict.json"
  fi
}

# The judge's own Ollama, for one run: a container on an --internal network (no egress, so
# /api/pull reaches nothing) that holds only it and the judge, with the model store mounted
# read-only (/api/delete fails) and nothing else writable. It does not share the assistant's
# Ollama (one request and one loaded model at a time there).
JUDGE_OLLAMA="mazimi-formjudge-ollama"
judge_net() {
  local img models
  img="$(flag OLLAMA_IMAGE)"; models="$(flag FORM_JUDGE_MODELS_VOLUME mazimi_ollama_models)"
  [[ -n "$img" ]] || { LOG "OLLAMA_IMAGE is not set: no judge"; return 1; }
  docker network inspect "$JUDGE_NET" >/dev/null 2>&1 || docker network create --internal "$JUDGE_NET" >/dev/null
  [[ "$(docker network inspect -f '{{.Internal}}' "$JUDGE_NET")" == true ]] || { LOG "$JUDGE_NET is not internal"; return 1; }
  docker rm -f "$JUDGE_OLLAMA" >/dev/null 2>&1 || true
  docker run -d --rm --name "$JUDGE_OLLAMA" --network "$JUDGE_NET" --network-alias ollama \
    --read-only --cap-drop ALL --security-opt no-new-privileges --memory 4g --memory-swap 4g --pids-limit 256 \
    --tmpfs /tmp --tmpfs /root/.ollama -v "$models:/models:ro" -e OLLAMA_MODELS=/models/models -e OLLAMA_NOPRUNE=1 \
    -e OLLAMA_NUM_PARALLEL=1 "$img" >/dev/null || { LOG "could not start the judge's Ollama"; return 1; }
  local i
  for i in $(seq 1 60); do
    docker exec "$JUDGE_OLLAMA" ollama list >/dev/null 2>&1 && return 0
    sleep 1
  done
  LOG "the judge's Ollama did not come up"; judge_net_detach; return 1
}
judge_net_detach() { docker rm -f "$JUDGE_OLLAMA" >/dev/null 2>&1 || true; }

housekeeping() {  # run dirs older than KEEP_DAYS are deleted (their state dirs keep the JSON)
  local d
  while IFS= read -r d; do
    rm -rf --one-file-system "$d" && LOG "deleted $(basename "$(dirname "$d")")/$(basename "$d")" || LOG "could not delete $d"
  done < <(find "$ROOT" -mindepth 2 -maxdepth 2 -type d -name 'run-*' -mtime +"$KEEP_DAYS" -not -path "$ROOT/.state/*" 2>/dev/null)
  return 0
}

case "${1:-run}" in
  stop)
    kill_dir; : | state_put "$KILL"
    # Also stop the schedule, remembering whether it was on: 'start' restores what was there
    # (a timer installed but never enabled, deploy step 5, stays off).
    if systemctl is-enabled --quiet mazimi-form-auto.timer 2>/dev/null; then
      : | state_put "$KILL.timer-was-enabled"
      systemctl disable --now mazimi-form-auto.timer >/dev/null 2>&1 || LOG "could not disable the timer (the kill file still stops runs)"
    fi
    LOG "kill switch on ($KILL): nothing runs or imports"; exit 0 ;;
  start)
    rm -f "$KILL"
    if [[ -e "$KILL.timer-was-enabled" ]]; then
      systemctl enable --now mazimi-form-auto.timer >/dev/null 2>&1 || LOG "could not enable the timer"
      rm -f "$KILL.timer-was-enabled"
      LOG "kill switch off, timer enabled again"
    else
      LOG "kill switch off (the timer was not enabled before stop: left as it is)"
    fi
    exit 0 ;;
  rollback)  # before every run gate: it must work with the runner off and the kill switch on
    [[ "${2:-}" =~ ^[0-9a-f-]{36}$ && -n "${3:-}" ]] || { echo "usage: $0 rollback <resource uuid> <reason>" >&2; exit 2; }
    worker api form-auto-import rollback "$2" "$3"; exit 0 ;;
  failed)  # ExecStopPost when the unit did not succeed (timeout, crash): one Error event
    worker worker form-auto record "/data/failed/$(date -u +%Y%m%d)" failed "unit result: ${2:-unknown}" >/dev/null 2>&1 \
      || LOG "could not record the failed unit (${2:-unknown})"
    exit 0 ;;
  pending)
    worker api form-auto-import pending
    LOG "each import's packet: $ROOT/.state/<day>/<run>/postcheck/ (the run column is <day>/<run>; letters in .../keys/batch.key.json)"
    exit 0 ;;
  postcheck)
    [[ "${2:-}" =~ ^[0-9]+$ && "${3:-}" =~ ^(ok|rollback)$ && -n "${4:-}" ]] || { echo "usage: $0 postcheck <import id> ok|rollback <reason>" >&2; exit 2; }
    worker api form-auto-import postcheck "$2" "$3" "$4"; exit 0 ;;
esac

[[ "$(flag FORM_AUTO_ENABLED false)" == true ]] || { LOG "off (FORM_AUTO_ENABLED != true)"; exit 0; }
pinned "$FORMS_IMAGE"; pinned "$HWP2PDF_IMAGE"; pinned "$KORDOC_IMAGE"
exec 9>/run/lock/mazimi-form-auto.lock
flock -n 9 || { LOG "another run is active"; exit 0; }
install -d -m 0755 "$ROOT"; state_dir "$ROOT/.state"
ROOT="$(realpath -e "$ROOT")"

skip() { LOG "skipped: $1"; worker worker form-auto record "/data/skipped/$(date -u +%Y%m%d)" skipped "$1" >/dev/null || true; exit 0; }
[[ -e "$KILL" ]] && skip "kill switch $KILL"
free_gb=$(( $(df -Pk "$ROOT" | awk 'NR==2 {print $4}') / 1048576 ))
(( free_gb >= 22 )) || skip "$free_gb GB free < 22 GB (server floor 20 GB)"
health="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$(docker ps -q --filter label=com.docker.compose.project=mazimi --filter label=com.docker.compose.service=worker | head -1)" 2>/dev/null || true)"
[[ "$health" == healthy ]] || skip "worker not healthy ($health)"

case "${1:-run}" in
  finish) [[ "${2:-}" =~ ^[0-9]{8}/run-[0-9]{6}$ ]] || { echo "usage: $0 finish <YYYYMMDD>/run-<HHMMSS>" >&2; exit 2; }; finish "$2" ;;
  run)
    rc=0
    housekeeping
    while IFS= read -r v; do
      rel="${v#"$ROOT/.state/"}"; rel="${rel%/verdict/batch.verdict.json}"
      [[ "$rel" =~ ^[0-9]{8}/run-[0-9]{6}$ ]] && { finish "$rel" || rc=1; }
    done < <(find "$ROOT/.state" -mindepth 4 -maxdepth 4 -path '*/verdict/batch.verdict.json' -type f 2>/dev/null)
    new_run
    exit "$rc" ;;
  *) echo "usage: $0 run | finish <day>/<run> | rollback <resource uuid> <reason> | pending | postcheck <id> ok|rollback <reason> | stop | start" >&2; exit 2 ;;
esac
