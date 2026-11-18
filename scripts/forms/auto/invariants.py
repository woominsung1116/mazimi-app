# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9"]
# ///
"""The deterministic gate on a template's fields: one table of structural invariants.

Rules decide. Each invariant is a function over the page model (ruled cells, text lines, drawn
tick marks, the template's boxes) that returns findings, each with the invariant's name as
its reject reason, and, only where the structure is genuinely ambiguous, closed yes/no
questions for the judge. The judge never overrides a finding: a template with any finding is
rejected, whatever the judge answers. Field kinds come from the one classification the
proposer uses (propose_fields.classify / is_label_text); there are no per-case word lists here.

  invariant                 reviewer kind   what it checks
  label_box_adjacency       mislabelled     a text box tied structurally to a printed label (its
                                            own cell, the labelled cell left of it, its column
                                            header, the unit after a date part) names that label
  input_has_box             missing         every place that expects input has a box: blank
                                            cells right of a label (slots.py), blank grid cells
                                            under a column header that names an input, and one
                                            box per label where a label cell prints several
  person_section            wrong_person    a box in a repeated-row list (participants, family
                                            members) is never linked to the applicant's profile
                                            or required; slots.py's owner rules
  source_text_agreement     misplaced       the printed label a box is tied to is also in the
                                            source document's own text (kordoc IR), so the
                                            converted page did not move text away from boxes
  checkbox_groups_complete  missing         every printed tick mark has a tick box: glyphs (□,
                                            slots.py) and drawn squares alike
  date_signature_regions    wrong_type      date parts and signature marks have boxes of the
                                            right kind (slots.py)
  no_box_over_text          misplaced       a text box does not cover printed text (formulas
                                            and bracketed or marked hints aside)
  pages_are_forms           (asked only)    boxes on a page the page classifier does not call a
                                            form
Field kinds ("names an input", "the same field") come from propose_fields.classify /
is_label_text, the classification the app's fields.ts mirrors.

Ambiguous, so asked (one closed question about one box, kind box_label: "is box N the input
for label X?"): a box tied to no printed label; a structural tie to printed text that names
no known input ('의성군'); one box beside a label cell whose lines may be one wrapped label.
Weaker ties (text merely before the box on its line, above it in its cell, a row header, a
section label) neither reject nor ask.

Usage: python -I invariants.py <template.json> <blank.pdf> <out.slots.json> [--ir <x.ir.json> --source-sha256 <hex>] [--groups <g.json>]
Writes slots.py's output shape ({findings, questions, slots, counts}) with a "rule" on every
finding and per-rule counts, so verdicts.py and judge.py read it unchanged.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

import pdfplumber

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "kordoc"))
import check_ir  # noqa: E402
import slots  # noqa: E402
from propose_fields import SIGN_TOKEN, SUFFIXES, classify, is_label_text  # noqa: E402  (the proposer's own label classification)

TOL = 0.006  # page fraction: two ruled edges closer than this are the same line
TICK_MIN, TICK_MAX = 5.0, 16.0  # pt: a drawn square this size is a printed tick mark


@dataclass
class Cell:
    x0: float
    y0: float
    x1: float
    y1: float
    text: str

    @property
    def cx(self) -> float:
        return (self.x0 + self.x1) / 2

    @property
    def cy(self) -> float:
        return (self.y0 + self.y1) / 2


@dataclass
class Page:
    no: int
    cells: list
    lines: list
    ticks: list  # (x0, y0, x1, y1) page fractions of drawn squares and tick glyphs
    kind: str
    title: str


@dataclass
class Model:
    template: dict
    pages: list
    boxes: list  # (number on its page, field)
    ir_text: str = ""
    slots_result: dict = field(default_factory=dict)


def _cells(page) -> list[Cell]:
    return [Cell(c[0], c[1], c[2], c[3], c[4]) for c in slots.pdf_cells(page)]


def _ticks(page, chars) -> list[tuple]:
    W, H = float(page.width), float(page.height)
    out = []
    for r in page.rects:
        w, h = float(r["width"]), float(r["height"])
        if TICK_MIN <= w <= TICK_MAX and TICK_MIN <= h <= TICK_MAX and abs(w - h) <= 2.0:
            out.append((r["x0"] / W, r["top"] / H, r["x1"] / W, r["bottom"] / H))
    # Tick glyphs (□) are slots.py's rule, with its exclusions; drawn squares are this one's.
    # A glyph drawn over a square is one mark.
    uniq = []
    for t in out:
        if not any(abs(t[0] - u[0]) < TOL and abs(t[1] - u[1]) < TOL for u in uniq):
            uniq.append(t)
    return uniq


def build(template: dict, pdf_path: Path, ir: dict | None = None, slots_result: dict | None = None) -> Model:
    kinds = slots.page_kinds(pdf_path)
    pages = []
    with pdfplumber.open(pdf_path) as pdf:
        if len(pdf.pages) > slots.MAX_PAGES:
            raise SystemExit("refusing: more than 50 pages")
        for pno, page in enumerate(pdf.pages):
            chars = slots.page_chars(page)
            lines = slots.lines_of(chars)
            k = kinds[pno] if pno < len(kinds) else {"kind": "other"}
            pages.append(Page(pno, _cells(page), lines, _ticks(page, chars), k["kind"], slots.page_title(lines)))
    numbers = slots.box_numbers(template)
    boxes = [(numbers[f["id"]], f) for f in template["fields"]]
    ir_text = ""
    if ir:
        paragraphs = [b.get("text", "") for b in ir.get("blocks", []) if b.get("kind") != "table"]
        ir_text = slots.norm(" ".join([t for texts in slots.ir_cell_texts(ir).values() for t in texts] + paragraphs))
    return Model(template, pages, boxes, ir_text, slots_result or {})


# ---------------------------------------------------------------------------
# Geometry helpers
# ---------------------------------------------------------------------------
def _rect(f: dict) -> slots.Rect:
    return slots.field_rect(f)


def home_cell(cells: list[Cell], r: slots.Rect) -> Cell | None:
    cx, cy = (r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2
    inside = [c for c in cells if c.x0 - TOL <= cx <= c.x1 + TOL and c.y0 - TOL <= cy <= c.y1 + TOL]
    return min(inside, key=lambda c: (c.x1 - c.x0) * (c.y1 - c.y0)) if inside else None


def left_of(cells: list[Cell], c: Cell) -> list[Cell]:
    return [o for o in cells if abs(o.x1 - c.x0) < TOL and min(o.y1, c.y1) - max(o.y0, c.y0) >= 0.5 * min(o.y1 - o.y0, c.y1 - c.y0)]


def right_of(cells: list[Cell], c: Cell) -> list[Cell]:
    return [o for o in cells if abs(o.x0 - c.x1) < TOL and min(o.y1, c.y1) - max(o.y0, c.y0) >= 0.5 * min(o.y1 - o.y0, c.y1 - c.y0)]


def own_label(cells: list[Cell], c: Cell) -> list[Cell]:
    """Labelled cells left of c on its own row. A label spanning several rows heads a section
    ('팀원 정보' beside a list): it names the group, not the cell."""
    return [o for o in left_of(cells, c) if slots.labelish(o.text) and not slots.norm(o.text).isdigit()
            and (o.y1 - o.y0) <= 1.5 * (c.y1 - c.y0)]


def above_of(cells: list[Cell], c: Cell) -> list[Cell]:
    return [o for o in cells if abs(o.y1 - c.y0) < TOL and min(o.x1, c.x1) - max(o.x0, c.x0) >= 0.6 * min(o.x1 - o.x0, c.x1 - c.x0)]


def below_of(cells: list[Cell], c: Cell) -> list[Cell]:
    return [o for o in cells if abs(o.y0 - c.y1) < TOL and min(o.x1, c.x1) - max(o.x0, c.x0) >= 0.6 * min(o.x1 - o.x0, c.x1 - c.x0)]


def blank(text: str) -> bool:
    t = slots.norm(text)
    # A cell printing only a one-syllable unit ('원', '세') is a value cell; '학과' alone heads a column.
    return not t or bool(slots.PRINTING_AID.fullmatch(t)) or (t in slots.SUFFIX_WORDS and len(t) == 1)


def column_header(cells: list[Cell], c: Cell, boxed: list | None = None) -> Cell | None:
    """The first labelled cell straight above a cell in its own column, through blank cells and
    through data rows (a row that carries boxes is filled in, not a header). A printed cell
    much wider than the column is a section boundary, not its header. None when what is above
    is a sentence, a section boundary or nothing."""
    cur, seen, width = c, 0, c.x1 - c.x0
    while seen < 30:
        up = [o for o in cells if abs(o.y1 - cur.y0) < TOL and min(o.x1, c.x1) - max(o.x0, c.x0) >= 0.6 * min(o.x1 - o.x0, width)]
        if not up:
            return None
        cur = max(up, key=lambda o: min(o.x1, c.x1) - max(o.x0, c.x0))
        seen += 1
        if blank(cur.text):
            continue
        if cur.x1 - cur.x0 > 2.5 * width or min(cur.x1, c.x1) - max(cur.x0, c.x0) < 0.6 * width:
            return None  # a section boundary, or a header over only part of a wider cell
        row_boxed = any(min(b.y1, cur.y1) - max(b.y0, cur.y0) > 0.5 * (cur.y1 - cur.y0) for b in (boxed or []))
        if row_boxed:
            continue
        return cur if slots.labelish(cur.text) else None  # a sentence above is no column header
    return None


def row_header(cells: list[Cell], c: Cell, boxed: list) -> Cell | None:
    """The labelled cell at the start of a cell's row, through cells that carry boxes (a table
    whose columns are persons or periods: '관계 | [1] | [2] | [3]')."""
    cur = c
    for _ in range(30):
        left = [o for o in left_of(cells, cur) if (o.y1 - o.y0) <= 1.5 * (c.y1 - c.y0)]
        if not left:
            return None
        cur = left[0]
        if blank(cur.text) and any(b.inter(slots.Rect(cur.x0, cur.y0, cur.x1, cur.y1)) > 0.3 * (cur.x1 - cur.x0) * (cur.y1 - cur.y0) for b in boxed):
            continue
        return cur if slots.labelish(cur.text) and HANGUL.search(cur.text) else None
    return None


def office_row(cells: list[Cell], h: Cell) -> bool:
    """A header row with an office word in any cell (slots.py's OFFICE, the proposer's ADMIN_ONLY)."""
    return any(abs(o.y0 - h.y0) < TOL and abs(o.y1 - h.y1) < TOL and slots.OFFICE.search(slots.norm(o.text))
               and not slots.APPLICANT_CONTEXT.search(o.text) for o in cells)


def title_top(page: Page) -> float:
    """Top of the form's title line (slots.page_title), 0 when the page has none."""
    if not page.title:
        return 0.0
    for l in page.lines:
        if slots.norm(slots.line_text(l)) == slots.norm(page.title):
            return slots.band(l)[0]
    return 0.0


def header_row_labels(cells: list[Cell], h: Cell) -> int:
    """Labelled cells in the header row of h (same top and bottom edges)."""
    return sum(1 for o in cells if abs(o.y0 - h.y0) < TOL and abs(o.y1 - h.y1) < TOL and slots.labelish(o.text))


def inline_label(lines: list, r: slots.Rect, reach: float = 3.0) -> str:
    """Printed text on the box's line immediately left of it: the contiguous run of glyphs
    ending at the box (unruled forms: '성명: ____'), starting within `reach` glyph widths of
    it. Inside the box's own cell nothing can sit between, so the caller passes any reach."""
    best = ""
    for l in lines:
        y0, y1 = slots.band(l)
        if min(y1, r.y1) - max(y0, r.y0) < 0.4 * min(y1 - y0, r.y1 - r.y0):
            continue
        left = [ch for ch in l if ch["x1"] <= r.x0 + 0.004 and r.x0 - ch["x1"] < 0.12]
        run = []
        for ch in reversed(left):
            if run and run[-1]["x0"] - ch["x1"] > 1.5 * ch["s"]:
                break
            if not run and r.x0 - ch["x1"] > reach * ch["s"]:
                break
            run.append(ch)
        if run:
            best = slots.line_text(list(reversed(run))).strip()
    return best


def cell_text_before(lines: list, r: slots.Rect, boxed: list) -> str:
    """All printed text on the box's line inside its own cell, between the previous box on
    that line (or the cell's edge) and this box: nothing else can sit between them."""
    out = ""
    for l in lines:
        y0, y1 = slots.band(l)
        if min(y1, r.y1) - max(y0, r.y0) < 0.4 * min(y1 - y0, r.y1 - r.y0):
            continue
        edge = max([b.x1 for b in boxed if b.x1 <= r.x0 + 0.004 and min(b.y1, r.y1) - max(b.y0, r.y0) > 0], default=0.0)
        chars = [ch for ch in l if edge - 0.004 <= ch["x0"] and ch["x1"] <= r.x0 + 0.004]
        seg = slots.line_text(chars)
        if len(re.findall(r"[)）\]]", seg)) > len(re.findall(r"[(（\[]", seg)):
            # '계좌번호 : ( [ ]은행) [ ]': the previous box sits inside a bracket group that
            # qualifies the label before it; read the whole line up to this box.
            chars = [ch for ch in l if ch["x1"] <= r.x0 + 0.004]
        if chars:
            out = slots.line_text(chars).strip()
    return out


def suffix_label(lines: list, r: slots.Rect) -> str:
    """A unit printed right after the box names it ('[ ]년 [ ]월', '[  ]원', '[ ]세')."""
    for l in lines:
        y0, y1 = slots.band(l)
        if min(y1, r.y1) - max(y0, r.y0) < 0.4 * min(y1 - y0, r.y1 - r.y0):
            continue
        right = sorted((ch for ch in l if ch["x0"] >= r.x1 - 0.004 and ch["x0"] - r.x1 < 2 * ch["s"]), key=lambda c: c["x0"])
        if right:
            word = ""
            for ch in right:
                if word and ch["x0"] - prev["x1"] > 0.5 * ch["s"]:
                    break
                word += ch["t"]
                prev = ch
            if word and (word[0] in slots.UNITS or word in slots.SUFFIX_WORDS):
                return word[0] if word[0] in slots.UNITS else word
    return ""


HANGUL = re.compile(r"[가-힣]")


def last_word(text: str) -> str:
    """The label closest to the box in a run of printed text: what follows the last tick mark,
    bracket or space ('체능 □기타(' -> '기타'), punctuation trimmed. A closed bracket group at
    the end qualifies the label before it ('계좌번호 : ( 은행)' -> '계좌번호')."""
    text = re.sub(r"(\s*[(（\[][^)）\]]*[)）\]])+\s*$", "", text)
    parts = [re.sub(r"[:：)）\]]+$", "", p) for p in re.split(r"[□☐❏▢◻⬜○◯\s(（\[]+", text)]
    parts = [p for p in parts if p]
    return parts[-1] if parts else ""


def tie(page: Page, f: dict, boxed: list | None = None) -> tuple[str, str]:
    """(printed label the box is tied to, how), tried in this order: the unit printed right
    after it ('suffix' for a date part, else 'suffix_word'); inside its ruled cell, the text
    just left of it ('same_cell') or just above it ('cell_above'); the labelled cell left of it
    on its row ('left_cell'); the labelled header of its column or row, through cells that
    carry boxes ('column_header', 'row_header'); a label spanning several rows ('section');
    with no ruled cell, the text just left of it on its line ('same_line'). ('', '') when
    nothing qualifies (then the judge is asked)."""
    r = _rect(f)
    unit = suffix_label(page.lines, r)
    if unit:  # '[ ]년 [ ]월', '[ ]학기': the word after the box names it, not the one before
        return unit, "suffix" if f.get("part") else "suffix_word"
    home = home_cell(page.cells, r)
    if home is not None:
        inner = [[ch for ch in l if home.x0 - TOL <= ch["x0"] and ch["x1"] <= home.x1 + TOL and home.y0 - TOL <= ch["y0"] and ch["y1"] <= home.y1 + TOL]
                 for l in page.lines]
        inner = [l for l in inner if l]
        before = cell_text_before(inner, r, boxed or [])
        if re.search(r"[(（\[]\s*$", before):  # '( [ ] )종': a bracketed box is named after the bracket
            return "", "bracketed"
        text = last_word(before)
        if HANGUL.search(text):
            return text, "same_cell"
        above = [l for l in inner if slots.band(l)[1] <= r.y0 + 0.004]
        if above:
            text = slots.line_text(max(above, key=lambda l: slots.band(l)[1])).strip()
            if HANGUL.search(text):
                return text, "cell_above"
        own = [o for o in own_label(page.cells, home) if not blank(o.text) and HANGUL.search(o.text)]
        if own:
            return own[0].text, "left_cell"
        h = column_header(page.cells, home, boxed or [])
        if h is not None:
            return h.text, "column_header"
        h = row_header(page.cells, home, boxed or [])
        if h is not None:
            return h.text, "row_header"
        section = [o for o in left_of(page.cells, home) if not blank(o.text) and slots.labelish(o.text) and HANGUL.search(o.text)]
        if section:  # a label spanning several rows heads a section; it names the group, not the box
            return section[0].text, "section"
    text = last_word(inline_label(page.lines, r))
    if HANGUL.search(text):
        return text, "same_line"
    return "", ""


def column_names(page: Page, r: slots.Rect, label: str) -> bool:
    """Some printed line straight above the box, overlapping its column, names the box's label
    (a grid whose header row is not ruled into cells)."""
    for l in page.lines:
        y0, y1 = slots.band(l)
        if not (r.y0 - 0.3 <= y0 and y1 <= r.y0 + 0.002):
            continue
        over = [ch for ch in l if min(ch["x1"], r.x1) - max(ch["x0"], r.x0) > 0.5 * (ch["x1"] - ch["x0"])]
        text = slots.norm("".join(ch["t"] for ch in over))
        if len(text) >= 2 and agrees(text, label, "column_text"):
            return True
    return False


DATE_UNIT = {"year2": "년", "year4": "년", "month": "월", "day": "일"}


def qualifiers_out(t: str) -> str:
    """A label without its parenthetical qualifiers and row numbers ('자격증·교육명 (3행)' ->
    '자격증교육명'): what is left names the thing."""
    t = re.sub(r"[(（\[][^)）\]]*[)）\]]", "", t)
    t = re.sub(r"\d+행", "", t)
    return re.sub(r"[^0-9A-Za-z가-힣]", "", t)


def _bigrams(t: str) -> set:
    return {t[i:i + 2] for i in range(len(t) - 1)}


def agrees(printed: str, label: str, how: str, field: dict | None = None) -> bool:
    """The box's label names the printed label it is tied to: the slot check's agreement
    (containment, most syllable pairs), the same field kind in the proposer's classification,
    or at least half the syllable pairs of the shorter of the two, qualifiers aside. A date
    part must match its printed unit."""
    if how == "suffix":
        return DATE_UNIT.get((field or {}).get("part") or "") == printed
    p = re.sub(r"\d+\)$", "", slots.norm(printed))  # footnote markers: '비고(사유)4)'
    if slots.label_agrees(p, label, "value"):
        return True
    kind = classify(qualifiers_out(p))
    if kind and kind == classify(qualifiers_out(slots.norm(label))):
        return True  # the same field kind in the proposer's classification ('주민등록주소', '본인 주소')
    a, b = _bigrams(qualifiers_out(p)), _bigrams(qualifiers_out(slots.norm(label)))
    return bool(a and b) and len(a & b) >= 0.5 * min(len(a), len(b))


# ---------------------------------------------------------------------------
# Invariants
# ---------------------------------------------------------------------------
TEXTUAL = ("text", "date", "multiline")


DECISIVE = ("suffix", "same_cell", "left_cell", "column_header")


def is_unit(text: str) -> bool:
    t = slots.norm(text)
    return bool(t) and (t[0] in slots.UNITS and len(t) <= 2 or t in slots.SUFFIX_WORDS or t in SUFFIXES)


def label_box_adjacency(m: Model) -> tuple[list, list]:
    """A box's own label must name the printed label it is structurally tied to (DECISIVE ties).
    A mismatch rejects when the printed label names a known input; otherwise, and for a box
    tied to nothing that no printed column text names, the judge is asked about that box."""
    findings = [x for x in m.slots_result.get("findings", []) if x["kind"] == "mislabelled"]
    questions = []
    for n, f in m.boxes:
        if f["type"] not in TEXTUAL:
            continue
        page = m.pages[f["page"]]
        boxed = [_rect(g) for _, g in m.boxes if g["page"] == page.no and g is not f]
        printed, how = tie(page, f, boxed)
        if how == "bracketed":
            continue
        if not printed and column_names(page, _rect(f), f["label"]):
            continue  # unruled grid: printed text straight above in its column names it
        if not printed:
            questions.append({"id": f"tie-{f['id']}", "kind": "box_label", "page": f["page"] + 1, "box": n,
                              "label": f["label"], "printed": slots.norm(" ".join(slots.line_text(l) for l in page.lines
                                                                                      if abs(slots.band(l)[0] - _rect(f).y0) < 0.02))[:60]})
            continue
        if how not in DECISIVE or (how == "same_cell" and (is_unit(printed) or len(slots.norm(printed)) < 2)):
            continue
        if agrees(printed, f["label"], how, f):
            continue
        if how == "suffix" or is_label_text(qualifiers_out(slots.norm(printed))):
            findings.append({"page": f["page"] + 1, "box": n, "kind": "mislabelled",
                             "detail": f"box '{f['label'][:40]}' is tied to the printed label '{slots.norm(printed)[:40]}' ({how}) and does not name it"})
        else:  # printed text that names no known input ('의성군', '신랑'): asked, one box at a time
            questions.append({"id": f"tie-{f['id']}", "kind": "box_label", "page": f["page"] + 1, "box": n,
                              "label": f["label"], "printed": slots.norm(printed)[:60]})
    return findings, questions


def input_has_box(m: Model) -> tuple[list, list]:
    questions = []
    findings = [x for x in m.slots_result.get("findings", []) if x["kind"] == "missing" and not _is_date_or_sign(x)]
    for page in m.pages:
        boxes = [(n, f) for n, f in m.boxes if f["page"] == page.no]
        if not boxes:
            continue  # unboxed pages are slots.py's page_role question
        # Blank cells under a column header (a grid row of a table the applicant fills in).
        top = title_top(page)
        for c in page.cells:
            if not blank(c.text) or (c.x1 - c.x0) < 0.02 or (c.y1 - c.y0) < 0.01 or c.y1 <= top:
                continue  # cells above the form's title are its approval block, not input
            if sum(1 for o in page.cells if abs(o.y0 - c.y0) < TOL and abs(o.y1 - c.y1) < TOL) < 2:
                continue  # a row that is one cell wide is a spacer, not a grid row
            h = column_header(page.cells, c)
            # A column the applicant fills: its header names an input (the proposer's own label
            # classification), not an office stamp ('과장') or an option ('10구간').
            if h is None or not is_label_text(slots.norm(h.text)) or header_row_labels(page.cells, h) < 2:
                continue
            if office_row(page.cells, h):
                continue  # an approval line ('담당 | 과장'): the office signs it
            if own_label(page.cells, c):
                continue  # a value cell right of its own label: slots.py's rule
            covered = any(_rect(f).inter(slots.Rect(c.x0, c.y0, c.x1, c.y1)) >= 0.3 * (c.x1 - c.x0) * (c.y1 - c.y0) for _, f in boxes)
            if not covered:
                findings.append({"page": page.no + 1, "box": None, "kind": "missing",
                                 "detail": f"blank cell under the column '{slots.norm(h.text)[:30]}' near ({c.x0:.3f},{c.y0:.3f}) has no box"})
        # A label cell printing several labels on separate lines ('성명 / 연락처') needs a box per
        # label in the value cell right of it. Lines the proposer's classification sorts into two
        # or more different field kinds are several labels; otherwise the lines may be one wrapped
        # label ('대표자 / 성명'), so the judge is asked about that one box.
        for lab in page.cells:
            inner = [l for l in page.lines if lab.y0 - TOL <= slots.band(l)[0] and slots.band(l)[1] <= lab.y1 + TOL
                     and lab.x0 - TOL <= l[0]["x0"] and l[-1]["x1"] <= lab.x1 + TOL]
            texts = [slots.norm(slots.line_text(l)) for l in inner]
            labels = [t for t in texts if len(t) >= 2 and slots.labelish(t) and not t.startswith(("(", "（")) and not t[0].isdigit()]
            if len(labels) < 2 or any(re.search(r"[,，、]", t) for t in texts):
                continue  # a comma-separated list wrapped over lines is one text, not several labels
            for val in right_of(page.cells, lab):
                if (val.y1 - val.y0) > 1.5 * (lab.y1 - lab.y0) or slots.labelish(val.text) and not blank(val.text):
                    continue
                mine = [(n, f) for n, f in boxes if f["type"] in TEXTUAL and home_cell(page.cells, _rect(f)) is val]
                kinds = {classify(t) for t in labels} - {None}
                named = [t for t in labels if classify(t)] if len(kinds) >= 2 else []
                if len(named) >= 2 and len(mine) < len(named):
                    findings.append({"page": page.no + 1, "box": mine[0][0] if mine else None, "kind": "missing",
                                     "detail": f"{len(mine)} box(es) for {len(named)} printed labels ({' / '.join(x[:16] for x in named)}): each needs its own"})
                elif not named and len(mine) == 1:
                    n, f = mine[0]
                    questions.append({"id": f"one-{f['id']}", "kind": "box_label", "page": page.no + 1, "box": n,
                                      "label": f["label"], "printed": " / ".join(x[:24] for x in labels)})
    return _dedupe(findings), questions


def person_section(m: Model) -> tuple[list, list]:
    findings = [x for x in m.slots_result.get("findings", []) if x["kind"] == "wrong_person"]
    for n, f in m.boxes:
        if f["type"] not in TEXTUAL or not (f.get("mapping") in slots.APPLICANT_KEYS or f.get("required")):
            continue
        page = m.pages[f["page"]]
        home = home_cell(page.cells, _rect(f))
        if home is None:
            continue
        h = column_header(page.cells, home, [_rect(g) for _, g in m.boxes if g["page"] == f["page"] and g is not f]) if not own_label(page.cells, home) else None
        if h is None:
            continue
        # A list: the header row labels >= 3 columns and this column repeats as >= 2 blank or
        # boxed rows, each row starting with a sequence number or nothing. Never the applicant.
        if header_row_labels(page.cells, h) < 3:
            continue
        rows, cur, inside = 0, h, False
        while rows < 30:
            nxt = below_of(page.cells, cur)
            if not nxt or not blank(nxt[0].text):
                break
            first = min((o for o in page.cells if abs(o.y0 - nxt[0].y0) < TOL and abs(o.y1 - nxt[0].y1) < TOL), key=lambda o: o.x0, default=None)
            if first is not None and not (blank(first.text) or slots.norm(first.text).isdigit()):
                break
            cur = nxt[0]
            rows += 1
            inside = inside or cur is home
        if rows >= 2 and inside:
            why = f"linked to the profile key {f['mapping']}" if f.get("mapping") in slots.APPLICANT_KEYS else "marked required"
            findings.append({"page": f["page"] + 1, "box": n, "kind": "wrong_person",
                             "detail": f"box '{f['label'][:30]}' is a row of the list under '{slots.norm(h.text)[:24]}' ({rows} rows) but is {why}"})
    return _dedupe(findings), []


def source_text_agreement(m: Model) -> tuple[list, list]:
    if not m.ir_text:
        return [], []  # a PDF source has no second text to agree with
    findings = []
    for n, f in m.boxes:
        if f["type"] not in TEXTUAL:
            continue
        boxed = [_rect(g) for _, g in m.boxes if g["page"] == f["page"] and g is not f]
        printed, how = tie(m.pages[f["page"]], f, boxed)
        p = slots.norm(printed)
        if len(p) >= 2 and p not in m.ir_text:
            findings.append({"page": f["page"] + 1, "box": n, "kind": "misplaced",
                             "detail": f"the printed label '{p[:30]}' at box {n} is not in the source document's text"})
    return findings, []


def checkbox_groups_complete(m: Model) -> tuple[list, list]:
    findings = [x for x in m.slots_result.get("findings", []) if x["kind"] == "grouping"]
    for page in m.pages:
        boxes = [f for _, f in m.boxes if f["page"] == page.no]
        if not boxes:
            continue
        for t in page.ticks:
            cx, cy = (t[0] + t[2]) / 2, (t[1] + t[3]) / 2
            if bullet(page, t):
                continue
            hit = any(_rect(f).x0 - 0.006 <= cx <= _rect(f).x1 + 0.006 and _rect(f).y0 - 0.006 <= cy <= _rect(f).y1 + 0.006
                      and f["type"] == "checkbox" for f in boxes)
            if not hit and not any(_rect(f).x0 - 0.006 <= cx <= _rect(f).x1 + 0.006 and _rect(f).y0 - 0.006 <= cy <= _rect(f).y1 + 0.006
                                   for f in boxes):
                findings.append({"page": page.no + 1, "box": None, "kind": "missing",
                                 "detail": f"printed tick mark near ({t[0]:.3f},{t[1]:.3f}) has no tick box"})
    return _dedupe(findings), []


def bullet(page: Page, t: tuple) -> bool:
    """A square that starts a long line of text is a list bullet, not a tick mark."""
    for l in page.lines:
        y0, y1 = slots.band(l)
        if y0 - 0.004 <= (t[1] + t[3]) / 2 <= y1 + 0.004:
            after = [ch for ch in l if ch["x0"] >= t[2] - 0.002]
            before = [ch for ch in l if ch["x1"] <= t[0] + 0.002]
            return not before and len(after) >= 20
    return False


def date_signature_regions(m: Model) -> tuple[list, list]:
    return [x for x in m.slots_result.get("findings", []) if x["kind"] == "wrong_type" or (x["kind"] == "missing" and _is_date_or_sign(x))], []


def no_box_over_text(m: Model) -> tuple[list, list]:
    """A value or a signature written over printed text leaves both unreadable. Signature boxes
    are checked too and get no exemption, and a printed signing mark (SIGN_TOKEN) is never a
    hint although it is bracketed: people sign beside it, not over it. A seal (stamp) is
    pressed over its printed mark by design and is not checked here."""
    findings = []
    for n, f in m.boxes:
        if f["type"] not in TEXTUAL + ("signature",):
            continue
        r = _rect(f)
        page = m.pages[f["page"]]
        pad = 0.15 * min(r.w, r.h)
        covered = [ch for l in page.lines for ch in l
                   if r.x0 + pad <= (ch["x0"] + ch["x1"]) / 2 <= r.x1 - pad and r.y0 + pad <= (ch["y0"] + ch["y1"]) / 2 <= r.y1 - pad]
        inside = [ch for ch in covered
                  if not slots.PRINTING_AID.fullmatch(ch["t"]) and ch["t"] not in "_" and ch["s"] > 0.9 * median_size(page)]
        mark = bool(SIGN_TOKEN.search(slots.norm("".join(ch["t"] for ch in covered))))
        if f["type"] != "signature" and not mark:
            if sum(1 for c in inside if HANGUL.match(c["t"])) < 2:
                continue  # a formula or variable printed as a hint ('m=a+b+c'), not a label
            if all(hinted(l, inside) for l in page.lines if any(ch in l for ch in inside)):
                continue  # '(장학명 ... 참조)', '※ ...': a printed hint the applicant writes over
        if len(inside) >= 2:
            findings.append({"page": f["page"] + 1, "box": n, "kind": "misplaced",
                             "detail": f"box '{f['label'][:30]}' covers printed text '{''.join(c['t'] for c in inside)[:20]}'"})
    return findings, []


def hinted(line: list, inside: list) -> bool:
    """The covered glyphs of a line sit inside an open bracket, or the line starts with a hint
    mark: printed guidance, not a label."""
    first = next(i for i, ch in enumerate(line) if ch in inside)
    before = "".join(ch["t"] for ch in line[:first])
    opened = len(re.findall(r"[(（\[]", before)) > len(re.findall(r"[)）\]]", before))
    return opened or line[0]["t"] in slots.HINT_MARK


def median_size(page: Page) -> float:
    sizes = sorted(ch["s"] for l in page.lines for ch in l)
    return sizes[len(sizes) // 2] if sizes else 0.0


def pages_are_forms(m: Model) -> tuple[list, list]:
    """Boxes on a page the page classifier does not call a form: ambiguous, so asked."""
    questions = []
    for page in m.pages:
        if any(f["page"] == page.no for _, f in m.boxes) and page.kind != "form":
            questions.append({"id": f"page-form-{page.no + 1}", "kind": "page_form", "page": page.no + 1, "title": page.title[:60]})
    return [], questions


INVARIANTS = [
    ("label_box_adjacency", label_box_adjacency),
    ("input_has_box", input_has_box),
    ("person_section", person_section),
    ("source_text_agreement", source_text_agreement),
    ("checkbox_groups_complete", checkbox_groups_complete),
    ("date_signature_regions", date_signature_regions),
    ("no_box_over_text", no_box_over_text),
    ("pages_are_forms", pages_are_forms),
]


def _is_date_or_sign(f: dict) -> bool:
    return f["detail"].startswith(("date blank", "signature mark"))


def _dedupe(findings: list) -> list:
    seen, out = set(), []
    for x in findings:
        k = (x["page"], x["box"], x["kind"], x["detail"])
        if k not in seen:
            seen.add(k)
            out.append(x)
    return out


def check(template: dict, pdf_path: Path, ir: dict | None = None, groups: dict | None = None) -> dict:
    base = slots.check(template, pdf_path, ir, groups)
    m = build(template, pdf_path, ir, base)
    findings, questions, per_rule = [], [], {}
    for name, rule in INVARIANTS:
        fs, qs = rule(m)
        for x in fs:
            findings.append({**x, "rule": name, "origin": "slots" if any(x is b for b in base["findings"]) else "invariant"})
        questions.extend({**q, "rule": name} for q in qs)
        per_rule[name] = {"findings": len(fs), "questions": len(qs)}
    # slots.py's other questions (owner, label_role, colon_blank, group_mode, label_match, page_role) stay.
    questions = base["questions"] + questions
    counts = {**base["counts"], "findings": len(findings), "questions": len(questions), "rules": per_rule}
    return {**base, "findings": _dedupe(findings), "questions": questions, "counts": counts}


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
    groups = json.loads(Path(a.groups).read_text()) if a.groups and Path(a.groups).exists() else None
    result = check(template, Path(a.pdf), ir, groups)
    Path(a.out).write_text(json.dumps(result, ensure_ascii=False, indent=1))
    print(Path(a.template).name, json.dumps({k: result["counts"][k] for k in ("findings", "questions")}),
          json.dumps({k: v["findings"] for k, v in result["counts"]["rules"].items() if v["findings"]}))


if __name__ == "__main__":
    main()
