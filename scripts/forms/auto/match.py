# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9"]
# ///
"""Gate: is this file the programme's current application form?

The collector attaches whatever a notice page links, so a board-wide attachment or last
year's form can sit on a programme. Checks, per template_ready form:
  title    the form's own heading (top third of its first templated page) shares at least
           one distinctive word with the programme title (generic words such as 청년, 지원,
           사업, 신청서 and place names do not count)
  year     no 20XX printed in that heading, or on a '20XX년  월  일' / '20XX.  .  .' date line of a templated
           page, is older than the current year
  notice   the notice page's title does not say the intake closed (마감)
Writes <run>/match.json. Approval still needs the operator's attestation; this only keeps
obviously foreign or stale files away from the reviewer and the operator.

Usage: python -I match.py <run dir> <raw dl dir>
       python -I match.py --derive <run dir> <raw dl dir> <specs.txt> <out match.json>
--derive (the server's derive stage): only the slugs in specs.txt (letter=<gates>:<slug>), the
templated pages read from each template itself, never from the parse stage's manifest.
"""
from __future__ import annotations

import json
import re
import sys
from datetime import date
from pathlib import Path

import pdfplumber

GENERIC = set("청년 지원 사업 신청 신청서 서식 양식 모집 공고 안내 참여 운영 센터 시 군 구 도 년 및 의 등 제출 서류 붙임 별지 별첨 지원사업 지원신청서".split())
DATE_LINE_YEAR = re.compile(r"(20\d\d)\s*(?:년\s+월\s+일|\.\s+\.\s+\.)")  # '2025년  월  일': the year the form was printed for


SHORT_DISTRICTS = {"중구", "동구", "서구", "남구", "북구"}


def words(text: str) -> set[str]:
    """Distinctive words. Twin of distinctive_words in crates/worker/src/application_resources/discovery.rs."""
    toks = re.findall(r"[A-Za-z]{2,}|[가-힣]{2,}", text)
    out = set()
    for t in toks:
        t = re.sub(r"(지원|사업|신청서|신청|서식|대여|비|료|금)$", "", t) or t
        if t.startswith("청년") and len(t) >= 4:  # '청년월세' names the same thing as '청년 월세'
            t = t[2:]
        # Place names: 3+ syllables ending 시/군/구(청), short districts, centres and foundations.
        # Two-syllable words ending 시/구 (한시, 연구, 가구) are words.
        place = (len(t) >= 3 and re.search(r"(시|군|구)청?$", t)) or re.search(r"센터$|재단$", t) or t in SHORT_DISTRICTS
        if len(t) >= 2 and t not in GENERIC and not place:
            out.add(t)
    return out


def shares_word(a: set[str], b: set[str]) -> bool:
    """One distinctive word, or one containing the other ('면접정장' names '정장')."""
    return any(x in y or y in x for x in a for y in b)


def bigrams(text: str) -> set[str]:
    t = re.sub(r"[^가-힣A-Za-z]", "", text)
    return {t[i:i + 2] for i in range(len(t) - 1)} - {"청년", "지원", "사업", "신청", "청서", "원사", "년지"}


def main(run: str, dl: str, entries: list | None = None, out_path: Path | None = None) -> None:
    R = Path(run)
    sources = {s["slug"]: s for s in json.loads((R / "sources.json").read_text())}
    manifest = entries if entries is not None else json.loads((R / "out/manifest.json").read_text())["forms"]
    year = date.today().year
    out = {}
    for e in manifest:
        if e["status"] != "template_ready":
            continue
        s = sources[e["slug"]]
        pdf = R / "derived" / f"{e['slug']}.pdf" if s["format"] != "pdf" else Path(dl) / f"{e['slug']}.pdf"
        first = min(e["templated_pages"]) - 1
        with pdfplumber.open(pdf) as doc:
            page = doc.pages[first]
            head = re.sub(r"\s+", " ", page.crop((0, 0, page.width, page.height / 3)).extract_text() or "")
            body = " ".join(doc.pages[n - 1].extract_text() or "" for n in e["templated_pages"])
        prog = s["program"]
        common = sorted(w for w in words(head) if shares_word({w}, words(prog)))
        # Spacing differs between a form and a programme record ('면접정장' vs '면접 정장'): 2-grams.
        shared = bigrams(head) & bigrams(prog)
        years = [int(y) for y in re.findall(r"(20\d\d)\s*년?", head)] + [int(y) for y in DATE_LINE_YEAR.findall(body)]
        notice_title = (s.get("notice") or {}).get("title", "")
        problems = []
        if not common and len(shared) < 3:
            problems.append(f"form heading shares no distinctive word with the programme ('{head[:60]}' vs '{prog}')")
        if any(y < year for y in years):
            problems.append(f"form heading prints an earlier year: {sorted(set(years))}")
        notice = s.get("notice") or {}
        if notice.get("error") or not notice_title:
            problems.append(f"notice page not verified ({notice.get('error') or 'no title'}): intake cannot be confirmed")
        # closed_lines is not used: notice pages list other notices '(※접수 마감)' in their sidebars.
        if re.search(r"마감", notice_title.split(" - ")[0]):
            problems.append(f"notice title says closed: {notice_title[:80]}")
        out[e["slug"]] = {"pass": not problems, "problems": problems, "heading": head[:120], "programme": prog,
                          "common_words": sorted(common), "shared_bigrams": len(shared), "years": years, "notice_title": notice_title[:120]}
        print(e["slug"], "PASS" if not problems else "FAIL", f"{len(problems)} problem(s), details in match.json")
    (out_path or R / "match.json").write_text(json.dumps(out, ensure_ascii=False, indent=1))


def from_templates(run: str, specs: str) -> list[dict]:
    """Manifest-shaped entries for the slugs in specs.txt, pages from the template files."""
    out = []
    for line in Path(specs).read_text().splitlines():
        slug = line.rsplit(":", 1)[-1]
        if not re.fullmatch(r"[a-z0-9-]{1,64}", slug):
            continue
        t = json.loads((Path(run) / "out" / f"{slug}.template.json").read_text())
        pages = sorted({int(f["page"]) + 1 for f in t["fields"]})
        if pages:
            out.append({"slug": slug, "status": "template_ready", "templated_pages": pages})
    return out


if __name__ == "__main__":
    if sys.argv[1:2] == ["--derive"] and len(sys.argv) == 6:
        run, dl, specs, dst = sys.argv[2:6]
        main(run, dl, from_templates(run, specs), Path(dst))
    else:
        main(*sys.argv[1:3])
