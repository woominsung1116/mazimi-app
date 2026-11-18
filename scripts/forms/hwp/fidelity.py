# /// script
# requires-python = ">=3.10"
# dependencies = ["olefile==0.47", "pypdfium2==4.30.0", "pdfplumber==0.11.9", "pillow>=10.4"]
# ///
"""Fidelity evidence for HWP/HWPX -> PDF conversions.

For each converted PDF this writes, under --out:
  <slug>-preview.png   side by side: the HWP's own preview image | PDF page 1
  <slug>-pN.png        every converted page, for a human to look at
and prints one JSON line per form with:
  text_coverage   share of the document's own words (BodyText records for
                  HWP, section XML for HWPX) found in the PDF text layer;
                  word level, so wrapped table cells do not count as missing
  missing         up to 20 words that are not in the PDF
  hard_page_starts sections + explicit page breaks in the source; more PDF
                  pages than this means a table or text overflowed a page

The numbers only flag candidates. The verdict (faithful or not) is a human
decision recorded in scripts/forms/hwp/conversions.json.

Usage:
  uv run scripts/forms/hwp/fidelity.py --raw DIR --pdf DIR --out DIR slug.hwp ...
Reads local files only.
"""
from __future__ import annotations

import argparse
import io
import json
import re
import struct
import zipfile
import zlib
from pathlib import Path
from xml.etree import ElementTree as ET

import olefile
import pdfplumber
import pypdfium2 as pdfium
from PIL import Image

Image.MAX_IMAGE_PIXELS = 20_000_000  # untrusted preview images: refuse decompression bombs

HWPTAG_PARA_HEADER = 66  # HWPTAG_BEGIN(16) + 50
HWPTAG_PARA_TEXT = 67


def hwp_texts(ole: olefile.OleFileIO) -> tuple[list[str], int]:
    """Paragraph text runs from every BodyText/SectionN stream, and the number
    of hard page starts (sections + top-level paragraphs with a page break)."""
    header = ole.openstream("FileHeader").read()
    compressed = bool(struct.unpack_from("<I", header, 36)[0] & 1)
    runs: list[str] = []
    hard_pages = 0
    for entry in sorted(ole.listdir(), key=lambda e: "/".join(e)):
        if len(entry) != 2 or entry[0] != "BodyText":
            continue
        data = ole.openstream(entry).read()
        if compressed:
            # Untrusted input: cap the inflated size (a small deflate stream can expand to GiBs).
            inflater = zlib.decompressobj(-15)
            data = inflater.decompress(data, 64 << 20)
            if inflater.unconsumed_tail:
                raise ValueError("BodyText section inflates beyond 64 MiB")
        hard_pages += 1
        pos = 0
        while pos + 4 <= len(data):
            head = struct.unpack_from("<I", data, pos)[0]
            tag, level, size = head & 0x3FF, (head >> 10) & 0x3FF, (head >> 20) & 0xFFF
            pos += 4
            if size == 0xFFF:
                size = struct.unpack_from("<I", data, pos)[0]
                pos += 4
            body = data[pos:pos + size]
            pos += size
            if tag == HWPTAG_PARA_HEADER and level == 0 and len(body) >= 12 and body[11] & 0x04:
                hard_pages += 1
            if tag != HWPTAG_PARA_TEXT:
                continue
            out, i = bytearray(), 0
            while i + 2 <= len(body):
                code = struct.unpack_from("<H", body, i)[0]
                if code < 32:
                    # Extended/inline controls occupy 8 code units; char controls 1.
                    i += 2 if code in (0, 10, 13, 24, 25, 26, 27, 28, 29, 30, 31) else 16
                    out += b" \x00"
                    continue
                out += body[i:i + 2]
                i += 2
            runs.append(out.decode("utf-16-le", errors="replace"))
    return runs, hard_pages


HWPX_MAX_ENTRY = 16 * 1024 * 1024  # one inflated entry; real section XML is well under 1 MiB
HWPX_MAX_RATIO = 200  # deflate ratio above this is a bomb, not a document
HWPX_MAX_SECTIONS = 50


def read_capped(zf: zipfile.ZipFile, name: str) -> bytes:
    """An entry of an untrusted HWPX, refusing zip bombs before and while inflating."""
    info = zf.getinfo(name)
    if info.file_size > HWPX_MAX_ENTRY or (info.file_size and (not info.compress_size or info.file_size / info.compress_size > HWPX_MAX_RATIO)):
        raise ValueError(f"hwpx entry too large: {name}")
    with zf.open(info) as fh:
        data = fh.read(HWPX_MAX_ENTRY + 1)
    if len(data) > HWPX_MAX_ENTRY:
        raise ValueError(f"hwpx entry too large: {name}")
    return data


def hwpx_texts(zf: zipfile.ZipFile) -> tuple[list[str], int]:
    runs, hard_pages = [], 0
    sections = sorted(n for n in zf.namelist() if re.match(r"Contents/section\d+\.xml$", n))
    if len(sections) > HWPX_MAX_SECTIONS:
        raise ValueError("hwpx has too many sections")
    for name in sections:
        root = ET.fromstring(read_capped(zf, name))
        hard_pages += 1 + sum(1 for p in root if p.tag.endswith("}p") and p.get("pageBreak") == "1")
        for el in root.iter():
            if el.tag.endswith("}t") and el.text:
                runs.append(el.text)
    return runs, hard_pages


# The embedded preview is attacker-controlled bytes: decode it only as a plain raster format
# (never FlashPix/OLE2, TIFF, JPEG 2000 or any other decoder Pillow would pick from content),
# at most 4 MiB and 20 MP.
PREVIEW_FORMATS = ["PNG", "BMP", "GIF", "JPEG"]
MAX_PREVIEW_BYTES = 4 * 1024 * 1024
Image.MAX_IMAGE_PIXELS = 20_000_000


def decode_preview(data: bytes) -> Image.Image:
    if len(data) > MAX_PREVIEW_BYTES:
        raise ValueError("preview image too large")
    img = Image.open(io.BytesIO(data), formats=PREVIEW_FORMATS)
    if img.width * img.height > Image.MAX_IMAGE_PIXELS:
        raise ValueError("preview image too many pixels")
    return img.convert("RGB")


def preview_of(path: Path) -> tuple[Image.Image | None, tuple[list[str], int]]:
    if path.suffix.lower() == ".hwpx":
        with zipfile.ZipFile(path) as zf:
            img = next((n for n in zf.namelist() if n.lower().startswith("preview/prvimage")), None)
            image = decode_preview(read_capped(zf, img)) if img else None
            return image, hwpx_texts(zf)
    with olefile.OleFileIO(str(path)) as ole:
        image = None
        if ole.exists("PrvImage"):
            if ole.get_size("PrvImage") > MAX_PREVIEW_BYTES:
                raise ValueError("preview image too large")
            image = decode_preview(ole.openstream("PrvImage").read(MAX_PREVIEW_BYTES + 1))
        return image, hwp_texts(ole)


def norm(text: str) -> str:
    # Middle dots and spacing differ between HWP glyphs and PDF text layers.
    return re.sub(r"[\s 　·•∙ㆍ‧・�]+", "", text)


def check(src: Path, pdf: Path, out: Path) -> dict:
    slug = src.stem
    preview, (runs, hard_pages) = preview_of(src)
    with pdfplumber.open(pdf) as doc:
        pdf_text = norm("".join(page.extract_text() or "" for page in doc.pages))
    tokens = [norm(w) for r in runs for w in r.split()]
    tokens = [t for t in tokens if len(t) >= 2]
    missing = list(dict.fromkeys(t for t in tokens if t not in pdf_text))
    doc = pdfium.PdfDocument(str(pdf))
    pages = []
    for i in range(len(doc)):
        image = doc[i].render(scale=110 / 72).to_pil().convert("RGB")
        image.save(out / f"{slug}-p{i + 1}.png")
        pages.append(image)
    result = {"slug": slug, "pages": len(pages), "hard_page_starts": hard_pages, "words": len(tokens),
              "text_coverage": round(1 - sum(t not in pdf_text for t in tokens) / len(tokens), 4) if tokens else None,
              "missing": missing[:20], "preview": preview is not None}
    if preview is not None:
        first = pages[0].resize(preview.size)
        side = Image.new("RGB", (preview.width * 2 + 10, preview.height), "white")
        side.paste(preview, (0, 0))
        side.paste(first, (preview.width + 10, 0))
        side.save(out / f"{slug}-preview.png")
    return result


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--raw", required=True)
    ap.add_argument("--pdf", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("files", nargs="+")
    args = ap.parse_args(argv)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    for name in args.files:
        src = Path(args.raw) / name
        pdf = Path(args.pdf) / f"{src.stem}.pdf"
        if not pdf.exists():
            print(json.dumps({"slug": src.stem, "error": "no pdf"}, ensure_ascii=False))
            continue
        print(json.dumps(check(src, pdf, out), ensure_ascii=False))


if __name__ == "__main__":
    main()
