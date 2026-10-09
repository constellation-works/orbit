#!/usr/bin/env python3
"""Reject forking sleep polls in shell strings embedded in Rust test sources."""

import json
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/test-shell-waits-allowlist.json"

# Skip Rust comments and character literals, and decode ordinary/raw strings.
# Join strings in source order: script builders often split a loop across
# push_str/format! calls, so scanning each literal independently misses it.
RUST_TOKEN = re.compile(
    r'//[^\n]*|/\*.*?\*/|\'(?:\\.|[^\'\\\n])\'|'
    r'(?:br|cr|r)(?P<hashes>\#*)"(?P<raw>.*?)"(?P=hashes)|'
    r'(?:b|c)?"(?P<normal>(?:\\.|[^"\\])*)"',
    re.DOTALL,
)
SHELL_TOKEN = re.compile(
    r'\#[^\n]*|\'(?:[^\']*)\'|"(?:\\.|[^"\\])*"|'
    r'\$\(\([^\n]*?\)\)|\\\n|&&|\|\||[;\n|&()]|[^\s;|&()]+',
    re.DOTALL,
)


def strings(source):
    chunks = []
    lines = []
    for match in RUST_TOKEN.finditer(source):
        value = match.group("raw")
        if value is None:
            value = match.group("normal")
            if value is None:
                continue
            value = re.sub(r'\\\n\s*', '', value)
            escapes = {"n": "\n", "r": "\r", "t": "\t", '"': '"', "\\": "\\", "0": "\0"}
            value = re.sub(r'\\([nrt"\\0])', lambda m: escapes[m[1]], value)
        chunks.append(value)
        lines.extend([source.count("\n", 0, match.start()) + 1] * (len(value) + 1))
    return "\n".join(chunks), lines


def sleep_loops(source):
    script, lines = strings(source)
    return scan_shell(script, lines)


def scan_shell(script, lines, enclosing=()):
    stack = []
    start = True
    findings = []
    previous = None
    for match in SHELL_TOKEN.finditer(script):
        token = match[0]
        if token.startswith("#") or token == "\\\n":
            continue
        if previous == "-c" and token.startswith(("'", '"')):
            # A quoted script passed to sh/bash -c is executable shell, too.
            nested = token[1:-1]
            findings.extend(scan_shell(nested, [lines[match.start()]] * len(nested),
                                       enclosing + tuple(stack)))
        previous = token
        if token in (";", "\n", "&&", "||", "|", "&", "(", ")"):
            start = True
            continue
        if start and token in ("while", "until", "for", "select"):
            stack.append(dict(kind=token, offset=match.start(), header=None, sleep=False))
            start = token in ("while", "until")
            continue
        if start and token == "do":
            if stack:
                loop = stack[-1]
                loop["header"] = " ".join(script[loop["offset"]:match.end()].split())
            start = True
            continue
        if start and token == "done":
            if stack:
                loop = stack.pop()
                if loop["kind"] in ("while", "until") and loop["sleep"]:
                    findings.append((lines[loop["offset"]], loop["header"] or loop["kind"]))
            start = False
            continue
        if start and token in ("if", "then", "else", "elif", "!", "exec", "command", "{"):
            start = True
            continue
        command = token[1:-1] if token.startswith(("'", '"')) else token
        if start and command.rsplit("/", 1)[-1] == "sleep":
            for loop in (*enclosing, *stack):
                loop["sleep"] = True
        # Assignment prefixes do not consume a command's position.
        if not (start and re.match(r'^[A-Za-z_][A-Za-z_0-9]*=', token)):
            start = False
    return findings


def main():
    try:
        exceptions = json.loads(ALLOWLIST.read_text())
        remaining = {}
        for entry in exceptions:
            key = (entry["path"], entry["loop"])
            if not entry["reason"].strip() or key in remaining:
                raise ValueError("each exception needs a reason and a unique path/loop")
            remaining[key] = entry["reason"]
        errors = []
        for path in sorted((ROOT / "crates").rglob("*.rs")):
            relative = path.relative_to(ROOT)
            if "tests" not in relative.parts[2:]:
                continue
            for line, header in sleep_loops(path.read_text()):
                key = (relative.as_posix(), header)
                if key in remaining:
                    remaining.pop(key)
                else:
                    errors.append(f"{relative}:{line}: shell sleep poll: {header}")
        errors.extend(f"{path}: stale shell-wait exception: {header}" for path, header in remaining)
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        print("test shell waits: no unapproved forking sleep polls")
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"test shell waits: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
