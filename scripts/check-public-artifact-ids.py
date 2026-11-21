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
TEST_CFG = "#[cfg(test)]"
OUTER_ATTRIBUTE = re.compile(r"\s*#\[[^\]]*\]")
INLINE_MODULE = re.compile(r"\s*(?:pub(?:\s*\([^)]*\)\s*|\s+))?mod\s+\w+\s*\{")
BRACE = re.compile(r"[{}]")
LEXICAL_BOUNDARY = re.compile(r"""//|/\*|['"]""")
BLOCK_COMMENT_BOUNDARY = re.compile(r"/\*|\*/")


def production_lines(path: Path):
    """Yield line numbers and literal contents outside inline Rust test modules."""
    brace_depth = 0
    pending_test_module = False
    pending_string = None
    block_depth = 0

    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        # Lex every line, including excluded ones, so module filtering and
        # literal scanning always use the same string and comment boundaries.
        code, contents, pending_string, block_depth = lex_rust_line(
            line, pending_string, block_depth
        )
        start = 0
        while start < len(code):
            if brace_depth:
                for brace in BRACE.finditer(code, start):
                    brace_depth += 1 if brace.group() == "{" else -1
                    if not brace_depth:
                        start = brace.end()
                        break
                else:
                    break
                continue

            item_start = start
            while attribute := OUTER_ATTRIBUTE.match(code, item_start):
                pending_test_module |= attribute.group().strip() == TEST_CFG
                item_start = attribute.end()
            if pending_test_module:
                if module := INLINE_MODULE.match(code, item_start):
                    pending_test_module = False
                    brace_depth = 1
                    start = module.end()
                    continue
                # A non-module item consumes the attribute. Blank, comment,
                # and attribute-only lines leave it pending for the next item.
                if code[item_start:].strip():
                    pending_test_module = False

            # Another item can start on this line. Scan any production literals
            # before its cfg attribute, then consider the next item separately.
            next_cfg = code.find(TEST_CFG, item_start)
            end = len(code) if next_cfg == -1 else next_cfg
            for column, content in contents:
                if start <= column < end:
                    yield line_number, content
            start = end


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


def lex_rust_line(line: str, pending, block_depth: int):
    """Return masked Rust syntax, literal contents with offsets, and lexer state."""
    code = list(line)
    contents = []
    index = 0
    while index < len(line):
        if pending is not None:
            end = string_closer_at(line, index, *pending)
            if end == -1:
                contents.append((index, line[index:]))
                code[index:] = " " * (len(line) - index)
                break
            contents.append((index, line[index:end]))
            after = end + len(pending[1])
            code[index:after] = " " * (after - index)
            index = after
            pending = None
            continue

        if block_depth:
            boundary = BLOCK_COMMENT_BOUNDARY.search(line, index)
            end = boundary.end() if boundary else len(line)
            code[index:end] = " " * (end - index)
            if boundary:
                block_depth += 1 if boundary.group() == "/*" else -1
            index = end
            continue

        boundary = LEXICAL_BOUNDARY.search(line, index)
        if not boundary:
            break
        index = boundary.start()
        token = boundary.group()
        if token == "//":
            code[index:] = " " * (len(line) - index)
            break
        if token == "/*":
            code[index:index + 2] = "  "
            block_depth = 1
            index = boundary.end()
        elif token == "'":
            end = skip_char_or_lifetime(line, index)
            code[index + 1:end] = " " * (end - index - 1)
            index = end
        else:
            # Keep the opening quote as a placeholder for the literal token.
            hashes = raw_opener_at(line, index)
            if hashes is None:
                pending = ("normal", '"')
            else:
                pending = ("raw", '"' + "#" * hashes)
            index = boundary.end()
    return "".join(code), contents, pending, block_depth


def string_closer_at(line: str, index: int, kind: str, closer: str) -> int:
    """Find the closing delimiter on this line, respecting ordinary escapes."""
    if kind == "raw":
        return line.find(closer, index)
    while index < len(line):
        if line[index] == "\\":
            index += 2
            continue
        if line[index] == '"':
            return index
        index += 1
    return -1


def main() -> int:
    violations = []
    for source_root in SOURCE_ROOTS:
        for path in source_root.rglob("*.rs"):
            relative = path.relative_to(ROOT)
            if "tests" in relative.parts:
                continue
            for line_number, content in production_lines(path):
                if not (match := CONCRETE_ID.search(content)):
                    continue
                violations.append(
                    f"{relative}:{line_number}: concrete artifact ID {match.group(0)!r} "
                    "in a shipped string literal"
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
