#!/usr/bin/env python3
"""Inventory panic-pattern sites across the Orbit workspace.

Counts `.unwrap()`, `.expect(...)`, `panic!(...)`, `unreachable!(...)`,
`unimplemented!(...)`, and `todo!(...)` matches per crate and bucketizes
them by whether they fall inside a `#[cfg(test)]` scope. Used both as a
human-readable report and as a JSON oracle for the panic-audit task
(``T20260509-6``) baseline / regression tracking.

The heuristic for "inside `#[cfg(test)]`" is intentionally simple but
matches every Rust convention used in this repo today:

* Files whose path matches ``*_tests.rs``, ``*/tests/*``, ``*test_support*``,
  or ``*/fixtures/*`` are treated as test-only end-to-end.
* Otherwise, a brace-counting walk tracks whether each line lives inside
  any nested ``#[cfg(test)]`` (or ``#[cfg(any(test, ...))]`` /
  ``#[cfg(all(test, ...))]``) scope.

This is a heuristic, not a parser; the doc that consumes the script
spot-checks the buckets against ``cargo expand`` to keep us honest.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

PATTERN = re.compile(
    r"\.unwrap\(\)"
    r"|\.expect\("
    r"|\bpanic!\("
    r"|\bunreachable!\("
    r"|\bunimplemented!\("
    r"|\btodo!\("
)

CFG_TEST_ATTR = re.compile(
    r"^\s*#\[\s*cfg\((?:[^()]|\([^()]*\))*?\btest\b(?:[^()]|\([^()]*\))*\)\s*\]")

ALL_CRATES = [
    "orbit-agent",
    "orbit-cli",
    "orbit-cmd",
    "orbit-common",
    "orbit-core",
    "orbit-web",
    "orbit-engine",
    "orbit-exec",
    "orbit-mcp",
    "orbit-policy",
    "orbit-registry",
    "orbit-search",
    "orbit-store",
    "orbit-tools",
]

EXEC_CRITICAL_CRATES = {"orbit-engine", "orbit-agent", "orbit-tools", "orbit-exec"}


def is_test_only_path(path: Path) -> bool:
    name = path.name
    if name.endswith("_tests.rs"):
        return True
    parts = path.parts
    if "tests" in parts:
        return True
    if "fixtures" in parts:
        return True
    return any("test_support" in p for p in parts)


def is_blast_radius_path(path: Path, crate: str) -> bool:
    if crate not in EXEC_CRITICAL_CRATES:
        return False
    return not is_test_only_path(path)


def strip_string_and_char_literals(line: str) -> str:
    # Roughly remove "..." and '.' so brace counting in code isn't perturbed
    # by literals. Not a full Rust lexer, but adequate for brace tracking
    # in idiomatic source files.
    line = re.sub(r'"(?:\\.|[^"\\])*"', "", line)
    line = re.sub(r"'(?:\\.|[^'\\])*'", "", line)
    line = re.sub(r"//.*$", "", line)
    return line


TOP_LEVEL_CFG_TEST_MOD = re.compile(
    r"^#\[cfg\((?:[^()]|\([^()]*\))*?\btest\b(?:[^()]|\([^()]*\))*\)\]\s*\n"
    r"(?:#\[[^\]]*\]\s*\n)*"
    r"\s*(?:pub\s+)?mod\s+\w+\s*\{",
    re.MULTILINE,
)
INLINE_TEST_FN = re.compile(r"^\s*#\[(?:test|tokio::test)\b", re.MULTILINE)


def classify_lines(text: str) -> list[bool]:
    """Return a list[bool] aligned to file lines: True if line is "test-region".

    Heuristic (good enough for inventory + trend tracking):

    * Find the *last* column-zero `#[cfg(test)] mod <name>` declaration in
      the file. Every line from there to EOF is treated as test-region.
    * Additionally, any line within an `#[test]` / `#[tokio::test]` function
      body up to its closing brace is test-region. Brace counting here is
      scoped to the test fn only, which keeps it robust to format-string
      braces in surrounding code.

    This deliberately does NOT try to parse Rust. The companion doc spot-
    checks the buckets by hand against execution-critical crates.
    """
    n = len(text.splitlines())
    flags = [False] * n

    # 1. Last top-level `#[cfg(test)] mod ...`: everything from that line to EOF.
    # Scan with balanced parentheses: cfg(any(test, feature = "...")) nests.
    last_start: int | None = None
    for m in re.finditer(r"^#\[cfg\(", text, re.MULTILINE):
        depth = 0
        end = None
        for pos in range(m.end() - 1, len(text)):
            if text[pos] == "(":
                depth += 1
            elif text[pos] == ")":
                depth -= 1
                if depth == 0:
                    end = pos
                    break
        if end is None or not re.search(r"\btest\b", text[m.end():end]):
            continue
        rest = text[end + 1:]
        mod = re.match(
            r"\]\s*\n(?:#\[[^\]]*\]\s*\n)*\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{",
            rest)
        if mod:
            last_start = m.start()
    if last_start is not None:
        line_idx = text[:last_start].count("\n")
        for i in range(line_idx, n):
            flags[i] = True

    # 2. Any `#[test]` / `#[tokio::test]` function spans (anywhere in file).
    lines = text.splitlines()
    for m in INLINE_TEST_FN.finditer(text):
        attr_line = text[: m.start()].count("\n")
        # Walk forward to find the function-body opening brace, then track
        # nesting until it closes.
        depth = 0
        started = False
        for i in range(attr_line, n):
            line = lines[i]
            sanitized = strip_string_and_char_literals(line)
            for ch in sanitized:
                if ch == "{":
                    depth += 1
                    started = True
                elif ch == "}":
                    if depth > 0:
                        depth -= 1
            if started:
                flags[i] = True
                if depth == 0:
                    break

    return flags


def audit_crate(crate: str) -> dict:
    src = REPO_ROOT / "crates" / crate / "src"
    total = 0
    in_test = 0
    non_test = 0
    blast = 0
    sites: list[dict] = []
    if not src.is_dir():
        return {
            "total": 0,
            "in_cfg_test": 0,
            "non_test": 0,
            "blast_radius": 0,
            "sites": sites,
        }

    for path in sorted(src.rglob("*.rs")):
        rel = path.relative_to(REPO_ROOT)
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        path_is_test_only = is_test_only_path(rel)
        in_test_lines = None  # lazy
        for lineno, line in enumerate(text.splitlines(), start=1):
            if not PATTERN.search(line):
                continue
            total += 1
            if path_is_test_only:
                site_in_test = True
            else:
                if in_test_lines is None:
                    in_test_lines = classify_lines(text)
                site_in_test = in_test_lines[lineno - 1]
            if site_in_test:
                in_test += 1
            else:
                non_test += 1
                if is_blast_radius_path(rel, crate):
                    blast += 1
                    sites.append(
                        {
                            "file": str(rel),
                            "line": lineno,
                            "snippet": line.strip(),
                        }
                    )
    return {
        "total": total,
        "in_cfg_test": in_test,
        "non_test": non_test,
        "blast_radius": blast,
        "sites": sites,
    }


def main(argv: list[str]) -> int:
    json_mode = "--json" in argv
    sites_mode = "--sites" in argv

    per_crate: dict[str, dict] = {}
    for crate in ALL_CRATES:
        per_crate[crate] = audit_crate(crate)

    totals = {
        "total": sum(c["total"] for c in per_crate.values()),
        "in_cfg_test": sum(c["in_cfg_test"] for c in per_crate.values()),
        "non_test": sum(c["non_test"] for c in per_crate.values()),
        "blast_radius": sum(c["blast_radius"] for c in per_crate.values()),
    }

    if json_mode:
        out = {
            "schemaVersion": 1,
            "totals": totals,
            "per_crate": {
                crate: {k: v for k, v in data.items() if k != "sites"}
                for crate, data in per_crate.items()
            },
        }
        if sites_mode:
            out["blast_radius_sites"] = {
                crate: data["sites"]
                for crate, data in per_crate.items()
                if data["sites"]
            }
        json.dump(out, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    print(
        "Panic-pattern audit "
        "(.unwrap | .expect | panic! | unreachable! | unimplemented! | todo!)\n"
    )
    header = ("crate", "total", "in_cfg_test", "non_test", "blast_radius")
    print(f"{header[0]:<18} {header[1]:>8} {header[2]:>12} {header[3]:>10} {header[4]:>14}")
    print(f"{'-'*18} {'-'*8} {'-'*12} {'-'*10} {'-'*14}")
    for crate in ALL_CRATES:
        d = per_crate[crate]
        print(
            f"{crate:<18} {d['total']:>8} {d['in_cfg_test']:>12} "
            f"{d['non_test']:>10} {d['blast_radius']:>14}"
        )
    print(f"{'-'*18} {'-'*8} {'-'*12} {'-'*10} {'-'*14}")
    print(
        f"{'TOTAL':<18} {totals['total']:>8} {totals['in_cfg_test']:>12} "
        f"{totals['non_test']:>10} {totals['blast_radius']:>14}"
    )
    print(
        "\nblast_radius = non-test matches in "
        "{orbit-engine, orbit-agent, orbit-tools, orbit-exec} src/, "
        "excluding _tests.rs, /tests/, test_support, /fixtures/."
    )
    if sites_mode:
        print("\nBlast-radius sites:")
        for crate in ALL_CRATES:
            sites = per_crate[crate]["sites"]
            if not sites:
                continue
            print(f"\n  [{crate}]")
            for s in sites:
                print(f"    {s['file']}:{s['line']}  {s['snippet']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
