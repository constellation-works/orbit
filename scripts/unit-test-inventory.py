#!/usr/bin/env python3
"""Inventory unit tests per crate and ratchet them against a checked-in baseline.

A unit test is a `#[test]` or `#[<path>::test]` attribute (such as
`#[tokio::test]`) under `crates/<crate>/src/**`, whether it sits inline or in a
sibling `tests/` directory. This is the measure the boundary-first retirement
used (see docs/design-patterns/test_strategy.md), so the numbers stay comparable.

  unit-test-inventory.py                          print the inventory as JSON
  unit-test-inventory.py --check BASELINE         fail when a test is not admitted
  unit-test-inventory.py --write-baseline BASELINE  record the current test identities
"""

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

SCHEMA_VERSION = 2
TEST_ATTRIBUTE = re.compile(r"^\s*#\[(?:[A-Za-z_][A-Za-z0-9_]*::)*test\b")
FUNCTION = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(r#)?([A-Za-z_][A-Za-z0-9_]*)\b")
DEFAULT_ROOT = Path(__file__).resolve().parent.parent


def test_function_paths(text, relative_path):
    """Associate test attributes with function names, across intervening attributes/comments.

    Like the retirement inventory, this scans source rather than expanding Rust
    macros or evaluating cfg. Keep repeated identities: two inline modules in
    one file may have the same function name, and both consume an admission.
    """
    paths = []
    pending = None
    for number, line in enumerate(text.splitlines(), 1):
        if TEST_ATTRIBUTE.match(line):
            if pending is not None:
                raise ValueError(f"{relative_path}:{pending}: test attribute has no function name")
            pending = number
        if pending is not None:
            # Also accept attributes and the function header on the same line.
            header = line
            while True:
                stripped = re.sub(r"^\s*#\[.*?\]\s*", "", header)
                if stripped == header:
                    break
                header = stripped
            function = FUNCTION.match(header)
            if function:
                paths.append(f"{relative_path}::{function.group(1) or ''}{function.group(2)}")
                pending = None
    if pending is not None:
        raise ValueError(f"{relative_path}:{pending}: test attribute has no function name")
    return paths


def inventory(root):
    crates = {}
    for crate_dir in sorted((root / "crates").iterdir()):
        src = crate_dir / "src"
        if not src.is_dir():
            continue
        paths = []
        test_file_lines = 0
        for path in sorted(src.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            paths.extend(test_function_paths(text, path.relative_to(src).as_posix()))
            if "tests" in path.relative_to(src).parts[:-1]:
                test_file_lines += len(text.splitlines())
        crates[crate_dir.name] = {"test_functions": len(paths), "test_file_lines": test_file_lines,
                                  "test_function_paths": sorted(paths)}
    return {"schema_version": SCHEMA_VERSION, "crates": crates}


def baseline_of(inventory_doc):
    return {
        "schema_version": SCHEMA_VERSION,
        "crates": {name: {"test_function_paths": counts["test_function_paths"]}
                   for name, counts in inventory_doc["crates"].items()},
    }


def load_baseline(path):
    baseline = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(baseline, dict) or baseline.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"{path}: expected schema_version {SCHEMA_VERSION}")
    crates = baseline.get("crates")
    if not isinstance(crates, dict):
        raise ValueError(f"{path}: expected a crates object")
    for name, entry in crates.items():
        paths = entry.get("test_function_paths") if isinstance(entry, dict) else None
        if not isinstance(paths, list) or any(not isinstance(value, str) or not value for value in paths):
            raise ValueError(f"{path}: {name}: expected a test_function_paths array of non-empty strings")
    return crates


def over_baseline(current, baseline_crates):
    failures = []
    for name, counts in current["crates"].items():
        # A crate missing from the baseline has no admitted tests. Stale entries
        # are allowed so deleting tests never requires replenishing the allowance.
        admitted = baseline_crates.get(name, {}).get("test_function_paths", [])
        unadmitted = Counter(counts["test_function_paths"]) - Counter(admitted)
        if unadmitted:
            comparison = "above" if counts["test_functions"] > len(admitted) else "with tests outside"
            failures.append(
                f"unit-test ratchet: {name} has {counts['test_functions']} unit-test functions "
                f"under crates/{name}/src, {comparison} its baseline of {len(admitted)}. Admit the new tests "
                "under docs/design-patterns/test_strategy.md, then raise the baseline in the same "
                "change: scripts/unit-test-inventory.py --write-baseline scripts/unit-test-baseline.json\n"
                + "\n".join(f"  unadmitted: {identity}" for identity in sorted(unadmitted.elements()))
            )
    return failures


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT, help="repository root (default: this checkout)")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", type=Path, metavar="BASELINE", help="fail when a test is not admitted")
    mode.add_argument("--write-baseline", type=Path, metavar="BASELINE", help="write the current test identities as the baseline")
    args = parser.parse_args(argv)

    try:
        current = inventory(args.root)
    except (OSError, ValueError) as error:
        print(f"unit-test ratchet: cannot inventory tests: {error}", file=sys.stderr)
        return 2

    if args.write_baseline:
        args.write_baseline.write_text(json.dumps(baseline_of(current), indent=2, sort_keys=True) + "\n",
                                       encoding="utf-8")
        return 0

    if args.check:
        try:
            baseline_crates = load_baseline(args.check)
        except (OSError, ValueError) as error:
            print(f"unit-test ratchet: cannot read baseline: {error}", file=sys.stderr)
            return 2
        failures = over_baseline(current, baseline_crates)
        for failure in failures:
            print(failure, file=sys.stderr)
        if failures:
            return 1
        print("unit-test ratchet passed")
        return 0

    print(json.dumps(current, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
