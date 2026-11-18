"""Builds a blinded review packet for the independent visual reviewer.

For each template (given as letter=gates.json[:fidelity slug]) copies, under neutral names
(A, B, …): the gate images (blank, app-filled with groups applied, numbered boxes), every
page that got no boxes (from gates.py, PDF and HWP alike), the HWP preview side-by-side,
the programme name, fields-X.json (what was written into each numbered box, with its group)
and groups-X.json. Pages are numbered from 1 everywhere.

The key (letter -> slug, sha256 of the template and of the fill report the images came
from, and a random nonce the reviewer must echo) goes to <run>/keys/<packet>.key.json (or $KEYS_DIR),
outside the review/ folder the reviewer reads. verdicts.py accepts a verdict only if the
nonce matches and the template on disk still has the hash the reviewer saw.

Usage: python3 -I scripts/forms/auto/packet.py <packet dir> <fidelity png dir> <letter>=<gates.json>[:<fidelity slug>] ...
"""
from __future__ import annotations

import hashlib
import json
import os
import secrets
import shutil
import sys
from pathlib import Path


def main(argv: list[str]) -> None:
    out, fid = Path(argv[0]), Path(argv[1])
    out.mkdir(parents=True, exist_ok=False)
    run = out.parent.parent
    src = run / "sources.json"
    programme = {s["slug"]: s["program"] for s in json.loads(src.read_text())} if src.exists() else {}
    key: dict = {"_nonce": secrets.token_hex(8)}
    for spec in argv[2:]:
        letter, rest = spec.split("=", 1)
        gates_path, _, fslug = rest.partition(":")
        g = json.loads(Path(gates_path).read_text())
        if not g.get("pass"):
            raise SystemExit(f"{letter}: gates did not pass; only gate passes are reviewed")
        if not str(g["template"]).endswith(f"/out/{g['slug']}.template.json") or (fslug and g["slug"] != fslug):
            raise SystemExit(f"{letter}: gates report names another template")
        report_bytes = Path(g["fill_report"]).read_bytes()
        if hashlib.sha256(report_bytes).hexdigest() != g["fill_report_sha256"] \
                or hashlib.sha256(Path(g["template"]).read_bytes()).hexdigest() != g["template_sha256"]:
            raise SystemExit(f"{letter}: template or fill report changed after the gates ran")
        fill = json.loads(report_bytes)
        key[letter] = {"slug": g["slug"], "template": g["template"], "template_sha256": g["template_sha256"],
                       "fill_report_sha256": g["fill_report_sha256"]}
        if programme:
            (out / f"{letter}-programme.txt").write_text(programme.get(g["slug"], "") + "\n")
        for img in g["images"]:
            for f in img["files"]:
                kind = f.rsplit("-", 1)[1]  # blank.png / filled.png / boxes.png
                shutil.copy(f, out / f"{letter}-p{img['page']}-{kind}")
        for u in g.get("unboxed_pages", []):
            shutil.copy(u["file"], out / f"{letter}-p{u['page']}-no-boxes.png")
        if fslug and (fid / f"{fslug}-preview.png").exists():
            shutil.copy(fid / f"{fslug}-preview.png", out / f"{letter}-original-preview-vs-converted-p1.png")
        # Same order and numbering as the boxes image (gates.py numbers the fill report's fields per page).
        fields = []
        for page in sorted({f["page"] for f in fill["fields"]}):
            for n, f in enumerate([x for x in fill["fields"] if x["page"] == page], 1):
                fields.append({"page": page + 1, "box": n, "type": f["type"], "label": f["label"], "mapping": f.get("mapping"),
                               "required": f.get("required"), "fake_value_written": f.get("value"), "group": (f.get("group") or {}).get("id")})
        (out / f"fields-{letter}.json").write_text(json.dumps(fields, ensure_ascii=False, indent=1))
        if fill.get("groups"):
            (out / f"groups-{letter}.json").write_text(json.dumps(
                [{"id": g2["id"], "page": g2["page"] + 1, "question": g2["question"], "options": g2["options"], "mode": g2["mode"],
                  "basis": g2["basis"], "review_flag": g2["review_flag"]} for g2 in fill["groups"]], ensure_ascii=False, indent=1))
    # Outside review/, where the reviewer reads. On the server KEYS_DIR is a directory no
    # step that parses agency files can write (scripts/forms/server/form-auto.sh).
    keys = Path(os.environ["KEYS_DIR"]) if os.environ.get("KEYS_DIR") else run / "keys"
    keys.mkdir(exist_ok=True)
    (keys / f"{out.name}.key.json").write_text(json.dumps(key, ensure_ascii=False, indent=1))
    print(f"packet {out.name}: {len(key) - 1} template(s), {len(list(out.iterdir()))} files")


if __name__ == "__main__":
    main(sys.argv[1:])
