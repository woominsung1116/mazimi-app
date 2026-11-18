"""The field invariants (scripts/forms/auto/invariants.py), one rule at a time, on the synthetic
'invariants' fixture (make_fixtures.py), plus the held-out false pass the rules exist for.

Fail-first by construction: every test starts from the complete template, which no rule
rejects, and shows that one defect makes exactly the named rule reject it (or, where the
structure is ambiguous, ask the judge one closed question about that one box).

Run: uv run --no-project --with pdfplumber==0.11.9 --with pypdfium2==4.30.0 --with pillow --with numpy \
       --with olefile==0.47 --with pytest==8.3.3 python -m pytest scripts/forms/tests/test_invariants.py -q
"""
import copy
import hashlib
import json
import os
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "auto"))
sys.path.insert(0, str(HERE.parent / "kordoc"))
pytest.importorskip("pdfplumber")
import invariants  # noqa: E402

F = HERE / "fixtures"
PDF = F / "invariants.pdf"
LAY = json.loads((F / "layout.json").read_text())["invariants"]
W, H = LAY["page"]
C = LAY["cells"]


def box(rect_pt, typ, label, mapping=None, required=False, fid=None, part=None, page=0, inset=2):
    x0, y0, x1, y1 = rect_pt
    f = {"id": fid or label, "page": page, "rect": {"x": (x0 + inset) / W, "y": (y0 + inset) / H, "w": (x1 - x0 - 2 * inset) / W,
                                                     "h": (y1 - y0 - 2 * inset) / H},
         "type": typ, "label": label, "mapping": mapping, "required": required}
    if part:
        f["part"] = part
    return f


def complete() -> list[dict]:
    two = C["two_value"]
    mid = (two[1] + two[3]) / 2
    fields = [box(C["address"], "text", "주소", "address", True),
              box([two[0], two[1], two[2], mid], "text", "성명", "name", True, fid="name"),
              box([two[0], mid, two[2], two[3]], "text", "연락처", "phone", True, fid="phone"),
              box(LAY["ticks"][0], "checkbox", "동의함", fid="agree", inset=-1),
              box(LAY["ticks"][1], "checkbox", "동의하지 않음", fid="disagree", inset=-1)]
    for r in range(1, 5):
        for key, label in (("m_name", "성명"), ("m_birth", "생년월일"), ("m_phone", "연락처")):
            fields.append(box(C[f"{key}{r}"], "text", f"참여자 {label} ({r}행)", fid=f"{key}{r}"))
    for part, label in (("year2", "작성일 연도"), ("month", "작성일 월"), ("day", "작성일 일")):
        fields.append(box(LAY["date"][part], "text", label, "application_date", fid=part, part=part, inset=0))
    return fields


def template(fields: list[dict]) -> dict:
    return {"version": 1, "content_hash": hashlib.sha256(PDF.read_bytes()).hexdigest(), "page_count": 2, "source": "admin", "fields": fields}


def model(fields: list[dict]) -> invariants.Model:
    """The page model with no slots.py findings folded in: each rule is tested on its own."""
    return invariants.build(template(fields), PDF, None, {})


def without(fields, fid):
    return [f for f in fields if f["id"] != fid]


def replace(fields, fid, **change):
    out = copy.deepcopy(fields)
    for f in out:
        if f["id"] == fid:
            f.update(change)
    return out


RULES = dict(invariants.INVARIANTS)


def findings(rule: str, fields: list[dict]) -> list[dict]:
    return RULES[rule](model(fields))[0]


def questions(rule: str, fields: list[dict]) -> list[dict]:
    return RULES[rule](model(fields))[1]


def test_the_complete_template_breaks_no_rule():
    m = model(complete())
    for name, rule in invariants.INVARIANTS:
        fs, qs = rule(m)
        assert fs == [], (name, fs)
        assert [q for q in qs if q["kind"] != "page_form"] == [], (name, qs)


def test_every_rule_names_its_reject_reason():
    names = [n for n, _ in invariants.INVARIANTS]
    assert len(names) == len(set(names))
    assert {"label_box_adjacency", "input_has_box", "person_section", "source_text_agreement", "checkbox_groups_complete",
            "date_signature_regions", "no_box_over_text", "pages_are_forms"} == set(names)


# label_box_adjacency -------------------------------------------------------------------------
def test_a_box_named_for_another_label_than_its_cell_is_mislabelled():
    bad = replace(complete(), "주소", label="휴대전화")
    fs = findings("label_box_adjacency", bad)
    assert [f["kind"] for f in fs] == ["mislabelled"] and "주소" in fs[0]["detail"]


def test_a_date_part_box_must_match_the_unit_printed_after_it():
    bad = replace(complete(), "month", part="day", label="작성일 일")
    fs = findings("label_box_adjacency", bad)
    assert [f["kind"] for f in fs] == ["mislabelled"] and "(suffix)" in fs[0]["detail"]


def test_a_box_tied_to_no_printed_label_is_asked_about_not_rejected():
    fields = complete() + [box(LAY["free"], "text", "자기소개", fid="free")]
    assert findings("label_box_adjacency", fields) == []
    qs = questions("label_box_adjacency", fields)
    assert [(q["kind"], q["label"]) for q in qs] == [("box_label", "자기소개")]


# input_has_box -------------------------------------------------------------------------------
def test_a_list_row_with_no_box_is_missing():
    fs = findings("input_has_box", without(complete(), "m_name3"))
    assert [f["kind"] for f in fs] == ["missing"] and "성명" in fs[0]["detail"]


def test_one_box_for_two_printed_labels_is_missing_one():
    two = C["two_value"]
    fields = without(without(complete(), "phone"), "name") + [box(two, "text", "성명", "name", True, fid="name")]
    fs = findings("input_has_box", fields)
    assert [f["kind"] for f in fs] == ["missing"] and "2 printed labels" in fs[0]["detail"]


# person_section ------------------------------------------------------------------------------
def test_a_list_row_linked_to_the_applicant_is_the_wrong_person():
    fs = findings("person_section", replace(complete(), "m_name1", mapping="name"))
    assert [f["kind"] for f in fs] == ["wrong_person"] and "profile key name" in fs[0]["detail"]


def test_a_required_list_row_is_the_wrong_person():
    fs = findings("person_section", replace(complete(), "m_phone2", required=True))
    assert [f["kind"] for f in fs] == ["wrong_person"]


def test_the_applicants_own_rows_are_not_a_list():
    assert findings("person_section", complete()) == []  # 성명 and 연락처 above the list are mapped and required


# source_text_agreement -----------------------------------------------------------------------
def test_a_printed_label_missing_from_the_source_text_is_misplaced():
    m = model(complete())
    m.ir_text = invariants.slots.norm("성명 연락처 비고 연번 생년월일 개인정보 수집 동의")  # the source never says 주소
    fs = invariants.source_text_agreement(m)[0]
    assert [f["kind"] for f in fs] == ["misplaced"] and "주소" in fs[0]["detail"]
    m.ir_text = invariants.slots.norm("주소 " + m.ir_text)
    assert invariants.source_text_agreement(m)[0] == []


def test_a_pdf_source_has_no_second_text_to_disagree_with():
    assert findings("source_text_agreement", complete()) == []


# checkbox_groups_complete --------------------------------------------------------------------
def test_a_drawn_tick_square_with_no_tick_box_is_missing():
    fs = findings("checkbox_groups_complete", without(complete(), "disagree"))
    assert [f["kind"] for f in fs] == ["missing"] and "tick mark" in fs[0]["detail"]


# date_signature_regions (slots.py's date and signature findings, under this rule's name) -----
def test_date_and_signature_findings_come_from_the_slot_check():
    m = model(complete())
    m.slots_result = {"findings": [{"page": 1, "box": None, "kind": "missing", "detail": "date blank '월' near (0.5,0.4) has no box"},
                                   {"page": 1, "box": 3, "kind": "wrong_type", "detail": "box 3 is text, the slot wants a signature"},
                                   {"page": 1, "box": None, "kind": "missing", "detail": "blank cell '주소' near (0.2,0.1) has no box"}]}
    fs = invariants.date_signature_regions(m)[0]
    assert [f["detail"][:9] for f in fs] == ["date blan", "box 3 is "]


# no_box_over_text ----------------------------------------------------------------------------
def test_a_box_over_printed_text_is_misplaced():
    fs = findings("no_box_over_text", complete() + [box(C["sample"], "text", "비고", fid="note")])
    assert [f["kind"] for f in fs] == ["misplaced"] and "홍길동" in fs[0]["detail"]



def _mark_and_template(rect_of_mark):
    """basic.pdf (test_propose_fields' fixture) prints '신청인 ...... (서명 또는 인)': one signature
    box placed by rect_of_mark(mark box in page fractions)."""
    import pdfplumber
    from propose_fields import compact, tokens
    pdf = F / "basic.pdf"
    with pdfplumber.open(pdf) as doc:
        page = doc.pages[0]
        t = next(t for t in tokens(page.chars) if compact(t["text"]) == "(서명또는인)")
        mark = (t["x0"] / page.width, t["top"] / page.height, t["x1"] / page.width, t["bottom"] / page.height)
    x0, y0, x1, y1 = rect_of_mark(mark)
    f = {"id": "sign", "page": 0, "rect": {"x": x0, "y": y0, "w": x1 - x0, "h": y1 - y0}, "type": "signature", "label": "신청인 서명", "mapping": "signature", "required": True}
    return {"version": 1, "content_hash": hashlib.sha256(pdf.read_bytes()).hexdigest(), "page_count": 1, "source": "admin", "fields": [f]}, pdf


def test_a_signature_box_over_the_printed_signing_mark_is_misplaced():
    # The bracket that exempts a printed hint ('(장학명 ... 참조)') does not exempt '(서명 또는 인)':
    # a signature drawn over it leaves both unreadable.
    over, pdf = _mark_and_template(lambda m: (m[0] - 0.005, m[1] - 0.01, m[2] + 0.005, m[3] + 0.01))
    fs = invariants.no_box_over_text(invariants.build(over, pdf, None, {}))[0]
    assert [f["kind"] for f in fs] == ["misplaced"] and "서명" in fs[0]["detail"]
    beside, pdf = _mark_and_template(lambda m: (m[2] + 0.005, m[1] - 0.01, m[2] + 0.005 + (m[2] - m[0]), m[3] + 0.01))
    assert invariants.no_box_over_text(invariants.build(beside, pdf, None, {}))[0] == []


# The two production templates whose signature boxes covered '(서명 또는 인)' (QA B5), before and
# after the proposer fix. Their converted PDFs are not committed (like every official file):
# FORMS_DERIVED_DIR holds <slug>.pdf, checked against the template's content_hash.
SIGN = F / "sign-over-text"
DERIVED = Path(os.environ.get("FORMS_DERIVED_DIR", HERE.parents[2] / ".omc/forms/hwp/rhwp/pdf"))


def _derived(slug: str, t: dict) -> Path:
    pdf = DERIVED / f"{slug}.pdf"
    if not pdf.exists() or hashlib.sha256(pdf.read_bytes()).hexdigest() != t["content_hash"]:
        pytest.skip(f"converted PDF for {slug} not in {DERIVED}")
    return pdf


@pytest.mark.parametrize("slug,signatures", [("wonju-study-cafe", 3), ("wonju-ai-interview-experience", 2)])
def test_production_signature_boxes_over_the_mark_are_rejected_and_the_fix_passes(slug, signatures):
    prod = json.loads((SIGN / f"{slug}.prod.template.json").read_text())
    fixed = json.loads((SIGN / f"{slug}.fixed.template.json").read_text())
    pdf = _derived(slug, prod)
    assert fixed["content_hash"] == prod["content_hash"], "same converted PDF, no re-upload"
    fs = invariants.no_box_over_text(invariants.build(prod, pdf, None, {}))[0]
    numbers = invariants.slots.box_numbers(prod)
    signs = {numbers[f["id"]] for f in prod["fields"] if f["type"] == "signature"}
    assert len(signs) == signatures and {f["box"] for f in fs} == signs, fs
    assert invariants.no_box_over_text(invariants.build(fixed, pdf, None, {}))[0] == []


# pages_are_forms -----------------------------------------------------------------------------
def test_a_box_on_a_page_that_is_not_a_form_is_asked_about():
    fields = complete() + [box([60, 300, 300, 320], "text", "안내", fid="notice", page=1)]
    qs = questions("pages_are_forms", fields)
    assert [(q["kind"], q["page"]) for q in qs] == [("page_form", 2)]


# The whole table -----------------------------------------------------------------------------
def test_check_tags_every_finding_with_its_rule_and_origin():
    r = invariants.check(template(replace(complete(), "m_name1", mapping="name")), PDF)
    mine = [f for f in r["findings"] if f["origin"] == "invariant"]
    assert {f["rule"] for f in mine} == {"person_section"}
    assert r["counts"]["rules"]["person_section"]["findings"] >= 1


# The held-out false pass (item B, kept out of the repository): the rules reject it for its own
# defects, whatever the judge would answer.
ITEM_B = Path(os.environ.get("FORMS_ITEM_B_DIR", "/nonexistent"))


@pytest.mark.skipif(not (ITEM_B / "template.json").exists(), reason="held-out item B is not in the repository")
def test_held_out_item_b_is_rejected_by_the_rules():
    r = invariants.check(json.loads((ITEM_B / "template.json").read_text()), ITEM_B / "blank.pdf")
    rules = {f["rule"] for f in r["findings"] if f["origin"] == "invariant"}
    assert {"input_has_box", "person_section", "checkbox_groups_complete"} <= rules
