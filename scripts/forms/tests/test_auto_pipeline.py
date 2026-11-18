"""Decision rules of the daily auto pipeline (scripts/forms/auto): which collector rows are
tried, and whether a form belongs to its programme. Pure functions, no files or network.

Run: uv run --with pdfplumber==0.11.9 --with pytest==8.3.3 pytest scripts/forms/tests -q
"""
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "auto"))
import match  # noqa: E402
import choose as auto_select  # noqa: E402

NOW = datetime(2026, 10, 16, 9, 0, tzinfo=timezone.utc)
H = "a" * 64


def row(**kw):
    base = dict(file_format="hwp", available=True, in_approved_scope=True, status="candidate", check_status="ok",
                check_error=None, last_failure_code=None, template_revision=None, template_source_sha256=None,
                template_content_hash=None, content_hash=H, label="신청서 및 제출서류(서식).hwp", programs_sharing_url=1,
                closed_programs_sharing_url=0, benefit_valid_until=None, deadline_at=None,
                application_end_at=(NOW + timedelta(days=30)).isoformat())
    base.update(kw)
    return base


def verdict(**kw):
    return auto_select.decide(row(**kw), NOW)[0]


def test_module_is_the_pipeline_select_not_stdlib():
    assert hasattr(auto_select, "decide")


def test_open_window_single_program_form_is_tried():
    assert verdict() == "try"


def test_pdf_hwp_and_hwpx_are_tried_other_formats_are_not():
    assert verdict(file_format="hwpx") == "try"
    assert verdict(file_format="hwp") == "try"
    assert verdict(file_format="docx") == "reject"
    assert verdict(file_format="pdf") == "try"


def test_closed_program_and_outside_scope_are_rejected():
    assert verdict(available=False) == "reject"
    assert verdict(in_approved_scope=False) == "reject"


def test_collector_must_have_probed_ok():
    assert verdict(check_status="blocked") == "reject"
    assert verdict(check_status=None) == "reject"
    assert verdict(check_error="http 500") == "reject"
    assert verdict(status="verified", check_status=None) == "try"  # a review resets check_status


def test_live_template_on_same_bytes_is_not_redone_but_a_changed_file_is():
    assert verdict(template_revision=1, template_source_sha256=H) == "live"
    assert verdict(template_revision=1, template_source_sha256="b" * 64) == "try"


def test_board_wide_and_earlier_round_attachments_are_rejected():
    assert verdict(programs_sharing_url=8) == "reject"
    assert verdict(closed_programs_sharing_url=1) == "reject"
    assert verdict(label="(서식1) 신원진술서 제20호서식(동의서포함).hwp") == "reject"
    assert verdict(label="응시원서 등 제출서류(제5회 및 제6회).hwp") == "reject"


def test_intake_must_be_dated_and_open():
    assert verdict(application_end_at=None) == "reject"
    assert verdict(application_end_at=(NOW - timedelta(days=1)).isoformat()) == "reject"
    # the earliest of the three end dates decides
    assert verdict(deadline_at=(NOW - timedelta(hours=1)).isoformat()) == "reject"


def belongs(heading, programme):
    return match.shares_word(match.words(heading), match.words(programme)) or len(match.bigrams(heading) & match.bigrams(programme)) >= 3


def test_form_heading_must_share_a_distinctive_word_with_the_programme():
    assert belongs("2026년 원주시청년지원센터 면접정장 무료대여 신청서", "청년 면접 정장 지원")
    assert belongs("「원주시 청년지원센터」 AI 면접 및 역량검사 체험 지원 신청서", "AI 면접 및 역량 검사 체험 지원")
    assert belongs("2026년 청년 재직자 슈퍼패스 바우처 사업 신청서", "청년 재직자 슈퍼패스 바우처")


def test_generic_words_alone_do_not_make_a_match():
    assert not belongs("2025년 공공기관 맞춤형 취업지원사업 신청서", "청년 재직자 슈퍼패스 바우처")
    assert not belongs("신 원 진 술 서", "결혼축하금 지원사업")
    assert not belongs("청년 지원 사업 신청서", "청년 월세 지원 사업")


import groups  # noqa: E402  (scripts/forms/auto/groups.py)


def test_labels_split_into_question_and_option():
    assert groups.split_label("구분 취업준비생: 고등학생") == ("구분 취업준비생", "고등학생")
    assert groups.split_label("개인정보 수집·이용 동의함") == ("개인정보 수집·이용", "동의함")
    assert groups.split_label("개인정보 수집·이용 동의안함") == ("개인정보 수집·이용", "동의안함")
    assert groups.split_label("동의함") == ("", "동의함")
    assert groups.split_label("대학교 졸업생") == ("", "대학교 졸업생")


def test_negation_pairs_are_clear_binaries_other_pairs_are_not():
    for a, b in [("동의", "비동의"), ("등록", "미등록"), ("수혜", "미수혜"), ("희망", "희망하지 않음"), ("동의함", "동의하지 않음"), ("여", "부")]:
        assert groups.negation_pair(a, b), (a, b)
    for a, b in [("신청", "변경"), ("직장", "지역"), ("부", "모"), ("졸업(예정)", "중퇴")]:
        assert not groups.negation_pair(a, b), (a, b)


def test_only_explicit_wording_allows_several_answers():
    assert groups.MULTI.search("해당되는 항목에 모두 체크")
    assert groups.MULTI.search("복수 선택 가능")
    assert groups.MULTI.search("중복 체크 가능")
    assert not groups.MULTI.search("주거급여 중복신청 여부")
    assert not groups.MULTI.search("모두의 카드")


def test_one_answer_headings():
    assert groups.SINGLE_HEAD.search("구분")
    assert groups.SINGLE_HEAD.search("신청 구분")
    assert groups.SINGLE_HEAD.search("신청유형")
    assert not groups.SINGLE_HEAD.search("선택사항 수집·이용 동의")



def test_only_groups_with_evidence_go_into_the_template_and_meet_the_server_rules():
    box = lambda i, page=0, kind="checkbox": {"id": i, "page": page, "type": kind, "label": i, "mapping": None, "required": False, "rect": {"x": 0, "y": 0, "w": 0.01, "h": 0.01}}
    template = {"fields": [box("a"), box("b"), box("c"), box("d"), box("e"), box("f", 1), box("g"), box("h", kind="text"), box("i"), box("j")]}
    g = lambda q, opts, members, mode="single", flag=None: {"question": q, "options": opts, "members": members, "mode": mode, "review_flag": flag}
    result = {"groups": [
        g("개인정보 수집·이용", ["동의함", "동의안함"], ["a", "b"]),
        g("관심 분야", ["주거", "일자리"], ["c", "d"], "multi", "uncertain"),  # a guess: stays out
        g("(같은 줄)", ["예", "아니오"], ["e", "f"]),                        # two pages: refused
        g("구분", ["가", "나"], ["g", "h"]),                                  # a text box: refused
        g("(같은 줄)", ["예", "아니오"], ["i", "j"], "single", "merged_rows"),
    ]}
    t = groups.into_template(template, result)
    assert t["groups"] == [{"id": "g1", "mode": "single", "label": "개인정보 수집·이용"}, {"id": "g2", "mode": "single", "label": "예 / 아니오"}]
    assert {f["id"]: f.get("group") for f in t["fields"]} == {"a": "g1", "b": "g1", "c": None, "d": None, "e": None, "f": None, "g": None, "h": None, "i": "g2", "j": "g2"}
    assert "groups" not in groups.into_template({"fields": [box("a")]}, {"groups": []})


def test_postgres_timestamps_with_trimmed_fractions_parse_on_python_39():
    assert auto_select._ts("2026-11-30T09:00:00.12345+00:00").microsecond == 123450
    assert auto_select._ts("2026-11-30T09:00:00+00:00").hour == 9


def test_stale_live_template_is_redrawn_only_if_the_new_file_passes_every_rule():
    assert verdict(template_revision=1, template_source_sha256="b" * 64, programs_sharing_url=8) == "reject"
    assert verdict(template_revision=1, template_source_sha256="b" * 64, application_end_at=None) == "reject"


import hashlib  # noqa: E402
import json  # noqa: E402

import pytest  # noqa: E402

import verdicts  # noqa: E402


def _run(tmp_path, verdict_obj, tamper=False, slots=None, judge_obj=None, mode=None, monkeypatch=None):
    """A minimal run dir with one candidate that passed every machine step."""
    R = tmp_path
    (R / "out").mkdir(); (R / "rxcheck").mkdir(); (R / "keys").mkdir(); (R / "review").mkdir(); (R / "rderive" / "slots").mkdir(parents=True)
    (R / "rderive" / "slots" / "s.slots.json").write_text(json.dumps(slots if slots is not None else {"findings": [], "questions": []}))
    if judge_obj is not None:
        (R / "review" / "batch.judge.json").write_text(json.dumps(judge_obj))
    if monkeypatch is not None:
        monkeypatch.setenv("FORM_REVIEW_JUDGE", mode or "shadow")
    tpl = R / "out" / "s.template.json"
    tpl.write_text('{"fields": []}')
    row = {"id": "r1", "program_id": "p1", "program_title": "t", "host": "h", "label": "l", "content_hash": "c",
           "window_end": None, "verdict": "try", "reason": "ok"}
    (R / "candidates.json").write_text(json.dumps({"rows": [row]}))
    (R / "sources.json").write_text(json.dumps([{"resource_id": "r1", "slug": "s", "format": "pdf"}]))
    (R / "out" / "manifest.json").write_text(json.dumps({"forms": [{"slug": "s", "status": "template_ready", "field_count": 1, "templated_pages": [1]}]}))
    (R / "rderive" / "match.json").write_text(json.dumps({"s": {"pass": True}}))
    (R / "rxcheck" / "s.gates.json").write_text(json.dumps({"pass": True}))
    sha = hashlib.sha256(tpl.read_bytes()).hexdigest()
    (R / "keys" / "batch.key.json").write_text(json.dumps({"_nonce": "n0", "A": {"slug": "s", "template": str(tpl), "template_sha256": sha}}))
    if verdict_obj is not None:
        (R / "review" / "batch.verdict.json").write_text(json.dumps(verdict_obj))
    if tamper:
        tpl.write_text('{"fields": [1]}')
    verdicts.main(str(R))
    return json.loads((R / "out" / "import-manifest.json").read_text())["forms"]


def test_reviewed_pass_with_nonce_is_importable(tmp_path):
    assert len(_run(tmp_path, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}})) == 1


def test_import_manifest_carries_the_review_as_verifier(tmp_path):
    form = _run(tmp_path, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}})[0]
    r = form["review"]
    assert r["reviewer"] == "claude-reviewer" and len(r["verdict_sha256"]) == 64 and len(r["template_sha256"]) == 64


def test_a_slot_finding_blocks_import_even_when_the_reviewer_passes(tmp_path):
    slots = {"findings": [{"page": 1, "box": None, "kind": "missing", "detail": "x"}], "questions": []}
    assert _run(tmp_path, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}}, slots=slots) == []


def test_missing_slot_check_blocks_import(tmp_path):
    # An absent slots file is a failed check, never a pass.
    R = tmp_path
    forms = _run(R, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}})
    (R / "rderive" / "slots" / "s.slots.json").unlink()
    verdicts.main(str(R))
    assert forms and json.loads((R / "out" / "import-manifest.json").read_text())["forms"] == []


def test_shadow_judge_is_logged_but_never_decides(tmp_path, monkeypatch):
    forms = _run(tmp_path, {"nonce": "n0", "A": {"verdict": "fail", "findings": [{"kind": "missing"}]}},
                 judge_obj={"nonce": "n0", "A": {"verdict": "pass", "findings": []}}, mode="shadow", monkeypatch=monkeypatch)
    assert forms == []
    shadow = json.loads((tmp_path / "verdicts.json").read_text())["shadow"]
    assert shadow["judge_false_passes"] == 1 and shadow["n"] == 1


def test_judge_mode_is_refused_without_a_passed_gate(tmp_path, monkeypatch):
    with pytest.raises(SystemExit, match="refused"):
        _run(tmp_path, None, judge_obj={"nonce": "n0", "A": {"verdict": "pass", "findings": []}}, mode="judge", monkeypatch=monkeypatch)


def test_unknown_review_mode_stops(tmp_path, monkeypatch):
    with pytest.raises(SystemExit, match="FORM_REVIEW_JUDGE"):
        _run(tmp_path, None, mode="auto", monkeypatch=monkeypatch)


def test_template_changed_after_review_is_not_importable(tmp_path):
    assert _run(tmp_path, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}}, tamper=True) == []


def test_missing_verdict_is_not_importable(tmp_path):
    assert _run(tmp_path, None) == []


def test_wrong_nonce_or_malformed_verdict_stops(tmp_path):
    with pytest.raises(SystemExit):
        _run(tmp_path, {"nonce": "old", "A": {"verdict": "pass", "findings": []}})


def test_pass_with_findings_is_malformed(tmp_path):
    with pytest.raises(SystemExit):
        _run(tmp_path, {"nonce": "n0", "A": {"verdict": "pass", "findings": ["x"]}})


def test_fidelity_gate_reads_hwpx_sources(tmp_path):
    """An HWPX source is judged like an HWP one: its own text comes from Contents/section*.xml."""
    pytest.importorskip("olefile")
    pytest.importorskip("pypdfium2")
    import zipfile
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "hwp"))
    import fidelity
    src = tmp_path / "a.hwpx"
    with zipfile.ZipFile(src, "w") as zf:
        zf.writestr(zipfile.ZipInfo("mimetype"), "application/hwp+zip")
        zf.writestr("Contents/section0.xml", '<hs:sec xmlns:hs="s" xmlns:hp="p"><hp:p><hp:run><hp:t>우울증 치료비 신청서</hp:t></hp:run></hp:p></hs:sec>')
    preview, (runs, pages) = fidelity.preview_of(src)
    assert preview is None and runs == ["우울증 치료비 신청서"] and pages == 1
    gate = (Path(__file__).resolve().parents[1] / "auto/fidelity_gate.py").read_text()
    assert 'in ("hwp", "hwpx")' in gate and "s['format']" in gate


def test_hwpx_zip_bombs_are_refused_before_inflating(tmp_path):
    pytest.importorskip("olefile")
    pytest.importorskip("pypdfium2")
    import zipfile
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "hwp"))
    import fidelity
    bomb = tmp_path / "bomb.hwpx"
    with zipfile.ZipFile(bomb, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(zipfile.ZipInfo("mimetype"), "application/hwp+zip")
        zf.writestr("Contents/section0.xml", b"<a>" + b" " * (20 * 1024 * 1024) + b"</a>", compress_type=zipfile.ZIP_DEFLATED)
    with pytest.raises(ValueError, match="too large"):
        fidelity.preview_of(bomb)


def test_spacing_a_leading_youth_word_and_short_words_do_not_break_a_match():
    # Same rule as the collector (discovery.rs related): review H2 cases.
    assert belongs("2026년 청년월세 지원사업 신청서", "청년 월세 한시 특별지원")
    assert belongs("청년 월세 지원 신청서", "청년월세 한시 특별지원")
    assert "한시" in match.words("한시 특별 생활비") and "연구" in match.words("연구 장려")
    assert not match.words("완주군청 중구")


def test_hwp_preview_decodes_only_plain_raster_formats():
    """The embedded preview is agency bytes: TIFF (and FlashPix, JPEG 2000 …) never reach a decoder."""
    pytest.importorskip("olefile")
    import io as _io
    from PIL import Image
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "hwp"))
    import fidelity
    png, tif = _io.BytesIO(), _io.BytesIO()
    Image.new("RGB", (4, 4)).save(png, "PNG")
    Image.new("RGB", (4, 4)).save(tif, "TIFF")
    assert fidelity.decode_preview(png.getvalue()).size == (4, 4)
    with pytest.raises(Exception):
        fidelity.decode_preview(tif.getvalue())
    with pytest.raises(ValueError, match="too large"):
        fidelity.decode_preview(b"\x89PNG" + b"0" * (fidelity.MAX_PREVIEW_BYTES + 1))


def test_bytes_an_operator_rolled_back_are_never_tried_again():
    assert verdict(rolled_back=True) == "reject"
    assert verdict(rolled_back=False) == "try"


def test_a_malformed_shadow_judge_file_never_blocks_the_reviewer(tmp_path, monkeypatch):
    monkeypatch.setenv("FORM_REVIEW_JUDGE", "shadow")
    R = tmp_path
    forms = _run(R, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}}, judge_obj={"nonce": "WRONG"})
    assert len(forms) == 1
    assert "shadow_error" in json.loads((R / "verdicts.json").read_text())


def test_state_dir_holds_the_deciding_files_on_the_server(tmp_path, monkeypatch):
    """With --state, a verdict planted in the run dir does not count; the state dir's does."""
    monkeypatch.setenv("FORM_REVIEW_JUDGE", "shadow")
    R = tmp_path / "run"
    R.mkdir()
    _run(R, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}})  # local layout: importable
    S = tmp_path / "state"
    for d in ("keys", "verdict", "judge", "out"):
        (S / d).mkdir(parents=True)
    (S / "keys" / "batch.key.json").write_text((R / "keys" / "batch.key.json").read_text())
    verdicts.main(str(R), str(S))
    assert json.loads((S / "out" / "import-manifest.json").read_text())["forms"] == [], "run-dir verdict ignored"
    (S / "verdict" / "batch.verdict.json").write_text(json.dumps({"nonce": "n0", "A": {"verdict": "pass", "findings": []}}))
    verdicts.main(str(R), str(S))
    forms = json.loads((S / "out" / "import-manifest.json").read_text())["forms"]
    assert len(forms) == 1 and forms[0]["review"]["verdict_file"].endswith("state/verdict/batch.verdict.json")


def test_judge_mode_with_nothing_to_review_is_a_clean_no_op(tmp_path, monkeypatch, capsys):
    """Most runs have no template that reaches review (28 of 30 held-out items): no packet key,
    no judge file. That is a finished run with nothing to import, not a refused one."""
    monkeypatch.setenv("FORM_REVIEW_JUDGE", "judge")
    R = tmp_path / "run"
    R.mkdir()
    (R / "candidates.json").write_text(json.dumps({"rows": []}))
    (R / "sources.json").write_text("[]")
    S = tmp_path / "state"
    for d in ("keys", "verdict", "judge", "out"):
        (S / d).mkdir(parents=True)
    verdicts.main(str(R), str(S))  # no SystemExit
    out = json.loads((S / "out" / "verdicts.json").read_text())
    assert out["nothing_to_review"] is True and out["decider"] == "none"
    assert json.loads((S / "out" / "import-manifest.json").read_text())["forms"] == []
    assert "nothing to review" in capsys.readouterr().out
    # With a packet key the gate still applies.
    (S / "keys" / "batch.key.json").write_text(json.dumps({"_nonce": "n0"}))
    with pytest.raises(SystemExit, match="refused"):
        verdicts.main(str(R), str(S))


def test_what_the_parse_container_writes_never_decides(tmp_path):
    """Trust boundary (steps.sh): the parse container parses agency files with pdfium, so its
    slots/, match.json and gates/ may be forged. Only the derive and xcheck containers decide."""
    R = tmp_path
    ok = {"nonce": "n0", "A": {"verdict": "pass", "findings": []}}
    assert len(_run(R, ok)) == 1
    # A forged clean slot check and match in the parse locations do not help ...
    (R / "slots").mkdir()
    (R / "slots" / "s.slots.json").write_text(json.dumps({"findings": [], "questions": []}))
    (R / "match.json").write_text(json.dumps({"s": {"pass": True}}))
    (R / "gates").mkdir()
    (R / "gates" / "s.gates.json").write_text(json.dumps({"pass": True}))
    # ... when the derived slot check found something,
    (R / "rderive" / "slots" / "s.slots.json").write_text(json.dumps({"findings": [{"kind": "missing"}], "questions": []}))
    verdicts.main(str(R))
    assert json.loads((R / "out" / "import-manifest.json").read_text())["forms"] == []
    # ... or the derived match failed,
    (R / "rderive" / "slots" / "s.slots.json").write_text(json.dumps({"findings": [], "questions": []}))
    (R / "rderive" / "match.json").write_text(json.dumps({"s": {"pass": False, "problems": ["foreign form"]}}))
    verdicts.main(str(R))
    assert json.loads((R / "out" / "import-manifest.json").read_text())["forms"] == []
    # ... or the cross-checked gates failed.
    (R / "rderive" / "match.json").write_text(json.dumps({"s": {"pass": True}}))
    (R / "rxcheck" / "s.gates.json").write_text(json.dumps({"pass": False, "problems": {"renderers": ["disagree"]}}))
    verdicts.main(str(R))
    assert json.loads((R / "out" / "import-manifest.json").read_text())["forms"] == []


def test_the_judge_reads_only_the_derived_slot_check(tmp_path, monkeypatch):
    import judge

    class Yes:
        def identity(self):
            return {"model": "m", "digest": "d", "prompt": judge.PROMPT_VERSION}

        def ask(self, kind, msgs):
            return "yes", 0.0, {}

    R = tmp_path
    (R / "keys").mkdir(); (R / "review").mkdir(); (R / "out").mkdir(); (R / "slots").mkdir(); (R / "rderive" / "slots").mkdir(parents=True)
    tpl = R / "out" / "s.template.json"
    tpl.write_text("{}")
    sha = hashlib.sha256(tpl.read_bytes()).hexdigest()
    (R / "keys" / "batch.key.json").write_text(json.dumps({"_nonce": "n0", "A": {"slug": "s", "template": str(tpl), "template_sha256": sha}}))
    (R / "sources.json").write_text(json.dumps([{"slug": "s", "program": "p"}]))
    (R / "slots" / "s.slots.json").write_text(json.dumps({"findings": [], "questions": []}))  # forged by parse
    (R / "match.json").write_text(json.dumps({"s": {"heading": "p"}}))
    (R / "rderive" / "slots" / "s.slots.json").write_text(json.dumps({"findings": [{"page": 1, "box": None, "kind": "missing", "detail": "x"}], "questions": []}))
    monkeypatch.setattr(judge, "client_from_env", lambda: Yes())
    judge.cmd_batch(str(R))
    assert json.loads((R / "review" / "batch.judge.json").read_text())["A"]["verdict"] == "fail"


def test_a_gate_failure_is_reported_as_gates_not_as_a_programme_mismatch(tmp_path):
    """The derive stage (programme match) runs only for templates that passed the
    cross-checked gates, so a gate failure has no match result. The funnel must name the
    gates, the stage that actually stopped it."""
    R = tmp_path
    _run(R, {"nonce": "n0", "A": {"verdict": "pass", "findings": []}})
    (R / "rxcheck" / "s.gates.json").unlink()
    (R / "rderive" / "match.json").write_text("{}")
    verdicts.main(str(R))
    row = [r for r in json.loads((R / "verdicts.json").read_text())["rows"] if r.get("slug") == "s"][0]
    assert row["stage"] == "gates", row
