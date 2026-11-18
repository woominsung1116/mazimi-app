# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9", "pypdfium2==4.30.0", "pillow>=10.4", "numpy>=1.26", "olefile==0.47"]
# ///
"""Deterministic review, step 8a: every place on a templated page where the applicant writes,
ticks or signs must have a box of the right kind, and no box for someone else may be linked
to the applicant.

The slot enumerator is independent of propose_fields.py (which drew the boxes): it reads
  - the kordoc table IR of the source HWP/HWPX (scripts/forms/kordoc/ir.mjs): blank value
    cells next to a label cell, and the row-group header that names whose block a cell is in;
  - the printed glyphs of the PDF the app fills, for every format: □, ( ), 년 월 일,
    20 . . ., (서명 또는 인), 'label :', ____, a gap before 대학/학과/세/원;
  - for a PDF with no IR: blank ruled cells next to a label cell.
It shares only ruled-cell geometry (cells_of) and the label vocabularies with the proposer.

Findings use the reviewer's JSON shape (pages and boxes numbered from 1, boxes numbered per
page in template order, as in the review packet):
  missing       a slot with no box (office-use cells, printed instructions, headings and
                other people's signature cells excluded)
  wrong_type    a slot covered only by a box of the wrong kind (□ by text, (인) by text, a
                name/phone blank by a tick box, a year blank after a printed '20' by a
                full-year box)
  wrong_person  a box in another person's block (보호자, 직계존속, 위임받는 사람 …) or labelled
                for another person that is linked to a profile key the app fills, or marked
                required
  mislabelled   a text box whose label carries tick-box glyphs or consent answers; a tick-box
                label cut off at a dangling separator
What the rules cannot settle becomes a closed question for the text judge (judge.py): whose
block an unknown header names, whether an unboxed page is one the applicant fills, whether a
source-table label is a field label, whether a 'label :' line is a blank, single or multi for
a flagged group. The questions carry only printed text; the judge never sees a file.

Usage: python -I slots.py <template.json> <blank.pdf> <out.slots.json> [--ir <x.ir.json> --source-sha256 <hex>] [--groups <g.json>]
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))  # trusted repo code; -I keeps cwd out
sys.path.insert(0, str(HERE.parent / "kordoc"))

import pdfplumber  # noqa: E402

import check_ir  # noqa: E402  (scripts/forms/kordoc/check_ir.py)
from propose_fields import ADMIN_ONLY, OTHER_PERSON, SIGN_TOKEN, cells_of, classify, compact  # noqa: E402

BOX_GLYPHS = "□☐❏▢◻⬜"
UNITS = "년월일"
# Printed words after a blank that name what goes in it ('____대학 ____학과', '(   )세').
SUFFIX_WORDS = ("대학교", "대학", "학과", "학부", "전공", "학년", "학기", "학번", "개월", "세", "원", "명", "점", "호", "동", "층", "㎡")
# Words that make a blank the applicant's even inside an office row ('(신청일자: 20 . . .)').
APPLICANT_CONTEXT = re.compile(r"신청일|신청인|작성일|제출일|신청자")
# A total row: the sum goes under the amount column, not in every cell right of '계'.
TOTAL = re.compile(r"^(합?계|소계|총계|합\s*계)$")
INSTRUCTION = re.compile(r"^[※*]|작성\s*요령|기재\s*요령|유의\s*사항|기재하지|기재\s*하지|작성하지")
GLYPH_MENTION = re.compile(r"^\s*(안|에|표|란|칸|내|으로|를|은|는)(\s|$|에|시|하)")  # '□ 안에 ✓ 표시', '□에 체크'
CONSENT_WORDS = re.compile(r"동의함|동의안함|동의하지\s*않음|미동의")
DANGLING = re.compile(r"[-–·,:(（/]\s*$")
# A title or form number printed in a cell is bracketed ('[붙임 1]', '<개인정보 수집 및 이용
# 동의서>', '(제1호서식)'): punctuation, not words. A cell that repeats the page's own printed
# title is a heading too (heading_shape). Whose signature a mark is goes to the judge.
TITLE_BRACKETS = re.compile(r"^[\[(<〈《『「【]|[\]>〉》』」】]$")
MARKUP = re.compile(r"</?(?:u|sup|sub|b|i|s|em|strong)>")
# The office words of the proposer's ADMIN_ONLY without its '^*'/'^※' instruction marks (a
# '* 해당자만 기재' hint inside a blank cell is not an office cell).
OFFICE = re.compile("|".join(a for a in ADMIN_ONLY.pattern.split("|") if not a.startswith("^")))
# A blank before a printed '대학' asks for the college (공과대학), not the school: a box linked
# to the school name there names the wrong thing (the app would print '부산대학교 대학').
COLLEGE_SUFFIX = "대학"
# Profile keys whose value the app suggests into a box (apps/mobile/lib/formFill/fields.ts
# suggest). Sensitive kinds are never prefilled and a signature box takes no suggestion, so a
# link to rrn/account/signature on another person's box is not a wrong-person fill.
APPLICANT_KEYS = {"name", "name_en", "birth_date", "phone", "email", "address", "school", "major", "grade", "student_id", "stamp"}
# Labels that need written text: a tick box alone cannot answer them.
TEXT_KINDS = {"name", "name_en", "birth_date", "phone", "email", "address", "school", "major", "student_id", "rrn", "account", "bank"}
MAX_PAGES = 50


@dataclass
class Rect:
    x0: float
    y0: float
    x1: float
    y1: float

    @property
    def w(self) -> float:
        return self.x1 - self.x0

    @property
    def h(self) -> float:
        return self.y1 - self.y0

    def area(self) -> float:
        return max(0.0, self.w) * max(0.0, self.h)

    def inter(self, o: "Rect") -> float:
        return self.xov(o) * self.yov(o)

    def xov(self, o: "Rect") -> float:
        return max(0.0, min(self.x1, o.x1) - max(self.x0, o.x0))

    def yov(self, o: "Rect") -> float:
        return max(0.0, min(self.y1, o.y1) - max(self.y0, o.y0))

    def as_json(self) -> dict:
        return {"x": round(self.x0, 4), "y": round(self.y0, 4), "w": round(self.w, 4), "h": round(self.h, 4)}


@dataclass
class Slot:
    page: int  # 0-based
    kind: str  # value | checkbox | paren | date | sign | colon | underscore | suffix
    label: str
    region: Rect
    source: str  # ir | pdf
    owner: str = ""
    note: str = ""
    year_after_20: bool = False
    needs_text: bool = False
    context: str = ""  # printed text just before the slot on its line
    excluded: str = ""  # office | instruction | heading | total_row | unboxed_page
    covered_by: list = field(default_factory=list)
    status: str = ""  # covered | missing | wrong_type | excluded | question


def field_rect(f: dict) -> Rect:
    r = f["rect"]
    return Rect(r["x"], r["y"], r["x"] + r["w"], r["y"] + r["h"])


def norm(text: str) -> str:
    return compact(MARKUP.sub("", text or ""))


# ---------------------------------------------------------------------------
# PDF text layer
# ---------------------------------------------------------------------------
def page_chars(page) -> list[dict]:
    W, H = float(page.width), float(page.height)
    out = []
    for ch in page.chars:
        t = ch.get("text", "")
        if not t or t.isspace():
            continue
        out.append({"t": t, "x0": ch["x0"] / W, "x1": ch["x1"] / W, "y0": ch["top"] / H, "y1": ch["bottom"] / H,
                    "s": max(float(ch.get("size") or 10.0), 4.0) / W})
    return out


def lines_of(chars: list[dict]) -> list[list[dict]]:
    lines: list[list[dict]] = []
    for ch in sorted(chars, key=lambda c: (c["y0"], c["x0"])):
        cy = (ch["y0"] + ch["y1"]) / 2
        line = next((l for l in reversed(lines[-6:])
                     if abs((l[0]["y0"] + l[0]["y1"]) / 2 - cy) < 0.35 * (l[0]["y1"] - l[0]["y0"] + ch["y1"] - ch["y0"]) / 2 + 0.0015), None)
        if line is None:
            lines.append([ch])
        else:
            line.append(ch)
    for l in lines:
        l.sort(key=lambda c: c["x0"])
    return lines


def band(chars: list[dict]) -> tuple[float, float]:
    return min(c["y0"] for c in chars), max(c["y1"] for c in chars)


def line_text(line: list[dict]) -> str:
    out, prev = "", None
    for ch in line:
        if prev is not None and ch["x0"] - prev["x1"] > 0.25 * ch["s"]:
            out += " "
        out += ch["t"]
        prev = ch
    return out


def gap_rect(x0: float, x1: float, line: list[dict]) -> Rect:
    y0, y1 = band(line)
    return Rect(x0, y0, x1, y1)


def clip_left(x0: float, ch: dict, cells: list[tuple]) -> float:
    """A gap never starts before the ruled cell of the glyph that ends it ('신청일 |    년')."""
    cell = _cell_at(cells, ch)
    return max(x0, cell[0]) if cell is not None else x0


def is_instruction(line: list[dict]) -> bool:
    return bool(INSTRUCTION.search(line_text(line).strip()))


def word_before(line: list[dict], i: int, limit: int = 12) -> str:
    """The printed word that ends at line[i - 1] (stops at a visible gap)."""
    out, k = "", i - 1
    while k >= 0 and len(out) < limit:
        out = line[k]["t"] + out
        if k == 0 or line[k]["x0"] - line[k - 1]["x1"] > 0.6 * line[k]["s"]:
            break
        k -= 1
    return out


def _words(line: list[dict]) -> list[dict]:
    words: list[dict] = []
    for ch in line:
        if words and ch["x0"] - words[-1]["x1"] <= 0.45 * ch["s"] and ch["t"] not in "()（）" and words[-1]["t"][-1] not in "()（）":
            words[-1]["t"] += ch["t"]
            words[-1]["x1"] = ch["x1"]
        else:
            words.append({"t": ch["t"], "x0": ch["x0"], "x1": ch["x1"], "s": ch["s"]})
    return words


def letter_spaced(line: list[dict]) -> bool:
    """'명 지 대 학 교  총 장': a heading spread out by letter spacing."""
    words = [w for w in _words(line) if re.search(r"[가-힣]", w["t"])]
    return len(words) >= 4 and sum(len(w["t"]) == 1 for w in words) >= 0.75 * len(words)


def _big_or_open(cell) -> bool:
    """No ruled cell, or a frame around a whole section (a heading bullet sits there)."""
    return cell is None or (cell[3] - cell[1]) > 0.2 or (cell[2] - cell[0]) * (cell[3] - cell[1]) > 0.2


def _cell_at(cells: list[tuple], ch: dict):
    cx, cy = (ch["x0"] + ch["x1"]) / 2, (ch["y0"] + ch["y1"]) / 2
    inside = [c for c in cells if c[0] <= cx <= c[2] and c[1] <= cy <= c[3]]
    return min(inside, key=lambda c: (c[2] - c[0]) * (c[3] - c[1])) if inside else None


def glyph_slots(page_no: int, chars: list[dict], cells: list[tuple]) -> list[Slot]:
    """Slots read from printed glyphs only (independent of how the boxes were proposed)."""
    out: list[Slot] = []
    sizes = sorted(c["s"] for c in chars) or [0.0]
    body = sizes[len(sizes) // 2]  # the page's body text size
    for line in lines_of(chars):
        text = line_text(line)
        instr = "instruction" if is_instruction(line) else ""
        n = len(line)
        # □ tick boxes, each with the option text that follows it.
        for i, ch in enumerate(line):
            if ch["t"] not in BOX_GLYPHS:
                continue
            opt, last_x1 = "", ch["x1"]
            for nx in line[i + 1:]:
                if nx["t"] in BOX_GLYPHS or nx["x0"] - last_x1 > 2.5 * nx["s"]:
                    break
                opt += nx["t"]
                last_x1 = nx["x1"]
            ex = instr
            if GLYPH_MENTION.match(line_text(line[i + 1:i + 4])):
                ex = "instruction"
            # A section bullet: '□ 신청자격 등에 대한 서약' at the start of a line on the open page.
            elif i == 0 and len(compact(opt)) >= 2 and not any(c["t"] in BOX_GLYPHS for c in line[1:]) \
                    and (_big_or_open(_cell_at(cells, ch)) or (len(compact(opt)) >= 4 and min(c["s"] for c in line[1:]) >= 1.15 * body)):
                ex = "heading"
            elif re.search(r"확인$", compact(line_text(line[max(0, i - 3):i]))) or compact(opt) == "확인":
                ex = "office"  # '확인 □' beside an attachment list: the clerk ticks it
            out.append(Slot(page_no, "checkbox", compact(opt)[:40], Rect(ch["x0"], ch["y0"], ch["x1"], ch["y1"]), "pdf", excluded=ex))
        # '[ ]' tick boxes printed as brackets ('[ ] 동의  [ ] 미동의', '[ ]기타').
        for i, ch in enumerate(line):
            if ch["t"] != "[" or i + 1 >= n or line[i + 1]["t"] != "]" or line[i + 1]["x0"] - ch["x1"] < 0.6 * ch["s"]:
                continue
            opt = compact(line_text(line[i + 2:i + 8]))
            out.append(Slot(page_no, "checkbox", opt[:40], Rect(ch["x0"], ch["y0"], line[i + 1]["x1"], line[i + 1]["y1"]), "pdf", excluded=instr))
        # ( ) blanks, also '(우편번호:      )' and '만 (   )세'.
        for i, ch in enumerate(line):
            if ch["t"] not in "(（":
                continue
            j = next((k for k in range(i + 1, min(n, i + 30)) if line[k]["t"] in ")）"), None)
            if j is None:
                continue
            inner = line[i + 1:j]
            inner_text = compact("".join(c["t"] for c in inner))
            if SIGN_TOKEN.fullmatch(f"({inner_text})"):
                continue
            size, after = ch["s"], compact("".join(c["t"] for c in line[j + 1:j + 3]))
            before = word_before(line, i) if i > 0 and ch["x0"] - line[i - 1]["x1"] < 0.6 * size else ""
            if not inner:
                gap = line[j]["x0"] - ch["x1"]
                if gap >= 1.2 * size:
                    unit = any(after.startswith(u) for u in SUFFIX_WORDS)
                    # '고등학생( ), 대학생( )' are ticks; '만 (   )세', '기타(      )' and a lone wide
                    # '(        )' take writing.
                    options = sum(1 for k in range(n) if line[k]["t"] in "(（") >= 2
                    out.append(Slot(page_no, "paren", f"{before}( ){after}"[:24], gap_rect(ch["x1"], line[j]["x0"], line), "pdf",
                                    needs_text=unit or bool(re.search(r"기타", before)) or (gap >= 4 * size and not options),
                                    context=compact(line_text(line[max(0, i - 8):i])), excluded=instr))
                continue
            last = inner[-1]
            unit_text = compact("".join(c["t"] for c in inner))
            if inner[0]["x0"] - ch["x1"] >= 2.0 * size and any(unit_text == u for u in SUFFIX_WORDS):
                # '(              원)': the blank is before the printed unit inside the parentheses.
                out.append(Slot(page_no, "paren", f"{before}(  {unit_text})"[:24], gap_rect(ch["x1"], inner[0]["x0"], line), "pdf",
                                needs_text=True, context=compact(line_text(line[max(0, i - 8):i])), excluded=instr))
                continue
            if last["t"] in ":：" and line[j]["x0"] - last["x1"] >= 1.2 * size:
                out.append(Slot(page_no, "paren", inner_text[:-1][:16], gap_rect(last["x1"], line[j]["x0"], line), "pdf", needs_text=True, excluded=instr))
        # 년 월 일 blanks: a unit with a visible gap before it, at least two on the line.
        units = [i for i, ch in enumerate(line)
                 if ch["t"] in UNITS and i > 0 and ch["x0"] - line[i - 1]["x1"] >= 1.2 * ch["s"]
                 and not (i + 1 < n and line[i + 1]["t"] in "간도차수령" and line[i + 1]["x0"] - ch["x1"] < 0.3 * ch["s"])]
        if len(units) >= 2:
            for i in units:
                prev = line[i - 1]
                two = line[i]["t"] == "년" and i >= 2 and prev["t"] == "0" and line[i - 2]["t"] == "2" and (i < 3 or not line[i - 3]["t"].isdigit())
                out.append(Slot(page_no, "date", {"년": "연도", "월": "월", "일": "일"}[line[i]["t"]], gap_rect(clip_left(prev["x1"], line[i], cells), line[i]["x0"], line), "pdf",
                                year_after_20=two, needs_text=True, context=compact(line_text(line[max(0, units[0] - 12):units[0]])), excluded=instr))
        # Dotted dates: '2026.    .     .' or '20  .   .   .'.
        dots = [i for i, ch in enumerate(line) if ch["t"] == "." and i > 0 and ch["x0"] - line[i - 1]["x1"] >= 1.2 * ch["s"]]
        if len(dots) >= 2:
            first = dots[0]
            head = "".join(c["t"] for c in line[max(0, first - 4):first])
            if re.search(r"20\d{0,2}\.?$|\.$", head):
                for k, i in enumerate(dots):
                    out.append(Slot(page_no, "date", ("연도", "월", "일")[k % 3], gap_rect(line[i - 1]["x1"], line[i]["x0"], line), "pdf",
                                    year_after_20=k == 0 and head.endswith("20"), needs_text=True,
                                    context=compact(line_text(line[max(0, first - 12):first])), excluded=instr))
        # Signature marks, and the name blank in front of one ('신청인        (서명 또는 인)').
        flat = re.sub(r"\s", "", text)
        for m in re.finditer(r"\((?:서명|인|날인|인감|印)(?:(?:/|또는|,|·|및|혹은)(?:서명|인|날인|인감))*\)|㉿", flat):
            idx = _span_chars(line, m.start(), m.end())
            if idx:
                cs = [line[k] for k in idx]
                out.append(Slot(page_no, "sign", m.group(0), Rect(min(c["x0"] for c in cs), min(c["y0"] for c in cs), max(c["x1"] for c in cs), max(c["y1"] for c in cs)),
                                "pdf", owner=word_before(line, idx[0]) if idx[0] > 0 else "", excluded=instr))
                k = idx[0]
                if k > 0 and line[k - 1]["t"] not in ":：" and line[k]["x0"] - line[k - 1]["x1"] >= 3.0 * line[k]["s"]:
                    who = word_before(line, k)
                    if re.search(r"[가-힣]", who):
                        out.append(Slot(page_no, "colon", who[-12:], gap_rect(clip_left(line[k - 1]["x1"], line[k], cells), line[k]["x0"], line), "pdf",
                                        needs_text=True, note="name before a signature mark", excluded=instr))
        # 'label :   ' with blank space after the colon.
        for i, ch in enumerate(line):
            if ch["t"] not in ":：" or i == 0:
                continue
            nxt = line[i + 1] if i + 1 < n else None
            label = re.split(r"[)\].,(]", word_before(line, i))[-1]
            if not label or (nxt is not None and nxt["x0"] - ch["x1"] < 3.0 * ch["s"]):
                continue
            end = nxt["x0"] if nxt is not None else min(1.0, ch["x1"] + 0.25)
            cell = _cell_at(cells, ch)
            if cell is not None:
                end = min(end, cell[2])
            if end - ch["x1"] < 2.5 * ch["s"]:
                continue
            out.append(Slot(page_no, "colon", label[-12:], gap_rect(ch["x1"], end, line), "pdf", needs_text=True, excluded=instr))
        # ____ underscores.
        raw = "".join(c["t"] for c in line)
        for m in re.finditer(r"_{3,}", raw):
            cs = line[m.start():m.end()]
            out.append(Slot(page_no, "underscore", word_before(line, m.start()) if m.start() else "",
                            Rect(cs[0]["x0"], min(c["y0"] for c in cs), cs[-1]["x1"], max(c["y1"] for c in cs)), "pdf", needs_text=True, excluded=instr))
    out.extend(suffix_slots(page_no, chars, cells))
    return out


def with_line_context(slots: list[Slot], chars: list[dict]) -> None:
    """Context of a glyph slot: the printed text before it on its line (an office row such as
    '접수번호: ____ (2026년  월  일)' is recognised from its start)."""
    lines = lines_of(chars)
    for s in slots:
        if s.source != "pdf" or s.kind == "value":
            continue
        mid = (s.region.y0 + s.region.y1) / 2
        line = next((l for l in lines if band(l)[0] - 0.002 <= mid <= band(l)[1] + 0.002), None)
        if line:
            s.context = (s.context + "|" if s.context else "") + compact(line_text([c for c in line if c["x1"] <= s.region.x0 + 0.002]))[:40]


def _span_chars(line: list[dict], start: int, end: int) -> list[int]:
    """Indices of line chars covering [start, end) of the whitespace-free line text."""
    pos, idx = 0, []
    for k, ch in enumerate(line):
        for _ in ch["t"]:
            if start <= pos < end:
                idx.append(k)
            pos += 1
    return sorted(set(idx))


def suffix_slots(page_no: int, chars: list[dict], cells: list[tuple]) -> list[Slot]:
    """'      대학        학과': a gap before a unit word inside a ruled cell is a blank. A cell
    that holds only the word ('대학' as a right-aligned label) is a label, not a blank."""
    out = []
    for cell in cells:
        inside = [c for c in chars if cell[0] <= (c["x0"] + c["x1"]) / 2 <= cell[2] and cell[1] <= (c["y0"] + c["y1"]) / 2 <= cell[3]]
        for line in lines_of(inside):
            if is_instruction(line) or letter_spaced(line):
                continue
            words = _words(line)
            if len(words) == 1 and not words[0]["t"].endswith((")", "）")):
                continue  # a cell holding only '대학' is a label; '        원)' closes a blank
            prev_x1 = cell[0]
            for k, w in enumerate(words):
                word = w["t"]
                is_unit = not word.endswith((":", "：")) and any(word == s or (word.startswith(s) and len(word) <= len(s) + 1 and not re.search(r"[가-힣]", word[len(s):])) for s in SUFFIX_WORDS)
                spaced_label = k > 0 and len(word) == 1 and len(words[k - 1]["t"]) == 1 and re.fullmatch(r"[가-힣]", words[k - 1]["t"] or "") is not None
                if is_unit and not spaced_label and w["x0"] - prev_x1 >= 2.5 * w["s"] and not (k > 0 and words[k - 1]["t"].endswith(("(", "（"))):
                    out.append(Slot(page_no, "suffix", word, gap_rect(prev_x1, w["x0"], line), "pdf", needs_text=True))
                prev_x1 = w["x1"]
    return out


# ---------------------------------------------------------------------------
# Value cells: kordoc IR (HWP/HWPX) or ruled PDF cells (PDF)
# ---------------------------------------------------------------------------
def labelish(text: str) -> bool:
    c = norm(text)
    if not c or len(c) > 24 or c[0] in "※*-" or any(g in c for g in BOX_GLYPHS):
        return False
    words = MARKUP.sub("", text).split()
    if all(w in SUFFIX_WORDS or re.fullmatch(r"[년월일()/~.\-]+", w) for w in words):
        return False  # '대학   학과', '년  월': a value cell with unit words, or a printing aid
    if re.search(r"(니다|하며|하여|바랍니다|함\.|\d+부\.?$)", c):
        return False
    return bool(re.search(r"[가-힣A-Za-z]", c))


def ir_value_slots(ir: dict) -> list[dict]:
    """[{page (0-based), label, owner, direction}] for every empty IR cell right of a label
    cell, or under a header-row label cell."""
    out: list[dict] = []
    for block in ir.get("blocks", []):
        if block["kind"] == "table" and block["page"]:
            _table_slots(block["table"], block["page"] - 1, "", out)
    return out


def _at(cells: list[dict], r: int, c: int):
    for cell in cells:
        if cell["r"] <= r < cell["r"] + cell["rs"] and cell["c"] <= c < cell["c"] + cell["cs"]:
            return cell
    return None


def _table_slots(t: dict, page: int, outer_owner: str, out: list) -> None:
    cells = t["cells"]
    for cell in cells:
        owner = _owner(cells, cell) or outer_owner
        for nested in cell["tables"]:
            _table_slots(nested, page, owner, out)
        if not labelish(cell["text"]):
            continue
        right = _at(cells, cell["r"], cell["c"] + cell["cs"])
        below = _at(cells, cell["r"] + cell["rs"], cell["c"])
        blank = lambda x: x is not None and x is not cell and not norm(x["text"]) and not x["tables"]  # noqa: E731
        if blank(right):
            out.append({"page": page, "label": norm(cell["text"]), "owner": owner, "direction": "right"})
        elif (right is None or not blank(right)) and blank(below) and _header_row(cells, cell, blank):
            out.append({"page": page, "label": norm(cell["text"]), "owner": owner, "direction": "below"})


def _header_row(cells: list[dict], cell: dict, blank) -> bool:
    """A column heading: the cell is marked header, or its row holds two or more labels side by
    side and every cell right under them is blank (관계 | 성명 | 생년월일 over a member grid)."""
    if cell["header"]:
        return True
    row = [c for c in cells if c["r"] == cell["r"] and c["rs"] == cell["rs"] and labelish(c["text"])]
    return len(row) >= 2 and all(blank(_at(cells, c["r"] + c["rs"], c["c"])) for c in row)


def _owner(cells: list[dict], cell: dict) -> str:
    """Text of the nearest cell to the left spanning this cell's rows and more: a row-group
    header such as '①학생본인', '②직계존속', '위임받는 사람'."""
    best = None
    for o in cells:
        if o is cell or o["c"] + o["cs"] > cell["c"] or o["rs"] <= cell["rs"] or not norm(o["text"]):
            continue
        if o["r"] <= cell["r"] and cell["r"] + cell["rs"] <= o["r"] + o["rs"] and (best is None or o["c"] > best["c"]):
            best = o
    return norm(best["text"])[:40] if best else ""


def ir_cell_texts(ir: dict) -> dict:
    """page (0-based) -> every source table cell's text, nested tables included."""
    out: dict = {}

    def walk(t: dict, page: int) -> None:
        for c in t["cells"]:
            if c["text"].strip():
                out.setdefault(page, []).append(MARKUP.sub("", c["text"]))
            for n in c["tables"]:
                walk(n, page)

    for b in ir.get("blocks", []):
        if b["kind"] == "table" and b["page"]:
            walk(b["table"], b["page"] - 1)
    return out


def ir_owners(ir: dict) -> dict:
    """(page, label) -> [owner] for every labelled IR cell under a row-group header."""
    out: dict = {}

    def walk(t: dict, page: int, outer: str) -> None:
        for cell in t["cells"]:
            own = _owner(t["cells"], cell) or outer
            for nested in cell["tables"]:
                walk(nested, page, own)
            if own and labelish(cell["text"]):
                out.setdefault((page, norm(cell["text"])), []).append(own)

    prev = None
    for b in ir.get("blocks", []):
        if b["kind"] == "table" and b["page"]:
            walk(b["table"], b["page"] - 1, section_title(prev, b))
        prev = b
    return out


def section_title(prev: dict | None, table_block: dict) -> str:
    """A short paragraph right before a table on the same page names its block
    ('<타인 명의 수급자 정보>', '보호자 인적사항')."""
    if not prev or prev["kind"] == "table" or prev["page"] != table_block["page"]:
        return ""
    return block_title(prev.get("text", ""))


# A form's own title ('가족장학금 신청서', '실거주 확인서') above its first table names the form,
# not whose block it is.
FORM_NAME = re.compile(r"(신청서|확인서|동의서|서약서|신고서|계획서|소개서|지원서|명세서|청구서|요구서|위임장|추천서|각서|서식|양식)")


def block_title(text: str) -> str:
    t = norm(text)
    if not (2 <= len(t) <= 24) or FORM_NAME.search(t) or re.search(r"(니다|하며|바랍니다)$", t):
        return ""
    return t


def pdf_cells(page) -> list[tuple]:
    """(x0, y0, x1, y1, compact text, median font size) of every ruled cell, as page fractions."""
    W, H = float(page.width), float(page.height)
    out = []
    for c in cells_of(page):
        sizes = sorted(float(ch.get("size") or 0.0) for ch in c.chars if ch.get("text", "").strip())
        out.append((c.box.x0 / W, c.box.top / H, c.box.x1 / W, c.box.bottom / H, compact("".join(ch["text"] for ch in c.chars)),
                    sizes[len(sizes) // 2] if sizes else 0.0))
    return out


def right_of(lab: tuple, cells: list[tuple]) -> list[tuple]:
    return [c for c in cells if abs(c[0] - lab[2]) < 0.006 and min(c[3], lab[3]) - max(c[1], lab[1]) >= 0.5 * min(c[3] - c[1], lab[3] - lab[1])]


def below_of(lab: tuple, cells: list[tuple]) -> list[tuple]:
    return [c for c in cells if abs(c[1] - lab[3]) < 0.006 and min(c[2], lab[2]) - max(c[0], lab[0]) >= 0.6 * min(c[2] - c[0], lab[2] - lab[0])]


PRINTING_AID = re.compile(r"[년월일.\-~:/()@]*")
HINT_MARK = "※*(（"


def cell_size(c: tuple) -> float:
    return c[5] if len(c) > 5 else 0.0


def blank_text(c: tuple, label: tuple | None = None) -> bool:
    """A cell the applicant writes in: empty but for printing aids, or holding only a writing
    hint ('핸드폰 번호 기재시 선택입력 가능', '* 해당자만 기재'), told apart by typography, not
    words: it starts with a note mark, or it is printed smaller than its label."""
    t = norm(c[4])
    if PRINTING_AID.fullmatch(t):
        return True
    if len(t) > 40 or t.endswith((":", "：")):
        return False
    small = label is not None and cell_size(c) and cell_size(label) and cell_size(c) <= 0.9 * cell_size(label)
    return t[0] in HINT_MARK or bool(small)


def locate_value(slot: dict, cells: list[tuple], used: set) -> tuple[Rect | None, str]:
    """The PDF cell next to the IR label cell (same printed text, first unused occurrence in
    reading order). ('', rect) when found; (None, 'unlocated' | 'disagree') otherwise."""
    want = slot["label"]
    cand = [c for c in cells if c[4] == want and c not in used]
    if not cand:
        cand = [c for c in cells if want and c[4] and (c[4].startswith(want) or want.startswith(c[4])) and len(c[4]) >= max(2, len(want) - 2) and c not in used]
    if not cand:
        return None, "unlocated"
    lab = min(cand, key=lambda c: (round(c[1], 3), c[0]))
    used.add(lab)
    nb = right_of(lab, cells) if slot["direction"] == "right" else below_of(lab, cells)
    if not nb:
        return None, "unlocated"
    empty = [c for c in nb if blank_text(c, lab)]
    if not empty:
        return None, "disagree"  # the PDF prints text where the source table has a blank
    best = min(empty, key=lambda c: abs(c[1] - lab[1]) if slot["direction"] == "right" else abs(c[0] - lab[0]))
    return Rect(best[0], best[1], best[2], best[3]), ""


def pdf_value_slots(page_no: int, cells: list[tuple]) -> list[Slot]:
    """For a PDF with no IR: blank ruled cells right of (or under a heading row) a label cell."""
    out = []
    for lab in cells:
        if not labelish(lab[4]) or lab[2] - lab[0] < 0.02:
            continue
        right = right_of(lab, cells)
        blanks = [c for c in right if blank_text(c, lab) and c[2] - c[0] >= 0.02 and c[3] - c[1] >= 0.01]
        if blanks and len(right) == 1 and (lab[3] - lab[1]) <= 1.5 * (blanks[0][3] - blanks[0][1]):
            c = blanks[0]
            out.append(Slot(page_no, "value", lab[4][:40], Rect(c[0], c[1], c[2], c[3]), "pdf", note="right"))
    return out


# ---------------------------------------------------------------------------
# Coverage
# ---------------------------------------------------------------------------
def covers(slot: Slot, f: dict) -> bool:
    r, g = field_rect(f), slot.region
    if slot.kind in ("checkbox", "sign"):
        pad = 0.004
        cx, cy = (g.x0 + g.x1) / 2, (g.y0 + g.y1) / 2
        inside = r.x0 - pad <= cx <= r.x1 + pad and r.y0 - pad <= cy <= r.y1 + pad
        if slot.kind == "sign":
            return inside or (r.yov(Rect(g.x0, g.y0 - 0.01, g.x1, g.y1 + 0.01)) > 0 and r.xov(Rect(g.x0 - g.w, g.y0, g.x1 + g.w, g.y1)) > 0)
        return inside
    if slot.kind == "value":
        cx, cy = (r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2
        return (g.x0 <= cx <= g.x1 and g.y0 <= cy <= g.y1) or r.inter(g) >= 0.5 * max(r.area(), 1e-9)
    mid = (g.y0 + g.y1) / 2  # a gap on a printed line: the line inside the box's height, overlapping in x
    return r.y0 - 0.006 <= mid <= r.y1 + 0.006 and r.xov(g) >= 0.5 * min(g.w, r.w)


def compatible(slot: Slot, f: dict) -> bool:
    t = f["type"]
    if slot.kind == "checkbox":
        return t == "checkbox"
    if slot.kind == "sign":
        return t in ("signature", "stamp")
    if t == "checkbox":
        # An empty tick cell or a short '( )' after an option word takes a tick; a name,
        # phone or '(    )세' blank needs writing.
        return not slot.needs_text and (slot.kind != "value" or classify(slot.label) not in TEXT_KINDS)
    return True


# ---------------------------------------------------------------------------
# Main check
# ---------------------------------------------------------------------------
def box_numbers(template: dict) -> dict:
    """field id -> box number as in the review packet (per page, template order)."""
    out, per = {}, {}
    for f in template["fields"]:
        per[f["page"]] = per.get(f["page"], 0) + 1
        out[f["id"]] = per[f["page"]]
    return out


def page_kinds(pdf_path: Path) -> list[dict]:
    from auto.fidelity_gate import classify_pages  # trusted repo code: the same page rules as step 3
    return classify_pages(pdf_path)


def heading_shape(label: str, page_title: str = "") -> bool:
    """A heading by its shape: bracketed, or the page's own printed title (its largest-type
    line, page_title) repeated in a cell."""
    lab = norm(label)
    return bool(TITLE_BRACKETS.search(lab)) or (len(lab) >= 4 and bool(page_title) and lab in norm(page_title))


def page_title(lines: list) -> str:
    """The line printed in the largest type, when it stands out (1.3x the page's median size)."""
    sizes = sorted(ch["s"] for l in lines for ch in l)
    if not sizes:
        return ""
    med = sizes[len(sizes) // 2]
    big = max(lines, key=lambda l: max(ch["s"] for ch in l))
    return line_text(big) if max(ch["s"] for ch in big) >= 1.3 * med else ""


def exclusion(s: Slot, cells: list[tuple], page_head: str = "") -> str:
    if s.excluded:
        return s.excluded
    lab = norm(s.label)
    mine = bool(APPLICANT_CONTEXT.search(s.context) or APPLICANT_CONTEXT.search(lab))
    if not mine and (ADMIN_ONLY.search(lab) or (s.owner and ADMIN_ONLY.search(s.owner)) or ADMIN_ONLY.search(s.context) or _office_area(s, cells)):
        return "office"
    if s.kind == "value" and heading_shape(lab, page_head):
        return "heading"
    if s.kind == "value" and TOTAL.match(lab):
        return "total_row"
    return ""


def check(template: dict, pdf_path: Path, ir: dict | None = None, groups: dict | None = None) -> dict:
    fields = template["fields"]
    numbers = box_numbers(template)
    boxed = sorted({f["page"] for f in fields})
    slots: list[Slot] = []
    findings: list[dict] = []
    questions: list[dict] = []
    owners_ir = ir_owners(ir) if ir else {}
    ir_slots = ir_value_slots(ir) if ir else []
    layout = bool(ir) and ir.get("page_mode") == "layout"
    kinds = page_kinds(pdf_path)
    with pdfplumber.open(pdf_path) as pdf:
        if len(pdf.pages) > MAX_PAGES:
            raise SystemExit("refusing: more than 50 pages")
        page_cells, page_lines = [], []
        for pno, page in enumerate(pdf.pages):
            chars = page_chars(page)
            cells = pdf_cells(page)
            page_cells.append(cells)
            page_lines.append(lines_of(chars))
            ps = glyph_slots(pno, chars, cells)
            with_line_context(ps, chars)
            if layout:
                used: set = set()
                for s in (x for x in ir_slots if x["page"] == pno):
                    reg, why = locate_value(s, cells, used)
                    sl = Slot(pno, "value", s["label"][:40], reg or Rect(0, 0, 0, 0), "ir", owner=s["owner"], note=s["direction"])
                    head_text = page_title(page_lines[-1])
                    sl.excluded = exclusion(sl, cells, head_text) if reg else ("heading" if heading_shape(sl.label, head_text) else "")
                    if reg is None and not sl.excluded:
                        sl.status = "question"
                        questions.append({"id": f"label-{pno + 1}-{len(questions)}", "kind": "label_role", "page": pno + 1,
                                          "label": sl.label, "header": sl.owner, "why": why})
                    ps.append(sl)
            else:
                # No IR, or IR whose pages are only section approximations: the ruled cells of
                # the PDF are the only source of blank cells (never no source at all).
                ps.extend(pdf_value_slots(pno, cells))
            kind = kinds[pno]["kind"] if pno < len(kinds) else "other"
            head = kinds[pno]["head"] if pno < len(kinds) else ""
            title = page_title(page_lines[-1])
            for s in ps:
                s.excluded = exclusion(s, cells, title)
            slots.extend(ps)
            if pno not in boxed:
                live = [s for s in ps if not s.excluded and s.kind in ("checkbox", "value", "date", "sign", "paren", "underscore")]
                if live and kind != "others_only":
                    questions.append({"id": f"page-{pno + 1}", "kind": "page_role", "page": pno + 1, "title": head[:60],
                                      "page_kind": kind, "blanks": len(live), "sample": sorted({s.label for s in live if s.label})[:8]})
                for s in ps:
                    s.status = s.status or "excluded"
                    s.excluded = s.excluded or "unboxed_page"
                continue
            pf = [f for f in fields if f["page"] == pno]
            for s in ps:
                if s.status:
                    continue
                if s.excluded:
                    s.status = "excluded"
                    continue
                cov = [f for f in pf if covers(s, f)]
                s.covered_by = [f["id"] for f in cov]
                good = [f for f in cov if compatible(s, f)]
                if s.kind == "colon" and not cov:
                    if classify(s.label) or len(s.label) <= 3:
                        s.status = "missing"
                    else:
                        s.status = "question"
                        questions.append({"id": f"colon-{pno + 1}-{len(questions)}", "kind": "colon_blank", "page": pno + 1, "label": s.label})
                    continue
                if not cov and (s.kind == "sign" or (s.kind == "value" and s.owner)):
                    # Whose blank: the applicant's (a missing box) or someone else's, such as a
                    # recommender's (none needed). Asked for a signature mark, and for a value cell
                    # inside a named block (the structure says it belongs to someone).
                    s.status = "question"
                    questions.append({"id": f"owner-blank-{pno + 1}-{len(questions)}", "kind": "blank_owner", "page": pno + 1,
                                      "label": s.label, "header": s.owner, "slot": s.kind})
                elif not cov:
                    s.status = "missing"
                elif not good:
                    s.status = "wrong_type"
                elif s.year_after_20 and any(f["type"] != "checkbox" and f.get("part") != "year2" for f in good):
                    s.status = "wrong_type"
                    s.note = "a year blank after a printed '20' takes two digits: the box needs part year2 (a 4-digit year prints 2020xx)"
                else:
                    s.status = "covered"
                    for f in good:
                        # (Tick options are printed before or after their □, so only value cells are compared.)
                        if s.kind == "value" and s.label and not label_agrees(s.label, f["label"], s.kind):
                            questions.append({"id": f"match-{f['id']}", "kind": "label_match", "page": pno + 1, "box": numbers[f["id"]],
                                              "field": f["id"], "printed": s.label, "header": s.owner, "label": f["label"]})
                    dup = [f for f in good if s.kind == "suffix" and s.label == COLLEGE_SUFFIX and f.get("mapping") == "school"]
                    if dup:
                        f = dup[0]
                        findings.append({"page": pno + 1, "box": numbers[f["id"]], "kind": "mislabelled",
                                         "detail": f"box '{f['label'][:30]}' is linked to the school name but sits before the printed '대학': it asks for the college"})
            for s in ps:
                if s.status not in ("missing", "wrong_type"):
                    continue
                what = {"value": "blank cell", "checkbox": "tick box", "paren": "( ) blank", "date": "date blank", "sign": "signature mark",
                        "colon": "'label :' blank", "underscore": "____ blank", "suffix": "blank before"}[s.kind]
                detail = f"{what} '{s.label}' near ({s.region.x0:.3f},{s.region.y0:.3f})" + (f" in block '{s.owner}'" if s.owner and s.kind == "value" else "")
                if s.status == "missing":
                    findings.append({"page": pno + 1, "box": None, "kind": "missing", "detail": detail + " has no box"})
                else:
                    types = sorted({f["type"] for f in pf if f["id"] in s.covered_by})
                    findings.append({"page": pno + 1, "box": numbers.get(s.covered_by[0]), "kind": "wrong_type",
                                     "detail": detail + f" is covered by {'/'.join(types)}" + (f": {s.note}" if s.note else "")})
    findings.extend(field_rules(template, page_cells, owners_ir, numbers, questions, groups, page_lines, ir_cell_texts(ir) if layout else {}))
    return {"version": 1, "findings": findings, "questions": questions,
            "slots": [{**{k: v for k, v in asdict(s).items() if k != "region"}, "page": s.page + 1, "region": s.region.as_json()} for s in slots],
            "counts": _counts(slots, findings, questions), "ir_used": bool(ir), "ir_layout": layout}


def _bigrams(t: str) -> set:
    return {t[i:i + 2] for i in range(len(t) - 1)} or {t}


def label_agrees(printed: str, label: str, kind: str) -> bool:
    """The box label names the printed label (or tick option): containment, the same app
    kind, or most of the printed label's syllable pairs."""
    p = re.sub(r"\(.*?\)", "", norm(printed)) or norm(printed)
    b = norm(label.rsplit(": ", 1)[-1] if kind == "checkbox" else label)
    if not p or not b:
        return True
    if kind == "checkbox":
        p, b = p[:8], b[:8]
    if p in b or b in p:
        return True
    kp = classify(p)
    if kp and kp == classify(b):
        return True
    return len(_bigrams(p) & _bigrams(b)) >= 0.6 * len(_bigrams(p))


def _office_area(s: Slot, cells: list[tuple]) -> bool:
    """A slot inside an office cell, or in the cell right of an office label (접수번호 | ____)."""
    g = s.region
    home = _cell_at(cells, {"x0": g.x0, "x1": g.x1, "y0": g.y0, "y1": g.y1})
    if home is None:
        return False
    if home[4] and OFFICE.search(home[4]) and not APPLICANT_CONTEXT.search(home[4]):
        return True
    left = [c for c in cells if abs(c[2] - home[0]) < 0.006 and min(c[3], home[3]) - max(c[1], home[1]) >= 0.5 * min(c[3] - c[1], home[3] - home[1])]
    above = [c for c in cells if abs(c[3] - home[1]) < 0.006 and min(c[2], home[2]) - max(c[0], home[0]) >= 0.6 * min(c[2] - c[0], home[2] - home[0])]
    return any(c[4] and OFFICE.search(c[4]) and not APPLICANT_CONTEXT.search(c[4]) for c in left + above)


def _counts(slots: list[Slot], findings: list[dict], questions: list[dict]) -> dict:
    c: dict = {"slots": len(slots), "findings": len(findings), "questions": len(questions)}
    for s in slots:
        c[f"{s.kind}:{s.status}"] = c.get(f"{s.kind}:{s.status}", 0) + 1
    return c


def field_rules(template: dict, page_cells: list, owners_ir: dict, numbers: dict, questions: list[dict], groups: dict | None,
                page_lines: list | None = None, ir_texts: dict | None = None) -> list[dict]:
    out = []
    for f in template["fields"]:
        label, n, page = f["label"], numbers[f["id"]], f["page"]
        if f["type"] != "checkbox" and (any(g in label for g in BOX_GLYPHS) or CONSENT_WORDS.search(label)):
            out.append({"page": page + 1, "box": n, "kind": "mislabelled", "detail": f"label '{label[:60]}' carries printed tick-box options; it names the wrong thing"})
        if f["type"] == "checkbox" and DANGLING.search(label):
            out.append({"page": page + 1, "box": n, "kind": "mislabelled", "detail": f"tick-box label '{label[:60]}' is cut off"})
        if f["type"] == "checkbox":
            continue
        owner = field_owner(f, page_cells[page] if page < len(page_cells) else [], owners_ir,
                            page_lines[page] if page_lines and page < len(page_lines) else None)
        other = bool(OTHER_PERSON.search(compact(label)) or (owner and OTHER_PERSON.search(owner)))
        mapped = f.get("mapping") in APPLICANT_KEYS
        if other and (mapped or f.get("required")):
            why = f"linked to the profile key {f['mapping']}" if mapped else "marked required"
            out.append({"page": page + 1, "box": n, "kind": "wrong_person", "detail": f"box '{label[:40]}' is for someone else (block '{owner or label}') but is {why}"})
        elif owner and not other and mapped and not ADMIN_ONLY.search(owner) \
                and not (norm(owner) in norm(label) or norm(label) in norm(owner)):
            questions.append({"id": f"owner-{f['id']}", "kind": "owner", "page": page + 1, "box": n, "field": f["id"],
                              "header": owner, "label": label, "mapping": f.get("mapping")})
    by_id = {f["id"]: f for f in template["fields"]}
    for g in (groups or {}).get("groups", []):
        if not g.get("review_flag") and g.get("mode") in ("single", "multi"):
            continue
        stated, where = stated_mode(g, by_id, page_lines[g["page"]] if page_lines and g["page"] < len(page_lines) else [],
                                    (ir_texts or {}).get(g["page"], []))
        if stated is None:
            questions.append({"id": f"group-{g['id']}", "kind": "group_mode", "page": g["page"] + 1, "group": g["id"],
                              "question": g.get("question", ""), "options": g.get("options", [])[:12], "mode": g.get("mode")})
        elif stated != g.get("mode"):
            out.append({"page": g["page"] + 1, "box": None, "kind": "grouping",
                        "detail": f"group '{g.get('question', '')[:40]}' is {g.get('mode')} but the form states {stated} ({where})"})
    return out


RADIO = "○◯"


def stated_mode(g: dict, fields: dict, lines: list, ir_cells: list[str]) -> tuple[str | None, str]:
    """('single' | None, evidence) from the form's structure alone: every option printed after a
    ○ inside one source table cell, with no □, is a radio question. Wording ('택1', '중복 가능')
    is not read here (no word lists): such groups go to the judge, whose answer never fails a
    template (judge.py)."""
    opts = [norm(o)[:6] for o in g.get("options", []) if norm(o)]
    for c in ir_cells:
        if opts and sum(o in norm(c) for o in opts) == len(opts) and sum(c.count(r) for r in RADIO) >= len(opts) and not any(b in c for b in BOX_GLYPHS):
            return "single", "○ options in one cell"
    return None, ""


def field_owner(f: dict, cells: list[tuple], owners_ir: dict, lines: list | None = None) -> str:
    """Row-group header of the cell under the box: from the IR when the box's label cell is
    known there, else the taller cell to the left in the PDF table."""
    r = field_rect(f)
    cx, cy = (r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2
    home = [c for c in cells if c[0] <= cx <= c[2] and c[1] <= cy <= c[3]]
    if not home:
        return ""
    h = min(home, key=lambda c: (c[2] - c[0]) * (c[3] - c[1]))
    row = [c for c in cells if c[2] <= h[0] + 0.002 and min(c[3], h[3]) - max(c[1], h[1]) >= 0.6 * (h[3] - h[1]) and c[4]]
    label_cell = max(row, key=lambda c: c[2]) if row else None
    owners = owners_ir.get((f["page"], label_cell[4])) if label_cell is not None else None
    if owners:
        # The same label in two blocks ('성명' under ①학생본인 and ②직계존속): the k-th cell
        # with that text on the page, in reading order, is the k-th in the source table.
        same = sorted((c for c in cells if c[4] == label_cell[4]), key=lambda c: (round(c[1], 3), c[0]))
        k = same.index(label_cell) if label_cell in same else 0
        return owners[min(k, len(owners) - 1)]
    tall = [c for c in row if (c[3] - c[1]) > 1.3 * (h[3] - h[1]) and c[1] <= h[1] + 0.002 and c[3] >= h[3] - 0.002]
    if tall:
        return max(tall, key=lambda c: c[2])[4][:40]
    return title_above(h, cells, lines or [])


def table_of(cell: tuple, cells: list[tuple]) -> list[tuple]:
    """Cells connected to this one by shared edges: its ruled table."""
    group, todo = {cell}, [cell]
    while todo:
        c = todo.pop()
        for o in cells:
            if o in group:
                continue
            touch_x = min(c[2], o[2]) - max(c[0], o[0]) > -0.004
            touch_y = min(c[3], o[3]) - max(c[1], o[1]) > -0.004
            if touch_x and touch_y:
                group.add(o)
                todo.append(o)
    return list(group)


def title_above(cell: tuple, cells: list[tuple], lines: list[list[dict]]) -> str:
    """The printed line just above the cell's table (within 6% of the page): a section title."""
    t = table_of(cell, cells)
    top, x0, x1 = min(c[1] for c in t), min(c[0] for c in t), max(c[2] for c in t)
    above = [l for l in lines if top - 0.06 <= band(l)[1] <= top + 0.002 and min(l[-1]["x1"], x1) - max(l[0]["x0"], x0) > 0]
    if not above:
        return ""
    return block_title(line_text(max(above, key=lambda l: band(l)[1])))


def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("template")
    ap.add_argument("pdf")
    ap.add_argument("out")
    ap.add_argument("--ir")
    ap.add_argument("--source-sha256")
    ap.add_argument("--groups")
    a = ap.parse_args(argv)
    template = json.loads(Path(a.template).read_text())
    ir = check_ir.load(a.ir, a.source_sha256) if a.ir else None
    groups = json.loads(Path(a.groups).read_text()) if a.groups else None
    result = check(template, Path(a.pdf), ir, groups)
    Path(a.out).write_text(json.dumps(result, ensure_ascii=False, indent=1))
    # Counts only: labels are document text (in the JSON file).
    print(Path(a.template).name, json.dumps({k: result["counts"][k] for k in ("slots", "findings", "questions")}))


if __name__ == "__main__":
    main()
