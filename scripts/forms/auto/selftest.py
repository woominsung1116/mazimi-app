"""Fail-first evidence for the machine gates: each deliberate defect must trip its gate,
and the unmodified human-corrected template must pass.

Takes a known-good template (one of the live ones) and its blank PDF, writes mutated
copies, fills each through fill.cjs and checks it with gates.py.

Usage: python3 -I scripts/forms/auto/selftest.py <good template.json> <blank.pdf> <official sha256> <work dir>
Exit 0 only if the good template passes and every mutation fails on the expected check.
"""
from __future__ import annotations

import copy
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# PY overrides the interpreter (the forms job image has the dependencies installed).
UV = os.environ["PY"].split() if os.environ.get("PY") else \
    ["uv", "run", "--no-project", "-q", "--with", "pypdfium2==4.30.0", "--with", "pillow>=10.4", "--with", "numpy>=1.26", "--with", "scipy>=1.11", "python", "-I"]


def field(t: dict, fid: str) -> dict:
    return next(f for f in t["fields"] if f["id"] == fid)


def mutations(good: dict) -> list[tuple[str, str, dict, dict | None]]:
    """(name, expected failing check, template, fill.json rect patch or None)."""
    out = []
    first = [f for f in good["fields"] if f["page"] == 0]
    name = next(f for f in first if f.get("mapping") == "name")
    text = [f for f in first if f["type"] == "text" and f["rect"]["w"] > 0.15]

    t = copy.deepcopy(good)  # a box slid left onto its printed label
    f = field(t, name["id"])
    f["rect"]["x"] = max(0.02, f["rect"]["x"] - 0.15)
    f["rect"]["w"] = 0.14
    out.append(("box over its label", "over_print", t, None))

    t = copy.deepcopy(good)  # two boxes intersect
    a, b = text[0], text[1]
    field(t, b["id"])["rect"] = {**a["rect"], "x": a["rect"]["x"] + 0.01, "y": a["rect"]["y"] + 0.005}
    out.append(("overlapping boxes", "overlap", t, None))

    t = copy.deepcopy(good)  # a phone box too narrow for a phone number
    phone = next((f for f in first if f.get("mapping") == "phone"), text[0])
    field(t, phone["id"])["rect"]["w"] = 0.025
    out.append(("box too small", "overflow", t, None))

    # (A 주민등록번호 label with no mapping is not a defect: the app's isSensitive also reads
    # the label, so it masks the box and never prefills it.)
    t = copy.deepcopy(good)  # a signature box typed as plain text: the app would print a name there
    sig = next(f for f in t["fields"] if f["type"] == "signature")
    sig["type"], sig["mapping"] = "text", None
    out.append(("signature box as text", "sensitive", t, None))

    t = copy.deepcopy(good)  # a text box slid half a row down: it straddles the rule between two rows
    f = field(t, name["id"])
    f["rect"]["y"] += f["rect"]["h"] * 0.55
    out.append(("box straddling two rows", "over_print", t, None))

    ticks = [f for f in good["fields"] if f["type"] == "checkbox" and f["page"] == 0]
    if ticks:
        t = copy.deepcopy(good)  # a tick box spread over its printed option word (text on either side)
        f = field(t, ticks[0]["id"])
        f["rect"]["x"] -= 0.045
        f["rect"]["w"] += 0.09
        out.append(("tick box on label text", "over_print", t, None))

    boxes = [f for f in good["fields"] if f["type"] == "checkbox"]
    if len(boxes) >= 2 and len({f["page"] for f in boxes}) >= 2:
        a = boxes[0]
        b = next(f for f in boxes if f["page"] != a["page"])
        bad = {"groups": [{"id": "g1", "page": a["page"], "mode": "single", "members": [a["id"]]},
                          {"id": "g2", "page": a["page"], "mode": "single", "members": [a["id"], b["id"]]}]}
        out.append(("one-option group and a group across pages", "groups", copy.deepcopy(good), {"groups": bad}))

    t = copy.deepcopy(good)  # the checker's rect disagrees with where the app drew: ink outside boxes
    out.append(("ink outside boxes", "outside", t, {"id": name["id"], "dy": 0.06}))
    return out


def run(t: dict, tag: str, blank: str, sha: str, work: Path, patch: dict | None) -> dict:
    tpl = work / f"{tag}.template.json"
    tpl.write_text(json.dumps(t, ensure_ascii=False))
    filled, report = work / f"{tag}.filled.pdf", work / f"{tag}.fill.json"
    groups = []
    if patch and "groups" in patch:
        (work / f"{tag}.groups.json").write_text(json.dumps(patch["groups"]))
        groups = [str(work / f"{tag}.groups.json")]
        patch = None
    subprocess.run(["node", str(HERE / "fill.cjs"), str(tpl), blank, sha, "hwp" if t.get("derived") else "pdf", str(filled), str(report)] + groups, check=True)
    if patch:
        r = json.loads(report.read_text())
        for f in r["fields"]:
            if f["id"] == patch["id"]:
                f["rect"]["y"] += patch["dy"]
        report.write_text(json.dumps(r, ensure_ascii=False))
    subprocess.run(UV + [str(HERE / "gates.py"), tag, str(tpl), blank, str(filled), str(report), str(work)], check=True)
    return json.loads((work / f"{tag}.gates.json").read_text())


def main(good_path: str, blank: str, sha: str, work_dir: str) -> None:
    work = Path(work_dir)
    work.mkdir(parents=True, exist_ok=True)
    good = json.loads(Path(good_path).read_text())
    ok = True
    base = run(good, "good", blank, sha, work, None)
    print(f"good template: {'PASS' if base['pass'] else 'FAIL'}")
    ok &= base["pass"]
    for i, (name, expect, t, patch) in enumerate(mutations(good)):
        res = run(t, f"mut{i}", blank, sha, work, patch)
        hit = expect in res["problems"]
        print(f"mutation '{name}': expected {expect} -> {'caught' if hit else 'MISSED'} ({', '.join(res['problems'])})")
        ok &= hit
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main(*sys.argv[1:5])
