#!/usr/bin/env python3
"""Reject concrete workspace-local IDs embedded in shipped Rust string literals.

A literal may span lines. An ID on an interior line of a normal or raw string
is the same leak as one on the line that opens the literal. Line comments and
block comments are provenance and are not scanned.
"""

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
        # cfg applies to the next item, not a later module. Keep it across
        # blank/comment/attribute lines only; a test-only use/fn/const consumes it.
        item = strip_line_comment(line).strip()
        item = re.sub(r"#\[[^\]]*\]", "", item).strip()
        if pending_test_module and item:
            pending_test_module = False

        if in_test_module:
            brace_depth += line.count("{") - line.count("}")
            if brace_depth <= 0:
                in_test_module = False
            continue

        yield line_number, line


def strip_line_comment(line: str) -> str:
    """Return the line up to a // comment that is outside a string literal."""
    in_string = False
    index = 0
    while index < len(line):
        char = line[index]
        if in_string:
            if char == "\\":
                index += 2
                continue
            if char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "/" and line[index + 1:index + 2] == "/":
            return line[:index]
        index += 1
    return line


def skip_char_or_lifetime(line: str, index: int) -> int:
    """Advance past a character literal or lifetime that starts at index."""
    nxt = index + 1
    if nxt >= len(line):
        return len(line)
    if line[nxt] == "\\":
        if nxt + 2 < len(line) and line[nxt + 1] == "u" and line[nxt + 2] == "{":
            close = line.find("}", nxt + 3)
            nxt = len(line) if close == -1 else close + 1
        elif nxt + 1 < len(line) and line[nxt + 1] == "x":
            nxt = min(len(line), nxt + 4)
        else:
            nxt = min(len(line), nxt + 2)
        if nxt < len(line) and line[nxt] == "'":
            return nxt + 1
        return index + 1
    if nxt + 1 < len(line) and line[nxt + 1] == "'":
        return nxt + 2
    if line[nxt].isalpha() or line[nxt] == "_":
        nxt += 1
        while nxt < len(line) and (line[nxt].isalnum() or line[nxt] == "_"):
            nxt += 1
        return nxt
    return index + 1


def raw_opener_at(line: str, quote: int):
    """Hash count when the quote opens an r/br/cr string, else None.

    `foo_r"` is an ordinary string: only the exact prefixes `r`, `br` and `cr`
    are raw. `b"` and `c"` are ordinary escaped strings.
    """
    hashes = 0
    index = quote - 1
    while index >= 0 and line[index] == "#":
        hashes += 1
        index -= 1
    end = index + 1
    while index >= 0 and (line[index].isalnum() or line[index] == "_"):
        index -= 1
    if line[index + 1:end] in {"r", "br", "cr"}:
        return hashes
    return None


def unclosed_string_closer(line: str, block_depth: int):
    """Return the string still open at the end of the line, and the block depth.

    A line that ends inside a block comment opens no string. `//` ends the
    line. Raw closers are literal; ordinary strings honor backslash escapes.
    """
    index = 0
    while index < len(line):
        if block_depth > 0:
            if line.startswith("*/", index):
                block_depth -= 1
                index += 2
                continue
            if line.startswith("/*", index):
                block_depth += 1
                index += 2
                continue
            index += 1
            continue
        if line.startswith("//", index):
            break
        if line.startswith("/*", index):
            block_depth += 1
            index += 2
            continue
        if line[index] == "'":
            index = skip_char_or_lifetime(line, index)
            continue
        if line[index] == '"':
            hashes = raw_opener_at(line, index)
            if hashes is not None:
                closer = '"' + ("#" * hashes)
                end = line.find(closer, index + 1)
                if end == -1:
                    return ("raw", closer), block_depth
                index = end + len(closer)
                continue
            end = index + 1
            while end < len(line):
                if line[end] == "\\":
                    end += 2
                    continue
                if line[end] == '"':
                    index = end + 1
                    break
                end += 1
            else:
                return ("normal", '"'), block_depth
            continue
        index += 1
    return None, block_depth


def close_open_string(line: str, kind: str, closer: str):
    """Split a continuation line into content, whether it closed, and the rest.

    A raw closer of `"` and an ordinary closer of `"` are not the same: raw
    text does not treat a backslash as an escape.
    """
    if kind == "raw":
        end = line.find(closer)
        if end == -1:
            return line, False, ""
        return line[:end], True, line[end + len(closer):]
    index = 0
    while index < len(line):
        if line[index] == "\\":
            index += 2
            continue
        if line[index] == '"':
            return line[:index], True, line[index + 1:]
        index += 1
    return line, False, ""


def main() -> int:
    violations = []
    for source_root in SOURCE_ROOTS:
        for path in source_root.rglob("*.rs"):
            relative = path.relative_to(ROOT)
            if "tests" in relative.parts:
                continue
            pending = None
            block_depth = 0
            for line_number, line in production_lines(path):
                remainder = line
                if pending is not None:
                    content, closed, remainder = close_open_string(remainder, pending[0], pending[1])
                    if match := CONCRETE_ID.search(content):
                        violations.append(
                            f"{relative}:{line_number}: concrete artifact ID {match.group(0)!r} "
                            "in a shipped string literal"
                        )
                    if not closed:
                        continue
                    pending = None
                # Internal provenance belongs in ordinary source comments, which
                # are intentionally outside this shipped-string guard. A line
                # that contains a quote keeps the previous same-line check.
                code = strip_line_comment(remainder)
                if '"' in code and (match := CONCRETE_ID.search(code)):
                    violations.append(
                        f"{relative}:{line_number}: concrete artifact ID {match.group(0)!r} "
                        "in a shipped string literal"
                    )
                pending, block_depth = unclosed_string_closer(remainder, block_depth)

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
