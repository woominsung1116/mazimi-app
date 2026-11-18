#!/usr/bin/env bash
# Checks live form templates through the public endpoints the app calls
# (form-templates-prod-20261016.md §2.5). No token, GET only.
#
#   scripts/forms/auto/verify-prod.sh <out dir> <program_id>:<resource_id> ...
#
# Per pair: program detail lists the resource under application_resources.templates;
# form-template is 200 with fields; for HWP its derived.source_sha256 is set; form-pdf is
# 200 application/pdf with nosniff, and ETag == sha256(body) == template content_hash.
# Exit 1 if any check fails.
set -uo pipefail
API="${MAZIMI_API_URL:?set MAZIMI_API_URL (docs/ops-env.md)}/api/v1"
out="$1"; shift
mkdir -p "$out"
fail=0
for pair in "$@"; do
  prog="${pair%%:*}"; res="${pair##*:}"
  p="$out/$res"
  c1=$(curl -sS -o "$p.program.json" -w '%{http_code}' "$API/programs/$prog")
  c2=$(curl -sS -o "$p.template.json" -w '%{http_code}' "$API/programs/$prog/application-resources/$res/form-template")
  c3=$(curl -sS -D "$p.pdf.headers" -o "$p.pdf" -w '%{http_code}' "$API/programs/$prog/application-resources/$res/form-pdf")
  python3 -I - "$p" "$res" "$c1" "$c2" "$c3" <<'PY' || fail=1
import hashlib, json, sys
p, res, c1, c2, c3 = sys.argv[1:6]
problems = []
prog = json.load(open(p + ".program.json")) if c1 == "200" else {}
pol = prog.get("application_policy") or {}
listed = [t for t in ((pol.get("resources") or prog.get("application_resources") or {}).get("templates") or []) if t.get("id") == res]
if c1 != "200" or not listed:
    problems.append(f"program {c1}, resource listed={bool(listed)}")
tpl = json.load(open(p + ".template.json")) if c2 == "200" else {}
tpl = tpl.get("template", tpl)
fields = tpl.get("fields") or []
if c2 != "200" or not fields:
    problems.append(f"form-template {c2}, fields={len(fields)}")
fmt = listed[0].get("file_format") if listed else None
derived = tpl.get("derived") or {}
if fmt in ("hwp", "hwpx"):
    if not derived.get("source_sha256"):
        problems.append("HWP template without derived.source_sha256")
    headers = open(p + ".pdf.headers", encoding="latin-1").read().lower()
    body = open(p + ".pdf", "rb").read() if c3 == "200" else b""
    digest = hashlib.sha256(body).hexdigest()
    etag = next((l.split(":", 1)[1].strip().strip('"') for l in headers.splitlines() if l.startswith("etag:")), "")
    if c3 != "200" or "application/pdf" not in headers or "nosniff" not in headers:
        problems.append(f"form-pdf {c3} or headers missing pdf/nosniff")
    if not (digest == etag == tpl.get("content_hash")):
        problems.append(f"pdf sha {digest[:12]} etag {etag[:12]} template {str(tpl.get('content_hash'))[:12]} differ")
print(f"{res[:8]} {'OK' if not problems else 'FAIL'} fields={len(fields)} format={fmt} valid_until={listed[0].get('valid_until') if listed else None} "
      f"content_hash={str(tpl.get('content_hash'))[:12]} source_sha256={str(derived.get('source_sha256'))[:12]} {'; '.join(problems)}")
sys.exit(1 if problems else 0)
PY
done
exit $fail
