#!/usr/bin/env bash
# The machine steps of the form-template pipeline that need no network and no Docker. Shared
# by the local run.sh and the server runner (scripts/forms/server/form-auto.sh), which runs
# each stage in its own container of the forms job image with no network.
#
#   PY="python3 -I" scripts/forms/auto/steps.sh <run dir> <download dir> [fidelity|parse|fill|render|poppler|xcheck|derive|packet|all]
#
# Trust boundary: which process may produce which file. A stage that parses an agency file
# can be taken over by that file; what it writes may select work, never decide an import.
# On the server every stage is its own fresh container (no network, read-only root, only the
# listed directory writable); the importer re-checks hashes against the state dir.
#
#   stage     parses                         writes              decides
#   fidelity  raw HWP (olefile), PDFs        conversions.json,   conversion faithful (HWP)
#             (pdfium, pdfplumber)           fidelity/
#   parse     PDF (pdfplumber), proposal     out/ overlay/       nothing: the template under
#             overrides; groups, app fill,   gates/ groups/      review and which slugs go on
#             gates (pdfium)                 review/specs.txt    (selection only)
#   fill      blank (pdfplumber, pdf-lib)    rfill/              the fills the renderers draw
#   render    PDFs (pdfium)                  rpdfium/            pdfium's half of agreement
#   poppler   PDFs (poppler)                 rpoppler/           poppler's half of agreement
#   xcheck    PNG + JSON only                rxcheck/            gates (both renderers)
#   derive    blank (pdfplumber), validated  rderive/            programme match, slot check,
#             IR, template; never the raw                        the reviewable list
#             HWP; pdfplumber is the only parser it runs
#   packet    JSON + PNG only                review/batch, key   the packet nonce and hashes
# The judge reads only rderive/ and the key; verdicts.py reads conversions.json, rxcheck/,
# rderive/ and the state dir. Nothing parse writes is read by either.
#   all     every stage in order (the local run).
# Expects in the run dir: candidates.json, sources.json, derived/ (rhwp PDFs, already checked
# reproducible), h2o/ (reference PDFs, may be empty), ir/ (kordoc IR, may be empty).
set -euo pipefail
R="$1"; DL="$2"; STAGE="${3:-all}"
AUTO="$(cd "$(dirname "$0")" && pwd)"
FORMS="$(cd "$AUTO/.." && pwd)"
KEYS="${KEYS_DIR:-$R/keys}"
read -r -a PYCMD <<< "${PY:-python3 -I}"
case "$STAGE" in fidelity|parse|fill|render|poppler|xcheck|derive|packet|all) ;; *) echo "unknown stage $STAGE" >&2; exit 2 ;; esac
RF="$R/rfill"; RP="$R/rpdfium"; RQ="$R/rpoppler"; RX="$R/rxcheck"; RD="$R/rderive"

# The reviewer's images are made again after the parse stage, from the template and the bound
# blank (the collector-hashed PDF or the host-hashed converter output) only: images from the
# parse stage are never shown, so an exploit in the HWP, preview or proposal code cannot show
# clean pictures of a different template; and two independent renderers must agree, so neither
# can either. A template whose gates fail on either renderer is not reviewed. (The HWP preview
# comparison image still comes from the parse stage.)
each() {  # each <fn>: <fn> <slug> <official sha> <format> <blank> for every reviewable template
  local line slug sha fmt blank
  while IFS= read -r line; do
    slug="${line##*:}"
    [[ "$slug" =~ ^[a-z0-9-]{1,64}$ ]] || continue
    read -r sha fmt < <("${PYCMD[@]}" -c 'import json,sys; s=next(s for s in json.load(open(sys.argv[1])) if s["slug"] == sys.argv[2]); print(s["sha256"], s["format"])' "$R/sources.json" "$slug")
    blank="$R/derived/$slug.pdf"; [[ "$fmt" == pdf ]] && blank="$DL/$slug.pdf"
    "$1" "$slug" "$sha" "$fmt" "$blank" || echo "$1 failed for $slug" >&2
  done < "$R/review/specs.txt"
}
fill_one() {
  local tpl="$R/out/$1.template.json"
  "${PYCMD[@]}" "$AUTO/groups.py" "$tpl" "$4" "$RF/$1.groups.json" </dev/null >/dev/null &&
  node "$AUTO/fill.cjs" "$tpl" "$4" "$2" "$3" "$RF/$1.filled.pdf" "$RF/$1.fill.json" </dev/null >/dev/null &&
  node "$AUTO/fill.cjs" "$tpl" "$4" "$2" "$3" "$RF/$1.filled-groups.pdf" "$RF/$1.fill-groups.json" "$RF/$1.groups.json" </dev/null >/dev/null
}
render_one() {
  "${PYCMD[@]}" "$AUTO/gates.py" "$1" "$R/out/$1.template.json" "$4" "$RF/$1.filled.pdf" "$RF/$1.fill.json" "$RP" \
    "$RF/$1.filled-groups.pdf" "$RF/$1.fill-groups.json" --rasters "$RP/$1" </dev/null >/dev/null
}
poppler_one() {  # pages as <kind>-<n>.png, like gates.py --rasters
  local kind pdf f n
  rm -rf "$RQ/$1"; mkdir -p "$RQ/$1"
  for kind in blank filled shown; do
    case $kind in blank) pdf="$4" ;; filled) pdf="$RF/$1.filled.pdf" ;; shown) pdf="$RF/$1.filled-groups.pdf" ;; esac
    pdftoppm -r 144 -gray -png -l 50 "$pdf" "$RQ/$1/tmp-$kind" </dev/null || return 1
    for f in "$RQ/$1"/tmp-"$kind"-*.png; do
      n="${f##*-}"; n="${n%.png}"; n=$((10#$n))
      mv "$f" "$RQ/$1/$kind-$n.png"
    done
  done
}
xcheck_one() {
  "${PYCMD[@]}" "$AUTO/xcheck.py" "$1" "$R/out/$1.template.json" "$RF/$1.fill.json" "$RF/$1.fill-groups.json" \
    "$RP/$1.gates.json" "$RP/$1" "$RQ/$1" "$RX" </dev/null >/dev/null || true
  if "${PYCMD[@]}" -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["pass"] else 1)' "$RX/$1.gates.json" 2>/dev/null; then
    grep -E ":$1\$" "$R/review/specs.txt" | head -1 | { IFS= read -r line; echo "${line%%=*}=$RX/$1.gates.json:$1"; } >> "$RX/specs.txt"
  else
    echo "gates fail on the re-render or the renderers disagree for $1" >&2
  fi
}
stage() {  # stage fill|render|poppler|xcheck
  case "$1" in
    fill) mkdir -p "$RF"; each fill_one ;;
    render) mkdir -p "$RP"; each render_one ;;
    poppler) mkdir -p "$RQ"; each poppler_one ;;
    xcheck) mkdir -p "$RX"; : > "$RX/specs.txt"; each xcheck_one ;;
  esac
}

packet() {
  SPECS=()
  [[ -f "$RD/specs.txt" ]] || { echo "no derived specs: run the derive stage first" >&2; return 1; }
  while IFS= read -r line; do SPECS+=("$line"); done < "$RD/specs.txt"
  ((${#SPECS[@]})) || { echo "nothing to review"; return 0; }
  rm -rf "$R/review/batch"
  echo "== 7b review packet (images re-rendered from the bound blank, pdfium and poppler agreeing)"
  KEYS_DIR="$KEYS" "${PYCMD[@]}" "$AUTO/packet.py" "$R/review/batch" "$R/fidelity" "${SPECS[@]}"
  "${PYCMD[@]}" - "$AUTO/review-prompt.txt" "$R" "$KEYS" "${SPECS[@]}" > "$R/review/batch.prompt.txt" <<'PY'
import json, sys
prompt, R, keys, specs = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
key = json.load(open(f"{keys}/batch.key.json"))
letters = ", ".join(s.split("=", 1)[0] for s in specs)
print(open(prompt).read().replace("{packet}", f"{R}/review/batch").replace("{letters}", letters).replace("{nonce}", key["_nonce"]))
PY
}
derive() {
  # Programme match and the deterministic slot check, again, outside the parse container: from
  # the template, the bound blank (pdfplumber only), the validated IR and the fill's groups.
  # The reviewable list is the cross-checked list minus what fails the match.
  mkdir -p "$RD/slots"; : > "$RD/specs.txt"
  [[ -s "$RX/specs.txt" ]] || { echo '{}' > "$RD/match.json"; return 0; }
  "${PYCMD[@]}" "$AUTO/match.py" --derive "$R" "$DL" "$RX/specs.txt" "$RD/match.json" </dev/null || echo '{}' > "$RD/match.json"
  local spec slug fmt src_sha blank irargs
  while IFS= read -r spec; do
    slug="${spec##*:}"
    [[ "$slug" =~ ^[a-z0-9-]{1,64}$ ]] || continue
    read -r src_sha fmt < <("${PYCMD[@]}" -c 'import json,sys; s=next(s for s in json.load(open(sys.argv[1])) if s["slug"] == sys.argv[2]); print(s["sha256"], s["format"])' "$R/sources.json" "$slug")
    blank="$R/derived/$slug.pdf"; [[ "$fmt" == pdf ]] && blank="$DL/$slug.pdf"
    irargs=()
    [[ "$fmt" != pdf && -f "$R/ir/$slug.ir.json" ]] && irargs=(--ir "$R/ir/$slug.ir.json" --source-sha256 "$src_sha")
    # The field invariants (invariants.py, which runs slots.py's checks first). No slots.json
    # means a failed check: verdicts.py rejects the template.
    "${PYCMD[@]}" "$AUTO/invariants.py" "$R/out/$slug.template.json" "$blank" "$RD/slots/$slug.slots.json" ${irargs[@]+"${irargs[@]}"} \
      --groups "$RF/$slug.groups.json" </dev/null || echo "invariants.py failed for $slug" >&2
    if "${PYCMD[@]}" -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get(sys.argv[2], {}).get("pass") else 1)' "$RD/match.json" "$slug"; then
      echo "$spec" >> "$RD/specs.txt"
    fi
  done < "$RX/specs.txt"
  [[ -s "$RD/specs.txt" ]] || echo "no template passed the programme match: nothing to review"
}

fidelity() {
  mkdir -p "$R/fidelity"
  local HWP=()
  while IFS= read -r f; do HWP+=("$f"); done < <(cd "$DL" && ls -1 *.hwp *.hwpx 2>/dev/null || true)
  if ((${#HWP[@]})); then
    echo "== 3 fidelity (text, ink, preview; kordoc IR text as a second oracle, report only)"
    "${PYCMD[@]}" "$AUTO/fidelity_gate.py" --sources "$R/sources.json" --raw "$DL" --rhwp "$R/derived" --h2o "$R/h2o" \
      --ir "$R/ir" --out "$R/conversions.json" --png "$R/fidelity"
  else
    echo '{"version":1,"converter":{},"forms":{}}' > "$R/conversions.json"
  fi
}

case "$STAGE" in
  fidelity) fidelity; exit 0 ;;
  fill|render|poppler|xcheck) stage "$STAGE"; exit 0 ;;
  derive) derive; exit 0 ;;
  packet) packet; exit 0 ;;
  all) fidelity ;;
esac
mkdir -p "$R/gates" "$R/groups" "$R/review"

echo "== 4 propose fields (no human overrides unless scripts/forms/auto/overrides/<slug>.json exists)"
mkdir -p "$AUTO/overrides" 2>/dev/null || true
"${PYCMD[@]}" "$FORMS/propose_fields.py" batch --sources "$R/sources.json" --raw "$DL" \
  --out "$R/out" --review "$R/overlay" --overrides "$AUTO/overrides" --conversions "$R/conversions.json" --derived "$R/derived"

echo "== 6 checkbox groups, fill with fake values through the app code, machine gates"
"${PYCMD[@]}" - "$R" > "$R/gates/jobs.tsv" <<'PY'
import json, sys
R = sys.argv[1]
src = {s["slug"]: s for s in json.load(open(f"{R}/sources.json"))}
for e in json.load(open(f"{R}/out/manifest.json"))["forms"]:
    if e["status"] == "template_ready":
        s = src[e["slug"]]
        blank = f"{R}/derived/{e['slug']}.pdf" if s["format"] != "pdf" else f"{R}/dl/{e['slug']}.pdf"
        print("\t".join([e["slug"], s["sha256"], s["format"], blank]))
PY
fail_gate() {  # a crashed step is a failed gate, never a missing (or stale) result
  printf '{"slug":"%s","pass":false,"problems":{"%s":["%s"]},"images":[]}\n' "$1" "$2" "$3" > "$R/gates/$1.gates.json"
}
while IFS=$'\t' read -r -u 3 slug sha fmt blank; do
  [[ -f "$blank" ]] || blank="$DL/$slug.pdf"
  tpl="$R/out/$slug.template.json"
  "${PYCMD[@]}" "$AUTO/groups.py" "$tpl" "$blank" "$R/groups/$slug.groups.json" </dev/null \
    || { fail_gate "$slug" groups "groups.py crashed"; continue; }
  # Geometry gates run on a fill with EVERY tick box ticked; the single-tick fill (groups applied)
  # is only the reviewer's picture of what a user would produce.
  node "$AUTO/fill.cjs" "$tpl" "$blank" "$sha" "$fmt" "$R/gates/$slug.filled.pdf" "$R/gates/$slug.fill.json" </dev/null \
    || { fail_gate "$slug" fill "app fill failed"; continue; }
  node "$AUTO/fill.cjs" "$tpl" "$blank" "$sha" "$fmt" "$R/gates/$slug.filled-groups.pdf" "$R/gates/$slug.fill-groups.json" "$R/groups/$slug.groups.json" </dev/null \
    || { fail_gate "$slug" fill "app fill with groups failed"; continue; }
  "${PYCMD[@]}" "$AUTO/gates.py" "$slug" "$tpl" "$blank" "$R/gates/$slug.filled.pdf" "$R/gates/$slug.fill.json" "$R/gates" \
      "$R/gates/$slug.filled-groups.pdf" "$R/gates/$slug.fill-groups.json" </dev/null \
    || fail_gate "$slug" gates "gates.py crashed"
done 3< "$R/gates/jobs.tsv"

echo "== 7 review specs (gate passes among new candidates; selection only, every check is redone)"
"${PYCMD[@]}" - "$R" > "$R/review/specs.txt" <<'PY'
import json, sys
R = sys.argv[1]
cand = {r["id"]: r for r in json.load(open(f"{R}/candidates.json"))["rows"]}
n = 0
for s in json.load(open(f"{R}/sources.json")):
    try:
        g = json.load(open(f"{R}/gates/{s['slug']}.gates.json"))
    except FileNotFoundError:
        continue
    if g["pass"] and cand[s["resource_id"]]["verdict"] == "try":
        n += 1
        if n > 702:
            raise SystemExit("more than 702 templates in one batch: split the run")
        letter = "".join(chr(65 + d) for d in ([(n - 1) // 26 - 1] if n > 26 else []) + [(n - 1) % 26])
        print(f"{letter}={R}/gates/{s['slug']}.gates.json:{s['slug']}")
PY
[[ -s "$R/review/specs.txt" ]] || echo "no new candidate passed the machine gates: nothing to review"
if [[ "$STAGE" == all ]]; then stage fill; stage render; stage poppler; stage xcheck; derive; packet; fi
