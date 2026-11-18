#!/usr/bin/env python3
"""Fail when the Expo web export has an inline <script> that the app host's
Content-Security-Policy (infra/caddy/Caddyfile, {$APP_DOMAIN} block) does
not allow by hash. Without this, an Expo upgrade that changes the inline
hydrate script would ship a blank app.

Usage: python3 scripts/check-app-csp.py <export dir>
"""
from __future__ import annotations

import base64
import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def app_policy() -> str:
    text = (ROOT / "infra/caddy/Caddyfile").read_text()
    block = text[text.index("{$APP_DOMAIN") :]
    m = re.search(r'Content-Security-Policy "([^"]+)"', block)
    if not m:
        sys.exit("no Content-Security-Policy in the {$APP_DOMAIN} block")
    return m.group(1)


def main() -> int:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    export = Path(sys.argv[1])
    pages = sorted(export.rglob("*.html"))
    if not pages:
        sys.exit(f"no .html files under {export}")
    policy = app_policy()
    script_src = re.search(r"script-src ([^;]+)", policy).group(1)
    allowed = set(re.findall(r"'sha256-([A-Za-z0-9+/=]+)'", script_src))
    failed = 0
    for page in pages:
        html = page.read_text(errors="replace")
        for body in re.findall(r"<script(?![^>]*\bsrc=)[^>]*>(.*?)</script>", html, re.S):
            digest = base64.b64encode(hashlib.sha256(body.encode()).digest()).decode()
            if digest not in allowed:
                print(f"ERROR {page.relative_to(export)}: inline script sha256-{digest} is not in the app CSP")
                failed += 1
        if re.search(r"\son[a-z]+\s*=", re.sub(r"<script.*?</script>", "", html, flags=re.S)):
            print(f"ERROR {page.relative_to(export)}: inline event handler attribute (blocked by the app CSP)")
            failed += 1
    if failed:
        return 1
    print(f"ok: {len(pages)} pages, every inline script allowed by the app CSP")
    return 0


if __name__ == "__main__":
    sys.exit(main())
