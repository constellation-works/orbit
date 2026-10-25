#!/usr/bin/env python3
"""Reject test items left ungated while every user is compiled out on macOS.

A test refactor can leave an import, helper or fixture module ungated while
each remaining user sits under `#[cfg(target_os = "linux")]`. Linux compiles it
fine, but native macOS Clippy under `-D warnings` fails with `unused import` or
`never used`, and that check does not run before landing. This guard evaluates
the cfgs of a macOS target, with the module edges and cfg evaluation of
check-windows-portability.py, and fails when an item in a Rust test file or
`#[cfg(test)]` module is compiled on macOS while every use site found is not.
Each failure names the file, line and item.

Checked, all private (`pub` items are never reported):
- a named `use` import whose binding is lowercase or UPPER_SNAKE (modules,
  functions, consts and statics): every use of the name in the importing block
  and the modules declared inside it is compiled out on macOS;
- a glob `use` import whose block and nested modules keep no code that is
  compiled on macOS, yet still contain code that is not;
- a test helper `fn`, `const` or `static` with the same use-site rule;
- a fixture `mod` (`mod name;` or inline) with the same use-site rule.
An item with no use site at all is not reported: Linux builds warn about it too.

Known limits: the guard is lexical, so it does not resolve names or expand
macros. It covers imports, helpers and fixture modules, not dead code reached
through macros. Use sites are identifiers matching the item name (including
inline format arguments and derive/attribute arguments), so an equal name used
for something else only hides a finding. UpperCamel imports are skipped
because a trait import is also used implicitly through method calls. Items
under `#[allow(dead_code)]`, `#[allow(unused_imports)]` or `#[allow(unused)]`
(or `expect`) are skipped, and so are items that carry an attribute other than
cfg/doc/lint/inline ones (`#[test]`, `#[ctor]`, ...). Unknown cfgs (features,
`test`, `debug_assertions`) keep code live, as in the Windows guard.
"""

import argparse
from bisect import bisect_left
from dataclasses import dataclass
import importlib.util
from pathlib import Path
import re
import sys

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
DIRECTORIES = ("crates", "desktop")
PLATFORM = "macos"


def _load_portability():
    spec = importlib.util.spec_from_file_location(
        "check_windows_portability", SCRIPTS / "check-windows-portability.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


portability = _load_portability()

IDENT = re.compile(r"[A-Za-z_]\w*")
UPPER_SNAKE = re.compile(r"[A-Z][A-Z0-9_]*")
# Inline format arguments such as "{name}" or "{name:?}" use the name `name`.
FORMAT_CAPTURE = re.compile(r"(?<!\{)\{([A-Za-z_]\w*)(?::[^{}]*)?\}")
TEST_CFG = ("cfg", "(", "test", ")")
# Attributes that never name an item: gating, lint and documentation settings.
INERT_ATTRS = {"cfg", "cfg_attr", "doc", "path", "allow", "expect", "warn", "deny", "forbid"}
# Attributes that leave an item an ordinary helper (no #[test], #[ctor], ...).
PLAIN_ATTRS = INERT_ATTRS | {"inline", "must_use", "track_caller", "deprecated"}
# Attribute names that make a function a test entry point.
TEST_ATTRS = {"test", "rstest", "test_case", "bench", "ctor"}
IMPORT_LINTS = {"unused_imports", "unused"}
ITEM_LINTS = {"dead_code", "unused"}
SCOPE_KINDS = {"file", "mod", "fn"}


def parse_head(head):
    """Return (public, keyword, name, keyword offset) of an item header."""
    index = 0
    public = head[:1] == ["pub"]
    if public:
        index = 1
        if head[1:2] == ["("] and ")" in head:
            index = head.index(")", 1) + 1
    while index < len(head):
        value = head[index]
        if value in ("async", "unsafe", "default"):
            index += 1
        elif value == "extern" and head[index + 1:index + 2] and head[index + 1].startswith('"'):
            index += 2
        elif value == "const" and head[index + 1:index + 2] in (["fn"], ["async"], ["unsafe"], ["extern"]):
            index += 1
        else:
            break
    keyword = head[index] if index < len(head) else None
    name = head[index + 1] if index + 1 < len(head) else None
    if keyword == "static" and name == "mut":
        name = head[index + 2] if index + 2 < len(head) else None
    if keyword == "macro_rules":
        name = head[index + 2] if index + 2 < len(head) else None
    return public, keyword, name, index


def use_leaves(values, ends, start, end, prefix=()):
    """Yield (name, index) for each binding of the use tree in values[start:end].

    A glob yields ("*", index) with the path before it in the third position.
    """
    index = start
    while index < end:
        path = list(prefix)
        leaf = None
        while index < end and values[index] != ",":
            value = values[index]
            if value == "{":
                closing = ends[index]
                yield from use_leaves(values, ends, index + 1, closing, tuple(path))
                leaf = None
                index = closing + 1
                continue
            if value == "*":
                leaf = ("*", index, tuple(path))
            elif value == "as" and index + 1 < end:
                leaf = (values[index + 1], index + 1, ())
                index += 2
                continue
            elif IDENT.fullmatch(value):
                if value == "self" and path:
                    leaf = (path[-1], index, ())
                else:
                    path.append(value)
                    leaf = (value, index, ())
            index += 1
        if leaf:
            yield leaf
        index += 1


def lint_names(attrs):
    """Lint names allowed by `#[allow(..)]`/`#[expect(..)]` (also under cfg_attr)."""
    found = set()
    for attr in attrs:
        if attr[:1] in (("allow",), ("expect",)) or (
                attr[:1] == ("cfg_attr",) and ("allow" in attr or "expect" in attr)):
            found.update(value for value in attr if IDENT.fullmatch(value))
    return found


@dataclass
class Candidate:
    unit: object
    kind: str  # import, glob, helper or module
    name: str  # the identifier that use sites are matched against
    label: str
    index: int
    region: tuple
    exclude: tuple


class Unit:
    """One parsed Rust file plus the lazy indexes this guard needs."""

    def __init__(self, root, path, source, reachable):
        self.path = path
        self.relative = path.relative_to(root)
        self.source = source
        self.values = source.values
        self.live = source.live
        self.reachable = reachable
        parts = self.relative.parts
        self.test_file = "tests" in parts[:-1] or path.stem == "tests" or self.inner_cfg_test()
        self.parents = []
        self.includes = []  # (included file, token index) for each include!("file.rs")
        self.included = False
        self.test_ranges = [(block.start, block.end) for block in source.blocks
                            if TEST_CFG in block.attrs]
        self.lint_cache = {}
        self.sites = None

    def inner_attrs(self, start):
        values, ends = self.values, self.source.ends
        attrs = []
        index = start + 1
        while values[index:index + 3] == ["#", "!", "["]:
            closing = ends[index + 2]
            attrs.append(tuple(values[index + 3:closing]))
            index = closing + 1
        return attrs

    def inner_cfg_test(self):
        return TEST_CFG in self.inner_attrs(-1)

    def is_test(self, index):
        return self.test_file or any(start <= index <= end for start, end in self.test_ranges)

    def lints(self, block):
        """Lint names allowed where `block` (-1: the file) is compiled, from every enclosing scope."""
        if block in self.lint_cache:
            return self.lint_cache[block]
        self.lint_cache[block] = set()
        if block < 0:
            found = lint_names(self.inner_attrs(-1))
            for parent, module in self.parents:
                found |= lint_names(module.attrs) | parent.lints(module.block)
        else:
            owner = self.source.blocks[block]
            found = lint_names(owner.attrs + tuple(self.inner_attrs(owner.start))) | self.lints(owner.parent)
        self.lint_cache[block] = found
        return found

    def scope_kind(self, block):
        if block < 0:
            return "file"
        keyword = parse_head(list(self.source.blocks[block].head))[1]
        return {"mod": "mod", "fn": "fn"}.get(keyword, "other")

    def scope_range(self, block):
        if block < 0:
            return 0, len(self.values)
        owner = self.source.blocks[block]
        return owner.start, owner.end + 1

    def candidates(self):
        values, ends = self.values, self.source.ends
        for head in self.source.heads:
            if not self.live[head.start]:
                continue
            tokens = values[head.start:head.end]
            public, keyword, name, offset = parse_head(tokens)
            if public or keyword not in ("use", "fn", "const", "static", "mod"):
                continue
            if not (self.is_test(head.start) or TEST_CFG in head.attrs):
                continue
            if self.scope_kind(head.block) not in SCOPE_KINDS:
                continue
            region = self.scope_range(head.block)
            lints = self.lints(head.block) | lint_names(head.attrs)
            if keyword == "use":
                if lints & IMPORT_LINTS:
                    continue
                try:
                    stop = values.index(";", head.start)
                except ValueError:
                    continue
                for leaf, index, path in use_leaves(values, ends, head.start + offset + 1, stop):
                    if leaf == "*":
                        path = "".join(values[head.start + offset + 1:index + 1])
                        yield Candidate(self, "glob", leaf, f"glob import `{path}`", index, region,
                                        (head.start, stop + 1))
                    elif leaf[0].islower() or UPPER_SNAKE.fullmatch(leaf):
                        yield Candidate(self, "import", leaf, f"import `{leaf}`", index, region,
                                        (head.start, stop + 1))
                continue
            if lints & ITEM_LINTS or not name or name == "_" or IDENT.fullmatch(name) is None:
                continue
            if keyword != "mod" and (name == "main" or name.startswith("_")):
                continue
            if any(attr[:1] and attr[0] not in PLAIN_ATTRS for attr in head.attrs):
                continue
            stop = ends[head.end] + 1 if values[head.end] == "{" else head.end + 1
            if keyword == "mod":
                kind, label = "module", f"fixture module `{name}`"
            else:
                kind, label = "helper", f"test helper {keyword} `{name}`"
            yield Candidate(self, kind, name, label, head.start + offset + 1, region,
                            (head.start, stop))

    def index(self, names):
        """Index identifier tokens that name a candidate, with attribute liveness."""
        if self.sites is not None:
            return
        values, ends, live = self.values, self.source.ends, self.live
        size = len(values)
        self.ulive = list(live)
        self.entries = []  # (token index, compiled on macOS) of each test entry attribute
        self.inert = [False] * size
        self.in_use = [False] * size
        index = 0
        spans = []
        while index < size:
            if values[index] == "#" and index + 1 < size and values[index + 1] in ("!", "["):
                opening = index + 2 if values[index + 1] == "!" else index + 1
                if opening < size and values[opening] == "[":
                    spans.append((index, ends[opening], values[index + 1] == "!"))
                    index = ends[opening] + 1
                    continue
            index += 1
        position = 0
        while position < len(spans):
            last = position
            while last + 1 < len(spans) and spans[last + 1][0] == spans[last][1] + 1:
                last += 1
            following = spans[last][1] + 1
            flag = live[following] if following < size else False
            for opening, closing, inner in spans[position:last + 1]:
                name = values[opening + (3 if inner else 2)] if opening + 3 < size else ""
                path = []
                for value in values[opening + (3 if inner else 2):closing]:
                    if value in ("(", "="):
                        break
                    path.append(value)
                if path and path[-1] in TEST_ATTRS and not inner:
                    self.entries.append((opening, flag))
                for k in range(opening, closing + 1):
                    if inner or name in INERT_ATTRS:
                        self.inert[k] = True
                    else:
                        self.ulive[k] = flag
            position = last + 1
        for head in self.source.heads:
            _, keyword, _, offset = parse_head(values[head.start:head.end])
            if keyword == "use":
                try:
                    stop = values.index(";", head.start)
                except ValueError:
                    continue
                for k in range(head.start + offset, stop + 1):
                    self.in_use[k] = True
        sites = {}
        for k, value in enumerate(values):
            if self.inert[k]:
                continue
            raw_literal = self.source.items[k].raw_literal
            capture_text = raw_literal or value
            if raw_literal is not None or (value.endswith('"') and len(value) >= 2):
                for name in FORMAT_CAPTURE.findall(capture_text):
                    if name in names:
                        sites.setdefault(name, []).append(k)
            elif value in names and self.reference(k):
                sites.setdefault(value, []).append(k)
        self.sites = sites

    def reference(self, k):
        """Whether the identifier token at k can refer to an item brought into scope by name."""
        values = self.values
        # The original binding in an alias import also references the item.
        # The token's `ulive` value keeps a cfg on the use item in force.
        if self.in_use[k] and k + 1 < len(values) and values[k + 1] == "as":
            return True
        if k >= 1 and values[k - 1] in (".", "'", "$"):
            return False
        if k >= 2 and values[k - 1] == ":" and values[k - 2] == ":":
            # Only a path rooted at crate/self/super names a scope item by itself.
            return k >= 3 and values[k - 3] in ("crate", "self", "super")
        return not self.in_use[k] or values[k - 1] == "use"

    def code_counts(self, start, end, dead):
        """Count identifier tokens of ordinary code in [start, end) that are live and not."""
        live_code = dead_code = 0
        for k in range(start, end):
            if self.inert[k] or self.in_use[k] or IDENT.fullmatch(self.values[k]) is None:
                continue
            if self.ulive[k] and not dead:
                live_code += 1
            else:
                dead_code += 1
        return live_code, dead_code

    def entry_counts(self, start, end, dead):
        """Count test entry attributes in [start, end) that are compiled on macOS and not."""
        live_entries = dead_entries = 0
        for k, flag in self.entries:
            if start <= k < end:
                if flag and not dead:
                    live_entries += 1
                else:
                    dead_entries += 1
        return live_entries, dead_entries

    def site_counts(self, name, start, end, dead, exclude):
        """Count use sites of `name` in [start, end) that are compiled on macOS and not."""
        live_sites = dead_sites = 0
        positions = self.sites.get(name, ())
        for k in positions[bisect_left(positions, start):bisect_left(positions, end)]:
            if exclude and exclude[0] <= k < exclude[1]:
                continue
            if self.ulive[k] and not dead:
                live_sites += 1
            else:
                dead_sites += 1
        return live_sites, dead_sites


class Workspace:
    """Every parsed file, with the cross-file queries a candidate needs."""

    def __init__(self, root):
        sources, reachable = portability.scan(root, PLATFORM, DIRECTORIES)
        self.units = {path: Unit(root, path, source, path in reachable)
                      for path, source in sources.items()}
        for unit in self.units.values():
            for module in unit.source.modules:
                if module.child in self.units:
                    self.units[module.child].parents.append((unit, module))
        self.link_includes()
        self.propagate_tests()
        # Text included into another file shares that file's scope, which a
        # per-file region cannot see, so its own items are left alone.
        self.candidates = [candidate for unit in self.units.values()
                           if unit.reachable and not unit.included
                           for candidate in unit.candidates()]
        self.names = {candidate.name for candidate in self.candidates if candidate.kind != "glob"}

    def link_includes(self):
        for unit in self.units.values():
            values, index = unit.values, -1
            while True:
                try:
                    index = values.index("include", index + 1)
                except ValueError:
                    break
                if values[index + 1:index + 3] == ["!", "("] and index + 3 < len(values):
                    target = (unit.path.parent / values[index + 3].strip('"')).resolve()
                    if target in self.units:
                        unit.includes.append((target, index))
                        self.units[target].included = True

    def propagate_tests(self):
        """A module declared in test code, or under #[cfg(test)], is test code."""
        changed = True
        while changed:
            changed = False
            for unit in self.units.values():
                children = [(module.child, module.index, TEST_CFG in module.attrs)
                            for module in unit.source.modules]
                children += [(target, index, False) for target, index in unit.includes]
                for target, index, gated in children:
                    child = self.units.get(target)
                    if child is not None and not child.test_file and (gated or unit.is_test(index)):
                        child.test_file = changed = True

    def walk(self, unit, start, end, dead, seen, count):
        """Sum `count(unit, start, end, dead)` over a region and the files declared or included in it."""
        unit.index(self.names)
        total = count(unit, start, end, dead)
        children = [(module.child, module.index) for module in unit.source.modules]
        for target, index in children + unit.includes:
            child = self.units.get(target)
            if child is None or not start <= index < end:
                continue
            child_dead = dead or not unit.live[index]
            if (child.path, child_dead) in seen:
                continue
            seen.add((child.path, child_dead))
            found = self.walk(child, 0, len(child.values), child_dead, seen, count)
            total = (total[0] + found[0], total[1] + found[1])
        return total

    def evaluate(self, candidate):
        """Return (live, dead): macOS-compiled and compiled-out uses of the candidate."""
        unit, (start, end) = candidate.unit, candidate.region
        seen = set()
        if candidate.kind == "module":
            module = next(module for module in unit.source.modules if module.index == candidate.index - 1)
            if module.child in self.units:
                body = self.units[module.child]
                body_region = (0, len(body.values))
            else:
                body = unit
                body_region = candidate.exclude
            # A module holding tests is used by the test harness, not by name,
            # and one with no code compiled on macOS (`#![cfg(..)]`) is gated.
            if (self.walk(body, *body_region, False, set(), Unit.entry_counts)[0]
                    or not self.walk(body, *body_region, False, set(), Unit.code_counts)[0]):
                return 1, 0
            # The module's own file is its definition, not a use.
            seen.update({(module.child, False), (module.child, True)})
        if candidate.kind == "glob":
            return self.walk(unit, start, end, False, seen, Unit.code_counts)

        def sites(unit, start, end, dead, exclude=candidate.exclude):
            return unit.site_counts(candidate.name, start, end, dead,
                                    exclude if unit is candidate.unit else None)
        return self.walk(unit, start, end, False, seen, sites)


def check(root):
    workspace = Workspace(root)
    failures = []
    for candidate in workspace.candidates:
        live, dead = workspace.evaluate(candidate)
        if live == 0 and dead > 0:
            unit = candidate.unit
            line = unit.source.text.count("\n", 0, unit.source.items[candidate.index].offset) + 1
            reason = ("no code in its scope is compiled on macOS" if candidate.kind == "glob"
                      else "every use is compiled out on macOS")
            failures.append(
                f"{unit.relative}:{line}: {candidate.label} is not gated, but {reason} "
                '(for example under #[cfg(target_os = "linux")]); gate it the same way')
    return sorted(set(failures))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        failures = check(args.root.resolve())
    except (OSError, ValueError) as error:
        print(f"linux-gated test items: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("linux-gated test items: no ungated item used only by Linux-gated code")
    return 0


if __name__ == "__main__":
    sys.exit(main())
