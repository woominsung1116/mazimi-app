# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber==0.11.9"]  # pulls pdfminer.six 20251107 (CVE-2025-64512 fixed)
# ///
"""Propose 서류 작성 도우미 input boxes for a blank official PDF form.

Reads ruled table cells and text positions, finds label cells (성명, 생년월일,
주소, 연락처 ...) and proposes a box in the adjacent empty value cell. Output
is the exact template schema the admin API accepts (crates/core/src/
form_templates.rs): page index from 0, rect as fractions of the crop box with
the origin top-left. Every proposal is a draft for human review: overlay PNGs
show each box on the rendered page.

Usage:
  uv run scripts/forms/propose_fields.py propose <file.pdf> [--overrides o.json] [--out t.json] [--overlay DIR]
  uv run scripts/forms/propose_fields.py batch --sources sources.json --raw DIR --out DIR --review DIR [--overrides DIR]
      [--conversions scripts/forms/hwp/conversions.json --derived DIR]

HWP/HWPX sources are templated on the PDF converted from them (scripts/forms/hwp)
only when conversions.json judges the conversion faithful, and only on its
accepted pages. Such a template records derived {source_sha256, converter}.

Reads local files only. Never fetches anything and never stores filled values.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path

import pdfplumber
from pdfplumber.utils import resolve_all

sys.path.insert(0, str(Path(__file__).resolve().parent / "auto"))  # trusted repo code; -I keeps cwd/script dir out
import groups as checkbox_groups  # noqa: E402  (scripts/forms/auto/groups.py)

# ---------------------------------------------------------------------------
# Label vocabulary
# ---------------------------------------------------------------------------

# Mirrors apps/mobile/lib/formFill/fields.ts MAPPING_RULES (first match wins).
# templates.test.mjs checks every produced mapping against the app's own
# classifyLabel, so drift fails the test instead of shipping.
MAPPING_RULES: list[tuple[str, re.Pattern]] = [
    ("rrn", re.compile(r"주민(등록)?번호|주민등록(?!.*(주소|거주지))|외국인등록번호", re.I)),  # '주민등록(상) 주소' is an address
    ("account_holder", re.compile(r"예금주", re.I)),
    ("bank", re.compile(r"은행(명)?|금융기관", re.I)),
    ("account", re.compile(r"계좌", re.I)),
    ("signature", re.compile(r"서명|signature", re.I)),
    ("stamp", re.compile(r"\(인\)|날인|도장|직인", re.I)),
    ("application_date", re.compile(r"신청(일|일자|날짜)|작성(일|일자)|제출(일|일자)|접수일", re.I)),
    ("birth_date", re.compile(r"생년월일|출생일|birth", re.I)),
    ("email", re.compile(r"e-?mail|이메일|전자우편|메일주소", re.I)),
    ("phone", re.compile(r"휴대(폰|전화)|핸드폰|연락처|전화(번호)?|mobile|phone|\btel\b", re.I)),
    ("address", re.compile(r"주소|거주지|소재지|address", re.I)),
    ("student_id", re.compile(r"학번", re.I)),
    ("major", re.compile(r"학과|전공|학부", re.I)),
    ("grade", re.compile(r"학년", re.I)),
    ("school", re.compile(r"학교|대학(교)?명?|재학교", re.I)),
    ("name_en", re.compile(r"영문(성명|이름)", re.I)),
    ("name", re.compile(r"성명|이름|신청인|신청자", re.I)),
]
SENSITIVE = {"rrn", "account"}

# Labels worth a box even without a profile mapping.
GENERIC_LABELS = re.compile(
    r"^(소속|직업|직장명?|성별|나이|연령|관계|세대주|세대주와의관계|가구원수?|가족관계|국적|학적|학적사항|학적상태|재학구분|졸업(예정)?일?|"
    r"입학(년도|일)|학교구분|학기|성적|평점|평균평점|이수학점|취득학점|신청학기|직위|직급|"
    r"거주기간|전입일|임대인|임차인|보증금|임차보증금|월세|월임대료|임대료|임대차기간|계약기간|계약일자?|신청금액|지원금액|"
    r"추천인|보호자|부모|대상자|비고|팩스|FAX|우편번호|신청분야|분야|희망분야|"
    r"사업명|단체명|기관명|대표자|사업자등록번호|고용보험|취업처|회사명|자격증명|자격종목명?|시험명|응시일자?|시험일자?|"
    r"생년|출생년도|주민등록상주소|실거주지|자택|성명\(한글\)|성명\(한자\)|한자|직장|근무처|근무기간|소재지|면적|임차면적|"
    r"학교명|대학명|학과명|전공명|재학학년|성적평균|신청인과의관계|본인과의관계|관계인|취득일|합격일|발급일)$",
    re.I,
)
MULTILINE_LABELS = re.compile(r"사유|동기|계획|소개|내용|경력|활동|포부|하고싶은말|자기")
# Admin-only cells (결재란, 접수번호) never get a box.
ADMIN_ONLY = re.compile(r"결재|담당|과장|실장|팀장|국장|부장|원장|접수|처리기간|처리|관리번호|확인자|심사|검토|^\*|^※")
# Printed words after a blank that name what goes in it: '____ 대학교 ____ 학과'.
SUFFIXES = {"대학교": "학교명", "대학": "학교명", "학과": "학과", "학부": "학부", "전공": "전공", "학년": "학년", "학기": "학기", "학번": "학번",
            "학점": "학점", "원": "금액", "세": "나이", "명": "인원", "개월": "개월", "점": "점수", "㎡": "면적", "m²": "면적", "m2": "면적", "호": "호", "동": "동", "층": "층"}
# Text that sits inside an empty value cell as a printing aid, not content.
PLACEHOLDER_TOKEN = re.compile(r"^(?:[년월일]|@|-|~|～|:|\.|/|20|19|☎|\(|\)|\*|※)$")
SIGN_TOKEN = re.compile(r"\((?:서명|인|날인|인감)(?:(?:/|또는|,|·|및|혹은)(?:서명|인|날인|인감))*\)")
HINT = re.compile(r"예시|예\)|참조|참고|기재요령|작성요령|해당자|선택")
CHECK_GLYPHS = "□☐❏▢"
SEPARATORS = ".~～-@○◦:："
CIRCLED = "①②③④⑤⑥⑦⑧⑨"  # printed between blanks; always their own token
# Another person's fields: no profile mapping (sensitivity is kept) and never required.
# Broader than the app's isOtherPerson so landlords, employers and family rows are covered too.
OTHER_PERSON = re.compile(r"보호자|부모|부친|모친|아버지|어머니|배우자|대리인|추천인|담당자|교수|지도교사|가구원|세대원|임대인|학과장|확인자|대표자|관계인|가족|동거인|원가구|독립가구|기업|회사|사업주|고용주|수령인|위임받는|수임인|면접관|채용|부양자|구성원|타인|직계존속|비상연락")
RELATION = re.compile(r"관계")

PAD = 1.5  # pt inset from ruled lines
MIN_GAP = 10.0  # pt; narrower gaps inside a cell are not writable
NAME_MIN = 30.0  # pt; the narrowest name box beside a label or a signing mark


def classify(label: str) -> str | None:
    text = re.sub(r"[\s_]+", "", label)
    if not text:
        return None
    for kind, pattern in MAPPING_RULES:
        if pattern.search(text):
            return kind
    return None


def compact(text: str) -> str:
    return re.sub(r"\s+", "", text)


def clean_label(text: str) -> str:
    """'성    명' -> '성명'; keeps spaces between multi-syllable words. Invisible
    format/control characters (bidi overrides, zero-width) never reach a label."""
    text = "".join(ch for ch in text if unicodedata.category(ch) not in ("Cc", "Cf", "Co", "Cs") or ch in " \n\t")
    text = re.sub(r"\s+", " ", text).strip()
    parts = text.split(" ")
    if len(parts) > 1 and sum(len(p) == 1 for p in parts) >= len(parts) - 1:
        # '성 명', '주 소 (거 주 지)' style letter spacing.
        text = re.sub(r"(?<=[가-힣]) (?=[가-힣])", "", text)
    text = re.sub(r"^(?:[※*·•\-○◦●▪■ㅇ√✓]|\d+[.)])\s*", "", text).strip()
    text = re.sub(r"\s*[:：]$", "", text).strip()
    return text[:80]


def is_label_text(text: str) -> bool:
    c = compact(text)
    c = re.sub(r"^[※*·•]+", "", c)
    c = re.sub(r"[:：]$", "", c)
    if not c or len(c) > 16 or any(g in c for g in CHECK_GLYPHS) or is_sign(c) or is_placeholder(c):
        return False
    words = text.split()
    if len(words) > 1 and all(w in SUFFIXES for w in words):
        return False  # '대학교   학과   학년': blanks with unit words, a value cell
    if re.search(r"(습니다|니다|하며|하여|합니다|입니다|바랍니다|함\.)", c):
        return False
    c_no_paren = re.sub(r"\(.*?\)", "", c) or c
    return bool(classify(c_no_paren) or GENERIC_LABELS.match(c_no_paren) or GENERIC_LABELS.match(c) or (len(c_no_paren) <= 8 and MULTILINE_LABELS.search(c_no_paren)))


def is_short_label(text: str) -> bool:
    """A column/row heading that is not in the vocabulary: short Hangul words, no digits or sentences."""
    c = re.sub(r"\d+\)$", "", compact(text))  # footnote mark: 공적이전소득2)
    core = re.sub(r"\(.*?\)", "", c) or c
    if not (2 <= len(core) <= 14) or len(c) > 24 or ADMIN_ONLY.search(c) or any(g in c for g in CHECK_GLYPHS):
        return False
    if re.search(r"\d|[:：]|니다|요$|O,|○|×", c):
        return False
    return bool(re.fullmatch(r"[가-힣·․‧ㆍ/]+", core)) and core not in SUFFIXES


def is_other_person(label: str) -> bool:
    return bool(OTHER_PERSON.search(compact(label)))


def mapping_for(label: str) -> str | None:
    """The app's reading of the label, except: a relationship is not a name, and another
    person's field gets no profile link (sensitive kinds stay so the app still masks them)."""
    kind = classify(label)
    if kind in SENSITIVE or kind in ("signature", "stamp"):
        return kind
    if RELATION.search(compact(label)) or re.search(r"자택|비상|직장", compact(label)) or is_other_person(label):
        return None
    return kind


# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------


@dataclass
class Box:
    x0: float
    top: float
    x1: float
    bottom: float

    @property
    def w(self) -> float:
        return self.x1 - self.x0

    @property
    def h(self) -> float:
        return self.bottom - self.top


@dataclass
class Cell:
    box: Box
    chars: list = field(default_factory=list)

    @property
    def text(self) -> str:
        return text_of(self.chars)


def text_of(chars) -> str:
    """Reading-order text of chars, with a space wherever a visible gap sits."""
    if not chars:
        return ""
    rows: list[list] = []
    for ch in sorted(chars, key=lambda c: (round(c["top"]), c["x0"])):
        if rows and abs(rows[-1][0]["top"] - ch["top"]) < 3:
            rows[-1].append(ch)
        else:
            rows.append([ch])
    out = []
    for row in rows:
        row.sort(key=lambda c: c["x0"])
        line = ""
        prev = None
        for ch in row:
            if prev is not None and ch["x0"] - prev["x1"] > max(1.5, 0.25 * ch["size"]):
                line += " "
            line += ch["text"]
            prev = ch
        out.append(line)
    return "\n".join(out)


def y_overlap(a: Box, b: Box) -> float:
    return max(0.0, min(a.bottom, b.bottom) - max(a.top, b.top))


def x_overlap(a: Box, b: Box) -> float:
    return max(0.0, min(a.x1, b.x1) - max(a.x0, b.x0))


def group_lines(toks: list[dict]) -> list[list[dict]]:
    lines: list[list[dict]] = []
    for t in sorted(toks, key=lambda t: (t["top"], t["x0"])):
        line = next((l for l in lines if abs(l[0]["top"] - t["top"]) < 3), None)
        if line is None:
            lines.append([t])
        else:
            line.append(t)
    for line in lines:
        line.sort(key=lambda t: t["x0"])
    lines.sort(key=lambda l: l[0]["top"])
    return lines


def tokens(chars) -> list[dict]:
    """Chars on one baseline joined into tokens split at gaps of about half a glyph.
    A parenthetical spread out by letter spacing, '(서  명)', becomes one token."""
    toks: list[dict] = []
    for line in group_lines([{**ch, "text": ch["text"]} for ch in chars if not ch["text"].isspace()]):
        run: list[dict] = []
        for ch in line:
            last = run[-1] if run else None
            sep = ch["text"] in SEPARATORS or (last is not None and last["text"][-1] in SEPARATORS)
            if last and not sep and ch["x0"] - last["x1"] < max(1.5, 0.45 * ch["size"]):
                last["text"] += ch["text"]
                last["x1"] = ch["x1"]
                last["bottom"] = max(last["bottom"], ch["bottom"])
                last["top"] = min(last["top"], ch["top"])
            else:
                run.append({"text": ch["text"], "x0": ch["x0"], "x1": ch["x1"], "top": ch["top"], "bottom": ch["bottom"], "size": ch["size"]})
        merged: list[dict] = []
        i = 0
        while i < len(run):
            t = dict(run[i])
            if t["text"].startswith("(") and ")" not in t["text"]:
                j = i + 1
                text = t["text"]
                while j < len(run) and len(compact(text)) <= 16 and ")" not in text:
                    text += run[j]["text"]
                    j += 1
                if ")" in text and len(compact(text)) <= 16:
                    t.update(text=text, x1=run[j - 1]["x1"], bottom=max(r["bottom"] for r in run[i:j]))
                    merged.append(t)
                    i = j
                    continue
            merged.append(t)
            i += 1
        toks.extend(merged)
    return toks


def is_sign(text: str) -> bool:
    c = compact(text)
    return bool(SIGN_TOKEN.fullmatch(c)) or c == "㉿"


def is_placeholder(text: str) -> bool:
    c = re.sub(r"^[*※]+(?=.)", "", compact(text))
    if not c:
        return True
    if is_sign(c) or PLACEHOLDER_TOKEN.match(c):
        return True
    # Parenthetical hints such as (도로명주소) or (최소 1개월 기재), year stems, separators.
    return bool(re.fullmatch(r"\([^()]{1,14}\)|\(|\)|\d{2,4}|20\d{0,2}[.년]?|[년월일]+|[-~:./@*]+|\(?[.\s]*\)?", c))


# ---------------------------------------------------------------------------
# Proposal
# ---------------------------------------------------------------------------


@dataclass
class Proposal:
    page: int
    box: Box
    type: str
    label: str
    mapping: str | None
    required: bool = False
    format: str | None = None
    note: str = ""
    mark: Box | None = None  # the printed (서명)/(인) this box answers; claimed, so no second box is made for it
    date_part: str | None = None  # DATE_PARTS: this box holds one part of the application date


# Twin of DATE_PARTS in crates/core/src/form_templates.rs and apps/mobile/lib/formFill/fields.ts.
DATE_PARTS = ("year2", "year4", "month", "day")


def make(page_index: int, box: Box, label: str, ftype: str | None = None, note: str = "", part: bool = False,
         date_part: str | None = None) -> Proposal:
    """One rule for type, mapping and required from the label. A part of a split value
    (phone segment, year box) links to nothing, so no whole value is suggested into it;
    a sensitive part keeps its kind so the app still masks and clears it. The exception is
    a part of the applicant's own application date: the caller passes date_part (DATE_PARTS)
    only when the page structure says so, and the label must still read as the application
    date, so a 생년월일 or 기간 part never links. The app then suggests today's part."""
    label = label[:80]
    mapping = mapping_for(label)
    if date_part in DATE_PARTS and mapping == "application_date":
        return Proposal(page_index, box, "text", label, mapping, False, None, note, date_part=date_part)
    if part and mapping not in SENSITIVE:
        mapping = None
    if ftype is None:
        ftype = "text"
        if mapping in ("birth_date", "application_date"):
            ftype = "date"
        elif MULTILINE_LABELS.search(compact(label.split()[-1])) and box.h > 30:
            ftype = "multiline"
    if ftype in ("signature", "stamp"):
        mapping = ftype
    elif ftype == "checkbox":
        mapping = None
    elif mapping in ("signature", "stamp"):
        mapping = None
    if ftype == "date" and mapping not in ("birth_date", "application_date"):
        ftype = "text"
    required = not part and (mapping in ("name", "phone", "birth_date", "rrn", "address") or ftype in ("signature", "stamp")) and not is_other_person(label)
    return Proposal(page_index, box, ftype, label, mapping, required, "YYYY.MM.DD" if ftype == "date" else None, note)


def table_cells(page, extra: list[float]) -> list[Cell]:
    settings = {"vertical_strategy": "lines", "horizontal_strategy": "lines", "snap_tolerance": 3, "intersection_tolerance": 3, "explicit_vertical_lines": extra}
    cells: list[Cell] = []
    seen = set()
    for table in page.find_tables(settings):
        for c in table.cells:
            if c is None:
                continue
            key = tuple(round(v, 1) for v in c)
            if key in seen:
                continue
            seen.add(key)
            box = Box(*c)
            if box.w < 4 or box.h < 4:
                continue
            chars = [ch for ch in page.chars if box.x0 <= (ch["x0"] + ch["x1"]) / 2 <= box.x1 and box.top <= (ch["top"] + ch["bottom"]) / 2 <= box.bottom]
            cells.append(Cell(box, chars))
    # A cell that contains other cells is a table outline, not a cell.
    return [c for c in cells if not any(o is not c and o.box.w * o.box.h < c.box.w * c.box.h * 0.9 and o.box.x0 >= c.box.x0 - 1 and o.box.x1 <= c.box.x1 + 1 and o.box.top >= c.box.top - 1 and o.box.bottom <= c.box.bottom + 1 for o in cells)]


def cells_of(page) -> list[Cell]:
    """Ruled table cells. Tables drawn without an outer side border get a virtual one where
    three or more horizontal rules start or stop; cells found only that way are added to the
    plain ruled cells, never replace them."""
    hs = [e for e in page.edges if e["orientation"] == "h" and e["x1"] - e["x0"] > 60]
    ends: dict[int, list[float]] = {}
    for e in hs:
        for x in (e["x0"], e["x1"]):
            ends.setdefault(round(x / 3), []).append(e["top"])
    vs = [e for e in page.edges if e["orientation"] == "v"]
    extra = []
    for key, tops in ends.items():
        if len(tops) < 3:
            continue
        x = key * 3
        lo, hi = min(tops), max(tops)
        drawn = sum(max(0.0, min(v["bottom"], hi) - max(v["top"], lo)) for v in vs if abs(v["x0"] - x) < 3)
        if drawn < 0.5 * (hi - lo):  # the rules stop here but no border is drawn
            extra.append(float(x))
    cells = table_cells(page, [])
    if extra:
        for c in table_cells(page, extra):
            if not any(x_overlap(c.box, o.box) * y_overlap(c.box, o.box) > 1 for o in cells):
                cells.append(c)
    return cells


def home_cell(ch: dict, cells: list[Cell]) -> Cell | None:
    """Smallest ruled cell containing the glyph's centre, or None on the open page."""
    cx, cy = (ch["x0"] + ch["x1"]) / 2, (ch["top"] + ch["bottom"]) / 2
    inside = [c for c in cells if c.box.x0 <= cx <= c.box.x1 and c.box.top <= cy <= c.box.bottom]
    return min(inside, key=lambda c: c.box.w * c.box.h) if inside else None


def left_heading(cell: Cell, cells: list[Cell]) -> str:
    """First line of the cell immediately to the left, e.g. '관계인' beside '성명 :' lines."""
    for other in cells:
        if other is not cell and abs(other.box.x1 - cell.box.x0) <= 3 and y_overlap(other.box, cell.box) >= 0.6 * cell.box.h and other.chars:
            text = clean_label(other.text.replace("\n", " "))
            if text and len(compact(text)) <= 20:
                return text
            text = clean_label(other.text.split("\n")[0])
            if text and len(compact(text)) <= 14:
                return text
    return ""


def right_neighbor(cell: Cell, cells: list[Cell]) -> Cell | None:
    best = None
    for other in cells:
        if other is cell or abs(other.box.x0 - cell.box.x1) > 3:
            continue
        if y_overlap(other.box, cell.box) < 0.6 * min(cell.box.h, other.box.h):
            continue
        if best is None or abs(other.box.top - cell.box.top) < abs(best.box.top - cell.box.top):
            best = other
    return best


def below_neighbor(cell: Cell, cells: list[Cell]) -> Cell | None:
    best = None
    for other in cells:
        if other is cell or abs(other.box.top - cell.box.bottom) > 3:
            continue
        if x_overlap(other.box, cell.box) < 0.8 * min(cell.box.w, other.box.w):
            continue
        if best is None or abs(other.box.x0 - cell.box.x0) < abs(best.box.x0 - cell.box.x0):
            best = other
    return best


def left_context(cell: Cell, cells: list[Cell], depth: int = 0) -> str:
    """Section headers to the left that span more rows (신청인 | 성명 | ...), outermost first."""
    for other in cells:
        if other is cell or abs(other.box.x1 - cell.box.x0) > 3:
            continue
        if y_overlap(other.box, cell.box) >= 0.9 * cell.box.h and other.box.h > cell.box.h * 1.2:
            text = clean_label(other.text.split("\n※")[0].replace("\n", " "))
            text = re.sub(r"\d\)$", "", text).strip()
            if text and len(compact(text)) <= 14 and not is_placeholder(text):
                outer = left_context(other, cells, depth + 1) if depth < 2 else ""
                return f"{outer} {text}".strip()
    return ""


def gaps(cell: Cell, toks: list[dict], min_gap: float = MIN_GAP) -> list[tuple[Box, str]]:
    """Writable spans inside a value cell, each paired with the printed token that ends it."""
    inner = Box(cell.box.x0 + PAD, cell.box.top + PAD, cell.box.x1 - PAD, cell.box.bottom - PAD)
    if not toks:
        return [(inner, "")]
    out: list[tuple[Box, str]] = []
    cursor = inner.x0
    for tok in sorted(toks, key=lambda t: t["x0"]):
        if tok["x0"] - cursor >= min_gap:
            out.append((Box(cursor, inner.top, tok["x0"] - 1, inner.bottom), compact(tok["text"])))
        cursor = max(cursor, tok["x1"] + 1)
    if inner.x1 - cursor >= min_gap:
        out.append((Box(cursor, inner.top, inner.x1, inner.bottom), ""))
    return out


def stamp_box(tok: dict, cell_box: Box | None) -> Box:
    """Covers a printed (인) mark: a seal is pressed over it. Kept inside its cell."""
    h = max(tok["bottom"] - tok["top"], 8)
    side = h * 2.2
    cx, cy = (tok["x0"] + tok["x1"]) / 2, (tok["top"] + tok["bottom"]) / 2
    half_w = max(tok["x1"] - tok["x0"] + 6, side) / 2
    b = Box(cx - half_w, cy - side / 2, cx + half_w, cy + side / 2)
    if cell_box:
        b = Box(max(b.x0, cell_box.x0 + PAD), max(b.top, cell_box.top + PAD), min(b.x1, cell_box.x1 - PAD), min(b.bottom, cell_box.bottom - PAD))
    return b


def signature_box(tok: dict, chars: list[dict], bounds: Box) -> Box:
    """Beside a printed (서명)/(서명 또는 인) mark, never over it or any other printed glyph: a
    drawn signature crossing the printed words leaves both unreadable. Right of the mark up to
    the first glyph, the cell edge or the page's text margin (bounds); when that is too narrow,
    the right half of the free space left of it, so the name box before it keeps the rest.
    Same size as the mark's old box, cut back from the lines above and below."""
    h = max(tok["bottom"] - tok["top"], 8)
    side = h * 2.2
    want = max(tok["x1"] - tok["x0"] + 6, side)
    cy = (tok["top"] + tok["bottom"]) / 2
    ink = [c for c in chars if not c["text"].isspace()
           and not (tok["x0"] - 0.5 <= (c["x0"] + c["x1"]) / 2 <= tok["x1"] + 0.5 and tok["top"] - 1 <= (c["top"] + c["bottom"]) / 2 <= tok["bottom"] + 1)]
    on_line = [c for c in ink if c["bottom"] > tok["top"] + 1 and c["top"] < tok["bottom"] - 1]
    right0 = tok["x1"] + 1.5
    right1 = min([c["x0"] for c in on_line if c["x0"] >= tok["x1"] - 0.5] + [bounds.x1]) - 1.5
    left1 = tok["x0"] - 1.5
    left0 = max([c["x1"] for c in on_line if c["x1"] <= tok["x0"] + 0.5] + [bounds.x0]) + 1.5
    # Left of the mark the name box keeps at least half the room, and never less than its own
    # minimum width (NAME_MIN in line_fields/colon_lines plus the gaps around it).
    left_room = (left1 - left0) - max((left1 - left0) / 2, NAME_MIN + 4)
    if right1 - right0 >= side or right1 - right0 >= left_room:
        x0, x1 = right0, right0 + min(want, right1 - right0)
    else:
        x0, x1 = left1 - min(want, left_room), left1
    top, bottom = max(cy - side / 2, bounds.top), min(cy + side / 2, bounds.bottom)
    for c in ink:
        if c in on_line or c["x1"] <= x0 or c["x0"] >= x1:
            continue
        if (c["top"] + c["bottom"]) / 2 < cy:
            top = max(top, c["bottom"] + 0.5)
        else:
            bottom = min(bottom, c["top"] - 0.5)
    return Box(x0, top, x1, bottom)


def mark_box(kind: str, label: str, tok: dict, chars: list[dict], cell_box: Box | None, frame: Box) -> Box:
    """The box for a printed signing mark: a seal over (인), a signature beside (서명 …).
    Another person's seal becomes a signature box (enforce_invariants), so it sits beside
    the mark like one."""
    if kind == "stamp" and not is_other_person(label):
        return stamp_box(tok, cell_box)
    bounds = Box(cell_box.x0 + PAD, cell_box.top + PAD, cell_box.x1 - PAD, cell_box.bottom - PAD) if cell_box else frame
    return signature_box(tok, chars, bounds)


def text_frame(page, tok: dict) -> Box:
    """Open-page bounds for a box beside a token: the page's content margins (printed text and
    ruled lines), or the nearest drawn vertical rule on either side that runs past the token's
    line."""
    ink = [c for c in page.chars if not c["text"].isspace()] + list(page.edges)
    left = min((c["x0"] for c in ink), default=0.0)
    right = max((c["x1"] for c in ink), default=float(page.width))
    for e in page.edges:
        if e["orientation"] == "v" and e["top"] <= tok["top"] and e["bottom"] >= tok["bottom"]:
            if e["x0"] >= tok["x1"]:
                right = min(right, e["x0"] - PAD)
            elif e["x0"] <= tok["x0"]:
                left = max(left, e["x0"] + PAD)
    return Box(left, 0.0, right, float(page.height))


def tok_box(tok: dict) -> Box:
    return Box(tok["x0"], tok["top"], tok["x1"], tok["bottom"])


def sign_kind(token_text: str) -> str:
    return "signature" if "서명" in compact(token_text) else "stamp"


def sign_label(head: str, kind: str) -> str:
    return f"{head} {'서명' if kind == 'signature' else '(인)'}"


def date_part_of(k: int, toks: list[dict], span: Box) -> str | None:
    """The DATE_PARTS value of the k-th blank of a 년/월/일 (or '.') date. A year blank right
    after a printed '20' ('20  년') takes the last two digits (year2); after any other printed
    digits it is not a whole year blank, so it gets no part."""
    if k:
        return ("month", "day")[k - 1]
    before = [compact(t["text"]) for t in toks if span.x0 - 4 <= t["x1"] <= span.x0 + 1.5]
    if "20" in before:
        return "year2"
    return None if any(re.fullmatch(r"\d+", b) for b in before) else "year4"


def one_date(toks: list[dict]) -> bool:
    """One date on the line: each of 년/월/일 (or three dots) once, and no '~' period."""
    marks = [compact(t["text"]) for t in toks]
    if any(m in ("~", "～") for m in marks):
        return False
    units = [re.sub(r"^\d+", "", m) for m in marks if re.fullmatch(r"\d*년|월|일", m)]
    if units:
        return sorted(units) == sorted(["년", "월", "일"])
    return marks.count(".") in (2, 3)


def value_fields(page_index: int, label: str, cell: Cell) -> list[Proposal]:
    if any(ch["text"] in CHECK_GLYPHS + CIRCLED + "○×" for ch in cell.chars):
        return []  # a choice cell: its options become checkboxes, not a text box
    toks = tokens(cell.chars)
    if len(group_lines(toks)) > 2:
        return []  # several printed lines: an instruction block, not an input
    content = [t for t in toks if not is_placeholder(t["text"])]
    if any(HINT.search(compact(t["text"])) for t in content):
        return []  # 예시/참조 text: the cell is explained, not blank
    spans = gaps(cell, toks)
    out: list[Proposal] = []
    if content and all(compact(t["text"]) in SUFFIXES for t in content):
        # '_____ 대학교 _____ 학과 ___ 학년': the word after each blank names it.
        for span, end in spans:
            if end in SUFFIXES:
                name = SUFFIXES[end]
                # An amount or count is not profile data: only school words keep a link.
                linked = name in ("학교명", "학과", "학부", "전공", "학년", "학번")
                out.append(make(page_index, span, f"{label} {name}" if name not in label else label, note="blank before printed unit", part=not linked))
        return out
    if content:
        # Printed prefix such as '서울특별시 동대문구' or '010-': the free span that is left.
        free = [s for s, _ in spans]
        big = max(free, key=lambda s: s.w) if free else None
        prefix_len = len(compact("".join(t["text"] for t in content)))
        if big and prefix_len <= 12 and big.w >= 60 and big.w >= 0.4 * cell.box.w and not any(x_overlap(big, Box(t["x0"], t["top"], t["x1"], t["bottom"])) for t in content):
            return [make(page_index, big, label, note="after printed prefix")]
        return []
    sign_toks = [t for t in toks if is_sign(t["text"])]
    date_marks = [t for t in toks if compact(t["text"]) in ("년", "월", "일")]
    dots = [t for t in toks if compact(t["text"]) == "."]
    if len(date_marks) >= 2:
        # Parts of the date the label names; make() links them only when that is the application date.
        parts = {"년": "연도", "월": "월", "일": "일"}
        single = one_date(toks)
        for span, end in spans:
            if end in parts:
                part = date_part_of("년월일".index(end), toks, span) if single else None
                out.append(make(page_index, span, f"{label} {parts[end]}", "text", "date part before printed 년/월/일", part=True, date_part=part))
        return out
    if len(dots) >= 2:
        # '20 .  .  . ~ 20 .  .  .': year/month/day blanks, start and end when there is a '~'.
        tilde = next((t for t in toks if compact(t["text"]) in ("~", "～")), None)
        spans = gaps(cell, toks, 6)  # two-digit year blanks are narrow
        order = ["연도", "월", "일"]
        for span, end in spans:
            if end != ".":
                continue
            side_end = tilde is not None and span.x0 > tilde["x0"]
            side_dots = [t for t in dots if (tilde is None) or ((t["x0"] > tilde["x0"]) == side_end)]
            k = next((i for i, t in enumerate(side_dots) if t["x0"] >= span.x1), None)
            if k is None or k > 2:
                continue
            side = ("종료 " if side_end else "시작 ") if tilde is not None else ""
            # A range (시작/종료) is a period, never the application date.
            part = date_part_of(k, toks, span) if one_date(toks) else None
            out.append(make(page_index, span, f"{label} {side}{order[k]}", "text", "dotted date", part=True, date_part=part))
        return out
    for t in sign_toks:
        kind = sign_kind(t["text"])
        p = make(page_index, mark_box(kind, label, t, cell.chars, cell.box, cell.box), sign_label(label, kind), kind)
        p.mark = tok_box(t)
        out.append(p)
    writable = spans
    if sign_toks:
        # The name goes left of the mark, clear of the signature box.
        limit = min([p.box.x0 for p in out] + [t["x0"] for t in sign_toks]) - 1
        writable = [(Box(s.x0, s.top, min(s.x1, limit), s.bottom), e) for s, e in spans if s.x0 < limit - MIN_GAP]
    if not writable:
        return out
    if len(writable) > 1 and any(e in ("-", "@", "~", "～") for _, e in writable):
        for i, (span, _) in enumerate(writable, 1):
            out.append(make(page_index, span, f"{label} ({i})", "text", "split by printed separator", part=True))
        return out
    span = max((s for s, _ in writable), key=lambda s: s.w)
    out.append(make(page_index, span, label))
    return out


def colon_lines(page, page_index: int, used: list[Box], cells: list[Cell]) -> list[Proposal]:
    """'성    명 :' followed by blank space, inside one cell or on the open page. A short line
    above such a group in the same cell ('임 대 인(확인)') names whose fields they are."""
    out: list[Proposal] = []
    free = [ch for ch in page.chars if not any(b.x0 - 0.5 <= ch["x0"] and ch["x1"] <= b.x1 + 0.5 and b.top - 0.5 <= ch["top"] and ch["bottom"] <= b.bottom + 0.5 for b in used)]
    right_margin = max((ch["x1"] for ch in page.chars), default=page.width)

    groups: dict[int, tuple[Cell | None, list]] = {}
    for ch in free:
        cell = home_cell(ch, cells)
        groups.setdefault(id(cell), (cell, []))[1].append(ch)
    for cell, chars in groups.values():
        outer = (left_context(cell, cells) or left_heading(cell, cells)) if cell else ""
        section, last_bottom = "", None
        segments: list[tuple[list[dict], float | None]] = []
        for whole in group_lines(tokens(chars)):
            # '○ 성 명 :        ○ 생년월일 :' holds two questions; each blank ends at the next bullet.
            seg: list[dict] = []
            for t in whole:
                if compact(t["text"]) in ("○", "◦", "ㅇ") and seg:
                    segments.append((seg, t["x0"] - 4))
                    seg = []
                seg.append(t)
            segments.append((seg, None))
        for line, stop_x in segments:
            line = [t for t in line if compact(t["text"]) not in ("○", "◦", "ㅇ")]
            if not line:
                continue
            texts = [compact(t["text"]) for t in line]
            joined = "".join(texts)
            top, bottom = min(t["top"] for t in line), max(t["bottom"] for t in line)
            if last_bottom is not None and top - last_bottom > 40:
                section = ""
            last_bottom = bottom
            colon_at = next((i for i, t in enumerate(texts) if t.endswith(":") or t.endswith("：")), None)
            if colon_at is None:
                if 2 <= len(joined) <= 14 and not re.search(r"\d|니다|[.]$|^[(*※]|[-~]", joined) and not re.fullmatch(r"[년월일.]+", joined):
                    section = clean_label(" ".join(t["text"] for t in line))
                continue
            head_toks = line[:colon_at + 1]
            dated, head_toks = split_date_head(head_toks)
            head = clean_label(" ".join(t["text"] for t in head_toks))
            if dated:
                # '20 년 월 일  신청인 성명 :': the date the person named after it writes on.
                out.extend(date_parts(page_index, dated, top, bottom, applicant=not is_other_person(f"{outer} {head}")))
            if not head or not (is_label_text(head) or is_short_label(head)) or len(head_toks) > 6:
                continue
            rest = line[colon_at + 1:]
            x0 = line[colon_at]["x1"] + 2
            if rest:
                x1 = rest[0]["x0"] - 2
            elif cell:
                x1 = cell.box.x1 - PAD
            else:
                # Open page: stop at the frame's vertical rule, if one runs past this line.
                walls = [e["x0"] for e in page.edges if e["orientation"] == "v" and e["x0"] > x0 + 20 and e["top"] <= top and e["bottom"] >= bottom]
                x1 = (min(walls) - PAD) if walls else right_margin
            if stop_x is not None:
                x1 = min(x1, stop_x)
            sign = next((t for t in rest if is_sign(t["text"])), None)
            unit = rest[0] if rest and compact(rest[0]["text"]) in SUFFIXES and len(rest) == 1 else None
            if rest and not sign and not unit and not all(is_placeholder(t["text"]) for t in rest):
                continue
            label = f"{section} {head}".strip() if section and compact(section) not in compact(head) else head
            if outer and compact(outer) not in compact(label):
                label = f"{outer} {label}"
            h = bottom - top
            if sign:
                kind = sign_kind(sign["text"])
                sb = mark_box(kind, label, sign, page.chars, cell.box if cell else None, text_frame(page, sign))
                p = make(page_index, sb, sign_label(label, kind), kind)
                p.mark = tok_box(sign)
                out.append(p)
                x1 = min(x1, sb.x0, sign["x0"]) - 1
            if x1 - x0 >= NAME_MIN:
                out.append(make(page_index, Box(x0, top - h * 0.35, x1, bottom + h * 0.35), label, note="after colon"))
    return out


def date_parts(page_index: int, toks: list[dict], top: float, bottom: float, label: str = "작성일", applicant: bool = False) -> list[Proposal]:
    """Blanks before printed 년/월/일 on one line ('20  년  월  일', '2026년  월  일'). The caller
    says from the page structure whether this is the applicant's own writing date (a date line
    on a signing line, or right above the applicant's signing line); only then do the parts link
    to application_date."""
    out, prev_x, h = [], None, bottom - top
    linked = applicant and one_date(toks)
    for t in toks:
        c = compact(t["text"])
        if c.endswith("년") or c in ("월", "일"):
            end = c[-1]
            if c == end:
                gap_x0 = (prev_x if prev_x is not None else t["x0"] - 40) + 1
                if t["x0"] - 1 - gap_x0 >= 8:
                    box = Box(gap_x0, top - h * 0.3, t["x0"] - 1, bottom + h * 0.3)
                    name = {"년": "연도", "월": "월", "일": "일"}[end]
                    part = date_part_of("년월일".index(end), toks, box) if linked else None
                    out.append(make(page_index, box, f"{label} {name}", "text", "date line", part=True, date_part=part))
        prev_x = t["x1"]
    return out


def signs_next(page_lines: list[list[dict]], bottom: float) -> bool:
    """The next printed line below a bare date line (ending at `bottom`, within about five
    lines) is the applicant's own signing line: the date is the applicant's writing date
    (작성일). page_lines are the page's lines with nothing removed, so a signing line whose
    name and mark already have boxes still counts."""
    below = next((l for l in page_lines if l[0]["top"] > bottom - 1), None)
    if below is None or below[0]["top"] - bottom > 80:
        return False
    at = next((i for i, t in enumerate(below) if is_sign(t["text"])), None)
    return at is not None and not is_other_person(" ".join(t["text"] for t in below[:at]))


def split_date_head(toks: list[dict]) -> tuple[list[dict], list[dict]]:
    """'20 년 월 일 신청인 성명' -> (date tokens, the rest)."""
    texts = [compact(t["text"]) for t in toks]
    if "일" in texts and ("월" in texts):
        k = len(texts) - 1 - texts[::-1].index("일")
        if re.fullmatch(r"(20\d{0,2})?년월일", "".join(texts[:k + 1])):
            return toks[:k + 1], toks[k + 1:]
    return [], toks


def line_fields(page, page_index: int, used: list[Box]) -> list[Proposal]:
    """Free text outside value cells: 'YYYY년 MM월 DD일' lines and '신청인 ...... (서명 또는 인)'."""
    out: list[Proposal] = []
    free = [ch for ch in page.chars if not any(b.x0 - 0.5 <= ch["x0"] and ch["x1"] <= b.x1 + 0.5 and b.top - 0.5 <= ch["top"] and ch["bottom"] <= b.bottom + 0.5 for b in used)]
    page_lines = group_lines(tokens(page.chars))
    for line in group_lines(tokens(free)):
        texts = [compact(t["text"]) for t in line]
        joined = "".join(texts)
        top, bottom = min(t["top"] for t in line), max(t["bottom"] for t in line)
        h = bottom - top
        # Date line: '20  년   월   일' or '2026년  월  일' with nothing else on it.
        if re.fullmatch(r"(20\d{0,2})?년월일\.?", joined):
            out.extend(date_parts(page_index, line, top, bottom, applicant=signs_next(page_lines, bottom)))
            continue
        # Dotted date line: '   .    .    .' or '20  .   .   .' alone on its line.
        if re.fullmatch(r"(20\d{0,2})?\.\.\.?", joined) and len(line) <= 4:
            linked = signs_next(page_lines, bottom)
            for k, t in enumerate(x for x in line if compact(x["text"]) == "."):
                if k > 2:
                    break
                before = [x for x in line if x["x0"] < t["x0"]]
                start = (max(x["x1"] for x in before) + 1) if before else t["x0"] - 45
                if t["x0"] - 1 - start >= 8:
                    box = Box(start, top - h * 0.8, t["x0"] - 1, bottom + h * 0.8)
                    out.append(make(page_index, box, f"작성일 {('연도', '월', '일')[k]}", "text", "dotted date line", part=True,
                                    date_part=date_part_of(k, line, box) if linked else None))
            continue
        # Signature line: '<label> ...... (서명 또는 인)'.
        for i, t in enumerate(line):
            if not is_sign(texts[i]):
                continue
            dated, before = split_date_head(line[:i])
            # The label is the run of tokens after the last wide gap.
            start = 0
            for k in range(1, len(before)):
                if before[k]["x0"] - before[k - 1]["x1"] > 3 * before[k]["size"]:
                    start = k
            label_toks = [x for x in before[start:] if compact(x["text"]) not in (":", "：")]
            head = clean_label(" ".join(x["text"] for x in label_toks)) if label_toks else ""
            if dated:
                # '20 년 월 일  신청인 (서명)': the date the person signing here writes on.
                out.extend(date_parts(page_index, dated, top, bottom, applicant=not is_other_person(head)))
            if head and (len(compact(head)) > 12 or re.search(r"니다|[.]", head)):
                continue  # a sentence that mentions (서명): not a signing line
            head = head or "신청인"
            kind = sign_kind(t["text"])
            sb = mark_box(kind, head, t, page.chars, None, text_frame(page, t))
            p = make(page_index, sb, sign_label(head, kind), kind)
            p.mark = tok_box(t)
            out.append(p)
            if before:
                gap_x0 = before[-1]["x1"] + 2
                x1 = min(sb.x0, t["x0"]) - 1
                if x1 - gap_x0 >= NAME_MIN:
                    out.append(make(page_index, Box(gap_x0, top - h * 0.3, x1, bottom + h * 0.3), head, note="name before mark"))
    return out


def checkbox_fields(page, page_index: int, cells: list[Cell]) -> list[Proposal]:
    """A box over each printed check glyph (□, '[ ]', and ○ when a line offers several
    short options). Its label is the nearer neighbouring text."""
    out: list[Proposal] = []
    chars = sorted(page.chars, key=lambda c: (c["top"], c["x0"]))

    def label_near(x0: float, x1: float, top: float, stop: str) -> str:
        probe = {"x0": x0, "x1": x1, "top": top, "bottom": top + 1}
        cell = home_cell(probe, cells)
        line = sorted((ch for ch in chars if abs(ch["top"] - top) < 3 and not ch["text"].isspace() and home_cell(ch, cells) is cell), key=lambda c: c["x0"])
        after, prev = [], x1
        for ch in (c for c in line if c["x0"] >= x1 - 0.5):
            if ch["text"] in stop or ch["x0"] - prev > 2.5 * ch["size"]:
                break
            after.append(ch)
            prev = ch["x1"]
        before, prev = [], x0
        for ch in reversed([c for c in line if c["x1"] <= x0 + 0.5]):
            if ch["text"] in stop or prev - ch["x1"] > 2.5 * ch["size"] or ch["text"] in "(:：":
                break
            before.insert(0, ch)
            prev = ch["x0"]
        clean = lambda ws: clean_label(text_of(ws).strip(" ,)(:："))
        gap_after = after[0]["x0"] - x1 if after and clean(after) else 1e9
        gap_before = x0 - before[-1]["x1"] if before and clean(before) else 1e9
        if gap_after == gap_before == 1e9:
            return ""
        words = before if gap_before < gap_after * 0.8 else after
        option = clean(words)[:40]
        # What the options answer: the text before the first option on that line.
        first = min((c["x0"] for c in line if c["text"] in stop or c["text"] in CHECK_GLYPHS), default=x0)
        head = [c for c in line if c["x1"] <= min(first, x0) + 0.5]
        head_text = clean_label(re.split(r"[:：]", text_of(head))[0].strip(" (☞▶※"))
        if any(g in head_text for g in CHECK_GLYPHS):
            head_text = ""
        if not head_text and cell is not None:
            # Options alone in their cell answer the heading cell to the left.
            left = next((o for o in cells if o is not cell and abs(o.box.x1 - cell.box.x0) <= 3 and y_overlap(o.box, cell.box) >= 0.6 * cell.box.h and o.chars), None)
            if left is not None:
                head_text = clean_label(left.text.replace("\n", " ").strip(" ▶※"))
                head_text = head_text if len(compact(head_text)) <= 30 else ""
        if not head_text and cell is not None:
            # Options in a column answer the heading cell above them.
            above = next((o for o in cells if o is not cell and abs(o.box.bottom - cell.box.top) <= 3 and x_overlap(o.box, cell.box) >= 0.8 * cell.box.w and o.chars), None)
            if above is not None and not any(g in above.text for g in CHECK_GLYPHS + CIRCLED) and len(compact(above.text)) <= 20:
                head_text = clean_label(above.text.replace("\n", " "))
        if words is before and not re.search(r"[:：]", text_of(head)):
            return option  # 'A □   B □': without a 'question :' the text before is just the previous option
        if head_text and 2 <= len(compact(head_text)) <= 24 and compact(head_text) != compact(option):
            return f"{head_text}: {option}"[:80]
        return option

    def add(x0, top, x1, bottom, label):
        side = max(x1 - x0, bottom - top, 7)
        cx, cy = (x0 + x1) / 2, (top + bottom) / 2
        out.append(make(page_index, Box(cx - side / 2, cy - side / 2, cx + side / 2, cy + side / 2), label or "선택", "checkbox"))

    for g in (c for c in chars if c["text"] in CHECK_GLYPHS):
        label = label_near(g["x0"], g["x1"], g["top"], CHECK_GLYPHS + "[]○")
        if re.search(r"^에는|√\s*표|표를 합니다|해당되는 곳에", label):
            continue  # the legend explaining how to mark, not a choice
        add(g["x0"], g["top"], g["x1"], g["bottom"], label)
    # '[ ] 서면' brackets
    for a in (c for c in chars if c["text"] == "["):
        b = next((c for c in chars if c["text"] == "]" and abs(c["top"] - a["top"]) < 3 and 0 < c["x0"] - a["x1"] < 30), None)
        if b and not any(c for c in chars if abs(c["top"] - a["top"]) < 3 and a["x1"] < c["x0"] < b["x0"] and not c["text"].isspace()):
            add(a["x0"], a["top"], b["x1"], b["bottom"], label_near(a["x0"], b["x1"], a["top"], "[]" + CHECK_GLYPHS))
    # '(○, ×)': mark one. A box over each sign.
    pairs = set()
    for o in (c for c in chars if c["text"] == "○"):
        x = next((c for c in chars if c["text"] in "×Ｘ" and abs(c["top"] - o["top"]) < 3 and 0 < c["x0"] - o["x1"] < 25), None)
        if x is None:
            continue
        pairs.add(id(o))
        cell = home_cell(o, cells)
        head = left_heading(cell, cells) if cell else ""
        if cell is not None and not head:
            head = clean_label(text_of([c for c in chars if home_cell(c, cells) is cell and c["x1"] <= o["x0"] and abs(c["top"] - o["top"]) < 3]).strip(" ("))
        for g, word in ((o, "○(예)"), (x, "×(아니오)")):
            add(g["x0"], g["top"], g["x1"], g["bottom"], f"{head}: {word}" if head else word)
    # '① 해당  ② 해당없음': numbered options inside one cell, each with a short label.
    numbered = [c for c in chars if c["text"] in CIRCLED]
    for g in numbered:
        cell = home_cell(g, cells)
        peers = [c for c in numbered if abs(c["top"] - g["top"]) < 3 and home_cell(c, cells) is cell]
        if cell is None or len(peers) < 2 or len(compact(cell.text)) > 30:
            continue
        label = label_near(g["x0"], g["x1"], g["top"], CIRCLED + CHECK_GLYPHS)
        if label and len(compact(label.split(": ")[-1])) <= 10:
            add(g["x0"], g["top"], g["x1"], g["bottom"], label)
    # ○ radio options: two or more on a line, each with a short label.
    circles = [c for c in chars if c["text"] == "○" and id(c) not in pairs]
    for g in circles:
        nxt = min((c["x0"] for c in circles if abs(c["top"] - g["top"]) < 3 and c["x0"] > g["x1"]), default=1e9)
        if any(c["text"] in ":：" for c in chars if abs(c["top"] - g["top"]) < 3 and g["x1"] < c["x0"] < nxt):
            continue  # '○ 성명 :' is a bullet before a question, not an option
        peers = [c for c in circles if abs(c["top"] - g["top"]) < 3]
        label = label_near(g["x0"], g["x1"], g["top"], "○" + CHECK_GLYPHS)
        if len(peers) >= 2 and label and len(compact(label.split(": ")[-1])) <= 10:
            add(g["x0"], g["top"], g["x1"], g["bottom"], label)
    return out


def inline_fields(page_index: int, cells: list[Cell], used: list[Box]) -> list[Proposal]:
    """A label printed at the left of the very cell it asks for: '성 명(외국인의 경우 영문)      '."""
    out: list[Proposal] = []
    for cell in cells:
        if not cell.chars or any(x_overlap(cell.box, b) * y_overlap(cell.box, b) > 1 for b in used):
            continue
        toks = tokens(cell.chars)
        if len(group_lines(toks)) != 1:
            continue
        x_first, x_last = min(t["x0"] for t in toks), max(t["x1"] for t in toks)
        free = cell.box.x1 - PAD - (x_last + 2)
        if x_first > cell.box.x0 + 0.15 * cell.box.w or free < 60 or free < 0.35 * cell.box.w:
            continue
        text = clean_label(" ".join(t["text"] for t in toks))
        core = re.sub(r"\(.*?\)", "", compact(text))
        if len(core) > 12:
            continue
        if not (classify(core) or GENERIC_LABELS.match(core) or is_short_label(core)) or re.search(r"니다|[.]$|[:：,、]|.{3,}서$|(까지|부터|등)$", text) or HINT.search(text):
            continue
        if ADMIN_ONLY.search(compact(text)):
            continue
        if any(abs(o.box.x1 - cell.box.x0) <= 3 and y_overlap(o.box, cell.box) > 0.6 * cell.box.h and (is_label_text(o.text) or is_short_label(o.text.replace("\n", " "))) for o in cells):
            continue  # a value cell (printed value or example) beside its label
        right = right_neighbor(cell, cells)
        if right is not None and any(not is_placeholder(t["text"]) for t in tokens(right.chars)):
            continue  # the answer is already printed in the next cell
        out.append(make(page_index, Box(x_last + 2, cell.box.top + PAD, cell.box.x1 - PAD, cell.box.bottom - PAD), text, note="label inside its own cell"))
    return out


def essay_fields(page_index: int, cells: list[Cell], used: list[Box], page_width: float) -> list[Proposal]:
    """A wide cell holding a question ('…기술하시오.') with blank space under it: a free-text answer box."""
    out: list[Proposal] = []
    for cell in cells:
        if cell.box.w < 0.5 * page_width or not cell.chars or any(x_overlap(cell.box, b) * y_overlap(cell.box, b) > 0 for b in used):
            continue
        text = clean_label(cell.text.replace("\n", " "))
        if not re.search(r"(시오|하세요|십시오|주세요|기술|작성)\.?\s*$|\?$", text) or len(group_lines(tokens(cell.chars))) > 3:
            continue
        text_bottom = max(ch["bottom"] for ch in cell.chars)
        free = Box(cell.box.x0 + PAD, text_bottom + 3, cell.box.x1 - PAD, cell.box.bottom - PAD)
        if free.h >= 40:
            label = re.sub(r"^\d+[.)]\s*", "", text)
            out.append(make(page_index, free, label, "multiline", "answer under question"))
    return out


def propose_page(page, page_index: int) -> list[Proposal]:
    cells = cells_of(page)
    proposals: list[Proposal] = []
    used: list[Box] = []
    for cell in cells:
        text = cell.text.replace("\n", " ")
        known = is_label_text(text)
        if not known and not is_short_label(text):
            continue
        if ADMIN_ONLY.search(compact(text)):
            continue
        label = clean_label(text)
        context = left_context(cell, cells)
        if context and compact(context) not in compact(label):
            label = f"{context} {label}"
        target = right_neighbor(cell, cells)
        found: list[Proposal] = []
        blankish = target is not None and all(is_placeholder(t["text"]) or compact(t["text"]) in SUFFIXES for t in tokens(target.chars))
        if target and not is_label_text(target.text) and (known or blankish):
            # One row label over several blank columns (3 months, several items): a box per column.
            row = [target]
            while True:
                nxt = right_neighbor(row[-1], cells)
                if nxt is None or abs(nxt.box.top - target.box.top) > 2 or abs(nxt.box.bottom - target.box.bottom) > 2:
                    break
                if not all(is_placeholder(t["text"]) or compact(t["text"]) in SUFFIXES for t in tokens(nxt.chars)) or compact(nxt.text) != compact(target.text):
                    break
                row.append(nxt)
            if len(row) > 1:
                for n, c in enumerate(row, 1):
                    found.extend(value_fields(page_index, f"{label} ({n})", c))
                    used.append(c.box)
            else:
                found = value_fields(page_index, label, target)
        if not found:
            below = below_neighbor(cell, cells)
            right_below = below_neighbor(target, cells) if target is not None else None
            right_is_heading = target is not None and (is_label_text(target.text) or is_short_label(target.text)
                                                       or (bool(target.chars) and len(compact(target.text)) <= 16 and right_below is not None and not right_below.chars))
            row_label = below is not None and any(abs(o.box.x1 - below.box.x0) <= 3 and y_overlap(o.box, below.box) > 0.6 * below.box.h and o.box.h <= 1.5 * below.box.h and (is_label_text(o.text) or is_short_label(o.text)) for o in cells)
            below_blank = below is not None and all(is_placeholder(t["text"]) or compact(t["text"]) in SUFFIXES for t in tokens(below.chars))
            if below and below_blank and not row_label and not is_label_text(below.text) and (right_is_heading or not target):
                # A heading row with blanks under it. If the blank repeats below, it is a list of
                # people (가족 구성): only the first row is proposed and it is never "self".
                again = below_neighbor(below, cells)
                again_labeled = again is not None and any(abs(o.box.x1 - again.box.x0) <= 3 and y_overlap(o.box, again.box) > 0.6 * again.box.h and o.chars for o in cells)
                if again and not again.chars and not again_labeled and abs(again.box.x0 - below.box.x0) < 3 and abs(again.box.x1 - below.box.x1) < 3 and not below.chars:
                    # Every blank row under the heading is one member.
                    member = label
                    rows, cur = [below], again
                    while cur is not None and not cur.chars and abs(cur.box.x0 - below.box.x0) < 3 and abs(cur.box.x1 - below.box.x1) < 3 and len(rows) < 12:
                        rows.append(cur)
                        cur = below_neighbor(cur, cells)
                    for n, c in enumerate(rows, 1):
                        for prop in value_fields(page_index, f"{member} ({n}행)", c):
                            # A list row may be anyone: never linked to the profile, never required.
                            if prop.mapping not in SENSITIVE:
                                prop.mapping = None
                            prop.required = False
                            found.append(prop)
                        used.append(c.box)
                else:
                    found = value_fields(page_index, label, below)
                target = below
        if found:
            proposals.extend(found)
            used.append(target.box)
            used.append(cell.box)
    proposals.extend(checkbox_fields(page, page_index, cells))
    proposals.extend(essay_fields(page_index, cells, used + [p.box for p in proposals], page.width))
    proposals.extend(inline_fields(page_index, cells, used + [p.box for p in proposals]))
    # A printed signing mark answered by a box beside it is claimed too, so no later pass
    # reads it again and makes a second box for it.
    taken = used + [p.box for p in proposals] + [p.mark for p in proposals if p.mark]
    proposals.extend(colon_lines(page, page_index, taken, cells))
    taken = taken + [p.box for p in proposals] + [p.mark for p in proposals if p.mark]
    proposals.extend(line_fields(page, page_index, taken))
    return dedupe(proposals)


def dedupe(props: list[Proposal]) -> list[Proposal]:
    """Drops a proposal that mostly covers an earlier one (two labels claiming one cell)."""
    kept: list[Proposal] = []
    for p in props:
        clash = False
        for k in kept:
            if k.page != p.page:
                continue
            inter = x_overlap(k.box, p.box) * y_overlap(k.box, p.box)
            if inter > 0.5 * min(k.box.w * k.box.h, p.box.w * p.box.h):
                clash = True
                break
        if not clash:
            kept.append(p)
    return kept


# ---------------------------------------------------------------------------
# Template assembly
# ---------------------------------------------------------------------------


def to_field(p: Proposal, crop: Box, index: int) -> dict:
    x = (p.box.x0 - crop.x0) / crop.w
    y = (p.box.top - crop.top) / crop.h
    w = p.box.w / crop.w
    h = p.box.h / crop.h
    x, y = max(0.0, x), max(0.0, y)
    w, h = min(w, 1 - x), min(h, 1 - y)
    out = {
        "id": f"p{p.page + 1}-f{index:02d}",
        "page": p.page,
        "rect": {"x": round(x, 4), "y": round(y, 4), "w": round(max(w, 0.005), 4), "h": round(max(h, 0.005), 4)},
        "type": p.type,
        "label": p.label[:80],
        "mapping": p.mapping,
        "required": p.required,
    }
    if p.format:
        out["format"] = p.format
    if p.date_part:
        out["part"] = p.date_part
    return out


def reading_key(p: Proposal):
    return (p.page, round(p.box.top / 4), p.box.x0)


def apply_overrides(fields: list[dict], overrides: dict) -> list[dict]:
    """Human corrections after looking at the overlay. Matched by page + label, not by id."""
    def match(f, m):
        same = f["label"] == m["label"] if "label" in m else bool(re.search(m["pattern"], f["label"]))
        near = ("near_y" not in m or abs(f["rect"]["y"] - m["near_y"]) < 0.008) and ("near_x" not in m or abs(f["rect"]["x"] - m["near_x"]) < 0.01)
        return f["page"] == m["page"] and same and near
    for rule in overrides.get("drop", []):
        fields = [f for f in fields if not match(f, rule)]
    for rule in overrides.get("drop_re", []):
        fields = [f for f in fields if not (f["page"] == rule["page"] and re.search(rule["pattern"], f["label"]))]
    for extra in overrides.get("add", []):
        fields.append(dict(extra))
    for grid in overrides.get("grid", []):
        fields.extend(grid_fields(grid))
    # Last, so a correction can also rename or retype an added or grid field.
    for rule in overrides.get("set", []):
        hits = [f for f in fields if match(f, rule["match"])]
        if not hits:
            raise SystemExit(f"override matched nothing: {rule['match']}")
        for f in hits:
            for k, v in rule["fields"].items():
                if v is None and k in ("format", "font_size", "part"):
                    f.pop(k, None)
                else:
                    f[k] = v
    return fields


def enforce_invariants(fields: list[dict]) -> list[dict]:
    """Re-applied after human overrides, so a correction cannot break the safety rules:
    another person's box never links to the user's profile (sensitive kinds stay so the
    app masks them), and their seal is a signature box because the app suggests the
    user's remembered name for any stamp it does not recognise as someone else's;
    a date format only on date boxes; labels stay plain text."""
    for f in fields:
        f["label"] = "".join(ch for ch in f["label"] if unicodedata.category(ch) not in ("Cc", "Cf", "Co", "Cs")).strip()
        if is_other_person(f["label"]):
            if f["type"] == "stamp":
                f["type"], f["mapping"] = "signature", "signature"
                f["label"] = re.sub(r"\s*\((인|날인)\)$", "", f["label"]) + " 서명(또는 날인)"
            elif f.get("mapping") not in SENSITIVE and f["type"] not in ("signature",):
                f["mapping"] = None
        if f["type"] != "date":
            f.pop("format", None)
        if f["type"] != "text":
            f.pop("part", None)
        if f.get("mapping") == "application_date" and classify(f["label"]) != "application_date":
            f["mapping"] = None  # a renamed date part ('동의일 연도') is not the application date
    return fields


def grid_fields(grid: dict) -> list[dict]:
    """A table of blank cells (family members, accounts) from measured column and row edges.
    {"page", "prefix", "columns": [[label, x0, x1], ...], "rows": [[name, y0, y1], ...]}.
    Rows are other people unless "self" is set: no profile link, only sensitive kinds kept."""
    out = []
    inset = grid.get("inset", 0.002)
    for row_name, y0, y1 in grid["rows"]:
        for col in grid["columns"]:
            col_label, x0, x1 = col[:3]
            ftype = col[3] if len(col) > 3 else "text"
            label = " ".join(p for p in (grid.get("prefix", ""), row_name, col_label) if p)[:80]
            kind = classify(label)
            mapping = kind if (kind in SENSITIVE or grid.get("self")) else None
            if ftype in ("signature", "stamp"):
                mapping = ftype
            elif mapping in ("signature", "stamp"):
                mapping = None
            out.append({"id": "g", "page": grid["page"], "rect": {"x": round(x0 + inset, 4), "y": round(y0 + inset, 4), "w": round(x1 - x0 - 2 * inset, 4), "h": round(y1 - y0 - 2 * inset, 4)},
                        "type": ftype, "label": label, "mapping": mapping, "required": False})
    return out


def propose(pdf_path: Path, overrides: dict | None = None) -> tuple[dict, list[Proposal]]:
    data = pdf_path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    with pdfplumber.open(pdf_path) as pdf:
        if any(p.rotation for p in pdf.pages):
            raise ValueError("rotated page: the app does not support rotated templates")
        # pdfplumber maps the crop box with the MediaBox height only; an offset
        # MediaBox origin would shift every rect, so such files are refused.
        if any(abs(float(v)) > 0.5 for p in pdf.pages for v in (resolve_all(p.page_obj.attrs.get("MediaBox")) or [0, 0])[:2]):
            raise ValueError("MediaBox does not start at (0, 0): box positions would shift")
        props: list[Proposal] = []
        crops: list[Box] = []
        pages = overrides.get("pages") if overrides else None
        for i, page in enumerate(pdf.pages):
            crops.append(Box(*page.cropbox))
            if pages is not None and i not in pages:
                continue
            props.extend(propose_page(page, i))
        props.sort(key=reading_key)
        fields = [to_field(p, crops[p.page], n) for n, p in enumerate(props, 1)]
        fields = enforce_invariants(apply_overrides(fields, overrides or {}))
        fields = renumber(fields)
        over = [f"{f['id']} '{f['label']}' covers '{text}'" for f in fields if (text := covered_text(pdf.pages[f["page"]], f, crops[f["page"]]))]
        if over:
            raise ValueError("box over printed text: " + "; ".join(over)[:300])
        template = {"version": 1, "content_hash": digest, "page_count": len(pdf.pages), "source": "admin", "fields": fields}
        template = add_groups(template, pdf_path)
    return template, props


def add_groups(template: dict, pdf_path: Path) -> dict:
    """Checkbox groups from the final boxes (overrides applied), so options a human added are
    grouped too: one answer for 동의함/동의안함 and for a 구분 row (scripts/forms/auto/groups.py)."""
    return checkbox_groups.into_template(template, checkbox_groups.build(template, pdf_path))


def covered_text(page, field: dict, crop: Box) -> str:
    """Printed letters and digits whose centre lies inside a box: the value or signature would
    be written over them and neither stays readable. Punctuation and symbols a box edge grazes
    do not count. A tick box sits on its printed □ and a seal on its printed (인) by design, so
    a checkbox is not checked and a stamp may cover its own mark."""
    if field["type"] == "checkbox":
        return ""
    r = field["rect"]
    x0, top = crop.x0 + r["x"] * crop.w, crop.top + r["y"] * crop.h
    x1, bottom = x0 + r["w"] * crop.w, top + r["h"] * crop.h
    marks = [tok_box(t) for t in tokens(page.chars) if is_sign(t["text"])] if field["type"] == "stamp" else []
    out = []
    for ch in page.chars:
        cx, cy = (ch["x0"] + ch["x1"]) / 2, (ch["top"] + ch["bottom"]) / 2
        if not unicodedata.category(ch["text"][:1] or " ").startswith(("L", "N")) or not (x0 < cx < x1 and top < cy < bottom):
            continue
        if any(m.x0 <= cx <= m.x1 and m.top <= cy <= m.bottom for m in marks):
            continue
        out.append(ch["text"])
    return "".join(out)


def renumber(fields: list[dict]) -> list[dict]:
    fields.sort(key=lambda f: (f["page"], round(f["rect"]["y"] * 200), f["rect"]["x"]))
    per_page: dict[int, int] = {}
    for f in fields:
        per_page[f["page"]] = per_page.get(f["page"], 0) + 1
        f["id"] = f"p{f['page'] + 1}-f{per_page[f['page']]:02d}"
    return fields


# ---------------------------------------------------------------------------
# Overlay rendering for human review
# ---------------------------------------------------------------------------

COLORS = {"text": (0, 102, 255), "multiline": (0, 160, 160), "date": (140, 60, 220), "checkbox": (0, 150, 60), "signature": (220, 40, 40), "stamp": (230, 120, 0)}
FONT_PATH = Path(__file__).resolve().parents[2] / "apps/mobile/assets/fonts/NanumGothic-Regular.ttf"


def render_overlay(pdf_path: Path, template: dict, out_dir: Path, slug: str, resolution: int = 110) -> list[Path]:
    """One PNG per page with fields: boxes on the page, id -> label legend on the right."""
    from PIL import Image, ImageDraw, ImageFont

    out_dir.mkdir(parents=True, exist_ok=True)
    font = ImageFont.truetype(str(FONT_PATH), 11) if FONT_PATH.exists() else ImageFont.load_default()
    small = ImageFont.truetype(str(FONT_PATH), 9) if FONT_PATH.exists() else font
    written = []
    with pdfplumber.open(pdf_path) as pdf:
        pages = sorted({f["page"] for f in template["fields"]})
        for i in pages:
            page = pdf.pages[i]
            rendered = page.crop(page.cropbox).to_image(resolution=resolution).original.convert("RGB")
            W, H = rendered.size
            img = Image.new("RGB", (W + 380, H), "white")
            img.paste(rendered, (0, 0))
            draw = ImageDraw.Draw(img, "RGBA")
            ly = 8
            for f in template["fields"]:
                if f["page"] != i:
                    continue
                r = f["rect"]
                box = [r["x"] * W, r["y"] * H, (r["x"] + r["w"]) * W, (r["y"] + r["h"]) * H]
                color = COLORS.get(f["type"], (0, 0, 0))
                sensitive = f.get("mapping") in SENSITIVE or classify(f["label"]) in SENSITIVE
                draw.rectangle(box, outline=color + (255,), width=2, fill=color + (45,))
                short = f["id"].split("-")[1][1:]
                draw.text((box[0] + 2, box[1] + 1), short, fill=(200, 0, 0) if sensitive else color, font=small)
                tag = f"{short} [{f['type']}] {f['label']}" + (f" → {f['mapping']}" if f.get("mapping") else "") + (" !민감" if sensitive else "") + (" *" if f.get("required") else "")
                draw.text((W + 8, ly), tag[:52], fill=(200, 0, 0) if sensitive else color, font=font)
                ly += 15
            path = out_dir / f"{slug}-p{i + 1}.png"
            img.save(path)
            written.append(path)
    return written


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def sensitive_fields(template: dict) -> list[str]:
    return [f["label"] for f in template["fields"] if f.get("mapping") in SENSITIVE or classify(f["label"]) in SENSITIVE]


def cmd_propose(args) -> None:
    overrides = json.loads(Path(args.overrides).read_text()) if args.overrides else None
    template, _ = propose(Path(args.pdf), overrides)
    text = json.dumps(template, ensure_ascii=False, indent=2) + "\n"
    if args.out:
        Path(args.out).write_text(text)
    else:
        sys.stdout.write(text)
    if args.overlay:
        for p in render_overlay(Path(args.pdf), template, Path(args.overlay), Path(args.pdf).stem):
            print(p, file=sys.stderr)


HWP_REJECTED = "HWP/HWPX 변환본이 원본과 다름(conversions.json): "
HWP_REASON = ("HWP/HWPX 원본. 변환 판정 없음: scripts/forms/hwp로 변환하고 conversions.json에 "
              "판정(faithful/rejected)을 기록한 뒤 --conversions/--derived로 다시 실행한다")


def cmd_batch(args) -> None:
    sources = json.loads(Path(args.sources).read_text())
    conversions = json.loads(Path(args.conversions).read_text()) if args.conversions else {}
    if conversions and not args.derived:
        raise SystemExit("--conversions needs --derived (the directory of converted PDFs)")
    raw, out, review = Path(args.raw), Path(args.out), Path(args.review)
    out.mkdir(parents=True, exist_ok=True)
    previous = {}
    manifest_path = out / "manifest.json"
    if manifest_path.exists():
        previous = {e["slug"]: e for e in json.loads(manifest_path.read_text()).get("forms", [])}
    entries = []
    for src in sources:
        slug = src["slug"]
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", slug):
            raise SystemExit(f"bad slug {slug!r}: lowercase letters, digits and '-' only")
        entry = {k: src.get(k) for k in ("slug", "program", "agency", "notice_url", "file_url", "retrieved_at", "format", "sha256", "byte_size")}
        ov_path = Path(args.overrides) / f"{slug}.json" if args.overrides else None
        overrides = json.loads(ov_path.read_text()) if ov_path and ov_path.exists() else None
        entry["notes"] = " / ".join(n for n in ((overrides or {}).get("note"), src.get("notes")) if n)
        if overrides and overrides.get("skip"):
            entry.update(status="skipped", reason=overrides["skip"], template=None, field_count=0, sensitive_fields=[])
            entries.append(entry)
            continue
        derived = None
        if src.get("format") != "pdf":
            conversion = conversions.get("forms", {}).get(slug)
            if not str(src.get("format", "")).startswith("hwp") or not conversion:
                entry.update(status="hwp_unconverted" if str(src.get("format", "")).startswith("hwp") else "skipped",
                             reason=src.get("unconverted_reason") or HWP_REASON,
                             template=None, field_count=0, sensitive_fields=[])
                entries.append(entry)
                continue
            if conversion["source_sha256"] != src["sha256"]:
                raise SystemExit(f"{slug}: conversions.json judged other bytes than sources.json")
            if conversion["verdict"] != "faithful":
                entry.update(status="hwp_unfaithful", reason=HWP_REJECTED + conversion["reason"],
                             template=None, field_count=0, sensitive_fields=[])
                entries.append(entry)
                continue
            source_path = raw / f"{slug}.{src['format']}"
            if hashlib.sha256(source_path.read_bytes()).hexdigest() != src["sha256"]:
                raise SystemExit(f"{slug}: file hash differs from sources.json")
            pdf_path = Path(args.derived) / f"{slug}.pdf"
            if hashlib.sha256(pdf_path.read_bytes()).hexdigest() != conversion["pdf_sha256"]:
                raise SystemExit(f"{slug}: converted PDF differs from conversions.json; reconvert or re-judge")
            accepted = sorted(p["page"] - 1 for p in conversion["pages"])
            overrides = dict(overrides or {})
            if not set(overrides.setdefault("pages", accepted)) <= set(accepted):
                raise SystemExit(f"{slug}: overrides template pages that conversions.json did not accept")
            derived = {"source_sha256": src["sha256"], "converter": conversions["converter"]["identity"]}
        else:
            files = list(raw.glob(f"{slug}.*"))
            pdf_path = next((f for f in files if f.suffix.lower() == ".pdf"), None)
            if not pdf_path:
                entry.update(status="skipped", reason="원본 파일 없음", template=None, field_count=0, sensitive_fields=[])
                entries.append(entry)
                continue
            if hashlib.sha256(pdf_path.read_bytes()).hexdigest() != src["sha256"]:
                raise SystemExit(f"{slug}: file hash differs from sources.json")
        try:
            template, _ = propose(pdf_path, overrides)
        except ValueError as e:
            entry.update(status="skipped", reason=str(e), template=None, field_count=0, sensitive_fields=[])
            entries.append(entry)
            continue
        if len(template["fields"]) > 200:
            raise SystemExit(f"{slug}: {len(template['fields'])} fields, the API accepts at most 200; trim with an override")
        if not template["fields"]:
            entry.update(status="skipped", reason="입력 칸을 찾지 못함", template=None, field_count=0, sensitive_fields=[])
            entries.append(entry)
            continue
        if derived:
            template["derived"] = derived
            entry["derived_pdf"] = {
                "sha256": template["content_hash"], "byte_size": pdf_path.stat().st_size,
                "converter": derived["converter"],
                "fidelity": {str(p["page"]): p["evidence"] for p in conversions["forms"][slug]["pages"]},
            }
        name = f"{slug}.template.json"
        (out / name).write_text(json.dumps(template, ensure_ascii=False, indent=2) + "\n")
        render_overlay(pdf_path, template, review, slug)
        entry.update(status="template_ready", reason=None, template=name, pages=template["page_count"],
                     templated_pages=sorted({f["page"] + 1 for f in template["fields"]}), field_count=len(template["fields"]),
                     sensitive_fields=sensitive_fields(template),
                     # The preparer looked at every overlay and corrected it; a human still has to.
                     overlay_checked_by_preparer=bool(overrides and overrides.get("reviewed")), human_review="pending")
        entries.append(entry)
        print(f"{slug}: {len(template['fields'])} fields", file=sys.stderr)
    manifest = {"version": 1, "generated_by": "scripts/forms/propose_fields.py batch", "forms": entries}
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("propose")
    p.add_argument("pdf")
    p.add_argument("--overrides")
    p.add_argument("--out")
    p.add_argument("--overlay")
    p.set_defaults(func=cmd_propose)
    b = sub.add_parser("batch")
    b.add_argument("--sources", required=True)
    b.add_argument("--raw", required=True)
    b.add_argument("--out", required=True)
    b.add_argument("--review", required=True)
    b.add_argument("--overrides")
    b.add_argument("--conversions")
    b.add_argument("--derived")
    b.set_defaults(func=cmd_batch)
    args = ap.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
