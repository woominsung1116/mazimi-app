"""Detection heuristics on synthetic fixtures (not real agency forms).

Run: uv run --with pdfplumber==0.11.9 --with pytest==8.3.3 pytest scripts/forms/tests -q
"""
import hashlib
import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import propose_fields as pf  # noqa: E402

FIX = HERE / "fixtures"
LAYOUT = json.loads((FIX / "layout.json").read_text())


def run(name):
    template, _ = pf.propose(FIX / f"{name}.pdf")
    return template


def by_label(template):
    return {f["label"]: f for f in template["fields"]}


def inside(field, name, key, tol=0.5):
    """The field box lies within the fixture's ruled value cell."""
    pw, ph = LAYOUT[name]["page"]
    x0, top, x1, bottom = LAYOUT[name]["cells"][key]
    r = field["rect"]
    fx0, fy0, fx1, fy1 = r["x"] * pw, r["y"] * ph, (r["x"] + r["w"]) * pw, (r["y"] + r["h"]) * ph
    return fx0 >= x0 - tol and fy0 >= top - tol and fx1 <= x1 + tol and fy1 <= bottom + tol


def test_template_is_bound_to_the_exact_file_bytes():
    t = run("basic")
    assert t["content_hash"] == hashlib.sha256((FIX / "basic.pdf").read_bytes()).hexdigest()
    assert t["version"] == 1 and t["source"] == "admin" and t["page_count"] == 1


def test_label_cells_get_a_box_in_the_adjacent_value_cell():
    t = by_label(run("basic"))
    assert t["성명"]["mapping"] == "name" and inside(t["성명"], "basic", "name")
    assert t["생년월일"]["type"] == "date" and t["생년월일"]["mapping"] == "birth_date" and inside(t["생년월일"], "basic", "birth")
    assert t["주소"]["mapping"] == "address" and inside(t["주소"], "basic", "address")


def test_spaced_labels_are_joined():
    assert "성명" in by_label(run("basic")), "'성    명' must read as 성명"


def test_sensitive_cells_are_mapped_so_the_app_asks_at_fill_time():
    t = by_label(run("basic"))
    assert t["주민등록번호"]["mapping"] == "rrn" and inside(t["주민등록번호"], "basic", "rrn")
    assert t["계좌번호"]["mapping"] == "account" and inside(t["계좌번호"], "basic", "account")
    assert set(pf.sensitive_fields(run("basic"))) == {"주민등록번호", "계좌번호"}


def test_printed_separators_split_the_cell_and_never_overlap_the_print():
    t = by_label(run("basic"))
    parts = [t["휴대전화 (1)"], t["휴대전화 (2)"]]
    assert all(inside(p, "basic", "phone") and p["mapping"] is None for p in parts)
    assert parts[0]["rect"]["x"] + parts[0]["rect"]["w"] <= parts[1]["rect"]["x"]


def test_date_and_signature_lines_outside_tables():
    t = by_label(run("basic"))
    for part in ("연도", "월", "일"):
        assert t[f"작성일 {part}"]["type"] == "text"
    sign = t["신청인 서명"]
    assert sign["type"] == "signature" and sign["mapping"] == "signature" and sign["required"]
    assert t["신청인"]["mapping"] == "name"
    assert t["신청인"]["rect"]["x"] + t["신청인"]["rect"]["w"] <= sign["rect"]["x"] + 1e-6, "name box sits left of the mark"


def test_header_row_labels_take_the_cell_below():
    t = by_label(run("header"))
    assert inside(t["학교명"], "header", "school") and t["학교명"]["mapping"] == "school"
    assert inside(t["학과"], "header", "major") and t["학과"]["mapping"] == "major"
    assert inside(t["학년"], "header", "grade") and t["학년"]["mapping"] == "grade"


def test_a_cell_with_printed_content_is_not_an_input():
    t = run("header")
    assert not any(inside(f, "header", "filled") for f in t["fields"]), "예시 text means not blank"


def test_check_glyphs_become_checkboxes_with_their_option_text():
    t = by_label(run("header"))
    yes, no = t["개인정보 수집·이용 동의: 동의함"], t["개인정보 수집·이용 동의: 동의하지 않음"]
    assert yes["type"] == "checkbox" and no["type"] == "checkbox" and yes["mapping"] is None
    assert yes["rect"]["w"] < 0.03 and yes["rect"]["x"] < no["rect"]["x"]


def test_reason_cells_are_multiline():
    f = by_label(run("header"))["신청 사유"]
    assert f["type"] == "multiline" and inside(f, "header", "reason")


def test_stamp_mark_inside_a_cell_gets_a_stamp_box_and_a_name_box_left_of_it():
    t = by_label(run("header"))
    stamp, name = t["신청인 (인)"], t["신청인"]
    assert stamp["type"] == "stamp" and stamp["mapping"] == "stamp" and inside(stamp, "header", "applicant")
    assert name["mapping"] == "name" and inside(name, "header", "applicant")
    assert name["rect"]["x"] + name["rect"]["w"] <= stamp["rect"]["x"]


def test_section_header_marks_another_persons_fields():
    t = by_label(run("section"))
    g = t["보호자 성명"]
    assert inside(g, "section", "guardian_name") and not g["required"]
    assert inside(t["보호자 연락처"], "section", "guardian_phone")


def test_printed_date_marks_split_into_parts_inside_the_cell():
    t = by_label(run("section"))
    parts = [t["신청일 연도"], t["신청일 월"], t["신청일 일"]]
    assert all(inside(p, "section", "date") for p in parts)
    xs = [p["rect"]["x"] for p in parts]
    assert xs == sorted(xs)



def test_application_date_parts_link_to_today_with_their_part():
    # '20  년  월  일' on its own line right above the applicant's signing line: the app
    # suggests today's part, and the printed '20' makes the year box two digits.
    t = by_label(run("basic"))
    assert [(t[f"작성일 {p}"]["mapping"], t[f"작성일 {p}"].get("part")) for p in ("연도", "월", "일")] == \
        [("application_date", "year2"), ("application_date", "month"), ("application_date", "day")]
    # '      년      월      일' in a 신청일 cell: the whole year is written.
    t = by_label(run("section"))
    assert [t[f"신청일 {p}"].get("part") for p in ("연도", "월", "일")] == ["year4", "month", "day"]
    assert all(t[f"신청일 {p}"]["mapping"] == "application_date" and t[f"신청일 {p}"]["type"] == "text" for p in ("연도", "월", "일"))


def test_birth_date_and_period_parts_never_link_to_the_application_date():
    assert all(f["mapping"] is None and "part" not in f for f in run("mixed")["fields"] if inside(f, "mixed", "range"))
    birth = pf.make(0, pf.Box(0, 0, 20, 10), "생년월일 연도", "text", part=True, date_part="year4")
    assert birth.mapping is None and birth.date_part is None
    tok = lambda text, x: {"text": text, "x0": x, "x1": x + 5, "top": 0, "bottom": 10}
    assert pf.one_date([tok("년", 10), tok("월", 30), tok("일", 50)])
    assert not pf.one_date([tok("년", 10), tok("월", 30), tok("일", 50), tok("~", 60), tok("년", 80), tok("월", 100), tok("일", 120)])


def test_a_bare_date_line_is_the_applicants_only_above_their_own_signing_line():
    line = lambda top, *texts: [{"text": t, "x0": 10 + 60 * i, "x1": 50 + 60 * i, "top": top, "bottom": top + 10} for i, t in enumerate(texts)]
    assert pf.signs_next([line(100, "년", "월", "일"), line(130, "신청인", "(서명 또는 인)")], 110)
    assert not pf.signs_next([line(100, "년", "월", "일"), line(130, "보호자", "(서명 또는 인)")], 110)
    assert not pf.signs_next([line(100, "년", "월", "일"), line(130, "원주시장 귀하"), line(160, "신청인", "(서명)")], 110)
    assert not pf.signs_next([line(100, "년", "월", "일"), line(300, "신청인", "(서명)")], 110)


def _printed_mark(name, text):
    import pdfplumber
    with pdfplumber.open(FIX / f"{name}.pdf") as pdf:
        page = pdf.pages[0]
        tok = next(t for t in pf.tokens(page.chars) if pf.compact(t["text"]) == text)
        return pf.Box(tok["x0"] / page.width, tok["top"] / page.height, tok["x1"] / page.width, tok["bottom"] / page.height)


def _overlaps(r, b):
    return min(r["x"] + r["w"], b.x1) > max(r["x"], b.x0) and min(r["y"] + r["h"], b.bottom) > max(r["y"], b.top)


def test_signature_box_sits_beside_the_printed_mark_clear_of_the_name():
    t = by_label(run("basic"))
    mark = _printed_mark("basic", "(서명또는인)")
    sign, name = t["신청인 서명"]["rect"], t["신청인"]["rect"]
    assert not _overlaps(sign, mark), "a signature drawn over '(서명 또는 인)' leaves both unreadable"
    assert not _overlaps(name, mark) and not _overlaps(sign, pf.Box(name["x"], name["y"], name["x"] + name["w"], name["y"] + name["h"]))
    # A seal is still pressed over its printed (인).
    assert _overlaps(by_label(run("header"))["신청인 (인)"]["rect"], _printed_mark("header", "(인)"))



def test_with_no_room_right_of_the_mark_the_signature_shares_the_left_and_the_name_keeps_its_width():
    glyph = lambda text, x0, x1: {"text": text, "x0": x0, "x1": x1, "top": 100, "bottom": 110}
    mark = {"text": "(서명또는인)", "x0": 200, "x1": 260, "top": 100, "bottom": 110}
    label = glyph("인", 130, 140)
    chars = [label] + [glyph(c, 200 + 10 * i, 210 + 10 * i) for i, c in enumerate("(서명또는인)")]
    sign = pf.signature_box(mark, chars, pf.Box(50, 0, 262, 800))
    assert sign.x1 <= mark["x0"], "never over the mark"
    assert sign.x0 - (label["x1"] + 2) - 1 >= pf.NAME_MIN, "the name box beside the label keeps its minimum width"
    # Another person's seal ends up a signature box (enforce_invariants), so it is placed like one.
    seal = pf.mark_box("stamp", "임대인", mark, chars, None, pf.Box(50, 0, 262, 800))
    assert seal.x1 <= mark["x0"] or seal.x0 >= mark["x1"]
    assert pf.mark_box("stamp", "신청인", mark, chars, None, pf.Box(50, 0, 262, 800)).x0 < mark["x1"], "the applicant's seal stays over (인)"


def test_a_box_over_printed_text_refuses_the_template():
    over_title = {"id": "x", "page": 0, "rect": {"x": 0.36, "y": 0.045, "w": 0.2, "h": 0.03}, "type": "signature", "label": "신청인 서명", "mapping": "signature", "required": True}
    with pytest.raises(ValueError, match="box over printed text"):
        pf.propose(FIX / "basic.pdf", {"add": [over_title]})


def test_checkbox_groups_are_written_into_the_template():
    t = run("header")
    assert t["groups"] == [{"id": "g1", "mode": "single", "label": "개인정보 수집·이용 동의"}]
    assert sorted(f["label"] for f in t["fields"] if f.get("group") == "g1") == ["개인정보 수집·이용 동의: 동의하지 않음", "개인정보 수집·이용 동의: 동의함"]
    assert "groups" not in run("basic"), "no checkboxes, no empty groups list"
    # Added by an override, grouped all the same: groups come from the final boxes.
    extra = [{"id": f"x{i}", "page": 0, "rect": {"x": x, "y": 0.9, "w": 0.02, "h": 0.015}, "type": "checkbox", "label": f"문자 수신 {o}", "mapping": None, "required": False}
             for i, (x, o) in enumerate(((0.3, "동의함"), (0.5, "동의안함")))]
    t, _ = pf.propose(FIX / "basic.pdf", {"add": extra})
    assert t["groups"] == [{"id": "g1", "mode": "single", "label": "문자 수신"}]


def test_ids_are_unique_and_in_reading_order():
    t = run("basic")
    ids = [f["id"] for f in t["fields"]]
    assert len(ids) == len(set(ids))
    ys = [f["rect"]["y"] for f in t["fields"]]
    assert ys == sorted(ys, key=lambda y: round(y * 200))


def test_overrides_drop_set_and_add_by_page_and_label():
    t, _ = pf.propose(FIX / "basic.pdf", {
        "drop": [{"page": 0, "label": "계좌번호"}],
        "set": [{"match": {"page": 0, "label": "주소"}, "fields": {"type": "multiline"}}],
        "add": [{"id": "x", "page": 0, "rect": {"x": 0.1, "y": 0.9, "w": 0.2, "h": 0.03}, "type": "text", "label": "비고", "mapping": None, "required": False}],
    })
    labels = by_label(t)
    assert "계좌번호" not in labels and labels["주소"]["type"] == "multiline" and "비고" in labels
    with pytest.raises(SystemExit):
        pf.propose(FIX / "basic.pdf", {"set": [{"match": {"page": 0, "label": "없는칸"}, "fields": {}}]})


def test_label_inside_its_own_cell_never_borrows_the_cell_below():
    t = run("mixed")
    name = [f for f in t["fields"] if f["mapping"] == "name" and inside(f, "mixed", "inline_name")]
    rrn = [f for f in t["fields"] if f["mapping"] == "rrn" and inside(f, "mixed", "inline_rrn")]
    assert len(name) == 1 and len(rrn) == 1
    assert not any(f["mapping"] == "name" and inside(f, "mixed", "inline_rrn") for f in t["fields"]), "a name box in the RRN row"


def test_two_bullet_questions_on_one_line_end_at_the_next_bullet():
    t = by_label(run("mixed"))
    name, birth = t["성명"], t["생년월일"]
    assert name["rect"]["x"] + name["rect"]["w"] < birth["rect"]["x"]
    assert birth["mapping"] == "birth_date"


def test_circle_or_cross_and_numbered_options_become_checkboxes():
    t = run("mixed")
    ox = [f for f in t["fields"] if inside(f, "mixed", "ox")]
    assert sorted(f["label"] for f in ox) == ["주거급여 수급여부: ×(아니오)", "주거급여 수급여부: ○(예)"]
    numbered = [f for f in t["fields"] if inside(f, "mixed", "numbered")]
    assert [f["label"] for f in sorted(numbered, key=lambda f: f["rect"]["x"])] == ["구직등록여부: 등록", "구직등록여부: 미등록"]
    assert all(f["type"] == "checkbox" and f["mapping"] is None for f in ox + numbered)


def test_printed_prefix_leaves_the_rest_of_the_cell():
    f = by_label(run("mixed"))["휴대전화"]
    assert inside(f, "mixed", "prefix") and f["rect"]["x"] > (LAYOUT["mixed"]["cells"]["prefix"][0] + 20) / LAYOUT["mixed"]["page"][0]


def test_dotted_range_is_split_into_start_and_end_parts():
    labels = {f["label"] for f in run("mixed")["fields"] if inside(f, "mixed", "range")}
    assert {"임대차기간 시작 월", "임대차기간 시작 일", "임대차기간 종료 월", "임대차기간 종료 일"} <= labels
    assert all(f["mapping"] is None for f in run("mixed")["fields"] if inside(f, "mixed", "range"))


def test_member_list_rows_are_never_linked_to_the_profile_but_stay_sensitive():
    t = run("mixed")
    for r in (1, 2, 3):
        names = [f for f in t["fields"] if inside(f, "mixed", f"member_name{r}")]
        rrns = [f for f in t["fields"] if inside(f, "mixed", f"member_rrn{r}")]
        assert len(names) == 1 and names[0]["mapping"] is None and not names[0]["required"]
        assert len(rrns) == 1 and rrns[0]["mapping"] == "rrn"


@pytest.mark.parametrize("label,kind", [
    ("성 명", "name"), ("주민등록번호", "rrn"), ("입금 계좌", "account"), ("예금주", "account_holder"),
    ("휴대전화", "phone"), ("E-mail", "email"), ("학교명", "school"), ("신청인 서명", "signature"), ("비고", None),
    ("주민등록주소", "address"), ("주민등록상 주소", "address"), ("주민등록상 거주지", "address"),
])
def test_classify_matches_the_app_vocabulary(label, kind):
    assert pf.classify(label) == kind


def test_overrides_cannot_link_another_persons_box_to_the_profile():
    t, _ = pf.propose(FIX / "basic.pdf", {"add": [
        {"id": "a", "page": 0, "rect": {"x": 0.1, "y": 0.8, "w": 0.2, "h": 0.03}, "type": "text", "label": "보호자 연락처", "mapping": "phone", "required": False},
        {"id": "b", "page": 0, "rect": {"x": 0.4, "y": 0.8, "w": 0.05, "h": 0.03}, "type": "stamp", "label": "임대인 (인)", "mapping": "stamp", "required": False},
        {"id": "c", "page": 0, "rect": {"x": 0.5, "y": 0.8, "w": 0.2, "h": 0.03}, "type": "text", "label": "보호자 주민등록번호", "mapping": "rrn", "required": False, "format": "YYYY.MM.DD"},
    ]})
    f = by_label(t)
    assert f["보호자 연락처"]["mapping"] is None
    # The app suggests the user's own name for an unrecognised seal; someone else signs instead.
    seal = f["임대인 서명(또는 날인)"]
    assert seal["type"] == "signature" and seal["mapping"] == "signature" and pf.classify(seal["label"]) == "signature"
    assert f["보호자 주민등록번호"]["mapping"] == "rrn" and "format" not in f["보호자 주민등록번호"]


def test_labels_drop_invisible_format_characters():
    assert pf.clean_label("성‮명​") == "성명"


def _hwp_batch(tmp_path, verdict, overrides=None, pdf_name="basic"):
    """One HWP source whose 'converted PDF' is a fixture; returns (manifest, out dir)."""
    raw, derived, out, review, ov = (tmp_path / d for d in ("raw", "derived", "out", "review", "ov"))
    for d in (raw, derived, ov):
        d.mkdir()
    source = b"\xd0\xcf\x11\xe0 fake HWP bytes for the test"
    (raw / "f.hwp").write_bytes(source)
    pdf = (FIX / f"{pdf_name}.pdf").read_bytes()
    (derived / "f.pdf").write_bytes(pdf)
    src_sha, pdf_sha = hashlib.sha256(source).hexdigest(), hashlib.sha256(pdf).hexdigest()
    sources = [{"slug": "f", "program": "p", "agency": "a", "notice_url": "https://x.go.kr/n", "file_url": "https://x.go.kr/f.hwp",
                "retrieved_at": "2026-10-12T00:00:00Z", "format": "hwp", "sha256": src_sha, "byte_size": len(source)}]
    (tmp_path / "sources.json").write_text(json.dumps(sources))
    form = {"verdict": verdict, "source_sha256": src_sha, "pdf_sha256": pdf_sha, "pages": [{"page": 1, "evidence": "preview"}],
            "reason": "text missing"}
    conv = {"converter": {"identity": "rhwp 0.8.6 test"}, "forms": {"f": form}}
    (tmp_path / "conv.json").write_text(json.dumps(conv))
    if overrides is not None:
        (ov / "f.json").write_text(json.dumps(overrides))
    pf.main(["batch", "--sources", str(tmp_path / "sources.json"), "--raw", str(raw), "--out", str(out), "--review", str(review),
             "--overrides", str(ov), "--conversions", str(tmp_path / "conv.json"), "--derived", str(derived)])
    return json.loads((out / "manifest.json").read_text()), out, src_sha, pdf_sha


def test_faithful_hwp_conversion_yields_a_template_bound_to_both_files(tmp_path):
    manifest, out, src_sha, pdf_sha = _hwp_batch(tmp_path, "faithful")
    entry = manifest["forms"][0]
    assert entry["status"] == "template_ready"
    t = json.loads((out / entry["template"]).read_text())
    assert t["content_hash"] == pdf_sha, "boxes sit on the converted PDF"
    assert t["derived"] == {"source_sha256": src_sha, "converter": "rhwp 0.8.6 test"}
    assert entry["sha256"] == src_sha and entry["derived_pdf"]["sha256"] == pdf_sha
    assert entry["derived_pdf"]["fidelity"] == {"1": "preview"}


def test_rejected_or_missing_hwp_conversion_yields_no_template(tmp_path):
    manifest, out, *_ = _hwp_batch(tmp_path, "rejected")
    entry = manifest["forms"][0]
    assert entry["status"] == "hwp_unfaithful" and "text missing" in entry["reason"]
    assert not list(out.glob("*.template.json"))


def test_hwp_template_pages_are_limited_to_the_judged_pages(tmp_path):
    with pytest.raises(SystemExit, match="did not accept"):
        _hwp_batch(tmp_path, "faithful", overrides={"pages": [0, 1]})


def test_a_reconverted_pdf_must_be_judged_again(tmp_path):
    _hwp_batch(tmp_path, "faithful")
    # conversions.json recorded basic.pdf; another PDF in --derived is refused.
    (tmp_path / "derived" / "f.pdf").write_bytes((FIX / "header.pdf").read_bytes())
    with pytest.raises(SystemExit, match="re-judge"):
        pf.main(["batch", "--sources", str(tmp_path / "sources.json"), "--raw", str(tmp_path / "raw"), "--out", str(tmp_path / "out"),
                 "--review", str(tmp_path / "review"), "--overrides", str(tmp_path / "ov"), "--conversions", str(tmp_path / "conv.json"),
                 "--derived", str(tmp_path / "derived")])


def test_date_part_boxes_say_which_part_and_a_year_after_printed_20_is_two_digits():
    t = by_label(run("basic"))  # '20      년      월      일'
    assert (t["작성일 연도"]["part"], t["작성일 월"]["part"], t["작성일 일"]["part"]) == ("year2", "month", "day")
    assert t["작성일 연도"]["mapping"] == "application_date", "the app suggests today's part"
    s = by_label(run("section"))  # '      년      월      일': no printed 20
    assert s["신청일 연도"]["part"] == "year4"
