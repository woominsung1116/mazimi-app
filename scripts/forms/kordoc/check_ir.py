"""Validates a kordoc IR file (scripts/forms/kordoc/ir.mjs, version 1) before any tool reads it.

The IR comes out of a sandboxed parser that read an untrusted agency file, so it is checked
like any untrusted input: a regular file (not a symlink), at most 8 MiB, strict JSON with the
exact shape below, bounded counts and string lengths, and bound to the source file by sha256.

Usage: python3 -I scripts/forms/kordoc/check_ir.py <file.ir.json> <source sha256>
Exit 0 and print "ok" only if valid. Standard library only (runs on the host's python3).
"""
from __future__ import annotations

import json
import os
import re
import stat
import sys

MAX_BYTES = 8 * 1024 * 1024
MAX_PAGES = 50
MAX_BLOCKS = 4000
MAX_TEXT = 4000
MAX_DEPTH = 4
MAX_CELLS = 5000
MAX_SPAN = 200
SHA = re.compile(r"^[0-9a-f]{64}$")
KINDS = {"paragraph", "heading", "list", "image", "separator", "table"}
WARNING = re.compile(r"^[A-Z_]{1,40}$")


class Invalid(Exception):
    pass


def need(cond: bool, why: str) -> None:
    if not cond:
        raise Invalid(why)


def is_int(v, lo: int, hi: int) -> bool:
    return isinstance(v, int) and not isinstance(v, bool) and lo <= v <= hi


def check_table(t, depth: int, budget: list) -> None:
    need(isinstance(t, dict) and set(t) == {"rows", "cols", "cells"}, "table keys")
    need(is_int(t["rows"], 0, MAX_CELLS) and is_int(t["cols"], 0, MAX_CELLS), "table size")
    need(isinstance(t["cells"], list), "cells")
    for c in t["cells"]:
        budget[0] += 1
        need(budget[0] <= MAX_CELLS * 4, "too many cells")
        need(isinstance(c, dict) and set(c) == {"r", "c", "rs", "cs", "text", "header", "tables"}, "cell keys")
        need(is_int(c["r"], 0, MAX_CELLS) and is_int(c["c"], 0, MAX_CELLS), "cell address")
        need(is_int(c["rs"], 1, MAX_SPAN) and is_int(c["cs"], 1, MAX_SPAN), "cell span")
        need(isinstance(c["text"], str) and len(c["text"]) <= MAX_TEXT, "cell text")
        need(isinstance(c["header"], bool), "cell header")
        need(isinstance(c["tables"], list), "nested tables")
        for n in c["tables"]:
            need(depth + 1 <= MAX_DEPTH, "nesting depth")
            check_table(n, depth + 1, budget)


def check(ir, source_sha256: str) -> None:
    need(isinstance(ir, dict), "not an object")
    need(set(ir) == {"version", "tool", "kordoc_version", "file", "source_sha256", "file_type",
                     "page_count", "page_mode", "warnings", "blocks"}, "top-level keys")
    need(ir["version"] == 1 and ir["tool"] == "kordoc", "version/tool")
    need(isinstance(ir["kordoc_version"], str) and re.match(r"^\d+\.\d+\.\d+$", ir["kordoc_version"]) is not None, "kordoc_version")
    need(isinstance(ir["file"], str) and 0 < len(ir["file"]) <= 255 and "/" not in ir["file"], "file")
    need(isinstance(ir["source_sha256"], str) and SHA.match(ir["source_sha256"]) is not None, "source_sha256")
    need(ir["source_sha256"] == source_sha256, "IR is not for this source file")
    need(ir["file_type"] in ("hwp", "hwpx"), "file_type")
    need(is_int(ir["page_count"], 0, MAX_PAGES), "page_count")
    need(ir["page_mode"] in (None, "layout", "section"), "page_mode")
    need(isinstance(ir["warnings"], list) and len(ir["warnings"]) <= 40
         and all(isinstance(w, str) and WARNING.match(w) for w in ir["warnings"]), "warnings")
    need(isinstance(ir["blocks"], list) and len(ir["blocks"]) <= MAX_BLOCKS, "blocks")
    budget = [0]
    for b in ir["blocks"]:
        need(isinstance(b, dict) and b.get("kind") in KINDS, "block kind")
        need(is_int(b.get("page"), 0, MAX_PAGES), "block page")
        if b["kind"] == "table":
            need(set(b) == {"page", "kind", "table"}, "table block keys")
            check_table(b["table"], 0, budget)
        else:
            need(set(b) == {"page", "kind", "text"} and isinstance(b["text"], str) and len(b["text"]) <= MAX_TEXT, "text block")


def load(path: str, source_sha256: str):
    """The validated IR, or Invalid. Never follows a symlink."""
    need(SHA.match(source_sha256 or "") is not None, "source sha256 argument")
    st = os.lstat(path)
    need(stat.S_ISREG(st.st_mode), "not a regular file")
    need(0 < st.st_size <= MAX_BYTES, "size")
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    with os.fdopen(fd, "rb") as fh:
        raw = fh.read(MAX_BYTES + 1)
    need(len(raw) <= MAX_BYTES, "size")

    def no_dupes(pairs):
        keys = [k for k, _ in pairs]
        need(len(keys) == len(set(keys)), "duplicate key")
        return dict(pairs)

    try:
        ir = json.loads(raw.decode("utf-8"), object_pairs_hook=no_dupes, parse_constant=lambda c: (_ for _ in ()).throw(Invalid(c)))
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as e:
        raise Invalid(f"json: {type(e).__name__}")
    check(ir, source_sha256)
    return ir


def main(argv: list) -> int:
    try:
        load(argv[0], argv[1])
    except (Invalid, OSError, IndexError) as e:
        print(f"invalid: {e}"[:200], file=sys.stderr)
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
