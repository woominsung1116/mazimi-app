#!/usr/bin/env python3
"""Score an assistant_topic_eval run (EVAL_OUT) against a fixture's `expect`.

usage: score.py FIXTURE EVAL_OUT [--db-container NAME --db NAME]

Per case: the harness's own `success` for search rows with labelled fields
(topic precision@3 >= 2/3 and a named row, or an honest none), plus each
`expect` key (see the fixture's note). A case passes when every check it has
passes. Deadlines of card programmes are read from the eval database.
"""
import json
import subprocess
import sys


def deadline_of(ids, container, db):
    if not ids:
        return {}
    sql = (
        "SELECT id, LEAST(deadline_at, application_end_at, benefit_valid_until) "
        "FROM programs WHERE id IN (%s)" % ",".join("'%s'" % i for i in ids)
    )
    out = subprocess.run(
        ["docker", "exec", container, "psql", "-U", "postgres", "-d", db, "-Atc", sql],
        capture_output=True, text=True, check=True,
    ).stdout
    found = {}
    for line in out.splitlines():
        pid, _, d = line.partition("|")
        found[pid] = d or None
    return found


def compact(s):
    return "".join(c for c in s if c.isalnum())


def check(case, row, container, db):
    exp = case.get("expect", {})
    answer = row.get("answer") or ""
    actions = (row.get("message") or {}).get("suggested_actions") or []
    labels = [a.get("label", "") for a in actions]
    cards = row.get("cards") or []
    executed = row.get("executed") or []
    brief = row.get("results_brief") or []
    fails = []
    # A labelled topic: two of the top three rows on it (an honest "none"
    # is not a pass here: the catalogue has rows for every labelled topic).
    if case.get("fields"):
        p = row.get("precision_top3")
        if p is None or p < 2 / 3:
            fails.append("topic(p@3=%s)" % p)
    if case.get("within_days") and not row.get("window_ok"):
        fails.append("window")
    if "docs" in exp or "docs_or_pick" in exp:
        docs = exp.get("docs") or exp.get("docs_or_pick")
        picked = any(a.get("type") == "pick_program" for a in actions)
        missing = [d for d in docs if compact(d) not in compact(answer)]
        if missing and not ("docs_or_pick" in exp and picked):
            fails.append("docs missing %s" % missing)
    if exp.get("none"):
        if row.get("top") or "찾지 못했" not in answer:
            fails.append("none: listed %d" % len(row.get("top") or []))
    if "searches" in exp:
        made = [sorted((b.get("search_args") or {}).get("fields") or [])
                for b in brief if b.get("tool") == "search_benefits" and b.get("relaxes") is None]
        for want in exp["searches"]:
            if sorted(want) not in made and not any(set(want) <= set(m) and len(m) == len(want) for m in made):
                fails.append("search %s not made (made %s)" % (want, made))
    if "alert_cards" in exp:
        alerts = [c for c in (row.get("confirmations") or []) if c.get("tool") == "set_alert"]
        if len(alerts) != exp["alert_cards"]:
            fails.append("alert cards %d" % len(alerts))
        found = [x for b in brief if b.get("tool") == "search_benefits" for x in (b.get("listed") or [])]
        ids = [c.get("program_id") for c in alerts] + [x["id"] for x in found]
        d = deadline_of([i for i in ids if i], container, db)
        dated = sorted(v for k, v in d.items() if v and k in [x["id"] for x in found])
        for c in alerts:
            v = d.get(c.get("program_id"))
            if not v:
                fails.append("alert on a programme with no deadline")
            elif dated and v != dated[0]:
                fails.append("alert not on the earliest (%s vs %s)" % (v, dated[0]))
    if exp.get("earliest_first"):
        top = row.get("top") or []
        if not top:
            fails.append("nothing listed")
        else:
            dates = [t.get("deadline") for t in top]
            if dates[0] is None or any(x is not None and x < dates[0] for x in dates):
                fails.append("first row is not the earliest: %s" % dates)
    if "cards" in exp and sorted(cards) != sorted(exp["cards"]):
        fails.append("cards %s" % cards)
    if "cards_or_pick" in exp:
        picked = any(a.get("type") == "pick_program" for a in actions)
        if sorted(cards) != sorted(exp["cards_or_pick"]) and not picked:
            fails.append("cards %s, no pick" % cards)
    for t in exp.get("tools_any", []):
        if t not in executed:
            fails.append("tool %s not run" % t)
    for t in exp.get("no_tools", []):
        if t in executed:
            fails.append("ran %s" % t)
    for p in exp.get("answer_has", []):
        if p not in answer:
            fails.append("answer lacks %r" % p)
    for p in exp.get("no_answer", []):
        if p in answer:
            fails.append("answer has %r" % p)
    for l in exp.get("no_labels", []):
        if l in labels:
            fails.append("button %r" % l)
    if "region" in exp:
        want = exp["region"]
        # Only the rows the answer names (its numbered lines).
        named = [t for t in (row.get("top") or []) if compact(t.get("title") or "")[-8:] in compact(answer)]
        for t in named:
            regions = json.loads(t.get("regions") or "[]")
            if regions and want not in regions and len(regions) < 17:
                # An exception is fine only when the answer labels the row.
                title = compact(t.get("title") or "")[-8:]
                if not (title and title in compact(answer) and "지역" in answer and "예외" in answer):
                    fails.append("row outside %s: %s" % (want, t.get("title")))
    return fails


def main():
    fixture = json.load(open(sys.argv[1]))
    out = json.load(open(sys.argv[2]))
    container = "mz-qa1031-pg"
    db = "mz_eval"
    if "--db-container" in sys.argv:
        container = sys.argv[sys.argv.index("--db-container") + 1]
    if "--db" in sys.argv:
        db = sys.argv[sys.argv.index("--db") + 1]
    rows = {r["id"]: r for r in out["rows"]}
    passed = 0
    for case in fixture["cases"]:
        row = rows.get(case["id"])
        if row is None:
            print("%s\tMISSING" % case["id"])
            continue
        fails = check(case, row, container, db)
        passed += not fails
        print("%s\t%s\t%s\t%s\t%s" % (
            case["id"], case.get("bug", "-"), "PASS" if not fails else "FAIL",
            row.get("planner"), "; ".join(fails)))
    print("passed %d/%d" % (passed, len(fixture["cases"])))


if __name__ == "__main__":
    main()
