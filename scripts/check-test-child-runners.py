#!/usr/bin/env python3
"""Reject hand-rolled test-binary re-execs that bypass run_child_test.

Scans Rust test sources (crates/*/tests/** and crates/*/src/**/tests/**).
A function that builds a Command from std::env::current_exe() or
env!("CARGO_BIN_EXE_*") and waits with .output(), .status(), .spawn(),
.wait(), or .wait_with_output() must call orbit_common::test_env::run_child_test
instead. Passing that command to run_child_test or run_child_test_within
has no raw wait, so it is clean.

scripts/test-child-runner-allowlist.json baselines sites that already do
this. Each entry needs a reason. An entry that no longer matches a site
fails, so exceptions are removed as sites migrate and are not left behind.
"""

import json
from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/test-child-runner-allowlist.json"

WAIT_METHODS = {"output", "status", "spawn", "wait", "wait_with_output"}
# Wrappers that still denote the executable path, not a sibling derived from it.
EXE_SUFFIXES = {
    "unwrap", "expect", "unwrap_or", "unwrap_or_else", "unwrap_or_default",
    "ok", "map_err",
    "as_os_str", "as_ref", "as_path", "as_deref",
    "to_path_buf", "to_owned", "to_os_string", "into_os_string",
    "clone", "into",
}
PATH_WRAPPERS = {("Path", "new"), ("PathBuf", "from"), ("OsStr", "new"), ("OsString", "from")}
OPENERS = {"(", "[", "{"}
CLOSERS = {")", "]", "}"}
CLOSER_OF = {"(": ")", "[": "]", "{": "}"}


class Token:
    def __init__(self, kind, text, line):
        self.kind = kind
        # Structural comparisons must never interpret literal contents as Rust tokens.
        self.text = text if kind != "string" else ""
        self.value = text if kind == "string" else None
        self.line = line


def tokenize(source):
    tokens = []
    i = 0
    n = len(source)
    line = 1
    while i < n:
        char = source[i]
        if char == "\n":
            line += 1
            i += 1
            continue
        if char.isspace():
            i += 1
            continue
        if char == "/" and i + 1 < n and source[i + 1] == "/":
            i += 2
            while i < n and source[i] != "\n":
                i += 1
            continue
        if char == "/" and i + 1 < n and source[i + 1] == "*":
            i += 2
            depth = 1
            while i < n and depth:
                if source[i] == "\n":
                    line += 1
                    i += 1
                elif source.startswith("/*", i):
                    depth += 1
                    i += 2
                elif source.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            continue
        raw = raw_string(source, i)
        if raw is not None:
            content, nxt, newlines = raw
            tokens.append(Token("string", content, line))
            line += newlines
            i = nxt
            continue
        if char in "bc" and i + 1 < n and source[i + 1] == "'":
            nxt = char_literal(source, i + 1)
            if nxt is not None:
                line += source.count("\n", i, nxt)
                i = nxt
                continue
        if char == "'":
            nxt = char_literal(source, i)
            if nxt is not None:
                line += source.count("\n", i, nxt)
                i = nxt
                continue
            if i + 1 < n and (source[i + 1].isalpha() or source[i + 1] == "_"):
                i += 2
                while i < n and (source[i].isalnum() or source[i] == "_"):
                    i += 1
                continue
            i += 1
            continue
        if char == '"':
            content, nxt = plain_string(source, i)
            tokens.append(Token("string", content, line))
            line += source.count("\n", i, nxt)
            i = nxt
            continue
        if char.isdigit():
            i = number(source, i)
            continue
        if char.isalpha() or char == "_":
            j = i + 1
            while j < n and (source[j].isalnum() or source[j] == "_"):
                j += 1
            tokens.append(Token("ident", source[i:j], line))
            i = j
            continue
        two = source[i:i + 2]
        if two in ("::", "->", "=>", "==", "!=", "&&", "||", "..", ">=", "<="):
            tokens.append(Token("punct", two, line))
            i += 2
            continue
        if char in "(){}[].,;!<>&=*?|&:#@":
            tokens.append(Token("punct", char, line))
        i += 1
    return tokens


def raw_string(source, i):
    j = i
    if source.startswith(("br", "cr"), j):
        j += 2
    elif source.startswith("r", j):
        j += 1
    else:
        return None
    hashes = 0
    while j < len(source) and source[j] == "#":
        hashes += 1
        j += 1
    if j >= len(source) or source[j] != '"':
        return None
    j += 1
    closer = '"' + ("#" * hashes)
    end = source.find(closer, j)
    if end < 0:
        content = source[j:]
        return content, len(source), content.count("\n")
    content = source[j:end]
    return content, end + len(closer), content.count("\n")


def plain_string(source, i):
    j = i + 1
    chars = []
    while j < len(source):
        char = source[j]
        if char == '"':
            return "".join(chars), j + 1
        if char == "\\" and j + 1 < len(source):
            chars.append(source[j + 1])
            j += 2
            continue
        if char == "\n":
            return "".join(chars), j
        chars.append(char)
        j += 1
    return "".join(chars), j


def char_literal(source, i):
    if i >= len(source) or source[i] != "'":
        return None
    j = i + 1
    if j < len(source) and source[j] == "\\":
        j += 2
        if j <= len(source) and source.startswith("u{", i + 1):
            end = source.find("}", i + 3)
            j = len(source) if end < 0 else end + 1
    elif j < len(source):
        j += 1
    else:
        return None
    if j < len(source) and source[j] == "'":
        return j + 1
    return None


def number(source, i):
    j = i + 1
    while j < len(source) and (source[j].isalnum() or source[j] == "_"):
        j += 1
    if j + 1 < len(source) and source[j] == "." and source[j + 1].isdigit():
        j += 1
        while j < len(source) and (source[j].isalnum() or source[j] == "_"):
            j += 1
    return j


def skip_group(tokens, open_index, end):
    """Return the index after the group that starts at open_index."""
    opener = tokens[open_index].text
    if opener not in CLOSER_OF:
        return None
    depth = 0
    i = open_index
    while i < end:
        text = tokens[i].text
        if text in OPENERS:
            depth += 1
        elif text in CLOSERS:
            depth -= 1
            if depth == 0 and text == CLOSER_OF[opener]:
                return i + 1
        i += 1
    return None


def skip_angle(tokens, open_index, end):
    depth = 0
    i = open_index
    while i < end:
        text = tokens[i].text
        if text == "<":
            depth += 1
        elif text == ">":
            depth -= 1
            if depth == 0:
                return i + 1
        elif text in OPENERS:
            nxt = skip_group(tokens, i, end)
            if nxt is None:
                return None
            i = nxt
            continue
        i += 1
    return None


def is_test_source(relative):
    parts = relative.parts
    return len(parts) >= 3 and parts[0] == "crates" and "tests" in parts[2:]


def command_type_names(tokens):
    """Local names of an imported Command, plus the bare name Command."""
    names = {"Command"}
    index = 0
    total = len(tokens)
    while index < total:
        if tokens[index].text != "use" or (
            index > 0 and tokens[index - 1].text not in (";", "{", "}")
        ):
            index += 1
            continue
        depth = 0
        cursor = index + 1
        while cursor < total:
            text = tokens[cursor].text
            if text == "{":
                depth += 1
            elif text == "}":
                depth -= 1
            elif text == ";" and depth == 0:
                break
            elif (
                tokens[cursor].kind == "ident"
                and text == "Command"
                and cursor + 2 < total
                and tokens[cursor + 1].text == "as"
                and tokens[cursor + 2].kind == "ident"
            ):
                names.add(tokens[cursor + 2].text)
                cursor += 3
                continue
            cursor += 1
        index = cursor + 1
    return names


def parse_signature(tokens, fn_index):
    """Return (name or None, body-open index or None) for the fn at fn_index."""
    total = len(tokens)
    cursor = fn_index + 1
    name = None
    if cursor < total and tokens[cursor].kind == "ident":
        name = tokens[cursor].text
        cursor += 1
    paren = angle = bracket = 0
    while cursor < total:
        text = tokens[cursor].text
        if text == "(":
            paren += 1
        elif text == ")":
            paren -= 1
        elif text == "[":
            bracket += 1
        elif text == "]":
            bracket -= 1
        elif text == "<":
            angle += 1
        elif text == ">":
            angle = max(0, angle - 1)
        elif text == "{" and paren == 0 and angle == 0 and bracket == 0:
            return name, cursor
        elif text == ";" and paren == 0 and angle == 0 and bracket == 0:
            return name, None
        cursor += 1
    return name, None


def match_brace(tokens, open_index):
    return skip_group(tokens, open_index, len(tokens))


def find_functions(tokens):
    """Return functions as (name, fn_line, body_start, body_end, fn_index, close)."""
    found = []
    index = 0
    total = len(tokens)
    while index < total:
        if tokens[index].kind == "ident" and tokens[index].text == "fn":
            name, body_open = parse_signature(tokens, index)
            if body_open is not None:
                body_close = match_brace(tokens, body_open)
                if body_close is not None:
                    if name is None:
                        name = f"fn@{tokens[index].line}"
                    # body_end is the closing brace; body_start is the first body token.
                    found.append((name, tokens[index].line, body_open + 1, body_close - 1, index, body_close - 1))
                    index = body_open + 1
                    continue
        index += 1
    counts = {}
    named = []
    for name, line, start, end, fn_index, close in found:
        counts[name] = counts.get(name, 0) + 1
        named.append((name, counts[name], line, start, end, fn_index, close))
    functions = []
    for name, occurrence, line, start, end, fn_index, close in named:
        if counts[name] > 1:
            name = f"{name}#{occurrence}"
        functions.append({
            "name": name,
            "line": line,
            "start": start,
            "end": end,
            "fn_index": fn_index,
            "close": close,
        })
    return functions


def match_exe_expr(tokens, start, end, bindings):
    """Return the index after an executable-path expression, or None."""
    cursor = match_exe_primary(tokens, start, end, bindings)
    if cursor is None:
        return None
    while cursor < end:
        if tokens[cursor].text == "?":
            cursor += 1
            continue
        if tokens[cursor].text != "." or cursor + 1 >= end or tokens[cursor + 1].kind != "ident":
            break
        method = tokens[cursor + 1].text
        if method not in EXE_SUFFIXES:
            break
        nxt = cursor + 2
        if nxt < end and tokens[nxt].text == "::":
            nxt += 1
            if nxt < end and tokens[nxt].text == "<":
                nxt = skip_angle(tokens, nxt, end)
                if nxt is None:
                    return None
        if nxt >= end or tokens[nxt].text != "(":
            break
        nxt = skip_group(tokens, nxt, end)
        if nxt is None:
            return None
        cursor = nxt
    return cursor


def match_exe_primary(tokens, start, end, bindings):
    if start >= end:
        return None
    text = tokens[start].text
    if text == "&":
        nxt = start + 1
        if nxt < end and tokens[nxt].text == "mut":
            nxt += 1
        return match_exe_expr(tokens, nxt, end, bindings)
    if text == "*":
        return match_exe_expr(tokens, start + 1, end, bindings)
    if text == "(":
        close = skip_group(tokens, start, end)
        if close is None:
            return None
        if match_exe_expr(tokens, start + 1, close - 1, bindings) != close - 1:
            return None
        return close
    env_end = match_env_macro(tokens, start, end)
    if env_end is not None:
        return env_end
    wrapped = match_path_wrap(tokens, start, end, bindings)
    if wrapped is not None:
        return wrapped
    called = match_current_exe(tokens, start, end)
    if called is not None:
        return called
    if (
        tokens[start].kind == "ident"
        and text in bindings
        and (start + 1 >= end or tokens[start + 1].text != "::")
    ):
        return start + 1
    return None


def match_env_macro(tokens, start, end):
    if (
        start + 4 < end
        and tokens[start].text == "env"
        and tokens[start + 1].text == "!"
        and tokens[start + 2].text == "("
        and tokens[start + 3].kind == "string"
        and tokens[start + 3].value.startswith("CARGO_BIN_EXE_")
        and tokens[start + 4].text == ")"
    ):
        return start + 5
    return None


def match_path_wrap(tokens, start, end, bindings):
    cursor = start
    if cursor < end and tokens[cursor].text == "::":
        cursor += 1
    if cursor >= end or tokens[cursor].kind != "ident":
        return None
    parts = []
    while cursor < end and tokens[cursor].kind == "ident":
        parts.append(tokens[cursor].text)
        if (
            cursor + 2 < end
            and tokens[cursor + 1].text == "::"
            and tokens[cursor + 2].kind == "ident"
        ):
            cursor += 2
            continue
        cursor += 1
        break
    if len(parts) < 2 or cursor >= end or tokens[cursor].text != "(":
        return None
    if (parts[-2], parts[-1]) not in PATH_WRAPPERS:
        return None
    close = skip_group(tokens, cursor, end)
    if close is None:
        return None
    if match_exe_expr(tokens, cursor + 1, close - 1, bindings) != close - 1:
        return None
    return close


def match_current_exe(tokens, start, end):
    cursor = start
    if cursor < end and tokens[cursor].text == "::":
        cursor += 1
    if cursor >= end or tokens[cursor].kind != "ident":
        return None
    while cursor < end and tokens[cursor].kind == "ident":
        if (
            tokens[cursor].text == "current_exe"
            and cursor + 1 < end
            and tokens[cursor + 1].text == "("
        ):
            return skip_group(tokens, cursor + 1, end)
        if cursor + 1 < end and tokens[cursor + 1].text == "::":
            cursor += 2
            continue
        return None
    return None


def consumes(matcher_end, end):
    return matcher_end == end


def first_arg_span(tokens, open_index, end):
    """Return (arg_start, arg_end, close) for the call whose '(' is open_index."""
    close = skip_group(tokens, open_index, end)
    if close is None:
        return None
    depth = 0
    arg_end = close - 1
    cursor = open_index + 1
    while cursor < close - 1:
        text = tokens[cursor].text
        if text in OPENERS:
            depth += 1
        elif text in CLOSERS:
            depth -= 1
        elif text == "," and depth == 0:
            arg_end = cursor
            break
        cursor += 1
    return open_index + 1, arg_end, close


def path_idents(tokens, start, end):
    """Return (parts, index after the path) when tokens[start] begins a path."""
    cursor = start
    if cursor < end and tokens[cursor].text == "::":
        cursor += 1
    if cursor >= end or tokens[cursor].kind != "ident":
        return None
    parts = []
    while cursor < end and tokens[cursor].kind == "ident":
        parts.append(tokens[cursor].text)
        if (
            cursor + 2 < end
            and tokens[cursor + 1].text == "::"
            and tokens[cursor + 2].kind == "ident"
        ):
            cursor += 2
            continue
        cursor += 1
        break
    return parts, cursor


def command_new_at(tokens, start, end, command_names, bindings):
    """If a re-exec Command::new starts at start, return the index after new(...)."""
    if start > 0 and tokens[start - 1].text == "::":
        return None
    parsed = path_idents(tokens, start, end)
    if parsed is None:
        return None
    parts, cursor = parsed
    if len(parts) < 2 or parts[-1] != "new" or parts[-2] not in command_names:
        return None
    if cursor >= end or tokens[cursor].text != "(":
        return None
    span = first_arg_span(tokens, cursor, end)
    if span is None:
        return None
    arg_start, arg_end, close = span
    if not consumes(match_exe_expr(tokens, arg_start, arg_end, bindings), arg_end):
        return None
    return close


def method_after(tokens, dot_index, end):
    """Return (method, method_line, index after the call, arg interiors) for `.name`."""
    if dot_index + 1 >= end or tokens[dot_index + 1].kind != "ident":
        return None
    method = tokens[dot_index + 1]
    cursor = dot_index + 2
    if cursor < end and tokens[cursor].text == "::":
        cursor += 1
        if cursor < end and tokens[cursor].text == "<":
            cursor = skip_angle(tokens, cursor, end)
            if cursor is None:
                return None
    interiors = []
    if cursor < end and tokens[cursor].text == "(":
        close = skip_group(tokens, cursor, end)
        if close is None:
            return None
        interiors.append((cursor + 1, close - 1))
        cursor = close
    while cursor < end and tokens[cursor].text == "?":
        cursor += 1
    return method.text, method.line, cursor, interiors


def binding_expr(tokens, expr_start, expr_end, command_names, bindings):
    """Classify a whole initializer as exe, command, or other."""
    if consumes(match_exe_expr(tokens, expr_start, expr_end, bindings), expr_end):
        return "exe"
    built = command_new_at(tokens, expr_start, expr_end, command_names, bindings)
    if built is None:
        return "other"
    cursor = built
    while cursor < expr_end and tokens[cursor].text == ".":
        parsed = method_after(tokens, cursor, expr_end)
        if parsed is None:
            return "other"
        method, _line, cursor, _interiors = parsed
        if method in WAIT_METHODS:
            return "other"
    if cursor == expr_end:
        return "command"
    return "other"


def semicolon(tokens, start, end):
    depth = 0
    cursor = start
    while cursor < end:
        text = tokens[cursor].text
        if text in OPENERS:
            depth += 1
        elif text in CLOSERS:
            depth -= 1
        elif text == ";" and depth == 0:
            return cursor
        cursor += 1
    return end


def simple_binding(tokens, let_index, end):
    """Return (name, expr_start, expr_end, next_index) for `let/const/static [mut] name [: type] = expr`."""
    cursor = let_index + 1
    if cursor < end and tokens[cursor].text == "mut":
        cursor += 1
    if cursor >= end or tokens[cursor].kind != "ident":
        return None
    name = tokens[cursor].text
    cursor += 1
    if cursor < end and tokens[cursor].text == ":":
        depth = 0
        cursor += 1
        while cursor < end:
            text = tokens[cursor].text
            if text in OPENERS or text == "<":
                depth += 1
            elif text in CLOSERS or text == ">":
                depth -= 1
            elif text == "=" and depth <= 0:
                break
            cursor += 1
    if cursor >= end or tokens[cursor].text != "=":
        return None
    expr_start = cursor + 1
    expr_end = semicolon(tokens, expr_start, end)
    next_index = expr_end + 1 if expr_end < end and tokens[expr_end].text == ";" else expr_end
    return name, expr_start, expr_end, next_index


class Scanner:
    def __init__(self, tokens, command_names, base_bindings):
        self.tokens = tokens
        self.command_names = command_names
        self.scopes = [dict(base_bindings)]
        self.findings = []

    def lookup(self, name):
        for scope in reversed(self.scopes):
            if name in scope:
                return scope[name]
        return None

    def visible_exe(self):
        names = set()
        shadowed = set()
        for scope in reversed(self.scopes):
            for name, kind in scope.items():
                if name in shadowed:
                    continue
                shadowed.add(name)
                if kind == "exe":
                    names.add(name)
        return names

    def scan(self, start, end, nested):
        index = start
        while index < end:
            jumped = nested_end(index, nested)
            if jumped is not None:
                index = jumped
                continue
            text = self.tokens[index].text
            if text == "{":
                self.scopes.append({})
                index += 1
                continue
            if text == "}":
                if len(self.scopes) > 1:
                    self.scopes.pop()
                index += 1
                continue
            if text in ("let", "const", "static"):
                bound = simple_binding(self.tokens, index, end)
                if bound is not None:
                    name, expr_start, expr_end, nxt = bound
                    self.scan(expr_start, expr_end, nested)
                    kind = binding_expr(
                        self.tokens, expr_start, expr_end,
                        self.command_names, self.visible_exe(),
                    )
                    self.scopes[-1][name] = kind
                    index = nxt
                    continue
            advanced = self.command_chain(index, end, nested)
            if advanced is not None:
                index = advanced
                continue
            index += 1

    def command_chain(self, start, end, nested):
        tokens = self.tokens
        if start >= end or nested_end(start, nested) is not None:
            return None
        built = command_new_at(tokens, start, end, self.command_names, self.visible_exe())
        from_binding = False
        if built is None:
            if (
                tokens[start].kind == "ident"
                and self.lookup(tokens[start].text) == "command"
                and (start == 0 or tokens[start - 1].text not in (".", "::"))
                and start + 1 < end
                and tokens[start + 1].text == "."
            ):
                built = start + 1
                from_binding = True
            else:
                return None
        cursor = built
        waited = False
        while cursor < end and tokens[cursor].text == ".":
            parsed = method_after(tokens, cursor, end)
            if parsed is None:
                break
            method, line, nxt, interiors = parsed
            for arg_start, arg_end in interiors:
                self.scan(arg_start, arg_end, nested)
            if method in WAIT_METHODS and not waited:
                self.findings.append((line, f".{method}()"))
                waited = True
            cursor = nxt
        return cursor if cursor > start else None


def nested_end(index, nested):
    for fn_index, close in nested:
        if fn_index <= index <= close:
            return close + 1
    return None


def module_bindings(tokens, functions):
    covered = [(fn["fn_index"], fn["close"]) for fn in functions]
    bindings = {}
    scopes = [bindings]
    index = 0
    total = len(tokens)
    while index < total:
        jump = nested_end(index, covered)
        if jump is not None:
            index = jump
            continue
        text = tokens[index].text
        if text == "{":
            scopes.append({})
            index += 1
            continue
        if text == "}":
            if len(scopes) > 1:
                scopes.pop()
            index += 1
            continue
        if text in ("const", "static"):
            bound = simple_binding(tokens, index, total)
            if bound is not None:
                name, expr_start, expr_end, nxt = bound
                visible = {
                    item
                    for scope in scopes
                    for item, kind in scope.items()
                    if kind == "exe"
                }
                kind = "exe" if consumes(match_exe_expr(tokens, expr_start, expr_end, visible), expr_end) else "other"
                scopes[-1][name] = kind
                index = nxt
                continue
        index += 1
    return {name: kind for name, kind in bindings.items() if kind == "exe"}


def function_findings(tokens, functions, command_names, base_bindings):
    findings = []
    for fn in functions:
        nested = [
            (other["fn_index"], other["close"])
            for other in functions
            if other is not fn and fn["start"] <= other["fn_index"] <= fn["end"]
        ]
        scanner = Scanner(tokens, command_names, base_bindings)
        scanner.scan(fn["start"], fn["end"], nested)
        ordinals = {}
        for line, call in scanner.findings:
            key = call
            ordinal = ordinals.get(key, 0)
            ordinals[key] = ordinal + 1
            findings.append({
                "function": fn["name"],
                "line": line,
                "call": call,
                "ordinal": ordinal,
            })
    return findings


def scan_file(path, relative):
    tokens = tokenize(path.read_text(encoding="utf-8"))
    functions = find_functions(tokens)
    command_names = command_type_names(tokens)
    base = module_bindings(tokens, functions)
    return function_findings(tokens, functions, command_names, base)


def load_allowlist():
    entries = json.loads(ALLOWLIST.read_text(encoding="utf-8"))
    if not isinstance(entries, list):
        raise ValueError("allowlist must be a list")
    remaining = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("each exception must be an object")
        key = (entry["path"], entry["function"], entry["call"], entry["ordinal"])
        reason = entry["reason"]
        if not isinstance(reason, str) or not reason.strip() or key in remaining:
            raise ValueError("each exception needs a reason and a unique path/function/call/ordinal")
        if not isinstance(entry["ordinal"], int) or entry["ordinal"] < 0:
            raise ValueError("ordinal must be a non-negative integer")
        if not isinstance(entry["call"], str) or not entry["call"].startswith("."):
            raise ValueError("call must be the waiting method, such as .output()")
        remaining[key] = reason
    return remaining


def main():
    try:
        remaining = load_allowlist()
        errors = []
        for path in sorted((ROOT / "crates").rglob("*.rs")):
            relative = path.relative_to(ROOT)
            if not is_test_source(relative):
                continue
            for finding in scan_file(path, relative):
                key = (relative.as_posix(), finding["function"], finding["call"], finding["ordinal"])
                if key in remaining:
                    remaining.pop(key)
                else:
                    errors.append(
                        f"{relative}:{finding['line']}: raw test-binary re-exec via "
                        f"Command{finding['call']}; use orbit_common::test_env::run_child_test"
                    )
        errors.extend(
            f"{path}: stale test-child-runner exception: {function} {call}#{ordinal}"
            for path, function, call, ordinal in remaining
        )
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        print("test child runners: no unapproved hand-rolled re-execs")
        return 0
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError, KeyError, TypeError) as error:
        print(f"test child runners: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
