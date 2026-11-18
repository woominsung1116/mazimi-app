"""Second-renderer check (scripts/forms/auto/xcheck.py): the reviewer's images are only made
when pdfium's pages and poppler's pages agree.

Fail-first: before xcheck.py the packet used pdfium's pages alone, so every tampered case
below would have reached the reviewer.
"""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import numpy as np
import pytest
from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "auto"))
import xcheck  # noqa: E402

FIX = HERE / "fixtures"


def page(seed: int = 0) -> np.ndarray:
    """A form-like page: ruled table, a few text-like ink blocks."""
    img = Image.new("L", (600, 840), 255)
    d = ImageDraw.Draw(img)
    for y in range(100, 700, 60):
        d.line([(40, y), (560, y)], fill=0, width=1)
    d.rectangle([40, 100, 560, 700], outline=0, width=2)
    for i in range(8):
        d.rectangle([60, 115 + 60 * i, 140 + 10 * ((i + seed) % 3), 125 + 60 * i], fill=30)
    return np.asarray(img)


def antialiased(a: np.ndarray) -> np.ndarray:
    """What another renderer draws for the same page: lighter hairlines, one-pixel shift."""
    b = a.astype(int).copy()
    b[b < 128] = 110
    b = np.roll(b, 1, axis=1)
    return b.astype(np.uint8)


def write(d: Path, pages: dict) -> Path:
    d.mkdir(parents=True, exist_ok=True)
    for kind, imgs in pages.items():
        for n, a in enumerate(imgs, 1):
            Image.fromarray(a).save(d / f"{kind}-{n}.png")
    return d


def three(a):
    return {"blank": [a], "filled": [a], "shown": [a]}


def test_the_same_page_from_two_renderers_agrees():
    a = page()
    assert xcheck.disagreement(a, antialiased(a))[0] < xcheck.MIN_PX
    assert xcheck.compare(three(a), three(antialiased(a))) == []


def test_a_box_or_an_erased_label_on_one_renderer_disagrees():
    a = page()
    boxed = Image.fromarray(a.copy())
    ImageDraw.Draw(boxed).rectangle([300, 400, 360, 424], outline=0, width=2)
    erased = a.copy()
    erased[112:128, 58:150] = 255
    for fake in (np.asarray(boxed), erased):
        probs = xcheck.compare(three(fake), three(antialiased(a)))
        assert probs and all("disagree" in p for p in probs), probs


def test_page_counts_and_sizes_must_match():
    a = page()
    assert "pages" in xcheck.compare({**three(a), "filled": [a, a]}, three(a))[0]
    assert "sizes" in xcheck.compare(three(a), three(np.pad(a, ((0, 9), (0, 0)), constant_values=255)))[0]


def test_pages_are_png_files_named_by_kind_only(tmp_path):
    d = write(tmp_path / "ok", three(page()))
    assert len(xcheck.pages(d)["blank"]) == 1
    bad = write(tmp_path / "gap", {"blank": [page()], "filled": [page()], "shown": [page()]})
    os.rename(bad / "shown-1.png", bad / "shown-2.png")
    with pytest.raises(ValueError):
        xcheck.pages(bad)
    jpg = write(tmp_path / "jpg", three(page()))
    Image.fromarray(page()).save(jpg / "blank-1.png", format="JPEG")
    with pytest.raises(Exception):
        xcheck.pages(jpg)
    link = write(tmp_path / "link", three(page()))
    os.remove(link / "blank-1.png")
    os.symlink(d / "blank-1.png", link / "blank-1.png")
    with pytest.raises(OSError):
        xcheck.pages(link)


def run_main(tmp_path, pdfium_pages, poppler_pages, pdfium_pass=True):
    tmp_path.mkdir(parents=True)
    tpl = tmp_path / "t.template.json"
    tpl.write_text(json.dumps({"version": 1, "fields": []}))
    fill = tmp_path / "fill.json"
    fill.write_text(json.dumps({"fields": [], "overflow": [], "sensitive": []}))
    pg = tmp_path / "pdfium.gates.json"
    pg.write_text(json.dumps({"pass": pdfium_pass}))
    out = tmp_path / "out"
    rc = xcheck.main("t", str(tpl), str(fill), str(fill), str(pg), str(write(tmp_path / "pd", pdfium_pages)),
                     str(write(tmp_path / "pp", poppler_pages)), str(out))
    return rc, json.loads((out / "t.gates.json").read_text())


def test_main_passes_only_with_agreement_and_both_gates(tmp_path):
    a = page()
    rc, g = run_main(tmp_path / "ok", three(a), three(antialiased(a)))
    assert rc == 0 and g["pass"] and g["renderers"]["agree"]
    # The reviewer's images are drawn from poppler's pages, here.
    assert all(Path(f).parent == tmp_path / "ok" / "out" for f in [u["file"] for u in g["unboxed_pages"]])
    fake = a.copy()
    fake[112:128, 58:150] = 255
    rc, g = run_main(tmp_path / "fake", three(fake), three(a))
    assert rc == 1 and not g["pass"] and not g["renderers"]["agree"] and g["problems"]["renderers"]
    rc, g = run_main(tmp_path / "pdfium-fail", three(a), three(a), pdfium_pass=False)
    assert rc == 1 and not g["pass"]
    rc, g = run_main(tmp_path / "missing", three(a), {"blank": [a], "filled": [a]})
    assert rc == 1 and not g["pass"] and g["problems"]["renderers"]


@pytest.mark.skipif(not shutil.which("pdftoppm"), reason="poppler not installed")
def test_pdfium_and_poppler_agree_on_a_fixture_form(tmp_path):
    pytest.importorskip("pypdfium2")
    import gates

    pdf = next(FIX.glob("*.pdf"))
    pd = gates.render(pdf)
    subprocess.run(["pdftoppm", "-r", "144", "-gray", "-png", str(pdf), str(tmp_path / "p")], check=True)
    pp = [xcheck.load_png(f) for f in sorted(tmp_path.glob("p-*.png"), key=lambda f: int(f.stem.split("-")[1]))]
    assert xcheck.compare({"blank": pd, "filled": pd, "shown": pd}, {"blank": pp, "filled": pp, "shown": pp}) == []
