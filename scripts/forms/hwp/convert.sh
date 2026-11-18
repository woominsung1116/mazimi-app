#!/usr/bin/env bash
# Converts official HWP/HWPX blanks in pinned local images.
# Usage: scripts/forms/hwp/convert.sh <h2o|rhwp|kordoc> <raw dir> <out dir> <file>...
#   h2o, rhwp  -> <out>/<name>.pdf      (HWP2PDF_IMAGE, default mazimi-hwp2pdf:2)
#   kordoc     -> <out>/<name>.ir.json  (KORDOC_IMAGE, default mazimi-kordoc:1): table IR only,
#                 kordoc never fills or writes a document (scripts/forms/kordoc/ir.mjs)
# The container has no network, a read-only root, reads the raw dir read-only and writes only
# to a fresh directory of its own file (so a converter taken over by one file cannot touch
# another file's output); the host moves the checked result into <out dir>. Prints
# "<file> <source sha256> <output sha256>" per converted file; failures go to stderr.
# CHECK_IMAGE (the forms job image) validates the IR with its own Python; without it the host
# python3 is used (local runs).
set -euo pipefail
FORMS_USER="${FORMS_USER:-1999:1999}"  # the forms user (scripts/forms/server/state.sh)
CU="${FORMS_USER%%:*}"; CG="${FORMS_USER##*:}"
IMAGE="${HWP2PDF_IMAGE:-mazimi-hwp2pdf:2}"
KIMAGE="${KORDOC_IMAGE:-mazimi-kordoc:1}"
HERE="$(cd "$(dirname "$0")" && pwd)"
engine="$1"; raw="$(cd "$2" && pwd)"; out="$(mkdir -p "$3" && cd "$3" && pwd)"; shift 3
case "$engine" in h2o|rhwp|kordoc) ;; *) echo "engine must be h2o, rhwp or kordoc" >&2; exit 2 ;; esac
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
fresh() {  # a fresh output directory for one file, writable by the container user only
  local d
  d="$(mktemp -d "$out/.conv.XXXXXXXX")"
  if [[ "$(id -u)" == 0 ]]; then chown "$FORMS_USER" "$d"; chmod 0700 "$d"; else chmod 0777 "$d"; fi
  printf '%s' "$d"
}
check_ir() {  # check_ir <ir file> <source sha256>
  if [[ -n "${CHECK_IMAGE:-}" ]]; then
    docker run --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges --pids-limit 64 \
      --memory 256m --user "$FORMS_USER" -v "$(dirname "$1"):/ir:ro" --entrypoint python "$CHECK_IMAGE" -I \
      /opt/repo/scripts/forms/kordoc/check_ir.py "/ir/$(basename "$1")" "$2"
  else
    python3 -I "$HERE/../kordoc/check_ir.py" "$1" "$2"
  fi
}
for file in "$@"; do
  case "$file" in *.hwp|*.hwpx) ;; *) echo "skip $file (not hwp/hwpx)" >&2; continue ;; esac
  [[ "$file" == */* || "$file" == .* ]] && { echo "skip $file (not a plain file name)" >&2; continue; }
  work="$(fresh)"
  if [[ "$engine" == kordoc ]]; then
    res="$out/${file%.*}.ir.json"
    rm -f "$res"
    # Parse only: 512 MiB (244 MiB measured for 11 files), 128 pids, 60 s, offline.
    docker run --rm --network none --read-only \
      --memory 512m --memory-swap 512m --pids-limit 128 --cap-drop ALL --security-opt no-new-privileges --user "$FORMS_USER" \
      --tmpfs /tmp:size=64m -e KORDOC_OFFLINE=1 \
      -v "$raw:/in:ro" -v "$work:/out" --entrypoint timeout "$KIMAGE" 60 \
      node /opt/kordoc/ir.mjs "/in/$file" "/out/${file%.*}.ir.json" >/dev/null 2>&1 || true
  else
    res="$out/${file%.*}.pdf"
    rm -f "$res"
    if [[ "$engine" == h2o ]]; then
      # An explicit user installation: the forms user (1999) has no passwd entry in the image, and
      # without one LibreOffice exits 77 ("User installation could not be completed").
      cmd=(soffice -env:UserInstallation=file:///home/conv/lo --headless --norestore --nolockcheck --convert-to pdf:writer_pdf_Export --outdir /out "/in/$file")
    else
      cmd=(rhwp export-pdf "/in/$file" -o "/out/${file%.*}.pdf")
    fi
    # LibreOffice needs a writable, conv-owned HOME: with a root-owned tmpfs
    # soffice hangs forever instead of failing. timeout bounds both engines.
    docker run --rm --network none --read-only \
      --memory 2g --memory-swap 2g --pids-limit 256 --cap-drop ALL --security-opt no-new-privileges \
      --user "$FORMS_USER" --tmpfs "/tmp:uid=$CU,gid=$CG" --tmpfs "/home/conv:uid=$CU,gid=$CG" -e HOME=/home/conv \
      -v "$raw:/in:ro" -v "$work:/out" --entrypoint timeout "$IMAGE" 180 "${cmd[@]}" >/dev/null 2>&1 || true
  fi
  # Accept only a regular, non-empty file: a compromised converter could leave a symlink
  # (e.g. to a key file) that later host tools would read.
  got="$work/$(basename "$res")"
  if [[ -f "$got" && ! -L "$got" && -s "$got" ]]; then
    # The IR must also be valid, size-capped JSON for exactly this source file (check_ir.py is
    # stdlib JSON validation; it opens the file with O_NOFOLLOW and parses no agency format).
    if [[ "$engine" == kordoc ]] && ! why="$(check_ir "$got" "$(sha "$raw/$file")" 2>&1)"; then
      echo "$file IR refused: ${why:0:200}" >&2
    else
      mv -f "$got" "$res"
    fi
  fi
  rm -rf --one-file-system "$work" 2>/dev/null || rm -rf "$work"
  if [[ -f "$res" && ! -L "$res" && -s "$res" ]]; then
    echo "$file $(sha "$raw/$file") $(sha "$res")"
  else
    echo "$file FAILED ($engine)" >&2
  fi
done
