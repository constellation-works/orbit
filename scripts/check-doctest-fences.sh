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
#
# Line comments (`///`, `//!`) and block comments (`/** ... */`,
# `/*! ... */`) both count. `/***` and `/**/` stay ordinary comments, as
# they do in rustc, so a banner is not a doc fence.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$repo_root" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
failures = []
scanned = 0


def line_doc_content(line):
    match = re.match(r"^\s*(///|//!)(.*)$", line)
    if not match:
        return None
    rest = match.group(2)
    if rest.startswith(" "):
        rest = rest[1:]
    return rest


def is_runnable_open(info):
    # info is the string after ``` or ~~~, stripped.
    tokens = [token.strip().lower() for token in re.split(r"[,\s]+", info) if token.strip()]
    if any(token == "ignore" for token in tokens):
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


def rust_lines(body):
    """Split like Rust `str::lines`: drop a trailing empty piece from a final newline."""
    if body == "":
        return []
    parts = body.split("\n")
    if body.endswith("\n"):
        parts.pop()
    return parts


def horizontal_prefix(texts):
    """Column of the decoration star, matching rustc's block-doc beautify.

    The prefix is applied only when every sampled line shares one. A fence
    written as ` * ```rust` is then the fence rustdoc runs; a line that is
    not that decoration is left alone.
    """
    if not texts:
        return None
    start = 0
    if not texts[0].lstrip().startswith("*"):
        start = 1
    begin = start
    end = len(texts)
    while begin < end and texts[begin].strip() == "":
        begin += 1
    while end > begin and texts[end - 1].strip() == "":
        end -= 1
    sample = texts[begin:end]
    if not sample:
        return None
    column = None
    for line in sample:
        for index, char in enumerate(line):
            if (column is not None and index > column) or char not in "* \t":
                return None
            if char == "*":
                if column is None:
                    column = index
                elif column != index:
                    return None
                break
        if column is None or column >= len(line):
            return None
    return sample[0][:column]


def beautify_block_lines(body_lines):
    """Drop the block-doc decoration rustc strips before markdown parsing.

    A one-line body is unchanged: rustc beautifies only when the doc string
    contains a newline. An empty first line is "all stars" because Rust's
    `Iterator::all` is true for an empty iterator, so `/**` followed by a
    newline does not hide the fence on the next line.
    """
    if len(body_lines) <= 1:
        return body_lines
    lines = list(body_lines)
    start = 0
    end = len(lines)
    if all(char == "*" for char in lines[0][1]):
        start = 1
    if end > start and lines[end - 1][1] != "" and all(char == "*" for char in lines[end - 1][1]):
        end -= 1
    lines = lines[start:end]
    if not lines:
        return []
    prefix = horizontal_prefix([text for _, text in lines])
    if prefix is None:
        return lines
    cleaned = []
    for lineno, text in lines:
        if text.startswith(prefix):
            text = text[len(prefix):]
            if text == "*" or text.startswith("* ") or text.startswith("**"):
                text = text[1:]
        cleaned.append((lineno, text))
    return cleaned


def ident_continue(char):
    return char == "_" or char.isalnum() or ord(char) > 127


def skip_cooked_string(text, index, line):
    """`index` points at the opening quote. Escapes and newlines stay inside."""
    length = len(text)
    index += 1
    while index < length:
        char = text[index]
        if char == "\\":
            if index + 1 < length and text[index + 1] == "\n":
                line += 1
            index += 2
            continue
        if char == "\n":
            line += 1
            index += 1
            continue
        if char == '"':
            return index + 1, line
        index += 1
    return index, line


def skip_raw_string(text, index, line, hashes):
    """`index` points at the opening quote. The closer is quote plus `hashes`."""
    length = len(text)
    index += 1
    closer = '"' + ("#" * hashes)
    while index < length:
        if text[index] == "\n":
            line += 1
        if text.startswith(closer, index):
            return index + len(closer), line
        index += 1
    return index, line


def skip_char_literal(text, index, line):
    """Consume a char or byte char. A lifetime tick advances one and stops."""
    length = len(text)
    if index + 1 < length and text[index + 1] == "\\":
        cursor = index + 2
        if cursor < length and text[cursor] == "u" and cursor + 1 < length and text[cursor + 1] == "{":
            cursor += 2
            while cursor < length and text[cursor] != "}":
                cursor += 1
            if cursor < length:
                cursor += 1
        elif cursor < length and text[cursor] == "x":
            cursor = min(length, cursor + 3)
        else:
            cursor += 1
        if cursor < length and text[cursor] == "'":
            return cursor + 1, line
        return index + 1, line
    if index + 2 < length and text[index + 2] == "'":
        return index + 3, line
    return index + 1, line


def try_skip_literal(text, index, line):
    """Skip a Rust string or char starting at `index`, or return None."""
    length = len(text)
    if index > 0 and ident_continue(text[index - 1]):
        return None
    for prefix in ("br", "cr", "b", "c", "r"):
        if not text.startswith(prefix, index):
            continue
        cursor = index + len(prefix)
        hashes = 0
        while cursor < length and text[cursor] == "#":
            hashes += 1
            cursor += 1
        if cursor < length and text[cursor] == '"':
            if "r" in prefix:
                return skip_raw_string(text, cursor, line, hashes)
            if hashes == 0:
                return skip_cooked_string(text, cursor, line)
        if hashes == 0 and prefix == "b" and cursor < length and text[cursor] == "'":
            return skip_char_literal(text, cursor, line)
    if text[index] == '"':
        return skip_cooked_string(text, index, line)
    if text[index] == "'":
        return skip_char_literal(text, index, line)
    return None


def parse_block(text, index, line):
    """Parse a block comment at `index`. Doc bodies exclude the delimiters.

    Nesting follows Rust: `*/` inside `/* */` does not end the outer comment.
    `/***` and `/**/` are ordinary comments. `/*!` is always an inner doc,
    including `/*!!`.
    """
    length = len(text)
    kind = None
    content_at = index + 2
    if text.startswith("/*!", index):
        kind = "inner"
        content_at = index + 3
    elif text.startswith("/**", index) and index + 3 < length and text[index + 3] not in "*/":
        kind = "outer"
        content_at = index + 3
    depth = 1
    cursor = index + 2
    cursor_line = line
    while cursor < length and depth:
        if text[cursor] == "\n":
            cursor_line += 1
            cursor += 1
            continue
        if text.startswith("/*", cursor):
            depth += 1
            cursor += 2
            continue
        if text.startswith("*/", cursor):
            depth -= 1
            if depth == 0:
                body = text[content_at:cursor] if kind else ""
                return cursor + 2, cursor_line, kind, body
            cursor += 2
            continue
        cursor += 1
    return length, cursor_line, None, ""


def block_doc_comments(text):
    """Return block doc comments in source order, with beautified body lines."""
    comments = []
    length = len(text)
    index = 0
    line = 1
    while index < length:
        if text[index] == "\n":
            line += 1
            index += 1
            continue
        if text.startswith("//", index):
            index += 2
            while index < length and text[index] != "\n":
                index += 1
            continue
        if text.startswith("/*", index):
            new_index, end_line, kind, body = parse_block(text, index, line)
            if kind:
                parts = rust_lines(body)
                body_lines = [(line + offset, part) for offset, part in enumerate(parts)]
                comments.append({
                    "start": index,
                    "start_line": line,
                    "end_line": end_line,
                    "contents": beautify_block_lines(body_lines),
                })
            if new_index <= index:
                index += 1
            else:
                index = new_index
                line = end_line
            continue
        skipped = try_skip_literal(text, index, line)
        if skipped is not None:
            new_index, line = skipped
            if new_index <= index:
                index += 1
            else:
                index = new_index
            continue
        index += 1
    return comments


def line_start_offsets(lines):
    offsets = []
    position = 0
    for line in lines:
        offsets.append(position)
        position += len(line) + 1
    return offsets


def physical_lines(text):
    if not text:
        return []
    lines = text.split("\n")
    if text.endswith("\n"):
        lines.pop()
    return lines


def doc_lines(text):
    """Doc-comment lines in source order, line docs and block docs together.

    Fence state is one stream per file, as it was when only line comments
    were visible: a fence opened in one comment closes in a later one.
    Lines that belong to a block doc are not also read as `///` lines.
    """
    lines = physical_lines(text)
    offsets = line_start_offsets(lines)
    blocks = block_doc_comments(text)
    occupied = set()
    for block in blocks:
        occupied.update(range(block["start_line"], block["end_line"] + 1))
    events = []
    for block in blocks:
        events.append((block["start"], block["contents"]))
    for lineno, line in enumerate(lines, start=1):
        if lineno in occupied:
            continue
        content = line_doc_content(line)
        if content is None:
            continue
        events.append((offsets[lineno - 1], [(lineno, content)]))
    events.sort(key=lambda event: event[0])
    for _, contents in events:
        for lineno, content in contents:
            yield lineno, content


def consider_fence(path, lineno, content, fence):
    """Return the next fence run. `fence` is the open run, or None."""
    stripped = content.strip()
    match = re.match(r"(`{3,}|~{3,})", stripped)
    if match is None:
        return fence
    run = match.group(1)
    char = run[0]
    info = stripped[len(run):].strip()
    if char == "`" and "`" in info:
        # CommonMark: a backtick opening info string cannot contain a backtick.
        return fence
    if fence is None:
        if is_runnable_open(info):
            relative = path.relative_to(root)
            failures.append(
                f"{relative}:{lineno}: runnable Rust doc fence {run}{info!r} "
                "would be skipped (doctest = false); tag it with ignore or a language"
            )
        return run
    if char != fence[0]:
        return fence
    if info == "" and len(run) >= len(fence):
        return None
    return fence


for path in sorted((root / "crates").glob("*/src/**/*.rs")):
    scanned += 1
    fence = None
    try:
        text = path.read_text(encoding="utf-8").replace("\r\n", "\n").replace("\r", "\n")
    except OSError:
        continue
    for lineno, content in doc_lines(text):
        fence = consider_fence(path, lineno, content, fence)

if failures:
    for failure in failures:
        print(f"check-doctest-fences: {failure}", file=sys.stderr)
    print(
        f"check-doctest-fences: {len(failures)} runnable Rust doc fence(s) in {scanned} files",
        file=sys.stderr,
    )
    sys.exit(1)
print("check-doctest-fences: no runnable Rust doc fences")
PY
