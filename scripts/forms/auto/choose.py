"""Step 1 of the form-template auto pipeline: decide which collector rows to try.

Reads the JSON array printed by candidates.sql and writes candidates.json:
every row with a verdict ("try", "live" or "reject") and the rule that decided it.
Pure function of the query output; touches no network and no files but its two
arguments. Standard library only, so it runs under `python3 -I`.

Usage: python3 -I scripts/forms/auto/choose.py <candidates.raw.json> <candidates.json>
"""
from __future__ import annotations

import json
import re
import sys
from datetime import datetime, timezone

FORMATS = {"pdf", "hwp", "hwpx"}  # HWPX: the collector detects it from its stored mimetype entry and hashes it.
# Files that board-wide recruitment notices attach; never a benefit application.
NOT_AN_APPLICATION = re.compile(r"신원진술서|채용\s*신체검사|신체검사서|응시원서|수요조사")
SHARED_LIMIT = 3  # one file on this many programs is a board attachment, not a program's form


def _ts(value: str | None) -> datetime | None:
    """Postgres JSON trims fraction digits ('…:00.12345+00:00'); Python 3.9 accepts only 3 or 6."""
    if not value:
        return None
    m = re.match(r"^(.*T\d\d:\d\d:\d\d)(?:\.(\d+))?(.*)$", value)
    if m:
        value = m.group(1) + (f".{(m.group(2) + '000000')[:6]}" if m.group(2) else "") + m.group(3)
    return datetime.fromisoformat(value)


def window_end(row: dict) -> datetime | None:
    ends = [d for d in (_ts(row.get(k)) for k in ("benefit_valid_until", "deadline_at", "application_end_at")) if d]
    return min(ends) if ends else None


def decide(row: dict, now: datetime) -> tuple[str, str]:
    """(verdict, reason). The first failing rule wins; order is cheapest-to-explain first."""
    if row.get("rolled_back"):
        return "reject", "an operator rolled back the automatic import of these bytes"
    if row["file_format"] not in FORMATS:
        return "reject", f"format {row['file_format']}: only PDF/HWP/HWPX are converted and gated"
    if not row["available"]:
        return "reject", "program not currently available (closed, retired or window over)"
    if not row["in_approved_scope"]:
        return "reject", "file URL outside every approved source scope"
    probed_ok = row["status"] == "verified" or (row["status"] == "candidate" and row["check_status"] == "ok")
    if not probed_ok or row.get("check_error"):
        return "reject", f"collector check not ok ({row['status']}/{row['check_status']}/{row.get('last_failure_code')})"
    live = row["template_revision"] is not None
    if live and (row["template_source_sha256"] or row["template_content_hash"]) == row["content_hash"]:
        return "live", f"template revision {row['template_revision']} already bound to these bytes"
    # A live template whose file changed is redrawn only if the new file passes every rule below.
    if NOT_AN_APPLICATION.search(row["label"] or ""):
        return "reject", f"not a benefit application form: {row['label'][:40]}"
    if row["programs_sharing_url"] >= SHARED_LIMIT:
        return "reject", f"same file attached to {row['programs_sharing_url']} programs: board-wide attachment"
    if row["closed_programs_sharing_url"] > 0:
        return "reject", "same file also attached to a closed program: an earlier round's form"
    end = window_end(row)
    if end is None:
        return "reject", "no dated application window: the current intake cannot be attested from data"
    if end <= now:
        return "reject", "application window already over"
    return "try", ("live template is stale (file changed): redraw; " if live else "") + f"window open until {end.date()}"


def main(raw_path: str, out_path: str) -> None:
    text = open(raw_path, encoding="utf-8").read()
    # The read-only helper prints status lines first (which may contain '['); the JSON array
    # (json_agg, spread over several lines) starts at the first line beginning with '[{' or '[]'.
    lines = text.splitlines()
    first = next(i for i, l in enumerate(lines) if l.startswith("[{") or l.strip() == "[]")
    body = "\n".join(lines[first:])
    rows = json.loads(body[:body.rindex("]") + 1])
    now = datetime.now(timezone.utc)
    out = []
    for row in rows:
        verdict, reason = decide(row, now)
        end = window_end(row)
        out.append({**row, "verdict": verdict, "reason": reason, "window_end": end.isoformat() if end else None})
    json.dump({"selected_at": now.isoformat(), "rows": out}, open(out_path, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    counts: dict[str, int] = {}
    for r in out:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1
    print(json.dumps(counts))
    for r in out:
        if r["verdict"] != "reject":
            # ids and rule names only: titles and labels are crawled text and stay in candidates.json
            print(r["verdict"], r["id"][:8], r["reason"].split(":")[0][:60])


if __name__ == "__main__":
    main(*sys.argv[1:3])
