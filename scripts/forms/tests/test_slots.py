"""Deterministic review (scripts/forms/auto/slots.py) on the synthetic fixture forms.

Fail-first by construction: each test starts from a template that covers every slot and
shows that removing or retyping one box (or linking another person's box) produces the
finding, while the complete template produces none.

Run: uv run --no-project --with pdfplumber==0.11.9 --with pypdfium2==4.30.0 --with pillow --with numpy \
       --with olefile==0.47 --with pytest==8.3.3 python -m pytest scripts/forms/tests/test_slots.py -q
"""
import hashlib
import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "auto"))
sys.path.insert(0, str(HERE.parent / "kordoc"))
pytest.importorskip("pdfplumber")
import slots  # noqa: E402
import check_ir  # noqa: E402

F = HERE / "fixtures"
L = json.loads((F / "layout.json").read_text())


def box(name, rect_pt, typ, label, mapping=None, required=False, fid=None):
    W, H = L[name]["page"]
    x0, y0, x1, y1 = rect_pt
    return {"id": fid or label, "page": 0, "rect": {"x": (x0 + 2) / W, "y": (y0 + 2) / H, "w": (x1 - x0 - 4) / W, "h": (y1 - y0 - 4) / H},
            "type": typ, "label": label, "mapping": mapping, "required": required}


def frac(r):
    return {"x": r["x"], "y": r["y"], "w": r["w"], "h": r["h"]}


def template(name, fields):
    return {"version": 1, "content_hash": hashlib.sha256((F / f"{name}.pdf").read_bytes()).hexdigest(), "page_count": 1,
            "source": "admin", "fields": fields}


def run(name, fields, ir=None):
    return slots.check(template(name, fields), F / f"{name}.pdf", ir)


def at(region, typ, label, mapping=None, fid=None):
    """A box exactly on a slot region (page fractions)."""
    return {"id": fid or label, "page": 0, "rect": frac(region), "type": typ, "label": label, "mapping": mapping, "required": False}


def complete_basic():
    c = L["basic"]["cells"]
    fields = [box("basic", c["name"], "text", "성명", "name"), box("basic", c["birth"], "date", "생년월일", "birth_date"),
              box("basic", c["rrn"], "text", "주민등록번호", "rrn"), box("basic", c["address"], "text", "주소", "address"),
              box("basic", c["phone"], "text", "휴대전화", "phone"), box("basic", c["account"], "text", "계좌번호", "account")]
    probe = run("basic", fields)
    for s in probe["slots"]:
        if s["status"] == "missing" or (s["status"] == "question" and s["kind"] == "sign"):
            typ = "signature" if s["kind"] == "sign" else "text"
            fields.append(at(s["region"], typ, {"sign": "신청인 서명", "colon": "신청인 성명"}.get(s["kind"], f"작성일 {s['label']}"),
                             "signature" if typ == "signature" else None, fid=f"g{len(fields)}"))
    return fields


def kinds(result):
    return sorted(f["kind"] for f in result["findings"])


def missing(result):
    """Missing-box findings, and a slot left to a box of the wrong kind (not the printed-20 year rule)."""
    return [f for f in result["findings"] if f["kind"] == "missing" or (f["kind"] == "wrong_type" and "printed '20'" not in f["detail"])]


def test_complete_template_has_no_findings_but_the_printed_20_year():
    r = run("basic", complete_basic())
    assert kinds(r) == ["wrong_type"] and "printed '20'" in r["findings"][0]["detail"] and "year2" in r["findings"][0]["detail"]


@pytest.mark.parametrize("label", ["성명", "주소", "계좌번호", "작성일 연도", "작성일 일", "신청인 서명", "신청인 성명"])
def test_every_dropped_box_is_reported_missing(label):
    assert not missing(run("basic", complete_basic()))
    fields = [f for f in complete_basic() if f["label"] != label]
    r = run("basic", fields)
    assert missing(r), f"dropping '{label}' went unnoticed"


def test_the_year_blank_after_a_printed_20_needs_a_two_digit_box():
    # basic.pdf prints '20      년': a box writing 2026 there prints 202026 (the template schema
    # has no two-digit year part, so every such form fails until it gets one).
    r = run("basic", complete_basic())
    year = [s for s in r["slots"] if s["kind"] == "date" and s["label"] == "연도"][0]
    assert year["year_after_20"] is True and year["status"] == "wrong_type"
    month = [s for s in r["slots"] if s["kind"] == "date" and s["label"] == "월"][0]
    assert month["status"] == "covered"
    # A year2 part box is the fix: it writes '26' after the printed '20'.
    fixed = [dict(f, part="year2") if f["label"] == "작성일 연도" else f for f in complete_basic()]
    assert run("basic", fixed)["findings"] == []


def test_signature_mark_covered_by_text_is_wrong_type():
    fields = [dict(f, type="text", mapping=None) if f["label"] == "신청인 서명" else f for f in complete_basic()]
    assert any(f["kind"] == "wrong_type" and "signature mark" in f["detail"] for f in run("basic", fields)["findings"])


def test_consent_ticks_need_tick_boxes_not_text():
    c = L["header"]["cells"]
    base = [box("header", c[k], "text", k) for k in ("school", "major", "grade", "filled", "reason", "applicant")]
    probe = run("header", base)
    ticks = [s for s in probe["slots"] if s["kind"] == "checkbox"]
    assert len(ticks) == 2 and all(s["status"] == "missing" for s in ticks)
    as_ticks = base + [at(s["region"], "checkbox", f"동의: {s['label']}", fid=f"t{i}") for i, s in enumerate(ticks)]
    assert not [f for f in run("header", as_ticks)["findings"] if "tick box" in f["detail"]]
    as_text = base + [at(s["region"], "text", f"동의 {i}", fid=f"t{i}") for i, s in enumerate(ticks)]
    assert sum(f["kind"] == "wrong_type" and "tick box" in f["detail"] for f in run("header", as_text)["findings"]) == 2


def test_guardian_block_box_linked_to_the_applicant_is_wrong_person():
    c = L["section"]["cells"]
    rest = [box("section", c["email"], "text", "이메일", "email"), box("section", c["date"], "text", "신청일")]
    unlinked = [box("section", c["guardian_name"], "text", "성명"), box("section", c["guardian_phone"], "text", "연락처")] + rest
    assert "wrong_person" not in kinds(run("section", unlinked))
    # The label says only '성명': the block header '보호자' printed in the tall cell to the left names the owner.
    linked = [box("section", c["guardian_name"], "text", "성명", "name"), box("section", c["guardian_phone"], "text", "연락처")] + rest
    r = run("section", linked)
    wp = [f for f in r["findings"] if f["kind"] == "wrong_person"]
    assert len(wp) == 1 and wp[0]["box"] == 1 and "보호자" in wp[0]["detail"]
    required = [dict(f, required=True) if f["label"] == "연락처" else f for f in unlinked]
    assert "wrong_person" in kinds(run("section", required))


def test_ir_row_header_names_the_owner_and_ir_blanks_must_be_boxed():
    """kordoc IR for section.pdf: '보호자' spans the 성명 and 연락처 rows."""
    table = {"rows": 4, "cols": 3, "cells": [
        {"r": 0, "c": 0, "rs": 2, "cs": 1, "text": "보호자", "header": False, "tables": []},
        {"r": 0, "c": 1, "rs": 1, "cs": 1, "text": "성명", "header": False, "tables": []},
        {"r": 0, "c": 2, "rs": 1, "cs": 1, "text": "", "header": False, "tables": []},
        {"r": 1, "c": 1, "rs": 1, "cs": 1, "text": "연락처", "header": False, "tables": []},
        {"r": 1, "c": 2, "rs": 1, "cs": 1, "text": "", "header": False, "tables": []},
        {"r": 2, "c": 0, "rs": 1, "cs": 2, "text": "이메일", "header": False, "tables": []},
        {"r": 2, "c": 2, "rs": 1, "cs": 1, "text": "", "header": False, "tables": []}]}
    pdf_sha = hashlib.sha256((F / "section.pdf").read_bytes()).hexdigest()
    ir = {"version": 1, "tool": "kordoc", "kordoc_version": "4.24.7", "file": "section.hwp", "source_sha256": pdf_sha, "file_type": "hwp",
          "page_count": 1, "page_mode": "layout", "warnings": [], "blocks": [{"page": 1, "kind": "table", "table": table}]}
    check_ir.check(ir, pdf_sha)
    assert slots.ir_owners(ir)[(0, "성명")] == ["보호자"]
    c = L["section"]["cells"]
    full = [box("section", c["guardian_name"], "text", "성명"), box("section", c["guardian_phone"], "text", "연락처"),
            box("section", c["email"], "text", "이메일", "email"), box("section", c["date"], "text", "신청일")]
    r = run("section", full, ir)
    assert [s for s in r["slots"] if s["source"] == "ir" and s["status"] == "covered"]
    dropped = run("section", [f for f in full if f["label"] != "연락처"], ir)
    # A blank inside a named block: who writes there is the judge's question (an applicant answer,
    # or unclear, is a missing box). No word list decides whose block '보호자' is.
    import judge
    q = [q for q in dropped["questions"] if q["kind"] == "blank_owner" and q["label"] == "연락처"]
    assert q and q[0]["header"] == "보호자"
    assert judge.to_finding(q[0], {"answer": "applicant"})["kind"] == "missing"
    assert judge.to_finding(q[0], {"answer": "unclear"})["kind"] == "missing"


def test_label_with_printed_tick_options_is_mislabelled_and_cut_off_options_too():
    fields = complete_basic()
    assert "mislabelled" not in kinds(run("basic", fields))
    fields[0] = dict(fields[0], label="동의함 ◻ 동의안함 ◻ 신청자 성명")
    assert "mislabelled" in kinds(run("basic", fields))
    assert slots.DANGLING.search("미 진학자 -") and not slots.DANGLING.search("미진학자")


def test_office_instruction_and_heading_slots_are_excluded():
    cells = [(0.1, 0.1, 0.3, 0.13, "접수번호"), (0.3, 0.1, 0.6, 0.13, "")]
    s = slots.Slot(0, "value", "접수번호", slots.Rect(0.3, 0.1, 0.6, 0.13), "ir")
    assert slots.exclusion(s, cells) == "office"
    mine = slots.Slot(0, "date", "연도", slots.Rect(0.3, 0.1, 0.35, 0.12), "pdf", context="접수번호|(신청일자:")
    assert slots.exclusion(mine, cells) == "", "the applicant's date inside an office row stays a slot"
    head = slots.Slot(0, "value", "(제1호서식)", slots.Rect(0.5, 0.0, 0.6, 0.05), "ir")
    assert slots.exclusion(head, []) == "heading"
    chars = [{"t": t, "x0": 0.1 + i * 0.02, "x1": 0.115 + i * 0.02, "y0": 0.5, "y1": 0.512, "s": 0.017} for i, t in enumerate("※□안에표시")]
    ticks = [x for x in slots.glyph_slots(0, chars, []) if x.kind == "checkbox"]
    assert ticks and all(x.excluded == "instruction" for x in ticks)


def test_tampered_ir_is_refused(tmp_path):
    p = tmp_path / "x.ir.json"
    p.write_text(json.dumps({"version": 1}))
    with pytest.raises(check_ir.Invalid):
        check_ir.load(str(p), "a" * 64)
    link = tmp_path / "l.ir.json"
    link.symlink_to(p)
    with pytest.raises(check_ir.Invalid):
        check_ir.load(str(link), "a" * 64)


def test_ir_without_layout_pages_still_enumerates_blank_cells_from_the_pdf():
    """An IR whose pages are section approximations cannot place cells: the PDF's ruled cells are used."""
    pdf_sha = hashlib.sha256((F / "section.pdf").read_bytes()).hexdigest()
    ir = {"version": 1, "tool": "kordoc", "kordoc_version": "4.24.7", "file": "s.hwp", "source_sha256": pdf_sha, "file_type": "hwp",
          "page_count": 1, "page_mode": "section", "warnings": [], "blocks": []}
    c = L["section"]["cells"]
    full = [box("section", c[k], "text", k) for k in ("guardian_name", "guardian_phone", "email", "date")]
    assert not [f for f in run("section", full, ir)["findings"] if f["kind"] == "missing" and "blank cell" in f["detail"]]
    dropped = run("section", [f for f in full if f["label"] != "guardian_phone"], ir)
    assert any(f["kind"] == "missing" and "blank cell" in f["detail"] for f in dropped["findings"])


def test_group_mode_comes_from_structure_only_and_wording_goes_to_the_judge():
    g = {"id": "g1", "page": 0, "question": "희망 분야", "options": ["취업", "창업", "진학"], "members": [], "mode": "multi"}
    assert slots.stated_mode(g, {}, [], ["○취업 ○창업 ○진학"])[0] == "single", "radio options in one cell"
    assert slots.stated_mode(g, {}, [], ["□취업 □창업 □진학"]) == (None, "")
    # Wording is not read (no word lists): the judge is asked.
    assert slots.stated_mode({**g, "question": "희망 분야 (택1)"}, {}, [], []) == (None, "")
    assert slots.stated_mode(g, {}, [], ["□취업 □창업 □진학 (중복 선택 가능)"]) == (None, "")
    assert not hasattr(slots, "SINGLE_WORDS") and not hasattr(slots, "MULTI_WORDS")


def test_a_blank_before_a_unit_inside_parentheses_is_a_slot():
    # '(                원)': the amount goes before the printed 원, inside the parentheses.
    chars = [{"t": "(", "x0": 0.30, "x1": 0.305, "y0": 0.5, "y1": 0.512, "s": 0.017},
             {"t": "원", "x0": 0.40, "x1": 0.415, "y0": 0.5, "y1": 0.512, "s": 0.017},
             {"t": ")", "x0": 0.415, "x1": 0.42, "y0": 0.5, "y1": 0.512, "s": 0.017}]
    got = [s for s in slots.glyph_slots(0, chars, []) if s.kind == "paren"]
    assert len(got) == 1 and got[0].needs_text and got[0].region.x0 == 0.305 and got[0].region.x1 == 0.40


def test_a_signature_mark_with_no_box_is_the_judges_question():
    """Whose signature a mark is (the applicant's or a recommender's) is not decided by a word
    list: the judge is asked, an "applicant" or unclear answer is a missing box."""
    import judge

    fields = [f for f in complete_basic() if f["label"] not in ("신청인 서명", "신청인 성명")]
    r = run("basic", fields)
    q = [q for q in r["questions"] if q["kind"] == "blank_owner"]
    assert q, [(s["kind"], s["status"]) for s in r["slots"] if s["kind"] == "sign"]
    assert judge.to_finding(q[0], {"answer": "applicant"})["kind"] == "missing"
    assert judge.to_finding(q[0], {"answer": "unclear"})["kind"] == "missing", "fails closed"
    assert judge.to_finding(q[0], {"answer": "someone_else"}) is None
