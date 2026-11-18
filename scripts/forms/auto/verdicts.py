"""Last step: one verdict per candidate row, and the import manifest of what passed everything.

A template is importable only if: selected (verdict try), downloaded with the collector's
hash, conversion judged faithful (HWP), fields proposed, the form matches the programme (match.py),
every machine gate passed, the deterministic slot check (slots.py) found nothing, and the
deciding reviewer returned "pass" with no findings. There is no human approval step: an
importable template is imported automatically (scripts/forms/auto/import.sh), and the review
recorded here is its verifier. Live rows (template already bound to the same bytes) are
reported, never re-imported.

The deciding reviewer (FORM_REVIEW_JUDGE):
  shadow (default), off   the independent reviewer's review/batch.verdict.json decides. In
                          shadow the local judge's review/batch.judge.json is checked the same
                          way and logged next to it (verdicts.json "shadow"), never used.
  judge                   review/batch.judge.json decides, and only if judge-gate.json records
                          a passed gate (zero false passes on selftest, the past batches and a
                          fully labelled held-out set) for the exact model digest and prompt
                          version that wrote it. Otherwise this run stops.
A verdict file counts only if it echoes the packet's nonce, answers exactly the letters given,
has the agreed shape, and the template on disk still hashes to what was reviewed.

Inputs (all in the run dir R): candidates.json, sources.json, conversions.json (if any HWP;
the fidelity container), out/manifest.json (selection only), rderive/match.json and
rderive/slots/<slug>.slots.json (the derive container), rxcheck/<slug>.gates.json (both
renderers). Nothing the parse container writes decides here (see steps.sh "Trust boundary").
keys/batch.key.json + review/batch.verdict.json and/or review/batch.judge.json (+ -log.json).
Outputs: R/verdicts.json, R/out/import-manifest.json (each form with its review evidence),
R/programs.json, counts on stdout.

On the server the trust files live outside the run dir, in a state dir no step that parses
agency files can write: keys/ (packet key), verdict/ (the reviewer's file), judge/ (the
judge's file and log), out/ (this script's outputs).

Usage: python3 -I scripts/forms/auto/verdicts.py <run dir> [--state <state dir>]
"""
from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODES = ("off", "shadow", "judge")


def load(p: Path, default):
    return json.loads(p.read_text()) if p.exists() else default


def sha(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def mode() -> str:
    m = os.environ.get("FORM_REVIEW_JUDGE", "shadow").strip() or "shadow"
    if m not in MODES:
        raise SystemExit(f"FORM_REVIEW_JUDGE must be one of {', '.join(MODES)}")
    return m


def read_verdicts(path: Path, key: dict, nonce: str) -> dict | None:
    """slug -> {verdict, findings} from one reviewer file (None when the file is absent)."""
    try:
        verdicts = load(path, None)
    except ValueError:
        raise SystemExit(f"{path.name}: not valid JSON")
    if verdicts is None:
        return None
    if not isinstance(verdicts, dict) or verdicts.pop("nonce", None) != nonce:
        raise SystemExit(f"{path.name}: nonce missing or wrong: not an answer to this packet")
    if set(verdicts) != set(key):
        raise SystemExit(f"{path.name}: letters {sorted(verdicts)} != packet {sorted(key)}")
    for letter, v in verdicts.items():
        if not isinstance(v, dict) or v.get("verdict") not in ("pass", "fail") or not isinstance(v.get("findings"), list) \
                or (v["verdict"] == "pass" and v["findings"]):
            raise SystemExit(f"{path.name}: malformed verdict for {letter}")
    out = {}
    for letter, info in key.items():
        v = verdicts.get(letter) or {"verdict": "missing", "findings": []}
        current = sha(Path(info["template"])) if Path(info["template"]).exists() else None
        if current != info["template_sha256"]:
            v = {"verdict": "stale", "findings": ["template changed after review"]}
        out[info["slug"]] = v
    return out


def judge_gate(log: Path) -> str | None:
    """None when the recorded gate admits the judge that wrote this run's file, else why not."""
    gate = load(HERE / "judge-gate.json", None)
    ident = load(log, {}).get("identity", {})
    if not gate:
        return "no judge-gate.json: the judge has not passed its gate"
    if not (gate.get("passed") is True and gate.get("false_passes") == 0 and gate.get("heldout_unlabelled") == 0):
        return "judge-gate.json does not record a passed gate"
    if not ident.get("digest") or (gate.get("model_digest"), gate.get("prompt")) != (ident.get("digest"), ident.get("prompt")):
        return "the judge that wrote this run is not the gated model/prompt"
    return None


def main(run: str, state: str | None = None) -> None:
    R = Path(run)
    S = Path(state) if state else None
    m = mode()
    key_path = S / "keys" / "batch.key.json" if S else R / "keys" / "batch.key.json"
    files = {"claude-reviewer": S / "verdict" / "batch.verdict.json" if S else R / "review" / "batch.verdict.json",
             "judge": S / "judge" / "batch.judge.json" if S else R / "review" / "batch.judge.json"}
    judge_log = files["judge"].with_name("batch.judge-log.json")
    OUT = S / "out" if S else R
    cands = load(R / "candidates.json", {"rows": []})["rows"]
    sources = {s["resource_id"]: s for s in load(R / "sources.json", [])}
    conv = load(R / "conversions.json", {}).get("forms", {})
    match = load(R / "rderive" / "match.json", {})
    manifest = {e["slug"]: e for e in load(R / "out/manifest.json", {"forms": []})["forms"]}
    # Only this run's batch (run dirs are never reused).
    reviewer: dict = {}
    judged: dict | None = None
    judge_error = None
    if key_path.exists():
        key = json.loads(key_path.read_text())
        nonce = key.pop("_nonce")
        reviewer = read_verdicts(files["claude-reviewer"], dict(key), nonce) or {}
        if m != "off":
            try:
                judged = read_verdicts(files["judge"], dict(key), nonce)
            except SystemExit as e:
                if m == "judge":
                    raise
                judged, judge_error = None, str(e)  # shadow: a bad judge file never blocks the reviewer's verdict
    if not key_path.exists():
        # Nothing reached review (no packet): a finished run with nothing to import, in every
        # mode. Only a run that has something to decide needs a deciding reviewer.
        deciding, decider = {}, "none"
    elif m == "judge":
        why = judge_gate(judge_log)
        if why:
            raise SystemExit(f"FORM_REVIEW_JUDGE=judge refused: {why}")
        deciding, decider = judged or {}, "judge"
    else:
        deciding, decider = reviewer, "claude-reviewer"
    decider_file = files.get(decider)
    evidence_base = {"reviewer": decider, "verdict_file": str(decider_file),
                     "verdict_sha256": sha(decider_file) if decider_file and decider_file.exists() else None, "run": R.name, "mode": m}
    if decider == "judge":
        evidence_base["judge"] = load(judge_log, {}).get("identity")
    rows, importable, programs, shadow = [], [], {}, {}
    counts = {"candidates": len(cands), "selected": 0, "live": 0, "downloaded": 0, "converted": 0,
              "auto_proposed": 0, "programme_match": 0, "gate_pass": 0, "slots_pass": 0, "reviewer_pass": 0, "importable": 0}
    for c in cands:
        out = {"resource_id": c["id"], "program_id": c["program_id"], "program": c["program_title"], "host": c["host"],
               "label": c["label"], "content_hash": c["content_hash"], "window_end": c.get("window_end"), "stage": "select", "result": c["verdict"], "reason": c["reason"]}
        rows.append(out)
        if c["verdict"] == "live":
            counts["live"] += 1
            continue
        if c["verdict"] != "try":
            continue
        counts["selected"] += 1
        s = sources.get(c["id"])
        if not s:
            out.update(stage="download", result="reject", reason="not downloaded with the collector's hash (see fetch log)")
            continue
        counts["downloaded"] += 1
        slug = s["slug"]
        out["slug"] = slug
        if s["format"] != "pdf":
            cv = conv.get(slug)
            if not cv or cv["verdict"] != "faithful":
                out.update(stage="convert", result="reject", reason=(cv or {}).get("reason") or "no conversion verdict")
                continue
        counts["converted"] += 1
        mf = manifest.get(slug)
        if not mf or mf["status"] != "template_ready":
            out.update(stage="propose", result="reject", reason=(mf or {}).get("reason") or "no template")
            continue
        counts["auto_proposed"] += 1
        # Gates first: the derive stage (programme match) runs only for templates that passed
        # the cross-checked gates, so a gate failure has no match result to report.
        g = load(R / "rxcheck" / f"{slug}.gates.json", None)
        if not g or not g["pass"]:
            out.update(stage="gates", result="reject", reason=json.dumps((g or {}).get("problems", "gates not run"), ensure_ascii=False)[:600])
            continue
        counts["gate_pass"] += 1
        mt = match.get(slug)
        if not mt or not mt["pass"]:
            out.update(stage="programme", result="reject", reason="; ".join((mt or {}).get("problems", ["programme match not run"])))
            continue
        counts["programme_match"] += 1
        if judged is not None and slug in judged and slug in reviewer:
            shadow[slug] = {"reviewer": reviewer[slug]["verdict"], "judge": judged[slug]["verdict"],
                            "agree": reviewer[slug]["verdict"] == judged[slug]["verdict"],
                            "judge_false_pass": judged[slug]["verdict"] == "pass" and reviewer[slug]["verdict"] == "fail",
                            "judge_findings": len(judged[slug]["findings"]), "reviewer_findings": len(reviewer[slug]["findings"])}
        sl = load(R / "rderive" / "slots" / f"{slug}.slots.json", None)
        if not sl or sl.get("findings"):
            out.update(stage="slots", result="reject", reason=json.dumps((sl or {}).get("findings", "slot check not run"), ensure_ascii=False)[:1500])
            continue
        counts["slots_pass"] += 1
        rv = deciding.get(slug)
        if not rv or rv["verdict"] != "pass" or rv["findings"]:
            out.update(stage="review", result="reject", reason=json.dumps(rv or "not reviewed", ensure_ascii=False)[:1500])
            continue
        counts["reviewer_pass"] += 1
        out.update(stage="import", result="importable", reason=f"{mf['field_count']} fields on pages {mf['templated_pages']}")
        tpl = R / "out" / f"{slug}.template.json"
        importable.append({**mf, "review": {**evidence_base, "template_sha256": sha(tpl) if tpl.exists() else None,
                                            "gates_sha256": sha(R / "rxcheck" / f"{slug}.gates.json"),
                                            "slots_sha256": sha(R / "rderive" / "slots" / f"{slug}.slots.json")}})
        programs[slug] = c["program_id"]
    counts["importable"] = len(importable)
    gsum = {"groups": 0, "single": 0, "multi": 0, "flagged": 0}
    for gp in sorted((R / "groups").glob("*.groups.json")) if (R / "groups").exists() else []:
        c = json.loads(gp.read_text())["counts"]
        for k in gsum:
            gsum[k] += c[k]
    counts["checkbox_groups_all_proposed"] = gsum
    # Judge questions with no valid answer (timeout, malformed reply, model error): each fails its
    # template closed, and the run event warns when there are any (worker form_auto::record).
    jlog = load(judge_log, {}) if m != "off" else {}
    counts["judge_unanswered"] = sum(1 for t in jlog.get("templates", {}).values() for q in t.get("questions", [])
                                     if q.get("why") == "no valid answer")
    result = {"mode": m, "decider": decider, "counts": counts, "rows": rows, "nothing_to_review": not key_path.exists()}
    if judge_error:
        result["shadow_error"] = judge_error[:300]
    if shadow:
        result["shadow"] = {"templates": shadow, "agree": sum(v["agree"] for v in shadow.values()),
                            "judge_false_passes": sum(v["judge_false_pass"] for v in shadow.values()), "n": len(shadow)}
    (OUT / "verdicts.json").write_text(json.dumps(result, ensure_ascii=False, indent=1))
    manifest_path = OUT / "import-manifest.json" if S else R / "out" / "import-manifest.json"
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps({"version": 1, "generated_by": "scripts/forms/auto/verdicts.py", "forms": importable}, ensure_ascii=False, indent=1))
    (OUT / "programs.json").write_text(json.dumps(programs, indent=1))
    print(json.dumps({"mode": m, "decider": decider, **counts}))
    if not key_path.exists():
        print("nothing to review: no template reached the review step; nothing imported")
    if shadow:
        print("shadow:", json.dumps({k: v for k, v in result["shadow"].items() if k != "templates"}))
    for r in rows:
        if r["result"] not in ("reject",) or r["stage"] != "select":
            print(f"{r['resource_id'][:8]} {r['stage']:8} {r['result']}")  # reasons (document text) are in verdicts.json


if __name__ == "__main__":
    args = sys.argv[1:]
    if len(args) == 3 and args[1] == "--state":
        main(args[0], args[2])
    elif len(args) == 1:
        main(args[0])
    else:
        raise SystemExit(__doc__)
