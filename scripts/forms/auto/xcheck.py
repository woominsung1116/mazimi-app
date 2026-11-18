# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow==13.0.0", "numpy>=1.26", "scipy>=1.11"]
# ///
"""Second-renderer check of the reviewer's images (closes the "pdfium fakes its own form" gap).

gates.py renders the blank and the app-filled PDFs with pdfium, so a PDF that compromised
pdfium could hand the reviewer clean-looking pages for a wrong template. Here the same pages,
rendered independently by poppler (pdftoppm, its own container), must agree with pdfium's,
and every gate runs again on poppler's pages. The reviewer's images (blank, filled, numbered
boxes) are drawn from poppler's pages in this step, which parses no PDF: it reads PNG pages
(Pillow, PNG only, size-capped) and JSON. A wrong template now needs both renderers
compromised the same way.

Agreement, per page: dark ink (< 128) in one rendering must have some ink (< 224) within
2 px in the other; any connected disagreement of MIN_PX or more pixels fails. Calibrated on
318 pages of 55 corpus PDFs (blank and filled), pdftoppm -r 144 against pdfium scale 2, with
the job image's poppler 25.03 (and 26.03 on a Mac): every genuine page had 0 disagreeing
pixels; a 60x24 box drawn at a random spot, or an inked 80x16 area erased, was caught on 313
and 311 of 318 pages (the misses fall inside shaded areas, where the tolerance for light ink
absorbs them).

Usage: python -I xcheck.py <slug> <template.json> <fill-all.json> <fill-review.json> <pdfium gates.json>
                           <pdfium pages dir> <poppler pages dir> <out dir>
Writes <out>/<slug>.gates.json (gates on poppler's pages; pass also needs pdfium's pass and
agreement; problems["renderers"] says why not) and the reviewer's images next to it.
"""
from __future__ import annotations

import json
import os
import re
import stat
import sys
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gates  # noqa: E402  (check_all only: gates.render, the pdfium part, is never called here)

KINDS = ("blank", "filled", "shown")
MAX_PAGES = 50
MAX_SIDE = 4200  # 2000 pt at scale 2, plus slack
MAX_BYTES = 20 * 1024 * 1024
MIN_PX = 6
SIZE_SLACK = 2  # px: the two renderers may round a page's size differently
STRUCT = np.ones((3, 3), bool)


def load_png(path: Path) -> np.ndarray:
    """A grayscale page from a regular PNG file; anything else is refused."""
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(fd, "rb") as fh:
        st = os.fstat(fh.fileno())
        if not (0 < st.st_size <= MAX_BYTES) or not stat.S_ISREG(st.st_mode):
            raise ValueError(f"{path.name}: not a regular file within the size cap")
        img = Image.open(fh, formats=["PNG"])
        if max(img.size) > MAX_SIDE or img.size[0] * img.size[1] > MAX_SIDE * MAX_SIDE:
            raise ValueError(f"{path.name}: page image too large")
        return np.asarray(img.convert("L"))


def pages(d: Path) -> dict[str, list[np.ndarray]]:
    names = {}
    for p in d.iterdir():
        m = re.fullmatch(r"(blank|filled|shown)-([1-9][0-9]?)\.png", p.name)
        if not m:
            raise ValueError(f"unexpected file {p.name[:40]}")
        names.setdefault(m.group(1), {})[int(m.group(2))] = p
    out = {}
    for k in KINDS:
        got = names.get(k, {})
        if not got or sorted(got) != list(range(1, len(got) + 1)) or len(got) > MAX_PAGES:
            raise ValueError(f"{k} pages missing or out of order")
        out[k] = [load_png(got[n]) for n in sorted(got)]
    return out


def disagreement(a: np.ndarray, b: np.ndarray) -> tuple[int, tuple[float, float] | None]:
    """(largest connected disagreement in px, its centre as page fractions)."""
    h, w = min(a.shape[0], b.shape[0]), min(a.shape[1], b.shape[1])
    a, b = a[:h, :w], b[:h, :w]
    near_a = ndimage.binary_dilation(a < 224, STRUCT, iterations=2)
    near_b = ndimage.binary_dilation(b < 224, STRUCT, iterations=2)
    m = ((a < 128) & ~near_b) | ((b < 128) & ~near_a)
    lab, n = ndimage.label(m, STRUCT)
    if not n:
        return 0, None
    sizes = ndimage.sum(m, lab, range(1, n + 1))
    i = int(np.argmax(sizes))
    sl = ndimage.find_objects(lab)[i]
    return int(sizes[i]), ((sl[1].start + sl[1].stop) / 2 / w, (sl[0].start + sl[0].stop) / 2 / h)


def compare(pd: dict, pp: dict) -> list[str]:
    problems = []
    for k in KINDS:
        if len(pd[k]) != len(pp[k]):
            problems.append(f"{k}: pdfium has {len(pd[k])} pages, poppler {len(pp[k])}")
            continue
        for n, (a, b) in enumerate(zip(pd[k], pp[k]), 1):
            if abs(a.shape[0] - b.shape[0]) > SIZE_SLACK or abs(a.shape[1] - b.shape[1]) > SIZE_SLACK:
                problems.append(f"{k} p{n}: page sizes differ ({a.shape[1]}x{a.shape[0]} vs {b.shape[1]}x{b.shape[0]})")
                continue
            px, where = disagreement(a, b)
            if px >= MIN_PX:
                problems.append(f"{k} p{n}: the renderers disagree ({px} px) near ({where[0]:.3f},{where[1]:.3f})")
    return problems


def main(slug, tpl, fill_all, fill_review, pdfium_gates, pdfium_dir, poppler_dir, out_dir) -> int:
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    why = []
    try:
        pg = json.loads(Path(pdfium_gates).read_text())
        pdfium_pass = pg.get("pass") is True
    except (OSError, ValueError):
        pdfium_pass = False
    if not pdfium_pass:
        why.append("the pdfium gates did not pass")
    try:
        pd, pp = pages(Path(pdfium_dir)), pages(Path(poppler_dir))
    except (OSError, ValueError) as e:
        result = {"slug": slug, "pass": False, "problems": {"renderers": [f"pages unreadable: {str(e)[:120]}"]}, "images": []}
        (out / f"{slug}.gates.json").write_text(json.dumps(result, ensure_ascii=False, indent=1))
        print(slug, "FAIL renderers")
        return 1
    disagree = compare(pd, pp)
    why += disagree
    result = gates.check_all(slug, tpl, fill_all, fill_review, pp["blank"], pp["filled"], pp["shown"], out)
    if why:
        result["problems"]["renderers"] = why
    result["renderers"] = {"second": "poppler pdftoppm", "agree": not disagree,
                           "pdfium_pass": pdfium_pass}
    result["pass"] = not result["problems"]
    (out / f"{slug}.gates.json").write_text(json.dumps(result, ensure_ascii=False, indent=1))
    print(slug, "renderers agree" if not why else f"renderers: {len(why)} problem(s)")
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    if len(sys.argv) != 9:
        raise SystemExit(__doc__)
    sys.exit(main(*sys.argv[1:9]))
