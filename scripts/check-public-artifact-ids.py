#!/usr/bin/env python3
"""Reject concrete workspace-local IDs embedded in shipped Rust string literals."""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
SOURCE_ROOTS = sorted((path for path in (ROOT / "crates").glob("*/src") if path.is_dir()))
CONCRETE_ID = re.compile(
    r"(?<![A-Za-z0-9])(?:ORB-|ADR-|L-)\d+\b|(?<![A-Za-z0-9])F\d{4}-\d{2}-\d{3}\b"
)


def production_lines(path: Path):
    """Yield source lines outside sibling and inline Rust test modules."""
    in_test_module = False
    brace_depth = 0
    pending_test_module = False

    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if "#[cfg(test)]" in line:
            pending_test_module = True

        if pending_test_module and re.search(r"\bmod\s+\w+\s*\{", line):
            pending_test_module = False
            in_test_module = True
            brace_depth = line.count("{") - line.count("}")
            continue
        if pending_test_module and re.search(r"\bmod\s+\w+\s*;", line):
            pending_test_module = False

        if in_test_module:
            brace_depth += line.count("{") - line.count("}")
            if brace_depth <= 0:
                in_test_module = False
            continue

        yield line_number, line


def main() -> int:
    violations = []
    for source_root in SOURCE_ROOTS:
        for path in source_root.rglob("*.rs"):
            if "tests" in path.parts:
                continue
            for line_number, line in production_lines(path):
                # Internal provenance belongs in ordinary source comments, which
                # are intentionally outside this shipped-string guard.
                code = line.split("//", maxsplit=1)[0]
                if '"' not in code:
                    continue
                if match := CONCRETE_ID.search(code):
                    violations.append(
                        f"{path.relative_to(ROOT)}:{line_number}: concrete artifact ID {match.group(0)!r} in a shipped string literal"
                    )

    if violations:
        print(
            "Public CLI/tool strings must use placeholders, not workspace-local artifact IDs; "
            "keep implementation provenance in ordinary source comments:",
            file=sys.stderr,
        )
        print("\n".join(violations), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
