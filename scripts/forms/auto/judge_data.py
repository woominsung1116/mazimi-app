# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9", "pypdfium2==4.30.0", "pillow>=10.4", "numpy>=1.26", "olefile==0.47"]
# ///
"""Few-shot examples and a labelled development set for judge.py, from human decisions only.

Every answer here comes from what a person decided on a reviewed template, never from a model:
  owner        a box the person linked to a profile key is the applicant's; a box left unlinked
               whose label or block names another person (보호자, 직계존속 …) is someone else's
  page_role    a page the person templated must be filled; a page left out of a reviewed
               template was judged optional or someone else's (any non-"applicant_must" answer
               counts as correct for it)
  label_match  the person's label for a box names the printed label under it (same); the label
               of another box on the page, with a different meaning, does not (different)
  programme    a form's heading against its own programme (yes) and another programme (no)
  group_mode   past reviewer corrections (dev only; the reviewer is independent of the judge)
Few-shot examples come from the SHOT agencies; the development set from every other reviewed
template, so no example is scored on itself. The held-out set (crawled blanks of other
agencies) is not touched here: its labels come from the labelling sheet (heldout.py).

Corpus: a JSON list of {set: train|past, name, template, blank, ir, source_sha256, groups,
review, programme} (see scripts/forms/auto/review_eval.py).

Usage:
  python -I judge_data.py shots <corpus.json> <out judge_shots.json>
  python -I judge_data.py devset <corpus.json> <out dev.jsonl>
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "kordoc"))

import pdfplumber  # noqa: E402

import check_ir  # noqa: E402
import slots  # noqa: E402
from propose_fields import OTHER_PERSON, compact  # noqa: E402

# Agencies whose reviewed templates give the few-shot examples. Every other reviewed template
# is development data. Keep this list disjoint from the past review batches (wonju, jeju,
# kongju) so the past-batch gate never scores an example the prompt already shows.
SHOT = ("gachon-", "gyeonggi-", "incheon-", "jeonju-", "keimyung-", "konkuk-")
PER_KIND = {"owner": 4, "page_role": 3, "label_match": 4, "programme": 2, "group_mode": 2, "label_role": 2, "colon_blank": 2}
SHOT_TEXT = 80  # characters kept of each quoted text in an example (prompt length is the latency)


def load(item: dict) -> tuple[dict, dict]:
    t = json.loads(Path(item["template"]).read_text())
    ir = check_ir.load(item["ir"], item["source_sha256"]) if item.get("ir") else None
    return t, slots.check(t, Path(item["blank"]), ir, item.get("groups"))


def heading(pdf: str, page: int) -> str:
    with pdfplumber.open(pdf) as doc:
        p = doc.pages[page]
        return re.sub(r"\s+", " ", p.crop((0, 0, p.width, p.height / 3)).extract_text() or "")[:120]


def questions_for(item: dict) -> list[dict]:
    """[{question, context, truth: [acceptable answers], source}] for one reviewed template."""
    t, res = load(item)
    out = []
    programme = item.get("programme", "")
    ctx = {"programme": programme}
    with pdfplumber.open(item["blank"]) as pdf:
        page_cells = [slots.pdf_cells(p) for p in pdf.pages]
    owners_ir = slots.ir_owners(check_ir.load(item["ir"], item["source_sha256"])) if item.get("ir") else {}
    numbers = slots.box_numbers(t)
    # owner
    for f in t["fields"]:
        if f["type"] == "checkbox":
            continue
        owner = slots.field_owner(f, page_cells[f["page"]], owners_ir)
        if not owner or slots.norm(owner) in slots.norm(f["label"]):
            continue
        q = {"id": f"owner-{f['id']}", "kind": "owner", "page": f["page"] + 1, "box": numbers[f["id"]], "header": owner,
             "label": f["label"], "mapping": f.get("mapping")}
        if f.get("mapping") in slots.APPLICANT_KEYS:
            out.append({"question": q, "context": ctx, "truth": ["applicant"], "source": "human mapping"})
        elif OTHER_PERSON.search(compact(f["label"])) or OTHER_PERSON.search(owner):
            out.append({"question": {**q, "mapping": "name"}, "context": ctx, "truth": ["other_person"], "source": "human left another person's box unlinked"})
    # page_role: unboxed pages the person left out, and the pages they templated
    templated = sorted({f["page"] for f in t["fields"]})
    for q in res["questions"]:
        if q["kind"] == "page_role":
            out.append({"question": q, "context": ctx, "truth": ["optional_or_conditional", "someone_else", "notice_or_instructions"],
                        "source": "human left the page out of the reviewed template"})
    for p in templated:
        sample = sorted({s["label"] for s in res["slots"] if s["page"] == p + 1 and s["label"] and s["status"] == "covered"})[:8]
        out.append({"question": {"id": f"page-{p + 1}", "kind": "page_role", "page": p + 1, "title": heading(item["blank"], p),
                                 "blanks": len(sample), "sample": sample}, "context": ctx, "truth": ["applicant_must"], "source": "human templated the page"})
    # label_match: printed label vs the person's label (same), and vs another box's label (different)
    covered = [s for s in res["slots"] if s["kind"] == "value" and s["status"] == "covered" and s["label"] and s["covered_by"]]
    by_id = {f["id"]: f for f in t["fields"]}
    for s in covered:
        f = by_id[s["covered_by"][0]]
        out.append({"question": {"id": f"match-{f['id']}", "kind": "label_match", "page": s["page"], "box": numbers[f["id"]],
                                 "printed": s["label"], "header": s["owner"], "label": f["label"]},
                    "context": ctx, "truth": ["same"], "source": "human label on the printed label"})
    for a in covered:
        b = next((x for x in covered if x["page"] == a["page"] and x is not a and x["covered_by"][0] != a["covered_by"][0]
                  and slots.classify(x["label"]) != slots.classify(a["label"])), None)
        if b is None:
            continue
        f = by_id[b["covered_by"][0]]
        out.append({"question": {"id": f"swap-{f['id']}", "kind": "label_match", "page": a["page"], "box": numbers[f["id"]],
                                 "printed": a["label"], "header": a["owner"], "label": f["label"]},
                    "context": ctx, "truth": ["different"], "source": "another box's human label"})
    for r in out:  # what slots.py settles without the judge (label_match is asked only when this is False)
        q = r["question"]
        if q["kind"] == "label_match":
            r["prefilter_agrees"] = slots.label_agrees(q["printed"], q["label"], "value")
    # programme
    if programme and templated:
        out.append({"question": {"id": "programme", "kind": "programme", "page": None, "title": heading(item["blank"], templated[0]),
                                 "programme": programme}, "context": ctx, "truth": ["yes"], "source": "its own programme"})
    # group_mode: reviewer corrections on past batches
    for q in res["questions"]:
        if q["kind"] == "group_mode" and item.get("review"):
            fixed = [x for x in item["review"]["findings"] if x["kind"] == "grouping" and re.search(re.escape(q["question"][:8]), x["detail"])]
            truth = ("one" if "single" in fixed[0]["detail"] else "several") if fixed else {"single": "one", "multi": "several"}[q["mode"]]
            out.append({"question": q, "context": ctx, "truth": [truth], "source": "past reviewer verdict"})
    return out


def cross_programme(items: list[dict], qs: dict) -> list[dict]:
    """Each form heading against the next template's programme (a different programme)."""
    out = []
    names = [i["name"] for i in items if any(q["question"]["kind"] == "programme" for q in qs[i["name"]])]
    for k, n in enumerate(names):
        other = next(i for i in items if i["name"] == names[(k + 1) % len(names)])
        mine = next(q for q in qs[n] if q["question"]["kind"] == "programme")
        if other["programme"] and other["programme"] != mine["question"]["programme"]:
            q = {**mine["question"], "id": "programme-other", "programme": other["programme"]}
            out.append({"question": q, "context": {"programme": other["programme"]}, "truth": ["no"], "source": "another template's programme", "template": n})
    return out


def build(corpus: str, shot: bool) -> list[dict]:
    items = [i for i in json.loads(Path(corpus).read_text()) if i["set"] == ("train" if shot else i["set"])]
    items = [i for i in items if i["name"].startswith(SHOT) == shot]
    if not shot:  # one entry per template file: a past item repeats a reviewed template
        seen, uniq = set(), []
        for i in sorted(items, key=lambda i: i["set"] != "past"):
            if (i["template"], json.dumps(i.get("groups"), sort_keys=True)) not in seen:
                seen.add((i["template"], json.dumps(i.get("groups"), sort_keys=True)))
                uniq.append(i)
        items = uniq
    qs = {i["name"]: [{**q, "template": i["name"]} for q in questions_for(i)] for i in items}
    out = [q for i in items for q in qs[i["name"]]] + cross_programme(items, qs)
    return out


def main(argv: list[str]) -> None:
    cmd, corpus, dst = argv
    rows = build(corpus, cmd == "shots")
    if cmd == "shots":
        picked: list[dict] = []
        for kind, n in PER_KIND.items():
            pool = [r for r in rows if r["question"]["kind"] == kind]
            # alternate answers so every choice the corpus has appears
            by_ans: dict[str, list] = {}
            for r in pool:
                by_ans.setdefault(r["truth"][0], []).append(r)
            while len([p for p in picked if p["kind"] == kind]) < n and any(by_ans.values()):
                for ans in list(by_ans):
                    if by_ans[ans] and len([p for p in picked if p["kind"] == kind]) < n:
                        r = by_ans[ans].pop(0)
                        q = {k: (v[:SHOT_TEXT] if isinstance(v, str) else [x[:30] for x in v[:4]] if isinstance(v, list) else v) for k, v in r["question"].items()}
                        picked.append({"kind": kind, "question": q, "context": {k: v[:SHOT_TEXT] for k, v in r["context"].items()}, "answer": r["truth"][0],
                                       "source": f"{r['template']}: {r['source']}"})
        Path(dst).write_text(json.dumps({"version": 1, "about": "Few-shot examples for judge.py, built by judge_data.py shots from human-reviewed templates of the SHOT agencies.",
                                         "shots": picked}, ensure_ascii=False, indent=1) + "\n")
        print(f"{len(picked)} shots", {k: sum(p['kind'] == k for p in picked) for k in PER_KIND})
    else:
        with open(dst, "w", encoding="utf-8") as fh:
            for r in rows:
                fh.write(json.dumps(r, ensure_ascii=False) + "\n")
        counts: dict = {}
        for r in rows:
            counts[r["question"]["kind"]] = counts.get(r["question"]["kind"], 0) + 1
        print(f"{len(rows)} dev questions", counts)


if __name__ == "__main__":
    main(sys.argv[1:])
