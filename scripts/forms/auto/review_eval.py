# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9", "pypdfium2==4.30.0", "pillow>=10.4", "numpy>=1.26", "olefile==0.47"]
# ///
"""Measures the deterministic review (slots.py) and, given judge answers, the whole step 8
against the past reviewer batches and against self-labelling mutations of reviewed templates.

  past       each of the 25 findings in scripts/forms/tests/fixtures/past-review-key.json:
             caught by slots.py alone, sent to the judge as a question, or missed; and every
             template the reviewer failed must fail (zero false passes)
  mutations  on reviewed templates (labels from the mutation itself, never from a model):
             drop     remove one box -> a missing (or wrong_type) finding at its place
             person   link a box in another person's block to the profile (name/phone/...),
                      and the same with the person word removed from its label
                      -> wrong_person, or an owner question for the judge
             type     a tick box retyped as text, a signature box as text -> wrong_type
             swap     two value boxes swap labels -> a label_match question for the judge
  judge      with --judge <answers.jsonl> (judge.py ask output on the dev set): accuracy per
             question kind, false passes (an answer that lets a defect through) and latency

Corpus JSON: [{set: train|past, name, template, blank, ir, source_sha256, groups, review, programme}]
('past' names start with the key's item id, e.g. 'b1A-...').

Usage: python -I review_eval.py <corpus.json> <out.json> [--judge answers.jsonl] [--drop-per-template 8]
"""
from __future__ import annotations

import argparse
import copy
import json
import re
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "kordoc"))

import check_ir  # noqa: E402
import slots  # noqa: E402
from propose_fields import OTHER_PERSON, compact  # noqa: E402

KEY = HERE.parent / "tests" / "fixtures" / "past-review-key.json"
NEAR = re.compile(r"near \(([0-9.]+),([0-9.]+)\)")


def run(item: dict, template: dict | None = None) -> dict:
    t = template or json.loads(Path(item["template"]).read_text())
    ir = check_ir.load(item["ir"], item["source_sha256"]) if item.get("ir") else None
    return slots.check(t, Path(item["blank"]), ir, item.get("groups"))


def finding_matches(f: dict, page: int, m: dict) -> bool:
    if f["kind"] not in m["kinds"] or (f.get("page") not in (page, None) and m["kinds"] != ["grouping"]):
        return False
    if "box" in m and f.get("box") != m["box"]:
        return False
    if "label_re" in m and not re.search(m["label_re"], f.get("detail", "")):
        return False
    if "near_y" in m:
        g = NEAR.search(f.get("detail", ""))
        if not g or not (m["near_y"][0] <= float(g.group(2)) <= m["near_y"][1]):
            return False
    return True


def question_matches(q: dict, page: int, m: dict) -> bool:
    """Would a 'fail' answer to this question produce the expected finding?"""
    from judge import KINDS
    kind = KINDS.get(q["kind"], {}).get("finding")
    if kind not in m["kinds"] or q.get("page") not in (page, None):
        return False
    if "box" in m and q.get("box") != m["box"]:
        return False
    if "label_re" in m and not re.search(m["label_re"], json.dumps(q, ensure_ascii=False)):
        return False
    return True


def past(items: list[dict], answers: dict) -> dict:
    key = json.loads(KEY.read_text())
    res = {it["name"].split("-")[0]: run(it) for it in items if it["set"] == "past"}
    rows, by_kind = [], {}
    for k in key["findings"]:
        r = res[k["item"]]
        det = [f for f in r["findings"] if finding_matches(f, k["page"], k["match"])]
        same = [f for f in r["findings"] if "same_spot" in k and finding_matches(f, k["page"], k["same_spot"])]
        qs = [q for q in r["questions"] if question_matches(q, k["page"], k["match"])]
        judged = [q for q in qs if answers.get((k["item"], q["id"])) not in (None, *_pass_answers(q))]
        status = "slots" if det else "judge" if judged else "question" if qs else "same_spot" if same else "missed"
        rows.append({**{x: k[x] for x in ("item", "page", "kind")}, "box": k.get("box"), "status": status})
        by_kind.setdefault(k["kind"], {}).setdefault(status, 0)
        by_kind[k["kind"]][status] += 1
    verdicts = {}
    for item, want in key["templates"].items():
        r = res[item]
        judge_findings = [q for q in r["questions"] if answers.get((item, q["id"])) not in (None, *_pass_answers(q))]
        unanswered = [q for q in r["questions"] if (item, q["id"]) not in answers]
        got = "fail" if r["findings"] else "fail" if judge_findings else ("pass" if not unanswered else "needs_judge")
        verdicts[item] = {"reviewer": want, "slots_findings": len(r["findings"]), "questions": len(r["questions"]),
                          "pipeline": got, "false_pass": want == "fail" and got == "pass"}
    return {"findings": rows, "by_kind": by_kind, "templates": verdicts,
            "false_passes": sum(v["false_pass"] for v in verdicts.values()),
            "fail_templates_failed_by_slots_alone": sum(1 for v in verdicts.values() if v["reviewer"] == "fail" and v["slots_findings"]),
            "fail_templates": sum(1 for v in verdicts.values() if v["reviewer"] == "fail")}


def _pass_answers(q: dict) -> tuple:
    from judge import KINDS
    if q["kind"] == "group_mode":  # never fails a template (judge.py: the deliberate exception)
        return tuple(KINDS["group_mode"]["choices"])
    return tuple(KINDS.get(q["kind"], {}).get("pass", ()))


def new_findings(base: dict, after: dict) -> list[dict]:
    seen = {json.dumps(f, sort_keys=True, ensure_ascii=False) for f in base["findings"]}
    return [f for f in after["findings"] if json.dumps(f, sort_keys=True, ensure_ascii=False) not in seen]


def new_questions(base: dict, after: dict) -> list[dict]:
    seen = {json.dumps(q, sort_keys=True, ensure_ascii=False) for q in base["questions"]}
    return [q for q in after["questions"] if json.dumps(q, sort_keys=True, ensure_ascii=False) not in seen]


def mutations(items: list[dict], drop_per: int) -> dict:
    out = {"drop": {"n": 0, "caught": 0, "misses": []}, "person": {"n": 0, "caught": 0, "to_judge": 0, "misses": []},
           "person_bare": {"n": 0, "caught": 0, "to_judge": 0, "misses": []}, "type": {"n": 0, "caught": 0, "misses": []},
           "swap": {"n": 0, "to_judge": 0, "caught": 0, "misses": []}}
    seen_tpl = set()
    for it in items:
        if it["template"] in seen_tpl:
            continue
        seen_tpl.add(it["template"])
        t = json.loads(Path(it["template"]).read_text())
        base = run(it, t)
        covered = {fid for s in base["slots"] if s["status"] == "covered" for fid in s["covered_by"]}
        # drop: boxes that cover a slot the enumerator sees (a box on nothing printed cannot be missed)
        cands = [f for f in t["fields"] if f["id"] in covered]
        step = max(1, len(cands) // drop_per) if drop_per else 1
        for f in cands[::step][:drop_per or None]:
            m = copy.deepcopy(t)
            m["fields"] = [x for x in m["fields"] if x["id"] != f["id"]]
            if not any(x["page"] == f["page"] for x in m["fields"]):
                continue  # the page would become unboxed: a page-level question instead
            after = run(it, m)
            r = f["rect"]
            hit = any(x["kind"] in ("missing", "wrong_type") and x.get("page") == f["page"] + 1 and _near(x, r) for x in new_findings(base, after))
            out["drop"]["n"] += 1
            out["drop"]["caught"] += hit
            if not hit:
                out["drop"]["misses"].append(f"{it['name']} p{f['page'] + 1} {f['type']} '{f['label'][:30]}'")
        nums = slots.box_numbers(t)
        # person: another person's box (the person's own label says so) linked to the applicant
        for f in t["fields"]:
            if f["type"] == "checkbox" or not OTHER_PERSON.search(compact(f["label"])) or f.get("mapping") in slots.APPLICANT_KEYS:
                continue
            kind = slots.classify(re.sub(r"보호자|부모|직계\s*존속|대리인|임대인|추천인|배우자|가구원|세대원|관계인|비상연락망?", "", f["label"])) or "name"
            kind = kind if kind in slots.APPLICANT_KEYS else "name"
            for variant in ("person", "person_bare"):
                m = copy.deepcopy(t)
                g = next(x for x in m["fields"] if x["id"] == f["id"])
                g["mapping"] = kind
                if variant == "person_bare":
                    g["label"] = re.split(r"\s+", g["label"].strip())[-1]
                    if OTHER_PERSON.search(compact(g["label"])):
                        continue
                after = run(it, m)
                hit = any(x["kind"] == "wrong_person" and x.get("box") == nums[f["id"]] and x.get("page") == f["page"] + 1 for x in new_findings(base, after))
                asked = any(q["kind"] == "owner" and q.get("field") == f["id"] for q in new_questions(base, after))
                out[variant]["n"] += 1
                out[variant]["caught"] += hit
                out[variant]["to_judge"] += (not hit) and asked
                if not hit and not asked:
                    out[variant]["misses"].append(f"{it['name']} p{f['page'] + 1} '{g['label'][:30]}' -> {kind}")
        # type: a covered tick box retyped as text, a covered signature box as text
        for s in base["slots"]:
            if s["status"] != "covered" or s["kind"] not in ("checkbox", "sign"):
                continue
            fid = s["covered_by"][0]
            m = copy.deepcopy(t)
            g = next(x for x in m["fields"] if x["id"] == fid)
            g["type"] = "text"
            after = run(it, m)
            hit = any(x["kind"] == "wrong_type" and x.get("box") == nums[fid] for x in new_findings(base, after))
            out["type"]["n"] += 1
            out["type"]["caught"] += hit
            if not hit:
                out["type"]["misses"].append(f"{it['name']} p{s['page']} {s['kind']} '{s['label'][:20]}'")
            if out["type"]["n"] % 6 == 5:
                break  # a few per template is enough
        # swap: two covered value boxes on a page exchange labels
        vals = [s for s in base["slots"] if s["status"] == "covered" and s["kind"] == "value" and s["covered_by"]]
        for a, b in zip(vals, vals[1:]):
            if a["page"] != b["page"] or a["covered_by"][0] == b["covered_by"][0]:
                continue
            fa, fb = a["covered_by"][0], b["covered_by"][0]
            la, lb = next(x["label"] for x in t["fields"] if x["id"] == fa), next(x["label"] for x in t["fields"] if x["id"] == fb)
            if slots.label_agrees(a["label"], lb, "value") and slots.label_agrees(b["label"], la, "value"):
                continue  # the two labels name the same thing: not a defect
            m = copy.deepcopy(t)
            for x in m["fields"]:
                if x["id"] == fa:
                    x["label"] = lb
                elif x["id"] == fb:
                    x["label"] = la
            after = run(it, m)
            asked = any(q["kind"] == "label_match" and q.get("field") in (fa, fb) for q in new_questions(base, after))
            hit = any(x["kind"] == "mislabelled" and x.get("box") in (nums[fa], nums[fb]) for x in new_findings(base, after))
            out["swap"]["n"] += 1
            out["swap"]["to_judge"] += asked
            out["swap"]["caught"] += hit
            if not asked and not hit:
                out["swap"]["misses"].append(f"{it['name']} p{a['page']} '{la[:20]}' <-> '{lb[:20]}'")
            if out["swap"]["n"] % 4 == 3:
                break
    for v in out.values():
        v["misses"] = v["misses"][:40]
    return out


def _near(f: dict, rect: dict) -> bool:
    g = NEAR.search(f.get("detail", ""))
    if not g:
        return f.get("box") is not None
    x, y = float(g.group(1)), float(g.group(2))
    return rect["x"] - 0.03 <= x <= rect["x"] + rect["w"] + 0.03 and rect["y"] - 0.03 <= y <= rect["y"] + rect["h"] + 0.03


def judge_scores(path: str) -> dict:
    from judge import KINDS
    rows = [json.loads(l) for l in open(path, encoding="utf-8")]
    out: dict = {}
    secs = [r["secs"] for r in rows if r.get("secs")]
    for r in rows:
        kind = r["question"]["kind"]
        o = out.setdefault(kind, {"n": 0, "correct": 0, "false_pass": 0, "false_fail": 0, "unclear": 0})
        o["n"] += 1
        truth, got = r["truth"], r["got"]
        o["correct"] += got in truth
        o["unclear"] += got == "unclear"
        if kind == "group_mode":
            defect = r["question"].get("mode") != {"one": "single", "several": "multi"}[truth[0]]
            passes = {"one": "single", "several": "multi"}.get(got) == r["question"].get("mode")
        else:
            defect = not set(truth) & set(KINDS[kind]["pass"])
            passes = got in KINDS[kind]["pass"]
        o["false_pass"] += defect and passes
        o["false_fail"] += (not defect) and not passes
    total = {k: sum(v[k] for v in out.values()) for k in ("n", "correct", "false_pass", "false_fail", "unclear")}
    return {"by_kind": out, "total": total, "latency_secs": {"median": statistics.median(secs) if secs else None,
            "p90": sorted(secs)[int(0.9 * (len(secs) - 1))] if secs else None, "max": max(secs) if secs else None, "n": len(secs)},
            "prompt_tokens_median": statistics.median([r["prompt_tokens"] for r in rows if r.get("prompt_tokens")]) if rows else None}


def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus")
    ap.add_argument("out")
    ap.add_argument("--judge")
    ap.add_argument("--past-answers", help="judge answers to the past items' questions (jsonl: item, id, got)")
    ap.add_argument("--drop-per-template", type=int, default=8)
    ap.add_argument("--skip-mutations", action="store_true")
    a = ap.parse_args(argv)
    items = json.loads(Path(a.corpus).read_text())
    answers = {}
    if a.past_answers:
        for l in open(a.past_answers, encoding="utf-8"):
            r = json.loads(l)
            answers[(r["item"], r["id"])] = r["got"]
    result = {"past": past(items, answers)}
    if not a.skip_mutations:
        result["mutations"] = mutations([i for i in items if i["set"] == "train"], a.drop_per_template)
    if a.judge:
        result["judge"] = judge_scores(a.judge)
    Path(a.out).write_text(json.dumps(result, ensure_ascii=False, indent=1))
    p = result["past"]
    print("past findings by kind:", json.dumps(p["by_kind"], ensure_ascii=False))
    print("past templates:", json.dumps({k: v["pipeline"] for k, v in p["templates"].items()}), "false passes:", p["false_passes"])
    for k, v in result.get("mutations", {}).items():
        print(f"mutation {k}: {json.dumps({x: y for x, y in v.items() if x != 'misses'})}")
    if a.judge:
        print("judge:", json.dumps(result["judge"]["total"]), json.dumps(result["judge"]["latency_secs"]))


if __name__ == "__main__":
    main()
