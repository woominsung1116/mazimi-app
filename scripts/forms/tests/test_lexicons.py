"""No new word lists in the deterministic review (user rule: decide from structure, or ask the
judge). The lists the review named were removed (APPLICANT, SIGNER, HEADING, HINT,
SINGLE_WORDS, MULTI_WORDS in slots.py; INJECTION, CHOICE_WORDS in judge.py). What remains in
slots.py, judge.py and the field invariants (invariants.py) is frozen here: adding a constant or extending one fails this test, so a new phrasing on a form
cannot be handled by appending a word.
"""
import ast
import hashlib
import re
import sys
from pathlib import Path

AUTO = Path(__file__).resolve().parents[1] / "auto"
sys.path.insert(0, str(AUTO))
import invariants  # noqa: E402
import judge  # noqa: E402
import slots  # noqa: E402

FROZEN = {
    ("slots", "APPLICANT_CONTEXT"): "3ac410c508de5022",
    ("slots", "APPLICANT_KEYS"): "1f38c64055ac8c5b",
    ("slots", "BOX_GLYPHS"): "390c0c6a2fe8add2",
    ("slots", "COLLEGE_SUFFIX"): "3817318381b73837",
    ("slots", "CONSENT_WORDS"): "0432a9e7b068530f",
    ("slots", "DANGLING"): "45bc4af3f631a1bd",
    ("slots", "FORM_NAME"): "b870809d6ba100a7",
    ("slots", "GLYPH_MENTION"): "e4f0e9135425bbfa",
    ("slots", "HINT_MARK"): "aacc6b909d7690e9",
    ("slots", "INSTRUCTION"): "db4f003be2d01a84",
    ("slots", "MARKUP"): "4e6e74ce5bc6df3d",
    ("slots", "OFFICE"): "425164cc24b87f4a",
    ("slots", "PRINTING_AID"): "5d1332285d6aa0d3",
    ("slots", "RADIO"): "3b12f36ad57e44ae",
    ("slots", "SUFFIX_WORDS"): "c58e0dee34628cae",
    ("slots", "TEXT_KINDS"): "da9a3bacf231ef91",
    ("slots", "TITLE_BRACKETS"): "01fc451070ea59dd",
    ("slots", "TOTAL"): "abaa321e42f9b837",
    ("slots", "UNITS"): "eb6fc936f94825d5",
    ("judge", "ALLOWED_HOSTS"): "3c72504484ee4659",
    ("judge", "ASCII_WORD"): "6de9afd23ff3b69a",
    ("invariants", "DECISIVE"): "0ba15a95b2857f19",
    ("invariants", "HANGUL"): "f0dae71f56267a4d",
    ("invariants", "TEXTUAL"): "5c106cba7e09fd3b",
}


def constants():
    out = {}
    for mod in (slots, judge, invariants):
        src = (AUTO / f"{mod.__name__}.py").read_text()
        for k, v in vars(mod).items():
            if k.isupper() and k not in ("PROMPT_VERSION", "SYSTEM") and isinstance(v, (re.Pattern, tuple, set, frozenset, str)) \
                    and re.search(rf"(?m)^{k}\s*=", src):
                text = v.pattern if isinstance(v, re.Pattern) else repr(sorted(v) if isinstance(v, (set, frozenset)) else v)
                out[(mod.__name__, k)] = hashlib.sha256(text.encode()).hexdigest()[:16]
    return out


def test_no_word_list_is_added_or_extended():
    got = constants()
    assert set(got) == set(FROZEN), f"new or removed constants: {set(got) ^ set(FROZEN)}"
    changed = [k for k in got if got[k] != FROZEN[k]]
    assert not changed, f"changed lexicons (decide from structure or ask the judge instead): {changed}"


def test_the_removed_lists_stay_removed():
    for mod, names in ((slots, ("APPLICANT", "SIGNER", "HEADING", "HINT", "SINGLE_WORDS", "MULTI_WORDS")),
                       (judge, ("INJECTION", "CHOICE_WORDS"))):
        for n in names:
            assert not hasattr(mod, n), n


# Every Korean string literal in the three modules, inline regexes and docstrings included: a new
# phrasing cannot be added inline either (the constants test above covers only named lists).
KOREAN_LITERALS = {"slots": (50, "a2d88dd95dfa56f9"), "judge": (0, "e3b0c44298fc1c14"), "invariants": (13, "1215197948b1884d")}


def korean_literals(name: str) -> list[str]:
    tree = ast.parse((AUTO / f"{name}.py").read_text())
    return sorted({n.value for n in ast.walk(tree) if isinstance(n, ast.Constant) and isinstance(n.value, str) and re.search(r"[가-힣]", n.value)})


def test_no_korean_word_is_added_inline():
    for name, (count, digest) in KOREAN_LITERALS.items():
        lits = korean_literals(name)
        got = (len(lits), hashlib.sha256("\x00".join(lits).encode()).hexdigest()[:16])
        assert got == (count, digest), f"{name}.py Korean literals changed ({got}): decide from structure or ask the judge"
