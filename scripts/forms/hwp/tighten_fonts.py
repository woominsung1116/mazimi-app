"""Build-time only: copies of the Nanum fonts with an HWP-like line box.

Hangul word processors size a line as font size x line-spacing percent; the
glyphs' own ascent/descent do not count. LibreOffice multiplies the font's
ascent+descent instead (about 1.15em for Nanum, 1.45em for Noto CJK), so
fixed-height table cells in converted forms overflow and clip text. These
copies keep every glyph and advance width and only set the vertical metrics
to ascent 0.85em + descent 0.15em. They are renamed (HwpSans/HwpSerif) so they
never shadow the originals; they are used only inside the converter image.

Usage (inside the image): python3 tighten_fonts.py <src dir> <out dir>
"""
import sys
from pathlib import Path

from fontTools.ttLib import TTFont

FACES = {
    "NanumGothic.ttf": ("HwpSans", "Regular"),
    "NanumGothicBold.ttf": ("HwpSans", "Bold"),
    "NanumMyeongjo.ttf": ("HwpSerif", "Regular"),
    "NanumMyeongjoBold.ttf": ("HwpSerif", "Bold"),
}


def tighten(src: Path, dst: Path, family: str, style: str) -> None:
    font = TTFont(src)
    upm = font["head"].unitsPerEm
    ascent, descent = round(upm * 0.85), round(upm * 0.15)
    hhea, os2 = font["hhea"], font["OS/2"]
    hhea.ascent, hhea.descent, hhea.lineGap = ascent, -descent, 0
    os2.sTypoAscender, os2.sTypoDescender, os2.sTypoLineGap = ascent, -descent, 0
    os2.usWinAscent, os2.usWinDescent = ascent, descent
    os2.fsSelection |= 1 << 7  # USE_TYPO_METRICS
    name = font["name"]
    full, ps = f"{family} {style}", f"{family}-{style}"
    for record in list(name.names):
        if record.nameID in (1, 2, 3, 4, 6, 16, 17):
            name.removeNames(nameID=record.nameID)
    for nid, value in ((1, family), (2, style), (3, f"mazimi:{ps}"), (4, full), (6, ps)):
        name.setName(value, nid, 3, 1, 0x409)
        name.setName(value, nid, 1, 0, 0)
    font.save(dst)


def main() -> None:
    src, out = Path(sys.argv[1]), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    for file, (family, style) in FACES.items():
        tighten(src / file, out / f"{family}-{style}.ttf", family, style)


if __name__ == "__main__":
    main()
