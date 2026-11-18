# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2==4.30.0", "pillow>=10.4", "numpy>=1.26", "scipy>=1.11"]
# ///
"""Gate (b): machine checks on a template filled with fake values (fill.cjs output).

Renders the blank PDF and the app-filled PDF at the same scale and checks, per page:
  overlap       no two boxes intersect
  over_print    no text/date/multiline box covers printed glyphs on the blank page,
                and no box is crossed by a vertical ruling line (it would span two cells)
  outside       every pixel of new ink lies inside some box (vertical slack: the app may
                draw a single line up to 1.2x the box height)
  collision     new ink never touches printed glyphs (ruling lines and underlines may be touched)
  drawn         every box received ink
  overflow      the app accepted every realistic fake value (from fill.cjs)
  sensitive     주민번호/계좌/서명 boxes are typed sensitive and never prefilled (from fill.cjs)
Writes <out>/<slug>.gates.json and PNGs for the independent reviewer:
  <slug>-pN-blank.png, <slug>-pN-filled.png, <slug>-pN-boxes.png (numbered boxes + legend).

  groups        every checkbox group has >= 2 checkbox options on one page and no box is in two groups

Also checked: a text box crossed by a horizontal ruling line in its middle (it straddles two
rows), and a tick box sitting on printed text (more than one glyph inside; a printed box or
circle is a single glyph). gates.json records sha256 of the template and of the fill report,
so a reviewer verdict can be bound to exactly these bytes (verdicts.py), and lists the pages
that got no boxes (the reviewer must see them too, PDF or HWP).

Usage: python -I gates.py <slug> <template.json> <blank.pdf> <filled-all.pdf> <fill-all.json> <out dir> [<filled-review.pdf> <fill-review.json>] [--rasters <dir>]
  filled-all: every tick box ticked (geometry checks); filled-review: groups applied (reviewer image, group info)
  --rasters: also save the rendered pages (blank-N.png, filled-N.png, shown-N.png) for xcheck.py,
             which runs the same checks on a second renderer's pages (see xcheck.py)
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy import ndimage

SCALE = 2.0
INSET = 2  # px kept clear of a box's own edges (its cell border, a neighbouring label's last pixel)
FONT = Path(__file__).resolve().parents[3] / "apps/mobile/assets/fonts/NanumGothic-Regular.ttf"
TEXTUAL = ("text", "date", "multiline")
COLORS = {"text": (0, 102, 255), "multiline": (0, 160, 160), "date": (140, 60, 220), "checkbox": (0, 150, 60), "signature": (220, 40, 40), "stamp": (230, 120, 0)}


def render(pdf: Path) -> list[np.ndarray]:
    import pypdfium2 as pdfium  # here only: xcheck.py runs these checks on pages it never rendered

    doc = pdfium.PdfDocument(str(pdf))
    if len(doc) > 50 or any(max(doc[i].get_size()) > 2000 for i in range(len(doc))):
        raise SystemExit("refusing to render: more than 50 pages or a page larger than 2000 pt")
    return [np.asarray(doc[i].render(scale=SCALE).to_pil().convert("L")) for i in range(len(doc))]


def px(rect: dict, shape) -> tuple[int, int, int, int]:
    H, W = shape
    return (int(round(rect["x"] * W)), int(round(rect["y"] * H)), int(round((rect["x"] + rect["w"]) * W)), int(round((rect["y"] + rect["h"]) * H)))


def glyph_mask(gray: np.ndarray) -> np.ndarray:
    """Printed glyphs: dark components that are not ruling lines or table grids. Long straight
    runs are removed first, so a label touching a cell border or an underline is not swallowed
    into the grid component."""
    dark = gray < 160
    lines = ndimage.binary_opening(dark, structure=np.ones((1, 25))) | ndimage.binary_opening(dark, structure=np.ones((25, 1)))
    dark = dark & ~lines
    lab, n = ndimage.label(dark, structure=np.ones((3, 3)))
    keep = np.zeros(n + 1, bool)
    for i, sl in enumerate(ndimage.find_objects(lab), 1):
        h, w = sl[0].stop - sl[0].start, sl[1].stop - sl[1].start
        area = int((lab[sl] == i).sum())
        line = (h <= 3 and w >= 6) or (w <= 3 and h >= 6)
        grid = (w > 40 or h > 40) and area < 0.2 * w * h
        keep[i] = not line and not grid and area >= 4
    return keep[lab]


def check_page(fields: list[dict], blank: np.ndarray, filled: np.ndarray) -> dict:
    problems: dict[str, list[str]] = {k: [] for k in ("overlap", "over_print", "outside", "collision", "drawn")}
    H, W = blank.shape
    boxes = [(f, px(f["rect"], blank.shape)) for f in fields]
    # overlap
    for i, (a, ra) in enumerate(boxes):
        for b, rb in boxes[i + 1:]:
            iw, ih = min(ra[2], rb[2]) - max(ra[0], rb[0]), min(ra[3], rb[3]) - max(ra[1], rb[1])
            if iw > 1 and ih > 1:
                problems["overlap"].append(f"{a['id']} '{a['label']}' x {b['id']} '{b['label']}'")
    glyphs = glyph_mask(blank)
    dark = blank < 160
    for f, (x0, y0, x1, y1) in boxes:
        if f["type"] == "checkbox":
            # A tick box may sit on one printed mark (□, ○, a bracket pair is cut by the inset);
            # several glyph components inside mean it covers label text.
            inner_g = glyphs[y0 + 1:y1 - 1, x0 + 1:x1 - 1]
            lab, n = ndimage.label(inner_g, structure=np.ones((3, 3)))
            whole = [sl for sl in ndimage.find_objects(lab) if sl[0].start > 0 and sl[1].start > 0 and sl[0].stop < inner_g.shape[0] and sl[1].stop < inner_g.shape[1]]
            if len(whole) > 1:
                problems["over_print"].append(f"{f['id']} '{f['label']}': tick box covers {len(whole)} printed glyphs (label text)")
            else:
                # Partly covered label text: glyph pixels of every component except the largest
                # (the printed box/circle itself) must stay under 15% of the box.
                sizes = sorted((int((lab == i).sum()) for i in range(1, n + 1)), reverse=True)
                if inner_g.size and sum(sizes[1:]) > 0.15 * inner_g.size:
                    problems["over_print"].append(f"{f['id']} '{f['label']}': tick box partly covers printed text")
            continue
        if f["type"] not in TEXTUAL:
            continue
        inner_g = glyphs[y0 + INSET:y1 - INSET, x0 + INSET:x1 - INSET]
        if inner_g.sum() >= 5:
            ys, xs = np.nonzero(inner_g)
            problems["over_print"].append(f"{f['id']} '{f['label']}': printed glyph pixels {int(inner_g.sum())} inside the box near ({(x0 + INSET + xs.mean()) / W:.3f},{(y0 + INSET + ys.mean()) / H:.3f})")
        inner_d = dark[y0 + INSET:y1 - INSET, x0 + INSET:x1 - INSET]
        if inner_d.size:
            lab, n = ndimage.label(inner_d)
            for sl in ndimage.find_objects(lab):
                h, w = sl[0].stop - sl[0].start, sl[1].stop - sl[1].start
                if w <= 3 and h >= 0.6 * inner_d.shape[0] and h >= 6:
                    problems["over_print"].append(f"{f['id']} '{f['label']}': a vertical ruling line crosses the box (spans two cells)")
                    break
                cy = (sl[0].start + sl[0].stop) / 2
                if h <= 3 and w >= 0.6 * inner_d.shape[1] and 0.25 * inner_d.shape[0] <= cy <= 0.75 * inner_d.shape[0]:
                    # an underline sits at the bottom; a rule through the middle means two rows
                    problems["over_print"].append(f"{f['id']} '{f['label']}': a horizontal ruling line crosses the middle of the box (spans two rows)")
                    break
    # new ink
    added = (blank.astype(int) - filled.astype(int)) > 50
    allowed = np.zeros_like(added)
    textual_zone = np.zeros_like(added)
    for f, (x0, y0, x1, y1) in boxes:
        slack_y = int(0.12 * (y1 - y0)) + 3
        allowed[max(0, y0 - slack_y):y1 + slack_y, max(0, x0 - 3):x1 + 3] = True
        if f["type"] in TEXTUAL:
            textual_zone[max(0, y0 - slack_y):y1 + slack_y, max(0, x0 - 3):x1 + 3] = True
        if f.get("value") and f.get("value") != "false" and added[y0:y1, x0:x1].sum() < 3:
            problems["drawn"].append(f"{f['id']} '{f['label']}': no ink inside the box")
    stray = added & ~allowed
    if stray.sum() > 2:
        lab, n = ndimage.label(stray, structure=np.ones((3, 3)))
        for sl in ndimage.find_objects(lab)[:8]:
            problems["outside"].append(f"ink outside every box near ({(sl[1].start + sl[1].stop) / 2 / W:.3f},{(sl[0].start + sl[0].stop) / 2 / H:.3f})")
    touch = ndimage.binary_dilation(added & textual_zone, iterations=1) & glyphs
    if touch.sum() > 10:
        ys, xs = np.nonzero(touch)
        problems["collision"].append(f"value ink touches printed glyphs ({int(touch.sum())} px) near ({xs.mean() / W:.3f},{ys.mean() / H:.3f})")
    return problems


def sheet(slug: str, page: int, fields: list[dict], blank: np.ndarray, filled: np.ndarray, out: Path) -> list[str]:
    font = ImageFont.truetype(str(FONT), 13) if FONT.exists() else ImageFont.load_default()
    small = ImageFont.truetype(str(FONT), 11) if FONT.exists() else font
    b = Image.fromarray(blank).convert("RGB")
    f = Image.fromarray(filled).convert("RGB")
    W, H = b.size
    boxes = Image.new("RGB", (W + 460, H), "white")
    boxes.paste(b, (0, 0))
    d = ImageDraw.Draw(boxes, "RGBA")
    ly = 8
    for n, fld in enumerate(fields, 1):
        x0, y0, x1, y1 = px(fld["rect"], blank.shape)
        c = COLORS.get(fld["type"], (0, 0, 0))
        d.rectangle([x0, y0, x1, y1], outline=c + (255,), width=2, fill=c + (40,))
        d.text((x0 + 2, y0 + 1), str(n), fill=(200, 0, 0), font=small)
        grp = fld.get("group")
        tag = f"{n} [{fld['type']}] {fld['label']}" + (f" -> {fld['mapping']}" if fld.get("mapping") else "") + (" *" if fld.get("required") else "") + (f" {{{grp['id']} {grp['mode']}{'?' if grp.get('review_flag') else ''}}}" if grp else "")
        d.text((W + 8, ly), tag[:60], fill=c, font=font)
        ly += 17
    # Groups: one outline around all options of a group, tagged "g1 single" / "g2 multi?" (? = flagged).
    seen = {}
    for fld in fields:
        if fld.get("group"):
            seen.setdefault(fld["group"]["id"], (fld["group"], []))[1].append(px(fld["rect"], blank.shape))
    for gid, (grp, rects) in seen.items():
        gx0, gy0 = min(r[0] for r in rects) - 6, min(r[1] for r in rects) - 6
        gx1, gy1 = max(r[2] for r in rects) + 6, max(r[3] for r in rects) + 6
        col = (200, 0, 160) if grp["mode"] == "single" else (230, 120, 0)
        d.rectangle([gx0, gy0, gx1, gy1], outline=col + (255,), width=2)
        d.text((gx0, gy0 - 13), f"{gid} {grp['mode']}{'?' if grp.get('review_flag') else ''}", fill=col, font=small)
    names = []
    for kind, img in (("blank", b), ("filled", f), ("boxes", boxes)):
        p = out / f"{slug}-p{page + 1}-{kind}.png"
        img.save(p)
        names.append(str(p))
    return names


def sha(path: str) -> str:
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main(slug: str, tpl: str, blank_pdf: str, filled_pdf: str, fill_json: str, out_dir: str, review_pdf: str | None = None,
         review_json: str | None = None, rasters: str | None = None) -> None:
    blank, filled = render(Path(blank_pdf)), render(Path(filled_pdf))
    shown = render(Path(review_pdf)) if review_pdf else filled
    if rasters:
        rd = Path(rasters)
        rd.mkdir(parents=True, exist_ok=True)
        for kind, pages in (("blank", blank), ("filled", filled), ("shown", shown)):
            for n, img in enumerate(pages, 1):
                Image.fromarray(img).save(rd / f"{kind}-{n}.png")
    check_all(slug, tpl, fill_json, review_json, blank, filled, shown, Path(out_dir))


def check_all(slug: str, tpl: str, fill_json: str, review_json: str | None, blank: list, filled: list, shown: list, out: Path) -> dict:
    """Every gate on rendered pages (grayscale arrays), whichever renderer drew them."""
    out.mkdir(parents=True, exist_ok=True)
    report = json.loads(Path(fill_json).read_text())
    review = json.loads(Path(review_json).read_text()) if review_json else report
    if [f["id"] for f in review["fields"]] != [f["id"] for f in report["fields"]]:
        raise SystemExit("the two fill reports list different fields")
    if not (len(blank) == len(filled) == len(shown)):
        raise SystemExit("page count changed by filling")
    result = {"slug": slug, "template": tpl, "template_sha256": sha(tpl), "fill_report": review_json or fill_json,
              "fill_report_sha256": sha(review_json or fill_json), "pages": {}, "images": []}
    totals: dict[str, list[str]] = {"overflow": [f"'{l}': realistic fake value did not fit" for l in report["overflow"]], "sensitive": report["sensitive"], "groups": []}
    types = {f["id"]: (f["page"], f["type"]) for f in report["fields"]}
    member_of: dict[str, str] = {}
    report = {**report, "groups": review.get("groups", [])}
    for g in report["groups"]:
        where = {types.get(m, (None, None)) for m in g["members"]}
        if len(g["members"]) < 2 or len({w[0] for w in where}) != 1 or any(w[1] != "checkbox" for w in where):
            totals["groups"].append(f"{g['id']}: needs >= 2 checkbox options on one page")
        for m in g["members"]:
            if m in member_of:
                totals["groups"].append(f"{m} in {member_of[m]} and {g['id']}")
            member_of[m] = g["id"]
    boxed = sorted({f["page"] for f in report["fields"]})
    if boxed and boxed[-1] >= len(blank):
        raise SystemExit("a box sits on a page the PDF does not have")
    for page in boxed:
        fields = [f for f in report["fields"] if f["page"] == page]
        probs = check_page(fields, blank[page], filled[page])
        result["pages"][page + 1] = probs
        for k, v in probs.items():
            totals.setdefault(k, []).extend(f"p{page + 1} {x}" for x in v)
        shown_fields = [f for f in review["fields"] if f["page"] == page]  # same order; carries group info
        result["images"].append({"page": page + 1, "files": sheet(slug, page, shown_fields, blank[page], shown[page], out)})
    result["unboxed_pages"] = []
    for page in range(len(blank)):
        if page not in boxed:  # every page without boxes goes to the reviewer (a form page may have been missed)
            p = out / f"{slug}-p{page + 1}-unboxed.png"
            Image.fromarray(blank[page]).save(p)
            result["unboxed_pages"].append({"page": page + 1, "file": str(p)})
    result["problems"] = {k: v for k, v in totals.items() if v}
    result["pass"] = not result["problems"]
    (out / f"{slug}.gates.json").write_text(json.dumps(result, ensure_ascii=False, indent=1))
    print(slug, "PASS" if result["pass"] else "FAIL", {k: len(v) for k, v in result["problems"].items()})  # details (labels) in gates.json
    return result


if __name__ == "__main__":
    args = sys.argv[1:]
    ras = None
    if "--rasters" in args:
        i = args.index("--rasters")
        ras = args[i + 1]
        del args[i:i + 2]
    main(*args[:8], rasters=ras)
