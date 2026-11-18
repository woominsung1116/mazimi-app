#!/usr/bin/env python3
"""Fail when a service reads an environment variable that compose.yml does
not pass to it.

compose.yml gives api, worker and web an explicit `environment:` allowlist
instead of the whole `.env.production` file (least privilege: the worker must
not hold JWT_SECRET or OAuth secrets, nothing holds SUPABASE_SERVICE_ROLE_KEY).
An allowlist that misses a variable the code reads silently turns a feature
off in production, so this check derives every runtime read from the source
and compares it with the allowlist.

Detected reads (Rust): `std::env::var("X")` / `var_os("X")`, a `&str`
constant passed to them, and any string literal or constant passed to a
local helper or closure whose body calls `env::var(<its parameter>)`, a
closure named `var` included. A computed name is an error unless it is a
helper forwarding its own parameter inside that helper's body. `format!`-built names
must be listed in DYNAMIC below; an unlisted dynamic read fails the check.
`env!`/`option_env!` are compile-time and ignored. Code under `#[cfg(test)]`
and test-only modules is ignored.

Detected reads (web): `process.env.X` and `process.env["X"]` in apps/web/src
and next.config.ts.

Usage: python3 scripts/check-service-env.py   (needs `docker compose`)
       python3 scripts/check-service-env.py --list   (print reads per service)
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NAME = r"[A-Z][A-Z0-9_]*"

# Read implicitly by libraries (tracing EnvFilter::try_from_default_env).
IMPLICIT = {"api": {"RUST_LOG"}, "worker": {"RUST_LOG"}}

# Names built with format!. Key: (file, marker found on the read's line or the
# line before it) -> how the names expand. "{SOURCE}" expands to every worker
# DataSource name (`fn name(&self) -> &'static str { "..." }`).
DYNAMIC = {
    ("crates/worker/src/pipeline.rs", "SOURCE_ALLOW_EMPTY_"): ["SOURCE_ALLOW_EMPTY_{SOURCE}"],
    ("crates/worker/src/source_health.rs", "SOURCE_ACTIVE_FLOOR_"): ["SOURCE_ACTIVE_FLOOR_{SOURCE}"],
    ("crates/core/src/database.rs", "max_connections_env"): [
        "API_DB_MAX_CONNECTIONS",
        "WORKER_DB_MAX_CONNECTIONS",
        "JOB_DB_MAX_CONNECTIONS",
        "MIGRATION_DB_MAX_CONNECTIONS",
    ],
    ("crates/api/src/routes/auth.rs", "_CLIENT_ID"): [
        "KAKAO_CLIENT_ID",
        "GOOGLE_CLIENT_ID",
    ],
    ("crates/api/src/routes/auth.rs", "_CLIENT_SECRET"): [
        "KAKAO_CLIENT_SECRET",
        "GOOGLE_CLIENT_SECRET",
    ],
}

# Reads that need no value to work and must NOT be passed. The worker redacts
# these secrets from persisted error text; a secret it does not hold cannot
# leak, so the loop needs nothing.
NOT_REQUIRED = {
    "crates/worker/src/error_safety.rs": "SECRET_ENV_KEYS redaction loop",
}
# Optional reads that must not be passed through compose: a bare name in the
# allowlist would copy the operator's own login shell value.
OPTIONAL_NAMES = {
    "USER",  # import bins: default reviewer name; `--operator` overrides it
    "PATH",  # worker form_check: finds pdftotext for a child whose environment is cleared; the image sets it
}

# The api links the worker crate (admin manual ingestion, opt-out intake,
# Ollama client, evidence helpers). Every worker read is therefore required by
# the api too, except the ones below, each checked to be reachable only from
# worker-only entry points (worker main loop, worker bins). A new worker read
# fails the check until it is either allowlisted for the api or added here.
API_UNREACHABLE_WORKER_FILES = {
    # Worker entry points and the jobs only they start.
    "crates/worker/src/main.rs",
    "crates/worker/src/bin/",
    "crates/worker/src/daily.rs",
    "crates/worker/src/storage.rs",  # StorageConfig::from_env: daily.rs
    "crates/worker/src/source_health.rs",  # OutputPolicy::from_env: daily.rs
    "crates/worker/src/source_monitoring.rs",  # freshness_policy_from_env: main, daily, corporate health
    "crates/worker/src/ops_email.rs",  # EmailConfig::from_env: operations dispatch, digest
    "crates/worker/src/operations.rs",  # from_env only in dispatch_*_from_env / webhook_configured (worker main, digest); api calls resolve_event_tx
    "crates/worker/src/enrichment.rs",  # EnrichmentConfig::from_env: run_pending_from_env (worker); api calls redact_bounded
    "crates/worker/src/ai_extraction/job.rs",  # config_from_env: run_from_env / queue (worker); api uses current_source, PROGRAM_TEXT_COLUMNS
    "crates/worker/src/ai_extraction/auto.rs",  # AutoApplyConfig::from_env: job.rs
    "crates/worker/src/corporate_auto/mod.rs",  # Flags::from_env: worker main, census, evidence, health, reextract, weekly audit, optout::run_scheduled
    "crates/worker/src/corporate_auto/reextract.rs",  # flag: reextract job (worker)
    "crates/worker/src/notifications/",  # KakaoClient/FCM from_env: notification dispatch (worker)
    "crates/worker/src/assistant_agent.rs",  # proactive + retention purge (worker); the api reads ASSISTANT_RETENTION_DAYS itself
    "crates/worker/src/domain_approvals.rs",  # renewal job (worker)
    "crates/worker/src/condition_reviews.rs",  # confirmation_config_from_env: daily, confirm bin
    "crates/worker/src/application_resources/",  # Job::from_env (worker); live_page uses fetch helpers only
    "crates/worker/src/link_resolver/",  # Job::from_env: worker main, daily link metrics
    "crates/worker/src/sources/benefit_pages.rs",  # BenefitPageFetcher::from_env: worker main, daily, bin; api calls preflight_robots
    "crates/worker/src/sources/national_welfare.rs",  # daily only
    "crates/worker/src/sources/local_welfare.rs",  # daily only
    "crates/worker/src/sources/financial.rs",  # daily only
    "crates/worker/src/form_auto/",  # Config::from_env: the worker `form-auto` bin only; the api's importer has its own config
}

# The worker links mazimi-core but never calls AppConfig::from_env (api only:
# crates/api/src/main.rs) nor WorkerApiKeys::from_env (unused), so the
# config.rs reads (JWT_SECRET among them) are not the worker's.
WORKER_UNREACHABLE_FILES = {
    "crates/core/src/config.rs",
}

SERVICES = {
    "api": ["crates/api/src", "crates/core/src"],
    "worker": ["crates/worker/src", "crates/core/src"],
}
WEB_PATHS = ["apps/web/src", "apps/web/next.config.ts"]


def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


ATTRS = r"((?:#\[[^\]]*\]\s*)*)"
MOD_DECL = re.compile(ATTRS + r"(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")


def strip_tests(text: str) -> str:
    """Blank out `#[cfg(test)] mod x { ... }` blocks (brace matched), keeping
    line numbers."""
    out = []
    i = 0
    pat = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")
    while True:
        m = pat.search(text, i)
        if not m:
            out.append(text[i:])
            break
        out.append(text[i : m.start()])
        depth = 1
        j = m.end()
        while j < len(text) and depth:
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
            j += 1
        out.append("\n" * text[m.start() : j].count("\n"))
        i = j
    return "".join(out)


def production_files(src: Path) -> list[Path]:
    """Files compiled into a non-test build: walk `mod x;` declarations from
    the crate roots (lib.rs, main.rs, bin/*.rs), skipping `#[cfg(test)]`
    ones and honouring `#[path = "..."]`."""
    roots = [p for p in (src / "lib.rs", src / "main.rs") if p.exists()]
    roots += sorted((src / "bin").glob("*.rs")) if (src / "bin").is_dir() else []
    seen: list[Path] = []
    stack = list(roots)
    while stack:
        f = stack.pop()
        if f in seen or not f.exists():
            continue
        seen.append(f)
        text = strip_tests(f.read_text())
        is_root = f.name in ("mod.rs", "lib.rs", "main.rs") or f.parent.name == "bin" and f in roots
        moddir = f.parent if is_root else f.parent / f.stem
        for m in MOD_DECL.finditer(text):
            attrs, name = m.group(1), m.group(2)
            if "cfg(test)" in attrs.replace(" ", ""):
                continue
            path_attr = re.search(r"#\[path\s*=\s*\"([^\"]+)\"\]", attrs)
            if path_attr:
                stack.append(f.parent / path_attr.group(1))
            else:
                stack += [moddir / f"{name}.rs", moddir / name / "mod.rs"]
    return seen


def source_names() -> list[str]:
    names = set()
    for f in (ROOT / "crates/worker/src/sources").rglob("*.rs"):
        for m in re.finditer(r"fn name\(&self\)\s*->\s*&(?:'static\s+)?str\s*\{\s*\"([a-z0-9_]+)\"", f.read_text()):
            names.add(m.group(1).upper())
    return sorted(names)


class Read:
    def __init__(self, name: str, file: str, line: int):
        self.name, self.file, self.line = name, file, line


def first_arg(text: str, pos: int) -> str:
    """The first call argument starting at `pos` (just after the `(`)."""
    depth = 0
    i = pos
    while i < len(text):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                break
            depth -= 1
        elif c == "," and depth == 0:
            break
        elif c == '"':
            i = text.index('"', i + 1)
        i += 1
    return text[pos:i].strip()


def rust_reads(paths: list[str]) -> tuple[list[Read], list[str]]:
    reads: list[Read] = []
    errors: list[str] = []
    sources = source_names()
    texts: dict[str, str] = {}
    for p in paths:
        for f in production_files(ROOT / p):
            texts[rel(f)] = strip_tests(f.read_text())

    # Global tables: `const X: &str = "NAME";` (by file stem) and helpers,
    # i.e. fns/closures taking `p: &str` whose body calls env::var(p) or
    # another helper with p.
    consts: dict[str, dict[str, str]] = {}
    for r, text in texts.items():
        for cname, value in re.findall(r"const\s+(\w+)\s*:\s*&(?:'static\s+)?str\s*=\s*\"(" + NAME + r")\"\s*;", text):
            consts.setdefault(cname, {})[r] = value
    def body_of(text: str, pos: int) -> str:
        """The brace-matched body that starts at the first `{` after pos."""
        start = text.find("{", pos)
        if start < 0:
            return ""
        depth, j = 0, start
        while j < len(text):
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        return text[start : j + 1]

    helper_defs = []  # (file, name, param, body)
    # Where each helper body sits: a parameter is "forwarded" only inside
    # its own helper's body, never anywhere else in the file.
    helper_spans: dict[tuple[str, str], list[tuple[int, int]]] = {}
    for r, text in texts.items():
        for m in re.finditer(r"fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(\s*(\w+)\s*:\s*&str", text):
            body = body_of(text, m.end())
            helper_defs.append((r, m.group(1), m.group(2), body))
        # Any fn with a `p: &str` parameter in any position forwards `p`
        # only inside its own body.
        for m in re.finditer(r"fn\s+\w+\s*(?:<[^>]*>)?\s*\(", text):
            depth, j = 1, m.end()
            while j < len(text) and depth:
                depth += {"(": 1, ")": -1}.get(text[j], 0)
                j += 1
            params = text[m.end() : j - 1]
            start = text.find("{", j)
            if start < 0:
                continue
            body = body_of(text, j)
            for param in re.findall(r"(?<![(\w])(\w+)\s*:\s*&(?:'\w+\s+)?str\b", params):
                helper_spans.setdefault((r, param), []).append((start, start + len(body)))
        for m in re.finditer(r"let\s+(\w+)\s*=\s*(?:move\s*)?\|\s*(\w+)\s*:\s*&str[^|]*\|", text):
            semi, brace = text.find(";", m.end()), text.find("{", m.end())
            body = body_of(text, m.end()) if 0 <= brace < semi else text[m.end() : semi]
            helper_defs.append((r, m.group(1), m.group(2), body))
            helper_spans.setdefault((r, m.group(2)), []).append((m.end(), m.end() + len(body) + 1))
    helpers: dict[str, set[str]] = {}  # name -> defining files
    changed = True
    while changed:
        changed = False
        for r, fname, param, body in helper_defs:
            if r in helpers.get(fname, set()):
                continue
            names = ["env::var", "env::var_os"] + list(helpers)
            # A method call (`conditions.flag(key)`) is not a helper call.
            if re.search(r"(?<![\w.])(?:" + "|".join(map(re.escape, names)) + r")\(\s*&?" + re.escape(param) + r"\s*[,)]", body):
                helpers.setdefault(fname, set()).add(r)
                changed = True
    helper_params = {(r, p) for r, f, p, _ in helper_defs if r in helpers.get(f, set())}

    def is_helper_call(name: str, r: str, text: str, pos: int) -> bool:
        if name in ("var", "var_os") and text[:pos].endswith("env::"):
            return True
        if name in ("var", "var_os") and text[:pos].endswith("::"):
            return False
        defs = helpers.get(name)
        if not defs:
            return False
        if r in defs or text[:pos].endswith("::"):
            return True
        return bool(re.search(r"use\s[^;]*(?:\b" + re.escape(name) + r"\b|::\*)[^;]*;", text))

    def resolve_const(arg: str, r: str) -> str | None:
        parts = arg.split("::")
        table = consts.get(parts[-1])
        if not table:
            return None
        if len(parts) == 1 and r in table:
            return table[r]
        if len(parts) > 1:
            for f, v in table.items():
                if Path(f).stem == parts[-2]:
                    return v
        return next(iter(table.values())) if len(table) == 1 else None

    call = re.compile(r"(?<![\w.])(" + "|".join(sorted(map(re.escape, ["var", "var_os", *helpers]))) + r")\(")
    for r, text in texts.items():
        lines = text.split("\n")
        for m in call.finditer(text):
            if not is_helper_call(m.group(1), r, text, m.start()):
                continue
            # Skip definitions (`fn helper(`).
            if re.search(r"fn\s+$", text[max(0, m.start() - 8) : m.start()]):
                continue
            arg = first_arg(text, m.end())
            ln = text.count("\n", 0, m.start()) + 1
            bare = arg.lstrip("&").strip()
            lit = re.fullmatch(r"\"(" + NAME + r")\"", bare)
            if lit:
                reads.append(Read(lit.group(1), r, ln))
                continue
            if re.fullmatch(r"[\w:]+", bare) and resolve_const(bare, r):
                reads.append(Read(resolve_const(bare, r), r, ln))
                continue
            if any(a <= m.start() < b for a, b in helper_spans.get((r, bare), [])):
                continue  # a helper forwarding its own parameter, inside its body
            if r in NOT_REQUIRED:
                continue
            context = lines[ln - 1] + "\n" + (lines[ln - 2] if ln >= 2 else "")
            matched = False
            for (dfile, marker), expansions in DYNAMIC.items():
                if dfile == r and (marker in context or marker in arg):
                    matched = True
                    for e in expansions:
                        for n in ([e.replace("{SOURCE}", s) for s in sources] if "{SOURCE}" in e else [e]):
                            reads.append(Read(n, r, ln))
            if not matched:
                errors.append(f"{r}:{ln}: env read with a computed name `{arg}`: add it to DYNAMIC in scripts/check-service-env.py")
    reads = [x for x in reads if x.name not in OPTIONAL_NAMES]
    return reads, errors


def web_reads() -> list[Read]:
    reads = []
    for p in WEB_PATHS:
        base = ROOT / p
        files = [base] if base.is_file() else [f for f in base.rglob("*") if f.suffix in (".ts", ".tsx", ".js", ".mjs")]
        for f in files:
            text = f.read_text()
            for m in re.finditer(r"process\.env(?:\.(" + NAME + r")|\[\s*[\"'](" + NAME + r")[\"']\s*\])", text):
                reads.append(Read(m.group(1) or m.group(2), rel(f), text.count("\n", 0, m.start()) + 1))
    return reads


def compose_allowlists() -> dict[str, set[str]]:
    out = subprocess.run(
        ["docker", "compose", "-f", str(ROOT / "compose.yml"), "config", "--no-interpolate", "--format", "json"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    cfg = json.loads(out)
    lists = {}
    for svc, spec in cfg["services"].items():
        if spec.get("env_file"):
            lists[svc] = None
            continue
        env = spec.get("environment") or {}
        lists[svc] = set(env.keys() if isinstance(env, dict) else [e.split("=", 1)[0] for e in env])
    return lists


# Set by Next.js itself at runtime, never by compose.
WEB_RUNTIME_PROVIDED = {"NODE_ENV", "NEXT_RUNTIME", "NEXT_PHASE"}


def main() -> int:
    required: dict[str, dict[str, list[Read]]] = {}
    errors: list[str] = []
    worker_reads, errs = rust_reads(SERVICES["worker"])
    errors += errs
    api_reads, errs = rust_reads(SERVICES["api"])
    errors += errs
    def excluded(r: Read, files: set[str]) -> bool:
        return any(r.file == u or (u.endswith("/") and r.file.startswith(u)) for u in files)

    worker_reads = [r for r in worker_reads if not excluded(r, WORKER_UNREACHABLE_FILES)]
    api_reads += [
        r for r in worker_reads if r.file.startswith("crates/worker/") and not excluded(r, API_UNREACHABLE_WORKER_FILES)
    ]
    for svc, reads in (("api", api_reads), ("worker", worker_reads), ("web", web_reads())):
        by_name: dict[str, list[Read]] = {}
        for r in reads:
            by_name.setdefault(r.name, []).append(r)
        for n in IMPLICIT.get(svc, ()):
            by_name.setdefault(n, [Read(n, "(library default)", 0)])
        if svc == "web":
            for n in WEB_RUNTIME_PROVIDED:
                by_name.pop(n, None)
        required[svc] = by_name

    if "--list" in sys.argv:
        for svc, by_name in required.items():
            print(f"[{svc}] {len(by_name)}")
            for n in sorted(by_name):
                print(f"  {n}  ({by_name[n][0].file}:{by_name[n][0].line})")
        return 0

    allow = compose_allowlists()
    failed = bool(errors)
    for e in errors:
        print(f"ERROR {e}")
    for svc, by_name in required.items():
        allowed = allow.get(svc)
        if allowed is None:
            print(f"ERROR {svc}: uses env_file; give it an explicit environment allowlist")
            failed = True
            continue
        for n in sorted(set(by_name) - allowed):
            first = by_name[n][0]
            print(f"ERROR {svc}: reads {n} ({first.file}:{first.line}) but compose.yml does not pass it")
            failed = True
    for svc, allowed in allow.items():
        if allowed and "SUPABASE_SERVICE_ROLE_KEY" in allowed:
            print(f"ERROR {svc}: SUPABASE_SERVICE_ROLE_KEY is passed but no code reads it")
            failed = True
    if "JWT_SECRET" in (allow.get("worker") or set()) and "JWT_SECRET" not in required["worker"]:
        print("ERROR worker: JWT_SECRET is passed but the worker does not read it")
        failed = True
    if not failed:
        print("ok: " + ", ".join(f"{s} reads {len(required[s])} vars, all allowlisted" for s in required))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
