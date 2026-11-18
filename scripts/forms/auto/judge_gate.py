"""Records whether the local judge may replace the independent reviewer (FORM_REVIEW_JUDGE=judge).

The gate passes only with zero false passes on all three, for one exact model and prompt:
  selftest   scripts/forms/auto/selftest.py exited 0 (every gate mutation caught)
  past       review_eval.py output with the judge's answers to the past batches' questions:
             past.false_passes == 0 (no template the reviewer failed would be imported)
  held-out   heldout.py score output: every item labelled by a person, no invalid row, zero
             false passes
judge-gate.json is what verdicts.py checks (model digest and prompt version must equal those
in the run's batch.judge-log.json). Anything short of a pass writes passed=false.

A change to how answers become findings (not to the questions or prompts) can be re-gated from
the logged answers without asking the model again: pass a log whose identity carries the new
prompt version and "rescored_from" the old one; the gate records both (--note says why).

Usage: python3 -I judge_gate.py <selftest exit code> <review_eval.json> <heldout-score.json> <judge-log.json> [--write] [--note <text>]
"""
from __future__ import annotations

import json
import sys
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent


def decide(selftest_rc: int, past: dict, heldout: dict, identity: dict) -> dict:
    reasons = []
    if selftest_rc != 0:
        reasons.append("selftest did not pass")
    p = past.get("past", {})
    if p.get("false_passes") != 0 or any(v.get("pipeline") == "needs_judge" for v in p.get("templates", {}).values()):
        reasons.append("past batches: a false pass or an unanswered judge question")
    if heldout.get("unlabelled") != 0 or heldout.get("invalid") or heldout.get("false_pass") != 0 or not heldout.get("n"):
        reasons.append(f"held-out: {heldout.get('unlabelled')} unlabelled, {heldout.get('false_pass')} false passes")
    if not identity.get("digest") or not identity.get("prompt"):
        reasons.append("judge identity (model digest, prompt version) unknown")
    return {"passed": not reasons, "reasons": reasons, "false_passes": (p.get("false_passes") or 0) + (heldout.get("false_pass") or 0),
            "heldout_unlabelled": heldout.get("unlabelled"), "heldout_n": heldout.get("n"),
            "model": identity.get("model"), "model_digest": identity.get("digest"), "prompt": identity.get("prompt"),
            "decided_at": datetime.now(timezone.utc).isoformat()}


def main(argv: list[str]) -> None:
    rc, past, heldout, log = argv[:4]
    logd = json.loads(Path(log).read_text())
    gate = decide(int(rc), json.loads(Path(past).read_text()), json.loads(Path(heldout).read_text()), logd.get("identity", {}))
    if logd.get("rescored_from"):
        gate["rescored_from"] = {"prompt": logd["rescored_from"], "model_calls": logd.get("model_calls")}
    if "--note" in argv:
        gate["note"] = argv[argv.index("--note") + 1][:500]
    print(json.dumps(gate, ensure_ascii=False))
    if "--write" in argv:
        (HERE / "judge-gate.json").write_text(json.dumps(gate, ensure_ascii=False, indent=1) + "\n")


if __name__ == "__main__":
    main(sys.argv[1:])
