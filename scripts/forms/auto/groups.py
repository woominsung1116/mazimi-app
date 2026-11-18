# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9"]
# ///
"""Checkbox groups: which tick boxes answer one question, and may more than one be ticked.

propose_fields.py writes the groups with evidence into the template (into_template), after
the overrides. Run as a script, this also writes the full result, uncertain groups and notes
included, as a sidecar <slug>.groups.json for the reviewer; fill.cjs ticks one option per
single group.

Grouping, per page:
  question   a box's label is "<question>: <option>" (propose_fields) or "<question> <option>"
             with a known answer word at the end (동의함/동의안함, 예/아니오, 해당/비해당 …).
             Boxes with the same question form a group. Rows under one heading word
             ('구분 취업준비생: …', '구분 기취업자: …') join when they are close (merged_rows).
  line       boxes on one printed line with no question of their own form a group.
Mode:
  single     a clear binary pair (동의함/동의안함, 예/아니오, 해당/비해당, 남/여, 유/무, 있음/없음),
             ○ radio options (the box sits on a printed ○), a 구분/유형 heading
  multi      "복수", "모두", "중복" printed in the question or on the line
  otherwise  multi, review_flag "uncertain": never force a one-answer rule on a guess
Every group must have >= 2 options, all checkboxes, all on one page; otherwise it is a problem.

Usage: python -I groups.py <template.json> <blank.pdf> <out.groups.json>
"""
from __future__ import annotations

import json
import re
import sys
import unicodedata
from pathlib import Path

import pdfplumber

BINARY = [("동의함", "동의안함"), ("동의함", "동의하지않음"), ("동의", "미동의"), ("예", "아니오"), ("예", "아니요"),
          ("해당", "비해당"), ("해당", "해당없음"), ("남", "여"), ("유", "무"), ("있음", "없음"), ("○(예)", "×(아니오)")]
ANSWER_WORDS = sorted({w for pair in BINARY for w in pair}, key=len, reverse=True)
# Explicit permission to tick several. '중복신청 여부' (a yes/no question about double
# application) is not one, so a bare 중복/모두 never counts.
MULTI = re.compile(r"(복수|중복)\s*(선택|체크|표시|응답|기재)|모두\s*(선택|체크|표시|기재)|해당\s*(되는\s*)?(항목|사항|곳)\S*\s*모두")
SINGLE_HEAD = re.compile(r"^(구분|유형|성별)(\s|$)|(구분|유형)$")


def negation_pair(a: str, b: str) -> bool:
    """X / 미X, 비X, 불X, X하지않음, X없음, 여/부: one option is the other's negation."""
    a, b = norm(a), norm(b)
    for x, y in ((a, b), (b, a)):
        if y in (f"미{x}", f"비{x}", f"불{x}", f"{x}하지않음", f"{x}안함", f"{x}없음") or (x, y) == ("여", "부"):
            return True
        if x.endswith("함") and y == x[:-1] + "하지않음":
            return True
    return False
ROW_GAP = 0.06  # page fraction: rows under one heading word further apart are separate questions


def norm(s: str) -> str:
    return re.sub(r"\s+", "", s)


def split_label(label: str) -> tuple[str, str]:
    if ": " in label:
        q, o = label.rsplit(": ", 1)
        return q.strip(), o.strip()
    c = label.strip()
    for w in ANSWER_WORDS:
        if norm(c) == norm(w):
            return "", w
        if norm(c).endswith(norm(w)):
            # Cut from the right until the stripped tail spells the answer word (label spacing varies).
            k = len(c)
            while k > 0 and norm(c[k:]) != norm(w):
                k -= 1
            return c[:k].strip(), w
    return "", c


def glyph_under(page, rect: dict) -> str:
    W, H = float(page.width), float(page.height)
    x0, x1, y0, y1 = rect["x"] * W, (rect["x"] + rect["w"]) * W, rect["y"] * H, (rect["y"] + rect["h"]) * H
    chars = [c["text"] for c in page.chars if x0 - 1 <= (c["x0"] + c["x1"]) / 2 <= x1 + 1 and y0 - 1 <= (c["top"] + c["bottom"]) / 2 <= y1 + 1]
    return "".join(chars)


def band_text(page, y0: float, y1: float) -> str:
    H = float(page.height)
    return " ".join(w["text"] for w in page.extract_words() if y0 * H <= w["top"] <= y1 * H)


def build(template: dict, pdf_path: Path) -> dict:
    boxes = [f for f in template["fields"] if f["type"] == "checkbox"]
    groups: list[dict] = []
    skipped: list[str] = []
    with pdfplumber.open(pdf_path) as pdf:
        for page_no in sorted({f["page"] for f in boxes}):
            page = pdf.pages[page_no]
            items = []
            for f in [b for b in boxes if b["page"] == page_no]:
                q, o = split_label(f["label"])
                items.append({"f": f, "q": q, "o": o, "y": f["rect"]["y"] + f["rect"]["h"] / 2, "glyph": glyph_under(page, f["rect"])})
            # 1. same question text
            by_q: dict[str, list] = {}
            loose = []
            for it in items:
                (by_q.setdefault(norm(it["q"]), []).append(it) if it["q"] else loose.append(it))
            # 2. boxes without a question: one printed line
            for it in loose:
                key = next((k for k, v in by_q.items() if k.startswith("@line") and abs(v[0]["y"] - it["y"]) < 0.006), None)
                by_q.setdefault(key or f"@line{it['y']:.4f}", []).append(it)
            # 3. rows under one heading word join ('구분 취업준비생' + '구분 기취업자')
            keys = [k for k in by_q if not k.startswith("@line")]
            merged: dict[str, str] = {}
            for i, a in enumerate(keys):
                for b in keys[i + 1:]:
                    qa, qb = by_q[a][0]["q"].split(), by_q[b][0]["q"].split()
                    if len(qa) > 1 and len(qb) > 1 and qa[0] == qb[0] and len(qa[0]) >= 2 and SINGLE_HEAD.search(qa[0]):
                        gap = min(abs(x["y"] - y["y"]) for x in by_q[a] for y in by_q[b])
                        if gap <= ROW_GAP:
                            merged[b] = merged.get(a, a)
            final: dict[str, dict] = {}
            for k, its in by_q.items():
                root = merged.get(k, k)
                g = final.setdefault(root, {"items": [], "merged_rows": False})
                g["items"].extend(its)
                g["merged_rows"] |= root != k
            for root, g in final.items():
                its = sorted(g["items"], key=lambda i: (round(i["y"], 3), i["f"]["rect"]["x"]))
                if len(its) < 2:
                    continue  # a lone tick box is an independent yes/no mark, not a choice
                if len({norm(i["o"]) for i in its}) < len(its):
                    skipped.append(f"p{page_no + 1} '{its[0]['q'] or '(같은 줄)'}': repeated identical options ({its[0]['o'][:20]}) are per-row items, left ungrouped")
                    continue
                question = its[0]["q"].split()[0] if g["merged_rows"] else its[0]["q"]
                options = [i["o"] for i in its]
                ys = [i["y"] for i in its]
                around = band_text(page, min(ys) - 0.04, max(ys) + 0.01)
                pair = len(options) == 2 and (any({norm(options[0]), norm(options[1])} == {norm(a), norm(b)} for a, b in BINARY)
                                              or negation_pair(options[0], options[1]))
                radio = all("○" in i["glyph"] for i in its)
                if MULTI.search(question) or MULTI.search(around):
                    mode, basis, flag = "multi", "printed 복수/모두/중복", None
                elif pair:
                    mode, basis, flag = "single", "binary pair", None
                elif radio:
                    mode, basis, flag = "single", "○ radio row", None
                elif SINGLE_HEAD.search(question):
                    mode, basis, flag = "single", f"heading '{question}'", "merged_rows" if g["merged_rows"] else None
                else:
                    mode, basis, flag = "multi", "uncertain: not a binary pair, no 복수 wording", "uncertain"
                if g["merged_rows"] and flag is None:
                    flag = "merged_rows"
                groups.append({"page": page_no, "question": question or "(같은 줄)", "options": options, "members": [i["f"]["id"] for i in its],
                               "mode": mode, "basis": basis, "review_flag": flag})
    for n, g in enumerate(groups, 1):
        g["id"] = f"g{n}"
    problems = []
    ids = {f["id"]: f for f in template["fields"]}
    for g in groups:
        pages = {ids[m]["page"] for m in g["members"]}
        if len(g["members"]) < 2 or len(pages) != 1 or any(ids[m]["type"] != "checkbox" for m in g["members"]):
            problems.append(f"{g['id']}: needs >= 2 checkbox options on one page")
    seen: dict[str, str] = {}
    for g in groups:
        for m in g["members"]:
            if m in seen:
                problems.append(f"{m} is in {seen[m]} and {g['id']}")
            seen[m] = g["id"]
    counts = {"checkboxes": len(boxes), "grouped": len(seen), "groups": len(groups),
              "single": sum(g["mode"] == "single" for g in groups), "multi": sum(g["mode"] == "multi" for g in groups),
              "flagged": sum(bool(g["review_flag"]) for g in groups)}
    return {"groups": groups, "counts": counts, "problems": problems, "ungrouped_notes": skipped}


MAX_GROUPS = 100  # crates/core form_templates.rs MAX_GROUPS


def group_label(g: dict) -> str:
    """The question, or the options themselves for a line of boxes with no question of its own.
    Plain text only, 1-80 characters (the server's group label rule)."""
    text = g["question"] if g["question"] != "(같은 줄)" else " / ".join(g["options"])
    text = "".join(ch for ch in text if unicodedata.category(ch) not in ("Cc", "Cf", "Co", "Cs"))
    return re.sub(r"\s+", " ", text).strip()[:80].strip()


def into_template(template: dict, result: dict) -> dict:
    """Writes the groups build() found into the template itself (field `group`, template
    `groups`), so the app keeps one answer for a single question. Only groups with evidence
    go in: single ones (a binary pair, a ○ radio row, a 구분/유형 heading) and multi ones the
    form prints as such. An uncertain group stays out: its boxes stay independent, which is
    what multi would mean anyway. Each kept group meets the server's rules (id, label, 2+
    checkbox options on one page); one that does not is left out, never half-written."""
    fields = {f["id"]: f for f in template["fields"]}
    for f in template["fields"]:
        f.pop("group", None)
    kept: list[dict] = []
    for g in result["groups"]:
        if g["review_flag"] == "uncertain" or any(m not in fields for m in g["members"]):
            continue
        members = [fields[m] for m in g["members"]]
        label = group_label(g)
        if len(members) < 2 or len({f["page"] for f in members}) != 1 or any(f["type"] != "checkbox" for f in members) or not label:
            continue
        if any(f.get("group") for f in members) or len(kept) == MAX_GROUPS:
            continue
        gid = f"g{len(kept) + 1}"  # the server's id rule: [a-z0-9_-]{1,40}
        for f in members:
            f["group"] = gid
        kept.append({"id": gid, "mode": g["mode"], "label": label})
    template.pop("groups", None)
    if kept:
        template["groups"] = kept
    return template


def main(tpl: str, pdf: str, out: str) -> None:
    result = build(json.loads(Path(tpl).read_text()), Path(pdf))
    Path(out).write_text(json.dumps(result, ensure_ascii=False, indent=1))
    # Counts only: questions and options are document text (see the JSON file).
    print(Path(tpl).name, json.dumps(result["counts"]), f"problems {len(result['problems'])}, ungrouped notes {len(result['ungrouped_notes'])}")


if __name__ == "__main__":
    main(*sys.argv[1:4])
