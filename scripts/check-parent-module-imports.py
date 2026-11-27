#!/usr/bin/env python3
"""Reject two `super` imports that Rust 1.89 cannot resolve."""

import os
from pathlib import Path
import sys


SKIP_DIRECTORIES = {".git", ".orbit", ".venv", "node_modules", "target"}
MESSAGE = (
    "Rust 1.89 rejects this parent-module alias with E0432; import the module "
    "by its full path (for example, `use crate::path::to::module as <name>;`)"
)


def _skip_quoted(source, start, quote):
    """Return the first position after an escaped quoted string or character."""
    index = start + 1
    while index < len(source):
        if source[index] == "\\":
            index += 2
        elif source[index] == quote:
            return index + 1
        else:
            index += 1
    return len(source)


def _skip_raw_string(source, start):
    """Return the end of a Rust raw string, or None if this is not one."""
    index = start
    if source.startswith("br", index) or source.startswith("cr", index):
        index += 2
    elif source[index] == "r":
        index += 1
    else:
        return None
    hashes = 0
    while index < len(source) and source[index] == "#":
        hashes += 1
        index += 1
    if index >= len(source) or source[index] != '"':
        return None
    closing = '"' + "#" * hashes
    end = source.find(closing, index + 1)
    return len(source) if end < 0 else end + len(closing)


def rust_tokens(source):
    """Tokenize enough Rust syntax to find use items outside comments/strings."""
    tokens = []
    index = 0
    line = 1
    length = len(source)

    def emit(value, token_line):
        tokens.append((value, token_line))

    while index < length:
        char = source[index]
        if char.isspace():
            if char == "\n":
                line += 1
            index += 1
            continue
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            index = length if end < 0 else end
            continue
        if source.startswith("/*", index):
            index += 2
            depth = 1
            while index < length and depth:
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    if source[index] == "\n":
                        line += 1
                    index += 1
            continue

        raw_end = _skip_raw_string(source, index)
        if raw_end is not None:
            line += source.count("\n", index, raw_end)
            index = raw_end
            continue
        if char == '"':
            end = _skip_quoted(source, index, '"')
            line += source.count("\n", index, end)
            index = end
            continue
        if char == "'":
            # Skip one scalar or one escape only when its quote follows; a
            # lifetime such as 'a does not have that shape.
            end = index + 1
            if end < length and source[end] == "\\":
                if source.startswith("\\u{", end):
                    brace = source.find("}", end + 3)
                    end = length if brace < 0 else brace + 1
                else:
                    end += 2
            else:
                end += 1
            if end < length and source[end] == "'":
                index = end + 1
            else:
                emit(char, line)
                index += 1
            continue

        if char.isalpha() or char == "_" or ord(char) >= 128:
            token_line = line
            end = index + 1
            if char == "r" and end < length and source[end] == "#":
                end += 1
            while end < length and (source[end].isalnum() or source[end] == "_" or ord(source[end]) >= 128):
                end += 1
            emit(source[index:end], token_line)
            index = end
            continue
        if source.startswith("::", index):
            emit("::", line)
            index += 2
            continue
        emit(char, line)
        index += 1
    return tokens


def _group_has_self_alias(tokens, opening):
    """Check top-level use-tree entries inside the brace at opening."""
    entries = []
    entry = []
    depth = 0
    index = opening + 1
    while index < len(tokens):
        token = tokens[index][0]
        if token == "}" and depth == 0:
            entries.append(entry)
            break
        if token in ("{", "(", "["):
            depth += 1
        elif token in ("}", ")", "]"):
            depth -= 1
        if token == "," and depth == 0:
            entries.append(entry)
            entry = []
        else:
            entry.append(token)
        index += 1
    return any(
        len(entry) == 3 and entry[0] == "self" and entry[1] == "as"
        for entry in entries
    )


def violations(source):
    tokens = rust_tokens(source)
    found = []
    for index, (token, line) in enumerate(tokens):
        if token != "use" or index + 2 >= len(tokens):
            continue
        following = [item[0] for item in tokens[index + 1:index + 4]]
        if following[0] == "super" and following[1] == "as" and following[2] != ";":
            if index + 4 < len(tokens) and tokens[index + 4][0] == ";":
                found.append(line)
            continue
        if following[:2] == ["super", "::"] and following[2] == "{":
            if _group_has_self_alias(tokens, index + 3):
                found.append(line)
    return found


def rust_sources(root):
    for current, directories, files in os.walk(root):
        directories[:] = sorted(name for name in directories if name not in SKIP_DIRECTORIES)
        for filename in sorted(files):
            if filename.endswith(".rs"):
                yield Path(current, filename)


def check(root):
    problems = []
    for path in rust_sources(root):
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            problems.append(f"{path}: cannot read Rust source: {error}")
            continue
        for line in violations(source):
            try:
                display = path.relative_to(root)
            except ValueError:
                display = path
            problems.append(f"{display}:{line}: {MESSAGE}")
    return problems


def main(argv):
    root = Path(argv[1]).resolve() if len(argv) == 2 else Path(__file__).resolve().parent.parent
    if len(argv) > 2:
        print("usage: check-parent-module-imports.py [repo_root]", file=sys.stderr)
        return 2
    problems = check(root)
    if problems:
        print("check-parent-module-imports: unsupported parent-module import:", file=sys.stderr)
        for problem in problems:
            print(problem, file=sys.stderr)
        return 1
    print("parent-module import guard passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
