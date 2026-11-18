"""Step 2: download each selected official file and its notice page, once.

Writes into <dl dir>, which must be new and empty (downloads are untrusted
data). The file's sha256 must equal the collector's content_hash; otherwise
the row is dropped ("hash_mismatch": the agency changed the file since the
probe, wait for the next probe). Also saves the notice page's visible text as
intake evidence (접수기간 lines), since approval attests the current round.

Politeness mirrors the collector: robots.txt per origin (4xx = allow-all per
RFC 9309; any other failure or a non-text body = fail closed), 30 s between
requests to one host, https only, no cross-host redirects, 8 MiB cap.
Standard library only; run with `python3 -I`.

Usage: python3 -I scripts/forms/auto/fetch.py <candidates.json> <dl dir> <sources.json> [--live]
  --live  also download rows whose template is already live (calibration set)
"""
from __future__ import annotations

import hashlib
import html
import json
import re
import sys
import time
import urllib.error
import urllib.request
import urllib.robotparser
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit

UA = "Mozilla/5.0 (compatible; Mazimi-Bot/1.0; +https://mazimi.kr/bot)"  # the collector's UA
MAX_BYTES = 8 * 1024 * 1024
INTERVAL = 30.0
_last: dict[str, float] = {}
_robots: dict[str, urllib.robotparser.RobotFileParser | None] = {}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urlsplit(newurl).hostname != urlsplit(req.full_url).hostname or not newurl.startswith("https://"):
            raise urllib.error.HTTPError(newurl, code, "cross-host or non-https redirect refused", headers, fp)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


OPENER = urllib.request.build_opener(NoRedirect)


def _wait(host: str) -> None:
    gap = time.monotonic() - _last.get(host, -1e9)
    if gap < INTERVAL:
        time.sleep(INTERVAL - gap)
    _last[host] = time.monotonic()


def get(url: str) -> tuple[int, str, bytes]:
    if not url.startswith("https://"):
        raise ValueError("https only")
    _wait(urlsplit(url).hostname or "")
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with OPENER.open(req, timeout=60) as res:
            body = res.read(MAX_BYTES + 1)
            return res.status, res.headers.get("Content-Type", ""), body
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Content-Type", "") if e.headers else "", b""
    except (urllib.error.URLError, OSError):
        return 0, "", b""  # network failure: the row is reported (http_0), never a crash of the whole run


def allowed(url: str) -> bool:
    parts = urlsplit(url)
    origin = f"https://{parts.hostname}"
    if origin not in _robots:
        status, ctype, body = 0, "", b""
        for _ in range(2):  # one retry on a network failure (status 0); still fail closed after
            try:
                status, ctype, body = get(origin + "/robots.txt")
            except Exception:
                status, ctype, body = 0, "", b""
            if status:
                break
        if 400 <= status < 500:
            rp = urllib.robotparser.RobotFileParser()
            rp.parse([])  # no robots.txt: no restriction
        elif status == 200 and ctype.startswith("text/plain"):
            rp = urllib.robotparser.RobotFileParser()
            rp.parse(body.decode("utf-8", "replace").splitlines())
        else:
            rp = None  # unavailable: fail closed, like the collector
        _robots[origin] = rp
    rp = _robots[origin]
    target = parts.path + (f"?{parts.query}" if parts.query else "")
    return rp is not None and rp.can_fetch(UA, target) and rp.can_fetch("Mazimi-Bot", target)


def slug_for(row: dict) -> str:
    labels = (row["host"] or "").split(".")
    site = next((l for l in labels if l not in ("www", "youth", "go", "or", "kr", "ac", "co")), "form")
    return re.sub(r"[^a-z0-9-]", "", f"{site}-{row['id']}".lower())[:64]  # full id: slugs never collide


def notice_text(body: bytes) -> dict:
    text = body.decode("utf-8", "replace")
    title = re.search(r"<title[^>]*>(.*?)</title>", text, re.S | re.I)
    text = re.sub(r"<(script|style)\b.*?</\1>", " ", text, flags=re.S | re.I)
    text = html.unescape(re.sub(r"<[^>]+>", "\n", text))
    lines = [re.sub(r"\s+", " ", l).strip() for l in text.splitlines()]
    lines = [l for l in lines if l]
    intake = [l[:200] for l in lines if re.search(r"접수|신청\s*기간|모집\s*기간|신청\s*기한", l)][:12]
    closed = [l[:120] for l in lines if re.search(r"접수\s*마감|모집\s*마감|마감\s*되었|종료\s*되었", l)][:5]
    return {"title": html.unescape(title.group(1)).strip()[:200] if title else "", "intake_lines": intake, "closed_lines": closed}


def main(argv: list[str]) -> None:
    cand_path, dl, out_path = argv[:3]
    live = "--live" in argv
    dl_dir = Path(dl)
    # Must be new (or an empty directory the server runner created): never mix with earlier downloads.
    dl_dir.mkdir(parents=True, exist_ok=True)
    if any(dl_dir.iterdir()):
        raise SystemExit(f"{dl_dir} is not empty")
    rows = [r for r in json.load(open(cand_path, encoding="utf-8"))["rows"] if r["verdict"] == "try" or (live and r["verdict"] == "live")]
    sources, results = [], []
    for row in rows:
        slug = slug_for(row)
        res = {"slug": slug, "resource_id": row["id"]}
        try:
            fetch_one(row, slug, res, dl_dir, sources)
        except Exception as e:  # one bad row never stops the day's run
            res["status"] = f"error_{type(e).__name__}"
        results.append(res)
        print(json.dumps(res, ensure_ascii=False))
    json.dump(sources, open(out_path, "w", encoding="utf-8"), ensure_ascii=False, indent=1)


def fetch_one(row: dict, slug: str, res: dict, dl_dir: Path, sources: list) -> None:
    if not row["url"].startswith("https://"):
        res["status"] = "not_https"
        return
    if not allowed(row["url"]):
        origin = f"https://{urlsplit(row['url']).hostname}"
        res["status"] = "robots_unavailable" if _robots.get(origin) is None else "robots_disallowed"
        return
    status, _, body = get(row["url"])
    digest = hashlib.sha256(body).hexdigest()
    if status != 200 or not body or len(body) > MAX_BYTES:
        res["status"] = f"http_{status}_or_size"
        return
    if digest != row["content_hash"]:
        res.update(status="hash_mismatch", sha256=digest)
        return
    with open(dl_dir / f"{slug}.{row['file_format']}", "xb") as fh:  # never overwrite
        fh.write(body)
    res.update(status="ok", sha256=digest)
    # The notice is fetched only from the file's own host over https (no other hosts: no SSRF).
    if urlsplit(row["source_url"]).hostname != urlsplit(row["url"]).hostname or not row["source_url"].startswith("https://"):
        notice = {"error": "notice on another host or not https: not fetched"}
    elif allowed(row["source_url"]):
        n_status, _, n_body = get(row["source_url"])
        notice = notice_text(n_body[:1024 * 1024]) if n_status == 200 else {"error": f"http {n_status}"}
    else:
        notice = {"error": "notice disallowed by robots"}
    sources.append({
        "slug": slug, "program": row["program_title"], "agency": row["provider_name"],
        "notice_url": row["source_url"], "file_url": row["url"],
        "retrieved_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "format": row["file_format"], "sha256": digest, "byte_size": len(body),
        "resource_id": row["id"], "program_id": row["program_id"], "window_end": row["window_end"],
        "verdict": row["verdict"], "notice": notice,
    })


if __name__ == "__main__":
    main(sys.argv[1:])
