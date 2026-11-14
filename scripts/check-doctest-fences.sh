#!/usr/bin/env bash
# Fail when a doc comment opens a fence that rustdoc would run as Rust.
#
# All library crates set `[lib] doctest = false`, so `cargo test --doc` is
# never invoked by CI. A future runnable fence would then be silently
# skipped. This static guard keeps that skip visible: bare ``` fences and
# Rust fences (`rust`, `no_run`, `should_panic`, `compile_fail`,
# `test_harness`, `edition*`) fail unless they carry `ignore`.
# Language-tagged fences (`text`, `json`, `yaml`, ...) pass, and bare
# closing fences pass.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$repo_root" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
pattern = root / "crates"
failures = []
scanned = 0

DOC_PREFIX = re.compile(r"^\s*///{1,}/? ?|^\s*//!/? ?")

def doc_content(line):
    m = re.match(r"^\s*(///|//!)(.*)$", line)
    if not m:
        return None
    rest = m.group(2)
    if rest.startswith(" "):
        rest = rest[1:]
    return rest

def is_runnable_open(info):
    # info is the string after ``` or ~~~, stripped.
    tokens = [t.strip().lower() for t in re.split(r"[,\s]+", info) if t.strip()]
    if any(t == "ignore" for t in tokens):
        return False
    if not info.strip():
        return True
    first = tokens[0] if tokens else ""
    if first == "rust" or first.startswith("rust"):
        return True
    if first in ("no_run", "norun", "should_panic", "compile_fail", "compile-fail", "test_harness"):
        return True
    if first.startswith("no_run") or first.startswith("should_panic") or first.startswith("compile_fail") or first.startswith("test_harness") or first.startswith("edition"):
        return True
    return False

for path in sorted((root / "crates").glob("*/src/**/*.rs")):
    scanned += 1
    in_fence = None  # None or the opening marker ("```" or "~~~")
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        continue
    for lineno, line in enumerate(lines, start=1):
        content = doc_content(line)
        if content is None:
            continue
        stripped = content.strip()
        marker = None
        if stripped.startswith("```"):
            marker = "```"
        elif stripped.startswith("~~~"):
            marker = "~~~"
        if marker is None:
            continue
        info = stripped[len(marker):].strip()
        if in_fence is None:
            # Opening fence.
            if is_runnable_open(info):
                failures.append(f"{path.relative_to(root)}:{lineno}: runnable Rust doc fence {marker}{info!r} would be skipped (doctest = false); tag it with ignore or a language")
            in_fence = marker
        else:
            # Inside a fence: only a bare matching marker closes it.
            if stripped == in_fence:
                in_fence = None
            # Otherwise fence content; ignore even if it looks like a fence.

if failures:
    for failure in failures:
        print(f"check-doctest-fences: {failure}", file=sys.stderr)
    print(f"check-doctest-fences: {len(failures)} runnable Rust doc fence(s) in {scanned} files", file=sys.stderr)
    sys.exit(1)
print("check-doctest-fences: no runnable Rust doc fences")
PY
