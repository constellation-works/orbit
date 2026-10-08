#!/usr/bin/env python3
"""Inventory unit tests per crate and ratchet them against a checked-in baseline.

A unit test is a `#[test]` or `#[<path>::test]` attribute (such as
`#[tokio::test]`) under `crates/<crate>/src/**`, whether it sits inline or in a
sibling `tests/` directory. This is the measure the boundary-first retirement
used (see docs/design-patterns/test_strategy.md), so the numbers stay comparable.

  unit-test-inventory.py                          print the inventory as JSON
  unit-test-inventory.py --check BASELINE         fail when a crate exceeds its baseline
  unit-test-inventory.py --write-baseline BASELINE  record the current counts
"""

import argparse
import json
import re
import sys
from pathlib import Path

SCHEMA_VERSION = 1
TEST_ATTRIBUTE = re.compile(r"^\s*#\[(?:[A-Za-z_][A-Za-z0-9_]*::)?test\b")
DEFAULT_ROOT = Path(__file__).resolve().parent.parent


def inventory(root):
    crates = {}
    for crate_dir in sorted((root / "crates").iterdir()):
        src = crate_dir / "src"
        if not src.is_dir():
            continue
        test_functions = 0
        test_file_lines = 0
        for path in sorted(src.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            test_functions += sum(1 for line in text.splitlines() if TEST_ATTRIBUTE.match(line))
            if "tests" in path.relative_to(src).parts[:-1]:
                test_file_lines += len(text.splitlines())
        crates[crate_dir.name] = {"test_functions": test_functions, "test_file_lines": test_file_lines}
    return {"schema_version": SCHEMA_VERSION, "crates": crates}


def baseline_of(inventory_doc):
    return {
        "schema_version": SCHEMA_VERSION,
        "crates": {name: {"test_functions": counts["test_functions"]}
                   for name, counts in inventory_doc["crates"].items()},
    }


def load_baseline(path):
    baseline = json.loads(path.read_text(encoding="utf-8"))
    if baseline.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"{path}: expected schema_version {SCHEMA_VERSION}")
    return baseline["crates"]


def over_baseline(current, baseline_crates):
    failures = []
    for name, counts in current["crates"].items():
        # A crate missing from the baseline has an allowance of zero.
        allowed = baseline_crates.get(name, {}).get("test_functions", 0)
        if counts["test_functions"] > allowed:
            failures.append(
                f"unit-test ratchet: {name} has {counts['test_functions']} unit-test functions "
                f"under crates/{name}/src, above its baseline of {allowed}. Admit the new tests "
                "under docs/design-patterns/test_strategy.md, then raise the baseline in the same "
                "change: scripts/unit-test-inventory.py --write-baseline scripts/unit-test-baseline.json"
            )
    return failures


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT, help="repository root (default: this checkout)")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", type=Path, metavar="BASELINE", help="fail when a crate exceeds its baseline")
    mode.add_argument("--write-baseline", type=Path, metavar="BASELINE", help="write the current counts as the baseline")
    args = parser.parse_args(argv)

    current = inventory(args.root)

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
