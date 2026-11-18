#!/usr/bin/env python3
"""Every file a Rust image compiles in with include_str!/include_bytes! is copied into that image,
and every binary a Rust image copies out of target/release is a binary target its build makes.

For each infra/docker/Dockerfile.* that runs `cargo build ... -p <package>`, this takes the
crates the package depends on (normal [dependencies] by path, transitively), finds every
include_str!/include_bytes! with a literal path in their non-test source, and checks the
resolved path sits under a COPY of the builder stage and is not left out by .dockerignore. Test code is skipped: files named
*tests.rs, modules declared under #[cfg(test)], and #[cfg(test)] inline modules.

Why: the image builds run only on pushes to main, so a missing COPY is first seen there
(10-31: crates/worker/src/form_auto/candidates.rs read scripts/forms/auto/candidates.sql,
which no Dockerfile copied, and the worker and api images failed to build).

Binaries: each `COPY --from=<builder> .../target/release/<name>` must be a binary target of a
package the builder stage builds: a [[bin]] whose path exists, or an auto-discovered
src/bin/<name>.rs / src/bin/<name>/main.rs (11-06: the discover-brands target was deleted while
infra/docker/Dockerfile.worker still copied it, so the worker image would have failed on main).

Run: python3 scripts/check-docker-includes.py   (exit 1 lists each uncovered include or binary)
"""
import fnmatch
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
INCLUDE = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)')
CFG_TEST = re.compile(r"#\[cfg\(test\)\]")
MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
MOD_OPEN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def sections(manifest: pathlib.Path) -> dict[str, list[str]]:
    """Cargo.toml lines by [section] (only what this check reads; no TOML library needed)."""
    out, cur = {}, ""
    for line in manifest.read_text().split("\n"):
        head = re.match(r"^\s*\[\[?([^\]]+)\]\]?\s*$", line)
        if head:
            cur = head.group(1).strip()
            continue
        out.setdefault(cur, []).append(line)
    return out


def package_name(manifest: pathlib.Path) -> str:
    for line in sections(manifest).get("package", []):
        m = re.match(r'^\s*name\s*=\s*"([^"]+)"', line)
        if m:
            return m.group(1)
    raise SystemExit(f"{manifest}: no package name")


def crates() -> dict:
    """package name -> (crate dir, path dependencies by package name). Normal [dependencies] only."""
    out = {}
    for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        deps = []
        for line in sections(manifest).get("dependencies", []):
            m = re.search(r'path\s*=\s*"([^"]+)"', line)
            if m:
                deps.append(package_name((manifest.parent / m.group(1) / "Cargo.toml").resolve()))
        out[package_name(manifest)] = (manifest.parent, deps)
    return out


def bin_targets(crate_dir: pathlib.Path) -> set:
    """Binary target names cargo builds for the crate: each [[bin]] whose source exists (its
    `path`, else src/bin/<name>.rs or src/main.rs), plus auto-discovered src/bin targets unless
    `autobins = false`."""
    text = (crate_dir / "Cargo.toml").read_text()
    names, blocks, cur = set(), [], None
    autobins = True
    for line in text.split("\n"):
        head = re.match(r"^\s*\[\[?([^\]]+)\]\]?\s*$", line)
        if head:
            cur = {} if head.group(1).strip() == "bin" and line.strip().startswith("[[") else None
            if cur is not None:
                blocks.append(cur)
            continue
        m = re.match(r'^\s*(name|path)\s*=\s*"([^"]+)"', line)
        if cur is not None and m:
            cur[m.group(1)] = m.group(2)
        if re.match(r"^\s*autobins\s*=\s*false", line):
            autobins = False
    for block in blocks:
        name = block.get("name")
        if not name:
            continue
        candidates = [crate_dir / block["path"]] if "path" in block else [
            crate_dir / "src/bin" / f"{name}.rs", crate_dir / "src/bin" / name / "main.rs",
            crate_dir / "src/main.rs"]
        if any(c.exists() for c in candidates):
            names.add(name)
    if autobins and (crate_dir / "src/bin").is_dir():
        for f in (crate_dir / "src/bin").iterdir():
            if f.suffix == ".rs":
                names.add(f.stem)
            elif f.is_dir() and (f / "main.rs").exists():
                names.add(f.name)
    return names


def released_binaries(dockerfile: pathlib.Path) -> list:
    """(line number, name) of each binary copied out of a build stage's target/release."""
    out = []
    for n, line in enumerate(dockerfile.read_text().split("\n"), 1):
        m = re.match(r"\s*COPY\s+--from=\S+\s+\S*/target/release/([\w.-]+)\s", line)
        if m:
            out.append((n, m.group(1)))
    return out


def closure(package: str, graph: dict) -> set:
    seen, todo = set(), [package]
    while todo:
        p = todo.pop()
        if p in seen or p not in graph:
            continue
        seen.add(p)
        todo.extend(graph[p][1])
    return seen


def code_lines(text: str):
    """Yields (line, in_test, test_mods) per line, skipping #[cfg(test)] inline modules."""
    lines = text.split("\n")
    test_mods, depth_end = set(), None
    pending_cfg_test = False
    depth = 0
    for line in lines:
        stripped = re.sub(r'"(?:\\.|[^"\\])*"', '""', line.split("//")[0])
        if depth_end is None and pending_cfg_test:
            decl = MOD_DECL.match(line)
            if decl:
                test_mods.add(decl.group(1))
                pending_cfg_test = False
            elif MOD_OPEN.match(line):
                depth_end = depth
                pending_cfg_test = False
            elif line.strip() and not line.strip().startswith("#["):
                pending_cfg_test = False
        if CFG_TEST.search(line):
            pending_cfg_test = True
        in_test = depth_end is not None
        yield line, in_test
        depth += stripped.count("{") - stripped.count("}")
        if depth_end is not None and depth <= depth_end and "}" in stripped:
            depth_end = None
    code_lines.test_mods = test_mods


def test_files(src: pathlib.Path) -> set:
    """Files that are test modules: *tests.rs, or declared under #[cfg(test)] `mod name;`."""
    out = set()
    for f in src.rglob("*.rs"):
        if f.name.endswith("tests.rs") or "tests" in f.relative_to(src).parts[:-1]:
            out.add(f)
        list(code_lines(f.read_text()))
        for name in code_lines.test_mods:
            base = f.parent if f.name in ("mod.rs", "lib.rs", "main.rs") else f.parent / f.stem
            for cand in (base / f"{name}.rs", base / name / "mod.rs"):
                if cand.exists():
                    out.add(cand)
                    if cand.name == "mod.rs":
                        out.update(cand.parent.rglob("*.rs"))
    return out


def includes(crate_dir: pathlib.Path):
    src = crate_dir / "src"
    skip = test_files(src)
    for f in sorted(src.rglob("*.rs")):
        if f in skip:
            continue
        for n, (line, in_test) in enumerate(code_lines(f.read_text()), 1):
            if in_test:
                continue
            for m in INCLUDE.finditer(line):
                target = (f.parent / m.group(1)).resolve()
                yield f.relative_to(ROOT), n, target


def builder_copies(dockerfile: pathlib.Path) -> tuple:
    """(packages built, COPY sources) of the stage that runs cargo build."""
    stages, cur = [], []
    for line in dockerfile.read_text().split("\n"):
        if line.startswith("FROM "):
            cur = []
            stages.append(cur)
        cur.append(line)
    for stage in stages:
        build = [l for l in stage if re.search(r"\bcargo build\b", l)]
        if not build:
            continue
        packages = [p for l in build for p in re.findall(r"-p\s+([\w-]+)", l)]
        sources = []
        for l in stage:
            m = re.match(r"COPY\s+(?!--from)(.+)", l)
            if m:
                sources.extend(m.group(1).split()[:-1])
        return packages, sources
    return [], []


def inside(path: pathlib.Path) -> bool:
    try:
        path.relative_to(ROOT)
        return True
    except ValueError:
        return False


def ignored(rel: str) -> bool:
    """True when .dockerignore leaves the path out of the build context (simple patterns, last match wins)."""
    path = ROOT / ".dockerignore"
    out = False
    for raw in (path.read_text().split("\n") if path.exists() else []):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        neg = line.startswith("!")
        pat = line[1:] if neg else line
        pat = pat.rstrip("/")
        parts = rel.split("/")
        prefixes = ["/".join(parts[:i]) for i in range(1, len(parts) + 1)]
        if any(fnmatch.fnmatchcase(p, pat) or fnmatch.fnmatchcase(p, "**/" + pat) for p in prefixes) \
                or fnmatch.fnmatchcase(rel, pat):
            out = not neg
    return out


def covered(target: pathlib.Path, sources: list) -> bool:
    rel = target.relative_to(ROOT).as_posix()
    if ignored(rel):
        return False
    for s in sources:
        s = s.rstrip("/")
        if rel == s or rel.startswith(s + "/"):
            return True
    return False


def main() -> int:
    graph = crates()
    problems, checked = [], 0
    for dockerfile in sorted((ROOT / "infra/docker").glob("Dockerfile.*")):
        packages, sources = builder_copies(dockerfile)
        if not packages:
            continue
        crate_names = set().union(*(closure(p, graph) for p in packages))
        built = set().union(*(bin_targets(graph[p][0]) for p in packages if p in graph))
        for n, name in released_binaries(dockerfile):
            checked += 1
            if name not in built:
                problems.append(f"{dockerfile.relative_to(ROOT)}:{n} copies target/release/{name}, "
                                f"which is no binary target of {', '.join(packages)}")
        for name in sorted(crate_names):
            for f, n, target in includes(graph[name][0]):
                checked += 1
                if not inside(target) or not covered(target, sources):
                    problems.append(f"{dockerfile.relative_to(ROOT)}: {f}:{n} includes "
                                    f"{target.relative_to(ROOT) if inside(target) else target}, "
                                    "which the builder stage does not COPY (or .dockerignore leaves out)")
    for p in problems:
        print(p)
    print(f"check-docker-includes: {checked} include(s) and binaries checked, {len(problems)} uncovered")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
