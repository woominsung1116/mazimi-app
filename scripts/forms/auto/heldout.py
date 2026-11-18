"""Held-out set for the gate that must pass before the local judge replaces the reviewer.

Items: crawled official blanks and the conversions rejected on 2026-10-16, minus every file
from an agency that gave a training or few-shot template (split by agency, so no form family
the rules or prompts were tuned on is scored). Labels come from a person filling the sheet,
never from the judge, the reviewer model or slots.py: the sheet shows the packet images and
the template, not any prediction. Predictions are written to a separate file and joined only
by `score`.

  python3 -I heldout.py split <files.jsonl> <dl dir> <rejected dir> <out.json>
      -> the item list (sha256, agency host, format, path), with the excluded files and why
  python3 -I heldout.py sheet <items.json> <run dir> <out.csv>
      -> one row per item for a person: template/packet paths and empty label columns
  python3 -I heldout.py score <labels.csv> <run dir> <out.json>
      -> per check kind: false passes (pipeline pass, person fail) and false fails; the gate
         needs zero false passes and every item labelled

Label columns: human_verdict (pass | fail | not_a_form | cannot_judge), human_kinds (missing,
misplaced, mislabelled, wrong_type, wrong_person, conversion, wrong_form, grouping, other;
';'-separated), labeller, labelled_at (ISO date).
"""
from __future__ import annotations

import csv
import hashlib
import json
import sys
from pathlib import Path
from urllib.parse import urlsplit

# Agencies of the reviewed templates the rules, few-shot examples and past batches came from
# (docs/validation/form-templates, the two wonju overrides). A crawled file from one of these
# hosts is not held out.
TRAINING_HOSTS = {
    "youth.incheon.go.kr", "bizok.incheon.go.kr", "www.incheon.go.kr",  # 인천광역시
    "www.wonju.go.kr",  # 원주시 (wonju overrides, past batches)
    "www.jeonju.go.kr", "www.jeju.go.kr", "www.gg.go.kr", "youth.gg.go.kr", "www.ddm.go.kr",
    "www.gachon.ac.kr", "www.kmu.ac.kr", "www.kongju.ac.kr", "www.konkuk.ac.kr", "www.mju.ac.kr", "www.pusan.ac.kr",
    "www.sangju.go.kr", "youth.seoul.go.kr", "www.shingu.ac.kr", "www.yp21.go.kr", "www.yeongju.go.kr",
}
TRAINING_AGENCY_WORDS = ("incheon", "wonju", "jeonju", "jeju", "gyeonggi", "dongdaemun", "gachon", "keimyung", "kongju",
                         "konkuk", "mju", "pusan", "sangju", "seoul.go.kr", "shingu", "yangpyeong", "yeongju", "sontaehee")
LABEL_COLS = ["human_verdict", "human_kinds", "labeller", "labelled_at"]
VERDICTS = {"pass", "fail", "not_a_form", "cannot_judge"}
KINDS = {"missing", "misplaced", "mislabelled", "wrong_type", "wrong_person", "conversion", "wrong_form", "grouping", "other"}


def sha(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def training_host(host: str) -> bool:
    """An exact training host, or any host whose labels name a training agency
    ('bizok.incheon.go.kr', 'youth.wonju.go.kr', 'seoul.go.kr' subdomains)."""
    labels = host.lower().split(".")
    return host in TRAINING_HOSTS or any(w in labels or (("." in w) and host.endswith(w)) for w in TRAINING_AGENCY_WORDS)


def split(files_jsonl: str, dl: str, rejected: str, out: str) -> None:
    hosts: dict[str, set] = {}
    for line in open(files_jsonl, encoding="utf-8"):
        r = json.loads(line)
        if r.get("sha256"):
            hosts.setdefault(r["sha256"], set()).add(urlsplit(r["url"]).hostname or "")
    items, excluded, seen = [], [], set()
    for p in sorted(Path(dl).iterdir()):
        if p.suffix not in (".hwp", ".hwpx"):
            excluded.append({"file": p.name, "why": "not HWP/HWPX (no kordoc IR)"})
            continue
        h = sha(p)
        hs = sorted(hosts.get(h, set()))
        if any(training_host(x) for x in hs):
            excluded.append({"file": p.name, "sha256": h, "hosts": hs, "why": "agency of a training template"})
            continue
        seen.add(h)
        items.append({"id": h[:12], "sha256": h, "format": p.suffix[1:], "hosts": hs, "path": str(p), "origin": "crawl"})
    for p in sorted(Path(rejected).iterdir()):
        h = sha(p)
        if h in seen:
            excluded.append({"file": p.name, "sha256": h, "why": "same bytes as a crawled item"})
            continue
        agency = p.stem.split("-")[0]
        if any(w in agency for w in TRAINING_AGENCY_WORDS):
            excluded.append({"file": p.name, "why": "agency of a training template"})
            continue
        seen.add(h)
        items.append({"id": h[:12], "sha256": h, "format": p.suffix[1:], "hosts": [agency], "path": str(p), "origin": "rejected_conversion"})
    Path(out).write_text(json.dumps({"items": items, "excluded": excluded}, ensure_ascii=False, indent=1))
    print(json.dumps({"items": len(items), "excluded": len(excluded), "agencies": len({h for i in items for h in i["hosts"]})}))


def stage_of(run: Path, slug: str) -> str:
    v = json.loads((run / "verdicts.json").read_text()) if (run / "verdicts.json").exists() else {"rows": []}
    row = next((r for r in v["rows"] if r.get("slug") == slug), None)
    return f"{row['stage']}:{row['result']}" if row else "not reached"


def sheet(items_json: str, run: str, out: str) -> None:
    R = Path(run)
    items = json.loads(Path(items_json).read_text())["items"]
    slugs = {s["sha256"]: s["slug"] for s in json.loads((R / "sources.json").read_text())} if (R / "sources.json").exists() else {}
    key = json.loads((R / "keys" / "batch.key.json").read_text()) if (R / "keys" / "batch.key.json").exists() else {}
    letter = {v["slug"]: k for k, v in key.items() if not k.startswith("_")}
    with open(out, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["id", "origin", "agency", "format", "source_file", "template", "packet_letter", "packet_dir", "pipeline_stage"] + LABEL_COLS)
        for it in items:
            slug = slugs.get(it["sha256"], "")
            tpl = R / "out" / f"{slug}.template.json"
            w.writerow([it["id"], it["origin"], ";".join(it["hosts"]), it["format"], it["path"], str(tpl) if slug and tpl.exists() else "",
                        letter.get(slug, ""), str(R / "review" / "batch") if slug in letter else "", stage_of(R, slug) if slug else "not converted"]
                       + [""] * len(LABEL_COLS))
    print(json.dumps({"rows": len(items), "unlabelled": len(items)}))


def score(labels_csv: str, run: str, out: str) -> None:
    """Gate numbers: a false pass is an item the pipeline would import that the person failed.

    Items are joined to the scored run by their file hash (the sheet's letters belong to the
    run the person looked at). A pipeline pass on a template other than the one the person
    labelled (the run produced different boxes) is not scored: it needs a new label."""
    R = Path(run)
    rows = list(csv.DictReader(open(labels_csv, encoding="utf-8")))
    judge = json.loads((R / "review" / "batch.judge.json").read_text()) if (R / "review" / "batch.judge.json").exists() else {}
    key = json.loads((R / "keys" / "batch.key.json").read_text()) if (R / "keys" / "batch.key.json").exists() else {}
    letter_of = {v["slug"]: k for k, v in key.items() if not k.startswith("_")}
    sources = json.loads((R / "sources.json").read_text()) if (R / "sources.json").exists() else []
    res = {"n": len(rows), "unlabelled": 0, "relabel_needed": 0, "false_pass": 0, "false_fail": 0, "agree": 0,
           "by_kind": {}, "invalid": [], "items": []}
    for r in rows:
        v = (r.get("human_verdict") or "").strip()
        if not v:
            res["unlabelled"] += 1
            continue
        kinds = {k.strip() for k in (r.get("human_kinds") or "").split(";") if k.strip()}
        if v not in VERDICTS or not kinds <= KINDS or (v == "fail") != bool(kinds) or not r.get("labeller"):
            res["invalid"].append(r["id"])
            continue
        src = next((s for s in sources if s["sha256"].startswith(r["id"])), None)
        letter = letter_of.get(src["slug"]) if src else None
        predicted = (judge.get(letter) or {}).get("verdict") if letter else "reject_before_review"
        passes = predicted == "pass"
        if passes and src:
            now = R / "out" / f"{src['slug']}.template.json"
            seen = Path(r.get("template") or "")
            if not seen.is_file() or not now.is_file() or sha(seen) != sha(now):
                res["relabel_needed"] += 1
                res["items"].append({"id": r["id"], "predicted": predicted, "human": v, "result": "relabel_needed"})
                continue
        human_ok = v == "pass"
        if passes and not human_ok:  # importing a non-form or an unjudgeable form is a false pass too
            res["false_pass"] += 1
            for k in kinds or {"not_a_form"}:
                res["by_kind"].setdefault(k, {"false_pass": 0, "false_fail": 0})["false_pass"] += 1
            outcome = "false_pass"
        elif human_ok and not passes:
            res["false_fail"] += 1
            outcome = "false_fail"
        else:
            res["agree"] += 1
            outcome = "agree"
        res["items"].append({"id": r["id"], "letter": letter, "predicted": predicted, "human": v, "kinds": sorted(kinds), "result": outcome})
    res["gate_passed"] = res["unlabelled"] == 0 and res["relabel_needed"] == 0 and not res["invalid"] and res["false_pass"] == 0
    Path(out).write_text(json.dumps(res, ensure_ascii=False, indent=1))
    print(json.dumps({k: v for k, v in res.items() if k != "items"}))


if __name__ == "__main__":
    cmd, *rest = sys.argv[1:]
    {"split": split, "sheet": sheet, "score": score}[cmd](*rest)
