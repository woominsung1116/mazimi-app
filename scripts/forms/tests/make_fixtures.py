# /// script
# requires-python = ">=3.10"
# dependencies = ["reportlab==4.2.5"]
# ///
"""Generates the tiny synthetic form PDFs used by test_propose_fields.py, test_slots.py and
test_invariants.py.

These are not real agency forms. Re-run after changing a layout:
  uv run scripts/forms/tests/make_fixtures.py
The tests read the committed PDFs and the expected cell positions from
fixtures/layout.json, which this script writes alongside them.
"""
import json
from pathlib import Path

from reportlab.lib.pagesizes import A4
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas

ROOT = Path(__file__).resolve().parents[3]
OUT = Path(__file__).resolve().parent / "fixtures"
pdfmetrics.registerFont(TTFont("Nanum", str(ROOT / "apps/mobile/assets/fonts/NanumGothic-Regular.ttf")))
# NanumGothic has no circled digits (①); the macOS system font does. Regenerating the
# 'mixed' fixture therefore needs macOS; the committed PDF is what the tests read.
pdfmetrics.registerFont(TTFont("Sym", "/System/Library/Fonts/Supplemental/AppleGothic.ttf"))
W, H = A4
layout: dict[str, dict] = {}


def doc(name: str):
    c = canvas.Canvas(str(OUT / f"{name}.pdf"), pagesize=A4, invariant=1)
    c.setFont("Nanum", 10)
    c.setLineWidth(0.8)
    layout[name] = {"page": [W, H], "cells": {}}
    return c


def cell(c, name, key, x, top, w, h, text="", align="center"):
    """Draws a ruled cell; coordinates are top-left based like pdfplumber."""
    c.rect(x, H - top - h, w, h)
    if text:
        y = H - top - h / 2 - 3.5
        if align == "center":
            c.drawCentredString(x + w / 2, y, text)
        else:
            c.drawString(x + 4, y, text)
    if key:
        layout[name]["cells"][key] = [x, top, x + w, top + h]


def text(c, x, top, s):
    c.drawString(x, H - top - 10, s)


# 1. Classic two-column applicant table + date and signature lines.
c = doc("basic")
text(c, 220, 40, "장학생 신청서")
cell(c, "basic", None, 50, 80, 80, 26, "성    명")
cell(c, "basic", "name", 130, 80, 150, 26)
cell(c, "basic", None, 280, 80, 80, 26, "생년월일")
cell(c, "basic", "birth", 360, 80, 180, 26)
cell(c, "basic", None, 50, 106, 80, 26, "주민등록번호")
cell(c, "basic", "rrn", 130, 106, 410, 26)
cell(c, "basic", None, 50, 132, 80, 26, "주    소")
cell(c, "basic", "address", 130, 132, 410, 26)
cell(c, "basic", None, 50, 158, 80, 26, "휴대전화")
cell(c, "basic", "phone", 130, 158, 410, 26, "010  -          -", align="left")
cell(c, "basic", None, 50, 184, 80, 26, "계좌번호")
cell(c, "basic", "account", 130, 184, 410, 26)
text(c, 250, 260, "20      년      월      일")
text(c, 300, 300, "신청인              (서명 또는 인)")
c.save()

# 2. Header row with values below, checkboxes, a filled sample cell.
c = doc("header")
text(c, 220, 40, "교내 장학 신청서")
for i, (label, key) in enumerate([("학교명", "school"), ("학과", "major"), ("학년", "grade")]):
    cell(c, "header", None, 50 + i * 160, 80, 160, 22, label)
    cell(c, "header", key, 50 + i * 160, 102, 160, 26)
cell(c, "header", None, 50, 140, 80, 26, "소속")
cell(c, "header", "filled", 130, 140, 400, 26, "예시: 공과대학 기계공학부", align="left")
text(c, 60, 190, "개인정보 수집·이용 동의   □ 동의함   □ 동의하지 않음")
cell(c, "header", None, 50, 220, 80, 60, "신청 사유")
cell(c, "header", "reason", 130, 220, 400, 60)
cell(c, "header", None, 50, 290, 80, 26, "신 청 인")
cell(c, "header", "applicant", 130, 290, 400, 26)
c.drawString(470, H - 290 - 13 - 3.5, "(인)")
c.save()

# 3. Section header spanning two rows (보호자), plus an e-mail split cell.
c = doc("section")
text(c, 220, 40, "청년 지원 신청서")
cell(c, "section", None, 50, 80, 60, 52, "보호자")
cell(c, "section", None, 110, 80, 70, 26, "성명")
cell(c, "section", "guardian_name", 180, 80, 360, 26)
cell(c, "section", None, 110, 106, 70, 26, "연락처")
cell(c, "section", "guardian_phone", 180, 106, 360, 26)
cell(c, "section", None, 50, 132, 130, 26, "이메일")
cell(c, "section", "email", 180, 132, 360, 26, "                    @", align="left")
cell(c, "section", None, 50, 158, 130, 26, "신청일")
cell(c, "section", "date", 180, 158, 360, 26, "      년      월      일", align="left")
c.save()

# 4. Patterns from real forms: a label inside its own cell above another such cell (the
# blank under '성명' is NOT the name box), bullet lines with two questions, (○, ×),
# numbered options, a member list, a printed prefix and a dotted date range.
c = doc("mixed")
text(c, 220, 40, "청년 지원 신청서")
cell(c, "mixed", "inline_name", 50, 70, 490, 26, "성 명(외국인은 영문)", align="left")
cell(c, "mixed", "inline_rrn", 50, 96, 490, 26, "주민등록번호", align="left")
text(c, 60, 140, "○ 성    명 :                          ○ 생년월일 :")
cell(c, "mixed", None, 50, 170, 120, 26, "주거급여 수급여부")
cell(c, "mixed", "ox", 170, 170, 100, 26, "(○,  ×)")
cell(c, "mixed", None, 270, 170, 100, 26, "구직등록여부")
cell(c, "mixed", "numbered", 370, 170, 170, 26)
c.setFont("Sym", 10)
c.drawString(374, H - 170 - 13 - 3.5, "① 등록    ② 미등록")
c.setFont("Nanum", 10)
cell(c, "mixed", None, 50, 210, 120, 26, "휴대전화")
cell(c, "mixed", "prefix", 170, 210, 370, 26, "010-", align="left")
cell(c, "mixed", None, 50, 236, 120, 26, "임대차기간")
cell(c, "mixed", "range", 170, 236, 370, 26, "20    .    .    .  ~  20    .    .    .", align="left")
for i, (label, key) in enumerate([("관계", "rel"), ("성명", "member_name"), ("주민등록번호", "member_rrn")]):
    cell(c, "mixed", None, 50 + i * 160, 280, 160, 22, label)
    for r in range(3):
        cell(c, "mixed", f"{key}{r + 1}", 50 + i * 160, 302 + r * 22, 160, 22)
c.save()

# 5. Field invariants (test_invariants.py): a label cell with two labels, a value cell holding
# a printed sample, drawn tick squares, a numbered member list, a date line and, on page 2, a
# notice with no blanks.
c = doc("invariants")
L5 = layout["invariants"]
c.setFont("Nanum", 16)
c.drawString(200, H - 40 - 16, "참여자 신청서")
c.setFont("Nanum", 10)
cell(c, "invariants", None, 50, 80, 80, 26, "주    소")
cell(c, "invariants", "address", 130, 80, 410, 26)
cell(c, "invariants", "two_label", 50, 106, 80, 44)
c.drawCentredString(90, H - 106 - 16, "성명")
c.drawCentredString(90, H - 106 - 36, "연락처")
cell(c, "invariants", "two_value", 130, 106, 410, 44)
cell(c, "invariants", None, 50, 150, 80, 26, "비    고")
cell(c, "invariants", "sample", 130, 150, 410, 26, "홍길동", align="left")
c.drawString(60, H - 190 - 10, "개인정보 수집 동의")
L5["ticks"] = []
for x, word in ((170, "동의함"), (240, "동의하지 않음")):
    c.rect(x, H - 190 - 9, 9, 9)
    c.drawString(x + 13, H - 190 - 10, word)
    L5["ticks"].append([x, 190, x + 9, 199])
for i, (label, key, w) in enumerate([("연번", "seq", 60), ("성명", "m_name", 140), ("생년월일", "m_birth", 140), ("연락처", "m_phone", 150)]):
    x = 50 + sum((60, 140, 140)[:i])
    cell(c, "invariants", None, x, 230, w, 22, label)
    for r in range(4):
        cell(c, "invariants", f"{key}{r + 1}", x, 252 + r * 22, w, 22, str(r + 1) if key == "seq" else "")
for x, word in ((250, "20"), (300, "년"), (350, "월"), (400, "일")):
    c.drawString(x, H - 360 - 10, word)
L5["date"] = {"year2": [264, 357, 298, 373], "month": [312, 357, 348, 373], "day": [362, 357, 398, 373]}
c.drawString(250, H - 400 - 10, "신청인")
L5["free"] = [300, 440, 420, 460]
c.showPage()
c.setFont("Nanum", 10)
for i, line in enumerate(["안내 사항", "1. 이 사업은 지역에 거주하는 청년의 활동을 지원하기 위한 사업입니다.",
                          "2. 신청서는 기한 안에 담당 부서로 제출하여 주시기 바랍니다.",
                          "3. 제출한 서류는 반환하지 않으며 사실과 다를 경우 지원이 취소됩니다.",
                          "4. 문의 사항은 담당 부서로 연락하여 주시기 바랍니다."]):
    c.drawString(60, H - 80 - i * 24 - 10, line)
c.save()

(OUT / "layout.json").write_text(json.dumps(layout, ensure_ascii=False, indent=1) + "\n")
print("wrote", sorted(p.name for p in OUT.iterdir()))
