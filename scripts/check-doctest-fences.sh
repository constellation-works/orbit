#!/usr/bin/env bash
set -euo pipefail

# Doctest fence guard [ORB-24429].
#
# Every workspace library sets `doctest = false`, so `cargo test --doc` and
# `rustdoc --test` run zero doctests. A doc fence that rustdoc would treat as
# Rust would then be silently skipped: it neither compiles nor runs, and
# nothing else notices. This guard fails such a fence so a future doctest is
# a reviewed diff (opt back into doctests or retag the fence) instead of dead
# documentation.
#
# It scans `crates/*/src/**/*.rs` doc comments (`///`, `//!`) for fenced code
# blocks and fails an opening fence that would run as Rust: a bare fence or a
# `rust` fence (including `rust,<attr>`), or a bare Rust attribute fence
# (`no_run`, `should_panic`, `compile_fail`, `test_harness`, `edition*`),
# unless the fence is explicitly `ignore`d. Language-tagged fences (`text`,
# `json`, `yaml`, `sh`, `console`, ...) are not Rust and pass. Closing fences
# (a bare fence while inside a block) pass. Only opening fences are reported.
#
# Re-check with: scripts/check-doctest-fences.sh
# Expect failure on: a doc comment with a bare ``` fence holding Rust code.

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
  echo "check-doctest-fences: ripgrep (rg) is required; install it before running" >&2
  exit 1
fi

python3 - <<'PY'
import re
import sys
from pathlib import Path

ROOT = Path(".")
SRC = ROOT / "crates"

# A fence run of three or more backticks or tildes, with its info string.
FENCE = re.compile(r"^(?P<mark>`{3,}|~{3,})(?P<info>.*)$")

# Bare Rust attribute fences that rustdoc treats as Rust without a `rust`
# prefix. `ignore` is deliberately absent: an ignored fence already runs
# zero tests, so disabling doctests changes nothing for it.
RUST_ATTRS = {"no_run", "should_panic", "compile_fail", "test_harness"}

def is_rust_fence(info):
    text = info.strip()
    if not text:
        return True
    # An info string can carry comma- or space-separated attributes
    # (`rust,no_run`, `rust ignore`). An explicitly ignored fence stays
    # ignored with doctests disabled, so it is not a silent skip.
    tokens = [t for t in re.split(r"[,\s]+", text) if t]
    if "ignore" in tokens:
        return False
    first = tokens[0] if tokens else ""
    if first == "rust":
        return True
    if first in RUST_ATTRS:
        return True
    if first.startswith("edition"):
        return True
    return False

def doc_content(line):
    stripped = line.lstrip()
    if stripped.startswith("///"):
        return stripped[3:]
    if stripped.startswith("//!"):
        return stripped[3:]
    return None

failures = []
files = sorted(SRC.glob("*/src/**/*.rs"))
if not files:
    print("check-doctest-fences: no crates/*/src/**/*.rs files found", file=sys.stderr)
    sys.exit(1)

for path in files:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as e:
        print(f"check-doctest-fences: cannot read {path}: {e}", file=sys.stderr)
        sys.exit(1)
    in_fence = False
    for number, line in enumerate(text.splitlines(), 1):
        content = doc_content(line)
        if content is None:
            continue
        body = content.lstrip()
        m = FENCE.match(body)
        if not m:
            continue
        info = m.group("info").strip()
        if not in_fence:
            if is_rust_fence(info):
                failures.append(f"{path}:{number}: doc fence would run as Rust (doctest = false skips it): {body.strip()}")
            in_fence = True
        else:
            # A bare fence closes the block; a fence with an info string
            # inside a block is content, not a new fence.
            if not info:
                in_fence = False
    if in_fence:
        failures.append(f"{path}: unclosed doc fence (missing closing ```)")

if failures:
    print("check-doctest-fences: runnable Rust doc fences found (doctest = false would skip them):", file=sys.stderr)
    for f in failures:
        print(f"  {f}", file=sys.stderr)
    print("Tag the fence with a non-Rust language (```text, ```json, ```yaml, ...) or ```ignore,", file=sys.stderr)
    print("or opt the crate back into doctests with an explicit reason.", file=sys.stderr)
    sys.exit(1)

print("check-doctest-fences: no runnable Rust doc fences under crates/*/src")
PY
