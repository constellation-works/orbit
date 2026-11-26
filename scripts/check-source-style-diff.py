#!/usr/bin/env python3
"""Reject new findings from the template's STD-12 checker in a delivery diff."""

from collections import Counter
from pathlib import Path
import os
import runpy
import subprocess
import sys
import tempfile
from types import SimpleNamespace

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
STYLE = SimpleNamespace(**runpy.run_path(str(SCRIPTS / "check-source-style.py")))
DIFF = SimpleNamespace(**runpy.run_path(str(SCRIPTS / "ci-test-affected.py")))


def findings(paths):
    """Run the shared rules one file at a time to bound memory on large diffs."""
    results = []
    for path in paths:
        source = Path(path).read_text(encoding="utf-8")
        lines, literals = STYLE.lex(source.removesuffix("\n"))
        if STYLE.leading_comments_say_generated(lines):
            continue

        def report(name, row, rule, message):
            text = lines[row - 1].text.strip() if row else ""
            results.append(((name, rule, message, text), row))

        STYLE.check_names([path], report)
        STYLE.check_blank_lines(path, lines, report)
        STYLE.check_width(path, lines, literals, report)
        STYLE.check_use_groups(path, lines, report)
        STYLE.check_skips(path, lines, report)
        STYLE.check_item_order(path, lines, 0, len(lines), 0, report)
        STYLE.check_deep_super(path, lines, report)
    return results


def base_tree(base):
    entries = {}
    for entry in DIFF.git("ls-tree", "-r", "-z", base).split(b"\0"):
        if entry:
            header, name = entry.split(b"\t", 1)
            mode, _, blob = header.split()
            if mode in (b"100644", b"100755"):
                entries[os.fsdecode(name)] = blob.decode("ascii")
    return entries


def previous_findings(paths, entries, root):
    """Materialize only changed base files and the manifests delimiting their crates."""
    existing = [path for path in paths if path in entries]
    for path in existing:
        target = root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(DIFF.git("cat-file", "blob", entries[path]))
        for parent in Path(path).parents:
            manifest = parent / "Cargo.toml"
            if str(manifest) in entries:
                (root / manifest).touch()
                break
    os.chdir(root)
    try:
        return Counter(key for key, _ in findings(existing))
    finally:
        os.chdir(ROOT)


def main():
    os.chdir(ROOT)
    if STYLE.configured_width() != "100":
        sys.exit("rustfmt.toml: STD-12 R11: set max_width = 100")

    tip = DIFF.comparison_base(os.environ.get("CI_TEST_BASE"))
    base = DIFF.git("merge-base", "HEAD", tip).decode().strip()
    paths = sorted(
        path for path in DIFF.changed_paths(base)
        if path.endswith(".rs") and Path(path).is_file() and not Path(path).is_symlink()
    )
    if not paths:
        return 0
    entries = base_tree(base)
    with tempfile.TemporaryDirectory(prefix="orbit-style-") as directory:
        previous = previous_findings(paths, entries, Path(directory))
    old_dirs = {str(parent) + "/" for path in entries for parent in Path(path).parents}
    failures = set()
    for key, row in findings(paths):
        name, rule, message, _ = key
        if rule == "R1" and name in old_dirs:
            continue
        if previous[key] > 0:
            previous[key] -= 1
        else:
            location = f"{name}:{row}" if row else name
            failures.add(f"{location}: STD-12 {rule}: {message}")
    for failure in sorted(failures):
        print(failure, file=sys.stderr)
    print(f"source-style: {len(paths)} changed Rust files, {len(failures)} new findings")
    return int(bool(failures))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"source-style: {error}")
