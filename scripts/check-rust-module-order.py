#!/usr/bin/env python3
"""Reject new STD-12 R20 violations in written Rust module scopes, even under cfg."""

from collections import Counter
from dataclasses import dataclass
import importlib
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
rust = importlib.import_module("check-windows-portability")

GROUP = {
    "mod": 0, "extern": 0,
    "use": 1,
    "macro_rules": 2, "macro": 2,
    "const": 3, "static": 3,
    "type": 4, "enum": 4, "struct": 4, "union": 4, "trait": 4, "impl": 4,
    "fn": 5, "global_asm": 5,
}
FUNCTIONS = GROUP["fn"]
MACROS = GROUP["macro_rules"]
SEMICOLON_ITEMS = {"use", "const", "static", "type"}
QUALIFIERS = {"async", "unsafe", "safe", "default", "auto"}
CONST_FN_FOLLOWERS = QUALIFIERS | {"extern", "fn"}


@dataclass(frozen=True)
class Item:
    kind: str
    name: str
    line: int
    offset: int

    @property
    def label(self):
        return f"`{self.kind} {self.name}`" if self.name else f"`{self.kind}`"

    @property
    def identity(self):
        return self.kind, self.name


def excludes_non_test(predicate):
    """Recognize positive cfg predicates that can be true only under test."""
    if predicate == ["test"]:
        return True
    if predicate[:2] in (["all", "("], ["any", "("]) and predicate[-1:] == [")"]:
        arguments = list(rust.split_arguments(predicate[2:-1]))
        combine = all if predicate[0] == "any" else any
        return bool(arguments) and combine(excludes_non_test(arg) for arg in arguments)
    return False


def macro_expectation(attribute):
    if attribute[:5] != ["expect", "(", "clippy", ":", ":"] or attribute[-1:] != [")"]:
        return False
    arguments = list(rust.split_arguments(attribute[2:-1]))
    lint = ["clippy", ":", ":", "arbitrary_source_item_ordering"]
    return len(arguments) == 2 and arguments[0] == lint and (
        len(arguments[1]) == 3 and arguments[1][:2] == ["reason", "="]
        and arguments[1][2].startswith('"') and bool(arguments[1][2].strip('"').strip())
    )


def item_keyword(values, ends, start):
    cursor = start
    if values[cursor] == "pub":
        cursor += 1
        if values[cursor:cursor + 1] == ["("]:
            cursor = ends[cursor] + 1
    while cursor < len(values):
        word = values[cursor]
        following = values[cursor + 1:cursor + 2]
        if word in QUALIFIERS or (word == "const" and following
                                  and following[0] in CONST_FN_FOLLOWERS):
            cursor += 1
        elif word == "extern":
            after_abi = cursor + 1
            if following and following[0].startswith('"'):
                after_abi += 1
            if values[after_abi:after_abi + 1] != ["fn"]:
                break
            cursor = after_abi
        else:
            break
    return cursor


def item_end(values, ends, start, end, kind):
    """Return the terminating delimiter, skipping nested token trees and generics."""
    angles = 0
    cursor = start
    while cursor < end:
        word = values[cursor]
        if word == ";":
            return cursor
        if word == "<":
            angles += 1
        elif word == ">" and angles and values[cursor - 1] != "-":
            angles -= 1
        elif cursor in ends:
            if kind is None or kind in {"global_asm", "macro_rules"}:
                return cursor
            if word == "{" and not angles and kind not in SEMICOLON_ITEMS:
                return cursor
            cursor = ends[cursor]
        cursor += 1
    return end


def macro_reexport_anchor(header, macros):
    # A local macro cannot be re-exported before its definition. Admit the
    # exact names (possibly grouped) in a public re-export under the expectation.
    names = [word for word in header if word not in ("{", "}", ",")]
    if names and all(name in macros for name in names):
        return max(macros[name] for name in names)
    return None


def module_findings(source):
    if source.startswith("#!") and not source[2:].lstrip().startswith("["):
        _, _, source = source.partition("\n")
        source = "\n" + source
    tokens = list(rust.tokens(source))
    if not tokens or "@generated" in source[:tokens[0].offset]:
        return
    values = [token.value for token in tokens]
    ends = rust.pairs(tokens)

    def scan(start, end, scope):
        previous = {}
        last_test = None
        macros = {}
        hoisted_macro = None
        expected_macro_order = False
        test_only = False
        cursor = start
        while cursor < end:
            start = cursor
            if values[start] == "#":
                inner = values[start + 1:start + 2] == ["!"]
                opening = start + 2 if inner else start + 1
                if values[opening:opening + 1] == ["["]:
                    close = ends[opening]
                    attribute = values[opening + 1:close]
                    if inner:
                        expected_macro_order |= bool(macro_expectation(attribute))
                    elif attribute[:2] == ["cfg", "("] and attribute[-1:] == [")"]:
                        test_only |= excludes_non_test(attribute[2:-1])
                    cursor = close + 1
                    continue
            if values[start] == ";":
                cursor += 1
                continue

            keyword = item_keyword(values, ends, start)
            kind = values[keyword] if keyword < end and values[keyword] in GROUP else None
            if (values[keyword:keyword + 1] in (["core"], ["std"])
                    and values[keyword + 1:keyword + 8] == [":", ":", "arch", ":", ":", "global_asm", "!"]):
                kind = "global_asm"
            stop = item_end(values, ends, keyword, end, kind)
            cursor = ends.get(stop, stop) + 1
            if kind is not None and stop < end:
                header = values[keyword + 1:stop]
                name = " ".join(header).replace(" : : ", "::") if kind in {"use", "impl", "extern"} else ""
                offset = tokens[keyword].offset
                if kind == "macro_rules":
                    name = header[1] if len(header) > 1 else ""
                    macros[name] = offset
                elif kind == "static":
                    name = next((word for word in header if word != "mut"), "")
                elif kind not in {"use", "impl", "extern", "global_asm"} and header:
                    name = header[0]
                item = Item(kind, name, source.count("\n", 0, offset) + 1, offset)
                if kind == "mod" and test_only:
                    last_test = item
                else:
                    group = GROUP[kind]
                    if expected_macro_order and kind == "mod":
                        # The early macro also sits before ordinary imports,
                        # which still have to follow the child declaration.
                        earlier_macro = previous.get(MACROS)
                        if earlier_macro is not None and earlier_macro.kind == "macro_rules":
                            hoisted_macro = earlier_macro
                    export_anchor = None
                    if expected_macro_order and kind == "use" and values[start] == "pub":
                        export_anchor = macro_reexport_anchor(header, macros)
                    prior = last_test
                    if prior is None:
                        for rank in range(FUNCTIONS, group, -1):
                            other = previous.get(rank)
                            if other is None or other is hoisted_macro:
                                continue
                            if export_anchor is None or export_anchor < other.offset:
                                prior = other
                                break
                    if prior is not None:
                        yield (scope, item.identity, prior.identity), item.line, (
                            f"{item.label} must precede test module {prior.label}" if last_test is not None
                            else f"{item.label} must precede {prior.label}"
                        )
                    if export_anchor is None:
                        previous[group] = item
                if kind == "mod" and values[stop] == "{":
                    yield from scan(stop + 1, ends[stop], (*scope, name))
            test_only = False

    yield from scan(0, len(values), ())


def git(*args):
    return subprocess.check_output(
        ["git", "-C", ROOT, *args], env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
    )


def comparison_base():
    explicit = os.environ.get("CI_TEST_BASE")
    if explicit:
        return git("merge-base", "HEAD", explicit).decode().strip()
    for ref in ("refs/remotes/origin/agent-main", "refs/heads/agent-main"):
        exists = subprocess.run(["git", "-C", ROOT, "show-ref", "--verify", "--quiet", ref],
                                check=False)
        if exists.returncode == 0:
            return git("merge-base", "HEAD", ref).decode().strip()
    raise ValueError("cannot resolve agent-main; set CI_TEST_BASE to the delivery base")


def changed_sources(base):
    paths = ("crates", "desktop")
    diff = git("diff", "--name-status", "--find-renames", "--no-ext-diff", "-z",
               base, "--", *paths)
    fields = iter(os.fsdecode(diff).split("\0")[:-1])
    changed = {}
    for status, old in zip(fields, fields):
        if status.startswith("R"):
            changed[next(fields)] = old
        else:
            changed[old] = None if status == "A" else old
    untracked = git("ls-files", "--others", "--exclude-standard", "-z", "--", *paths)
    for path in os.fsdecode(untracked).split("\0"):
        if path:
            changed.setdefault(path, None)
    return changed.items()


def main():
    failed = False
    try:
        base = comparison_base()
        for path, old in changed_sources(base):
            current = ROOT / path
            if current.suffix != ".rs" or current.is_symlink() or not current.is_file():
                continue
            previous = Counter()
            try:
                if old and old.endswith(".rs"):
                    previous.update(key for key, _, _ in module_findings(
                        git("show", f"{base}:{old}").decode("utf-8-sig")))
                for key, line, message in module_findings(current.read_text(encoding="utf-8-sig")):
                    if previous[key]:
                        previous[key] -= 1
                    else:
                        print(f"{path}:{line}: STD-12 R20: {message}", file=sys.stderr)
                        failed = True
            except ValueError as error:
                raise ValueError(f"{path}: {error}") from error
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Rust module order: {error}", file=sys.stderr)
        return 1
    if not failed:
        print(f"Rust module order: no new findings against {base[:12]}")
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
