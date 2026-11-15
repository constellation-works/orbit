#!/usr/bin/env python3
"""Reject recognizable immutable libc::openpty arguments reachable on macOS.

This lexical check is not a type checker or macro expander. It does not resolve
aliased calls, function pointers, macro-generated calls, pointer locals or other
unknown expressions; turbofish null calls are also outside its coverage.
"""

import argparse
import importlib.util
from pathlib import Path
import sys

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
spec = importlib.util.spec_from_file_location(
    "check_windows_portability", SCRIPTS / "check-windows-portability.py")
scanner = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = scanner
spec.loader.exec_module(scanner)


def immutable(values):
    # Strip parentheses surrounding the whole expression, but not casts inside it.
    while len(values) >= 2 and values[0] == "(" and values[-1] == ")":
        depth = 0
        for i, value in enumerate(values):
            depth += value == "("
            depth -= value == ")"
            if depth == 0:
                break
        if i != len(values) - 1:
            break
        values = values[1:-1]
    # A final explicit mutable cast determines the argument's type, even if
    # its operand was immutable. Conversely, a final const cast is unsafe.
    depth = 0
    casts = []
    for i, value in enumerate(values):
        if depth == 0 and values[i:i + 3] in (["as", "*", "const"], ["as", "*", "mut"]):
            casts.append(values[i + 2])
        depth += value in ("(", "[", "{")
        depth -= value in (")", "]", "}")
    if casts:
        return casts[-1] == "const"
    if values[:1] == ["&"]:
        return values[1:2] != ["mut"] and values[1:3] != ["raw", "mut"]
    if values[:2] == [":", ":"]:
        values = values[2:]
    for prefix in (["ptr", ":", ":"], ["std", ":", ":", "ptr", ":", ":"],
                   ["core", ":", ":", "ptr", ":", ":"]):
        if values[:len(prefix)] == prefix:
            tail = values[len(prefix):]
            return tail[:2] in (["null", "("], ["addr_of", "!"])
    return False


def findings(source):
    values = source.values
    for index in range(len(values)):
        if not source.live[index] or values[index:index + 5] != ["libc", ":", ":", "openpty", "("]:
            continue
        opening = index + 4
        closing = source.ends[opening]
        start = opening + 1
        arguments = []
        cursor = start
        while cursor < closing:
            value = values[cursor]
            if value in ("(", "[", "{"):
                cursor = source.ends[cursor] + 1
                continue
            if value == ",":
                if cursor > start:
                    arguments.append((start, cursor))
                start = cursor + 1
            cursor += 1
        if start < closing:
            arguments.append((start, closing))
        for start, end in arguments[3:5]:
            if immutable(values[start:end]):
                offset = source.items[start].offset
                last = source.items[end - 1]
                argument = source.text[offset:last.offset + len(last.value)]
                line = source.text.count("\n", 0, offset) + 1
                yield line, argument


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    root = parser.parse_args().root.resolve()
    try:
        sources, reachable = scanner.scan(root, "macos")
        failures = sorted({f"{context.path.relative_to(root)}:{line}: immutable openpty argument: {argument}"
                           for context in reachable
                           for line, argument in findings(sources[context])})
    except (OSError, ValueError) as error:
        print(f"openpty portability: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("openpty portability: no recognizable immutable arguments on macOS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
