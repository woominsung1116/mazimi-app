#!/usr/bin/env bash
# Form-template auto pipeline, local run (the same steps run unattended on the server through
# scripts/forms/server/form-auto.sh). The server is touched by one read-only SQL query.
#
#   scripts/forms/auto/run.sh [YYYYMMDD]            # run dir .omc/forms-auto/<day>/run-<HHMMSS> (always new)
#   CALIBRATE=1 scripts/forms/auto/run.sh           # also push live templates' files through
#                                                   # download/convert/gates (regression set)
#   scripts/forms/auto/run.sh --finish <run dir>    # after the reviewer's verdict: verdicts + import
#
# Steps: candidates, choose, download, convert (rhwp twice, H2Orestart reference, kordoc IR) +
# fidelity, propose, programme match, checkbox groups, fill + gates, review packet, slot check
# (steps.sh), the text judge (FORM_REVIEW_JUDGE shadow|judge; off skips it), then the reviewer.
# While FORM_REVIEW_JUDGE is not judge, the independent reviewer decides: spawn ONE read-only
# reviewer with review/batch.prompt.txt (it must echo the nonce), save its JSON as
# review/batch.verdict.json, then run --finish. A template with every gate passed, no slot
# finding and a reviewer pass with zero findings is imported automatically: there is no
# human approval step (verdicts.py records the review as the verifier).
#
# Every run writes into its own new directory, so no gate result, verdict or image from an
# earlier run can be mistaken for this run's (a rerun never trusts old files).
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
AUTO="$REPO/scripts/forms/auto"
PYDEPS=(--no-project -q --with olefile==0.47 --with pypdfium2==4.30.0 --with pdfplumber==0.11.9 --with pillow==13.0.0 --with pdfminer.six==20251230 --with numpy==2.5.4 --with scipy==1.18.1)

finish() {
  local R="$1"
  python3 -I "$AUTO/verdicts.py" "$R"
  # Publishes through the admin API with the review as verifier (a dry run without a token).
  node "$REPO/scripts/forms/import-templates.mjs" --manifest "$R/out/import-manifest.json" --programs "$R/programs.json" \
    --derived "$R/derived" ${IMPORT_APPLY:+--apply}
}

if [[ "${1:-}" == "--finish" ]]; then
  finish "$2"
  exit 0
fi

DAY="${1:-$(date -u +%Y%m%d)}"
[[ "$DAY" =~ ^[0-9]{8}(-[a-z0-9]+)?$ ]] || { echo "day must be YYYYMMDD[-tag]" >&2; exit 2; }
R="$REPO/.omc/forms-auto/$DAY/run-$(date -u +%H%M%S)"
[[ -e "$R" ]] && { echo "$R exists; wait a second and rerun" >&2; exit 2; }
# Production access comes from the environment (names: docs/ops-env.md).
: "${MAZIMI_HOST:?set MAZIMI_HOST (docs/ops-env.md)}" "${MAZIMI_SSH_USER:?set MAZIMI_SSH_USER}"
: "${MAZIMI_SSH_KEY:?set MAZIMI_SSH_KEY}" "${MAZIMI_SQL_RO:?set MAZIMI_SQL_RO}"
SSH=(ssh -i "$MAZIMI_SSH_KEY" -o ConnectTimeout=20 "$MAZIMI_SSH_USER@$MAZIMI_HOST" "sudo $MAZIMI_SQL_RO")
mkdir -p "$R"
echo "run dir: $R"

echo "== 1 candidates (read-only SQL)"
"${SSH[@]}" < "$AUTO/candidates.sql" > "$R/candidates.raw.json" 2> "$R/candidates.err"
python3 -I "$AUTO/choose.py" "$R/candidates.raw.json" "$R/candidates.json"

echo "== 2 download (fresh empty dir, collector hash must match)"
DL="$R/dl"
python3 -I "$AUTO/fetch.py" "$R/candidates.json" "$DL" "$R/sources.json" ${CALIBRATE:+--live}
HWP=()
while IFS= read -r f; do HWP+=("$f"); done < <(cd "$DL" && ls -1 *.hwp *.hwpx 2>/dev/null || true)

if ((${#HWP[@]})); then
  echo "== 3 convert (pinned images; rhwp twice for reproducibility; H2Orestart as reference; kordoc IR)"
  "$REPO/scripts/forms/hwp/convert.sh" rhwp "$DL" "$R/derived" "${HWP[@]}"
  "$REPO/scripts/forms/hwp/convert.sh" rhwp "$DL" "$R/derived-check" "${HWP[@]}" >/dev/null
  for f in "${HWP[@]}"; do
    p="${f%.*}.pdf"
    if [[ -s "$R/derived/$p" ]] && ! cmp -s "$R/derived/$p" "$R/derived-check/$p"; then
      echo "rhwp not reproducible for $f" >&2; rm -f "$R/derived/$p"
    fi
  done
  # Reference engine only: a failure leaves no reference PDF, which fidelity_gate treats as
  # "no second engine" (it cannot then confirm a drop, but the ink and preview checks still run).
  "$REPO/scripts/forms/hwp/convert.sh" h2o "$DL" "$R/h2o" "${HWP[@]}" >/dev/null || echo "H2Orestart reference failed" >&2
  # Table IR for the slot check; a failed parse only means no IR evidence for that form.
  "$REPO/scripts/forms/hwp/convert.sh" kordoc "$DL" "$R/ir" "${HWP[@]}" >/dev/null || echo "kordoc IR failed" >&2
fi

PY="uv run ${PYDEPS[*]} python -I" "$AUTO/steps.sh" "$R" "$DL"

if [[ -f "$R/keys/batch.key.json" && "${FORM_REVIEW_JUDGE:-shadow}" != off ]]; then
  echo "== 8 text judge (${FORM_REVIEW_JUDGE:-shadow}; FORM_JUDGE_URL=${FORM_JUDGE_URL:-http://ollama:11434})"
  python3 -I "$AUTO/judge.py" batch "$R" || echo "judge failed: no judge verdict for this run" >&2
fi

if [[ -f "$R/keys/batch.key.json" && "${FORM_REVIEW_JUDGE:-shadow}" != judge ]]; then
  echo "NEXT: spawn ONE read-only reviewer (opus) with $R/review/batch.prompt.txt;"
  echo "      save its JSON as $R/review/batch.verdict.json; then: $0 --finish $R"
else
  finish "$R"
fi
