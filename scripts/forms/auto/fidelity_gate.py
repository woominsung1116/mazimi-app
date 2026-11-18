# /// script
# requires-python = ">=3.10"
# dependencies = ["olefile==0.47", "pypdfium2==4.30.0", "pdfplumber==0.11.9", "pillow>=10.4", "numpy>=1.26"]
# ///
"""Step 3: automatic HWP -> PDF fidelity verdict (replaces the hand-written conversions.json).

For each HWP/HWPX in sources.json, with the rhwp PDF (shipped) and the LibreOffice +
H2Orestart PDF (reference engine only) already made by scripts/forms/hwp/convert.sh:

  text       every character of the HWP's own text, counted, must be in the rhwp text layer
             (order-free: engines read vertical labels in different orders). A character the
             reference engine has more often than rhwp is a dropped line: reject.
  ink        every word in the rhwp text layer must be drawn (clipped text: reject).
  preview    page 1 against the HWP's embedded preview image (the only ground truth):
             dark-pixel agreement both ways within a 2 px tolerance, after scaling.
  pages      which pages are forms (a title such as 신청서/동의서/확인서 in the top third,
             ruled cells present) and which are notices or instructions; only form pages
             get boxes. Pages that are only someone else's (추천서, 위임장) are left out.

Writes conversions.json in the format propose_fields.py batch expects (verdict,
pages with evidence) plus the metrics, and per-page PNGs for the reviewer.
Thresholds were calibrated on the two live templates and the known-bad
wonju interview-suit conversion (see the handoff doc); change them only with
that calibration set re-run.

Usage: uv run --script scripts/forms/auto/fidelity_gate.py --sources S --raw DIR --rhwp DIR --h2o DIR --out conversions.json --png DIR
(run through `uv run --no-project --with … python -I`, see run.sh)
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "scripts/forms/hwp"))  # trusted repo code; -I keeps cwd/script dir out

import numpy as np  # noqa: E402
import pdfplumber  # noqa: E402
import pypdfium2 as pdfium  # noqa: E402
from PIL import Image, ImageFilter  # noqa: E402

import fidelity  # noqa: E402  (scripts/forms/hwp/fidelity.py)

Image.MAX_IMAGE_PIXELS = 20_000_000
MAX_PAGE_PT = 2000  # an A3 sheet is 1191 pt; anything far larger is not a form and would render to GiBs


def page_size_ok(pdf: Path) -> bool:
    doc = pdfium.PdfDocument(str(pdf))
    return all(max(doc[i].get_size()) <= MAX_PAGE_PT for i in range(len(doc))) and len(doc) <= 50

CONVERTER = json.loads((REPO / "scripts/forms/hwp/conversions.json").read_text())["converter"]
PREVIEW_MIN = 0.90  # share of dark pixels matched within 2 px, both directions
FORM_TITLE = re.compile(r"(신청서|동의서|확인서|서약서|신고서|계획서|소개서|지원서|명세서|청구서|각서|이행서|확약서)")
OTHERS_ONLY = re.compile(r"(추천서|위임장|추천인|재직증명|경력증명|사업주\s*확인)")
NOT_FORM = re.compile(r"(공\s*고|작성\s*요령|작성\s*방법|유의\s*사항|안내문|제출\s*서류\s*목록)")


def words_of(src: Path) -> tuple[list[str], str]:
    _, (runs, _) = fidelity.preview_of(src)
    toks = [fidelity.norm(w) for r in runs for w in r.split()]
    return [t for t in toks if len(t) >= 2], [fidelity.norm(r) for r in runs]


def pdf_text(pdf: Path) -> str:
    with pdfplumber.open(pdf) as doc:
        return fidelity.norm("".join(p.extract_text() or "" for p in doc.pages))


def dark(img: Image.Image) -> np.ndarray:
    return np.asarray(img.convert("L")) < 200


def rules(mask: np.ndarray, axis: int, min_frac: float) -> list[tuple[float, float, float]]:
    """Ruling lines as (position, start, end), all as fractions of the page.
    axis 0: horizontal lines (scan rows), axis 1: vertical lines (scan columns)."""
    m = mask if axis == 0 else mask.T
    n, length = m.shape
    found = []
    for i in range(n):
        row = m[i]
        best, run, start, best_span = 0, 0, 0, (0, 0)
        for j, v in enumerate(row):
            if v:
                if run == 0:
                    start = j
                run += 1
                if run > best:
                    best, best_span = run, (start, j)
            else:
                run = 0
        if best >= min_frac * length:
            found.append((i, best_span[0], best_span[1]))
    merged: list[list[float]] = []
    for i, a, b in found:  # a thick line spans several pixel rows
        if merged and i - merged[-1][1] <= 2 and abs(a - merged[-1][2]) < 0.02 * length:
            merged[-1][1] = i
        else:
            merged.append([i, i, a, b])
    return [((g[0] + g[1]) / 2 / n, g[2] / length, g[3] / length) for g in merged]


def matched(a: list, b: list, tol: float) -> float:
    """Share of lines in a with a line in b at about the same place and extent."""
    if not a:
        return 1.0
    hit = 0
    for pos, s, e in a:
        if any(abs(pos - p2) <= tol and min(e, e2) - max(s, s2) >= 0.7 * min(e - s, e2 - s2) for p2, s2, e2 in b):
            hit += 1
    return hit / len(a)


def preview_score(src: Path, pdf: Path) -> float | None:
    """Page 1 against the embedded preview: every long ruling line (table rows and
    columns, underlines) must appear in both, within 1.5% of the page. rhwp lays rows
    out about 1% taller than Hangul does, so a pixel diff is the wrong instrument;
    a missing or extra row or column is what makes a converted form wrong."""
    preview, _ = fidelity.preview_of(src)
    if preview is None:
        return None
    if preview.width > 3000 or preview.height > 3000:
        return 0.0
    first = pdfium.PdfDocument(str(pdf))[0]
    # Render at the preview's own scale: downscaling a sharper render greys out 1 px rules.
    page = first.render(scale=preview.width / first.get_width()).to_pil().convert("RGB").resize(preview.size, Image.NEAREST)
    a, b = dark(preview), dark(page)
    ha, hb = rules(a, 0, 0.08), rules(b, 0, 0.08)
    va, vb = rules(a, 1, 0.032), rules(b, 1, 0.032)  # shorter vertical runs are large title glyph stems
    return round(min(matched(ha, hb, 0.015), matched(hb, ha, 0.015), matched(va, vb, 0.015), matched(vb, va, 0.015)), 4)


def invisible_words(pdf: Path) -> list[str]:
    """Words in the text layer with no ink where they sit: clipped or drawn off the page.
    rhwp 0.8.6 keeps such lines in the text layer (wonju interview-suit p3 signature block)."""
    out = []
    doc = pdfium.PdfDocument(str(pdf))
    with pdfplumber.open(pdf) as plumb:
        for i, page in enumerate(plumb.pages):
            scale = 2.0
            img = np.asarray(doc[i].render(scale=scale).to_pil().convert("L"))
            H, W = img.shape
            for w in page.extract_words(x_tolerance=1.5):
                x0, x1 = int(w["x0"] * scale), int(w["x1"] * scale) + 1
                y0, y1 = int(w["top"] * scale), int(w["bottom"] * scale) + 1
                if x0 >= W or y0 >= H or x1 <= 0 or y1 <= 0:
                    out.append(f"p{i + 1}:{w['text']}(off page)")
                    continue
                box = img[max(0, y0):min(H, y1), max(0, x0):min(W, x1)]
                if box.size == 0 or (box < 200).sum() < 3:
                    out.append(f"p{i + 1}:{w['text']}")
    return out


def classify_pages(pdf: Path) -> list[dict]:
    out = []
    with pdfplumber.open(pdf) as doc:
        for i, page in enumerate(doc.pages):
            top = page.crop((0, 0, page.width, page.height / 3))
            head = re.sub(r"\s+", "", top.extract_text() or "")
            body = re.sub(r"\s+", "", page.extract_text() or "")
            ruled = len([e for e in page.edges if e["orientation"] == "h"]) >= 4
            title = FORM_TITLE.search(head)
            kind = "form" if title and ruled else "other"
            if kind == "form" and OTHERS_ONLY.search(head) and not re.search(r"신청서|동의서", head):
                kind = "others_only"
            if kind == "form" and re.search(r"작성\s*요령|작성\s*방법|유의\s*사항", head[:30]):
                kind = "other"  # '대여신청서 작성요령': instructions about the form, not the form
            elif kind == "form" and NOT_FORM.search(head[:40]) and not re.search(r"신청서|동의서|확인서", head[:40]):
                kind = "other"
            out.append({"page": i + 1, "kind": kind, "head": head[:60], "chars": len(body)})
    return out


def kordoc_oracle(ir_path: Path, source_sha256: str, rhwp: Path) -> dict:
    """Characters of kordoc's own reading of the source that the rhwp text layer lacks.
    Report only: kordoc reads the HWP independently of both renderers; the verdict does
    not use it until it has been calibrated like the other checks."""
    sys.path.insert(0, str(REPO / "scripts/forms/kordoc"))
    import check_ir  # noqa: PLC0415  (trusted repo code)
    if not ir_path.exists():
        return {"available": False}
    try:
        ir = check_ir.load(str(ir_path), source_sha256)
    except (check_ir.Invalid, OSError) as e:
        return {"available": False, "error": str(e)[:80]}
    texts = []

    def walk(t: dict) -> None:
        for c in t["cells"]:
            texts.append(c["text"])
            for n in c["tables"]:
                walk(n)

    for b in ir["blocks"]:
        walk(b["table"]) if b["kind"] == "table" else texts.append(b.get("text", ""))
    keep = lambda t: Counter(ch for ch in t if ch.isalnum())  # noqa: E731
    k, r = keep(re.sub(r"</?\w+>", "", "".join(texts))), keep(pdf_text(rhwp))
    missing = sorted(ch for ch, n in k.items() if r[ch] < n)
    return {"available": True, "chars": sum(k.values()), "missing_in_rhwp": missing[:20], "missing_count": len(missing)}


def judge(src: Path, rhwp: Path, h2o: Path | None) -> dict:
    words, src_runs = words_of(src)
    r_text = pdf_text(rhwp)
    h_text = pdf_text(h2o) if h2o and h2o.exists() else ""
    keep = lambda t: Counter(ch for ch in t if ch.isalnum())  # noqa: E731
    src_c, r_c, h_c = keep("".join(src_runs)), keep(r_text), keep(h_text)
    r_missing = sorted(ch for ch, n in src_c.items() if r_c[ch] < n)
    dropped = [ch for ch in r_missing if h_text and h_c[ch] > r_c[ch]]
    score = preview_score(src, rhwp)
    pages = classify_pages(rhwp)
    hidden = invisible_words(rhwp)
    reasons = []
    if hidden:
        reasons.append(f"text present but not drawn (clipped): {', '.join(hidden[:8])}")
    if dropped:
        reasons.append(f"rhwp drops characters the reference engine draws: {', '.join(dropped[:8])}")
    if len(r_missing) > len(dropped):
        reasons.append(f"characters missing from the rhwp text layer: {', '.join([w for w in r_missing if w not in dropped][:8])}")
    if score is None:
        reasons.append("no embedded preview: page 1 has no ground truth")
    elif score < PREVIEW_MIN:
        reasons.append(f"page 1 differs from the embedded preview (agreement {score} < {PREVIEW_MIN})")
    forms = [p["page"] for p in pages if p["kind"] == "form"]
    if not forms:
        reasons.append("no form page found")
    evidence = lambda n: "preview" if n == 1 else "auto_words"  # noqa: E731
    return {
        "verdict": "rejected" if reasons else "faithful",
        "reason": "; ".join(reasons) if reasons else None,
        "pages": [] if reasons else [{"page": n, "evidence": evidence(n)} for n in forms],
        "metrics": {"words": len(words), "rhwp_missing": r_missing[:20], "rhwp_dropped_vs_h2o": dropped[:20], "invisible": hidden[:20],
                    "preview_agreement": score, "page_kinds": pages},
    }


def main(argv=None) -> None:
    ap = argparse.ArgumentParser()
    for name in ("--sources", "--raw", "--rhwp", "--h2o", "--out", "--png"):
        ap.add_argument(name, required=True)
    ap.add_argument("--ir", help="kordoc IR dir (scripts/forms/hwp/convert.sh kordoc): a second source-text oracle, reported only")
    args = ap.parse_args(argv)
    sources = json.loads(Path(args.sources).read_text())
    png = Path(args.png)
    png.mkdir(parents=True, exist_ok=True)
    forms = {}
    for s in sources:
        if s["format"] not in ("hwp", "hwpx"):
            continue
        # HWPX: text from Contents/section*.xml and Preview/PrvImage (fidelity.preview_of).
        src = Path(args.raw) / f"{s['slug']}.{s['format']}"
        rhwp = Path(args.rhwp) / f"{s['slug']}.pdf"
        if not rhwp.exists():
            forms[s["slug"]] = {"verdict": "rejected", "source_sha256": s["sha256"], "pdf_sha256": None, "pages": [], "reason": "rhwp produced no PDF"}
            continue
        try:
            if not page_size_ok(rhwp):
                raise ValueError("page larger than 2000 pt or more than 50 pages")
            result = judge(src, rhwp, Path(args.h2o) / f"{s['slug']}.pdf")
            fidelity.check(src, rhwp, png)  # page PNGs + preview side-by-side for the reviewer
        except Exception as e:  # one malformed file must not stop the batch; it is rejected
            forms[s["slug"]] = {"verdict": "rejected", "source_sha256": s["sha256"], "pdf_sha256": None, "pages": [], "reason": f"unreadable: {type(e).__name__}"}
            print(s["slug"], "rejected (unreadable)", file=sys.stderr)
            continue
        if args.ir:
            result["metrics"]["kordoc"] = kordoc_oracle(Path(args.ir) / f"{s['slug']}.ir.json", s["sha256"], rhwp)
        forms[s["slug"]] = {"source_sha256": s["sha256"], "pdf_sha256": hashlib.sha256(rhwp.read_bytes()).hexdigest(), **result}
        print(s["slug"], result["verdict"], "form pages", [p["page"] for p in result["pages"]], "preview", result["metrics"]["preview_agreement"], file=sys.stderr)
    about = ("Automatic verdicts by scripts/forms/auto/fidelity_gate.py: every HWP word in the rhwp text layer, "
             f"page 1 agrees with the embedded preview (>= {PREVIEW_MIN}), templated pages are form pages.")
    Path(args.out).write_text(json.dumps({"version": 1, "about": about, "converter": CONVERTER, "forms": forms}, ensure_ascii=False, indent=1) + "\n")


if __name__ == "__main__":
    main()
