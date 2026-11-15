#!/usr/bin/env python3
"""Reject recognizable Unix std APIs in Rust code reachable on Windows.

This is a lexical guard, not macro expansion or type checking. Unknown cfgs
are evaluated both ways so features/test/debug gates cannot hide violations.
Module edges propagate platform gates without filename/path allowlists.

The scanner is shared: check-linux-gated-test-items.py imports it and evaluates
the same cfgs and module edges for a macOS target (see PLATFORMS).
"""

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
TOKEN = re.compile(
    r'''(?:br|cr|r)\#*"|(?:b|c)?"(?:\\.|[^"\\])*"|(?:b)?'(?:\\.|[^'\\\n])'|r\#[A-Za-z_]\w*|[A-Za-z_]\w*|\S''',
    re.DOTALL,
)
# These names are distinctive enough to scan without receiver type inference.
# mode/set_mode/blocks are also ordinary application/tar methods; only inspect
# their qualified trait forms or the permissions() chain below.
METHODS = {
    "from_mode", "pre_exec", "before_exec", "dev", "ino", "nlink", "uid", "gid",
    "rdev", "atime", "atime_nsec", "mtime", "mtime_nsec", "ctime", "ctime_nsec", "blksize",
}
UNIX_TRAITS = {"PermissionsExt", "MetadataExt", "DirBuilderExt", "OpenOptionsExt"}
# First tokens of a header that Source records in `heads` and `blocks`; plain
# statements and expression blocks stay part of their enclosing item.
ITEM_HEADS = {
    "pub", "use", "fn", "const", "static", "mod", "async", "unsafe", "extern", "impl", "trait",
    "struct", "enum", "type", "union", "macro_rules",
}
# Known cfg atoms per target: bare flags, then key = "value" options. Anything
# else (features, test, debug_assertions, ...) is unknown and evaluates both ways.
PLATFORMS = {
    "windows": ({"unix": False, "windows": True},
                {"target_os": "windows", "target_family": "windows"}),
    "macos": ({"unix": True, "windows": False},
              {"target_os": "macos", "target_family": "unix", "target_vendor": "apple"}),
}


@dataclass
class Token:
    value: str
    offset: int
    raw_literal: str | None = None


def tokens(source):
    index = 0
    while index < len(source):
        if source[index].isspace():
            index += 1
        elif source.startswith("//", index):
            end = source.find("\n", index)
            index = len(source) if end < 0 else end + 1
        elif source.startswith("/*", index):
            depth = 1
            index += 2
            while depth and index < len(source):
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            if depth:
                raise ValueError("unterminated Rust comment")
        else:
            match = TOKEN.match(source, index)
            if match is None:
                raise ValueError("cannot tokenize Rust source")
            value = match[0]
            start = index
            index = match.end()
            raw_literal = None
            if re.fullmatch(r'(?:br|cr|r)\#*"', value):
                terminator = '"' + "#" * value.count("#")
                end = source.find(terminator, index)
                if end < 0:
                    raise ValueError("unterminated Rust raw string")
                index = end + len(terminator)
                raw_literal = source[start:index]
                value = '"<raw-string>"'
            yield Token(value, start, raw_literal)


def rust_string_value(literal):
    """Decode an ordinary Rust string body. None when an escape is unsupported."""
    text = literal.replace("\r\n", "\n")
    escapes = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
    value = []
    index = 0
    length = len(text)
    while index < length:
        char = text[index]
        if char != "\\":
            value.append(char)
            index += 1
            continue
        if index + 1 >= length:
            return None
        escaped = text[index + 1]
        if escaped in escapes:
            value.append(escapes[escaped])
            index += 2
            continue
        if escaped == "x" and index + 3 < length:
            digits = text[index + 2:index + 4]
            if re.fullmatch(r"[0-7][0-9a-fA-F]", digits):
                value.append(chr(int(digits, 16)))
                index += 4
                continue
            return None
        if escaped == "u" and index + 2 < length and text[index + 2] == "{":
            end = text.find("}", index + 3)
            if end < 0:
                return None
            digits = text[index + 3:end].replace("_", "")
            if not digits or len(digits) > 6 or re.fullmatch(r"[0-9a-fA-F]+", digits) is None:
                return None
            scalar = int(digits, 16)
            if scalar > 0x10FFFF or 0xD800 <= scalar <= 0xDFFF:
                return None
            value.append(chr(scalar))
            index = end + 1
            continue
        if escaped == "\n":
            index += 2
            while index < length and text[index] in " \t\r\n":
                index += 1
            continue
        return None
    return "".join(value)


def rust_path_literal(token):
    """Filesystem text of a `#[path]` string, or None when it is not one.

    Ordinary quotes are unescaped. Raw strings (`r"..."`, `r#"..."#`, and the
    byte/cstr raw forms) are literal aside from newline normalization. An
    identifier or an undecodable escape is not a path.
    """
    if token.raw_literal is not None:
        match = re.fullmatch(r'(?:br|cr|r)(#*)"(.*)"\1', token.raw_literal, re.DOTALL)
        if match is None:
            return None
        return match.group(2).replace("\r\n", "\n") or None
    value = token.value
    if len(value) >= 2 and value[0] == '"' and value[-1] == '"':
        return rust_string_value(value[1:-1]) or None
    return None


def pairs(items):
    stack = []
    result = {}
    for index, token in enumerate(items):
        if token.value in ("(", "[", "{"):
            stack.append(index)
        elif token.value in (")", "]", "}"):
            if not stack or items[stack[-1]].value != {")": "(", "]": "[", "}": "{"}[token.value]:
                raise ValueError("unbalanced Rust delimiters")
            start = stack.pop()
            result[start] = index
    if stack:
        raise ValueError("unbalanced Rust delimiters")
    return result


def split_arguments(values):
    start = depth = 0
    for index, value in enumerate(values):
        depth += value in ("(", "[", "{")
        depth -= value in (")", "]", "}")
        if value == "," and depth == 0:
            if index > start:
                yield values[start:index]
            start = index + 1
    if start < len(values):
        yield values[start:]


def cfg_values(values, platform="windows"):
    """Possible truth values on the platform's targets, including unknown features."""
    flags, options = PLATFORMS[platform]
    if len(values) >= 3 and values[1] == "(" and values[-1] == ")":
        args = [cfg_values(arg, platform) for arg in split_arguments(values[2:-1])]
        if values[0] == "not" and len(args) == 1:
            return {not value for value in args[0]}
        if values[0] in ("all", "any"):
            outcomes = {values[0] == "all"}
            for arg in args:
                outcomes = {(a and b) if values[0] == "all" else (a or b)
                            for a in outcomes for b in arg}
            return outcomes
    if len(values) == 1 and values[0] in flags:
        return {flags[values[0]]}
    if len(values) == 3 and values[1] == "=" and values[0] in options:
        return {values[2] == f'"{options[values[0]]}"'}
    return {False, True}


def attribute_values(values, platform="windows"):
    if values[:2] == ["cfg", "("] and values[-1:] == [")"]:
        return cfg_values(values[2:-1], platform)
    if values[:2] == ["cfg_attr", "("] and values[-1:] == [")"]:
        args = list(split_arguments(values[2:-1]))
        if len(args) >= 2:
            condition = cfg_values(args[0], platform)
            applied = {True}
            for attr in args[1:]:
                applied = {a and b for a in applied for b in attribute_values(attr, platform)}
            return ({True} if False in condition else set()) | (applied if True in condition else set())
    return {True}


def unix_use(values, prefix=()):
    """Expand grouped use paths just far enough to recognize std::os::unix."""
    for branch in split_arguments(values):
        path = list(prefix)
        index = 0
        while index < len(branch):
            value = branch[index]
            if value == "{":
                if unix_use(branch[index + 1:-1], tuple(path)):
                    return True
                break
            if value == "as":
                break
            if re.fullmatch(r"[A-Za-z_]\w*", value):
                path.append(value)
                if len(path) >= 3 and path[:2] == ["std", "os"] and path[2] in ("unix", "linux", "macos"):
                    return True
            index += 1
    return False


@dataclass
class Head:
    """An item or statement header: tokens [start, end) up to its `{` or `;`."""
    start: int
    end: int
    block: int
    attrs: tuple


@dataclass
class Block:
    """A braced body: tokens [start, end] are its `{` and `}`; parent is -1 at file level."""
    start: int
    end: int
    parent: int
    attrs: tuple
    head: tuple


@dataclass(frozen=True)
class ModuleContext:
    """A source file and the directory rule of this particular loading context."""
    path: Path
    parent_owned: bool


def module_context(path, parent_owned=False):
    """Normalize mod.rs to the same directory rule in every loading context."""
    return ModuleContext(path, parent_owned or path.name == "mod.rs")


@dataclass
class Module:
    """A `mod` declaration at token `index`; `child` is its loading context for `mod name;`."""
    name: str
    index: int
    block: int
    child: ModuleContext | None
    active: bool
    attrs: tuple


class Source:
    def __init__(self, context, platform="windows"):
        path = self.path = context.path
        self.platform = platform
        self.text = path.read_text(encoding="utf-8")
        self.items = list(tokens(self.text))
        self.values = [token.value for token in self.items]
        self.ends = pairs(self.items)
        self.live = [False] * len(self.items)
        self.edges = []
        self.heads = []
        self.blocks = []
        self.modules = []
        directory = path.parent if context.parent_owned else path.with_suffix("")
        self.walk(0, len(self.items), True, directory, path.parent)

    def walk(self, start, end, enabled, directory, path_directory, comma_items=False, block=-1):
        index = start
        active = enabled
        header = start
        path_override = None
        attrs = []
        while index < end:
            value = self.values[index]
            if value == "#" and index + 1 < end and self.values[index + 1] in ("!", "["):
                inner = self.values[index + 1] == "!"
                opening = index + 2 if inner else index + 1
                if self.values[opening] != "[":
                    index += 1
                    continue
                closing = self.ends[opening]
                attr = self.values[opening + 1:closing]
                allowed = True in attribute_values(attr, self.platform)
                if not inner:
                    attrs.append(tuple(attr))
                if inner:
                    enabled = enabled and allowed
                    active = active and allowed
                else:
                    active = active and allowed
                if len(attr) == 3 and attr[:2] == ["path", "="]:
                    path_override = rust_path_literal(self.items[opening + 3])
                index = closing + 1
                header = index
                continue
            self.live[index] = active
            if value in ("(", "["):
                closing = self.ends[index]
                # Preserve the enclosing item's gate through expression groups.
                self.walk(index + 1, closing, active, directory, path_directory,
                          comma_items=True, block=block)
                index = closing + 1
                continue
            if value in ("{", ";") or (value == "," and comma_items):
                head = self.values[header:index]
                module_at = next((i for i, token in enumerate(head[:-1]) if token == "mod"), None)
                module = None if module_at is None else head[module_at + 1]
                is_item = value != "," and head[:1] and head[0] in ITEM_HEADS
                if is_item:
                    self.heads.append(Head(header, index, block, tuple(attrs)))
                child = None
                if module and value == ";":
                    candidates = [path_directory / path_override] if path_override else [
                        directory / f"{module}.rs", directory / module / "mod.rs"]
                    for candidate in candidates:
                        if candidate.is_file():
                            context = module_context(candidate.resolve(), parent_owned=path_override is not None)
                            self.edges.append((context, active))
                            child = child or context
                if module:
                    self.modules.append(Module(module, header + module_at, block, child, active,
                                               tuple(attrs)))
                if value == "{":
                    closing = self.ends[index]
                    # An inline #[path] replaces that module's directory for
                    # its children, relative to the enclosing file. Ordinary
                    # children of a file module still use its stem, and a
                    # nested #[path] stays relative to the directory this
                    # module established.
                    if module and path_override:
                        child_directory = path_directory / path_override
                        child_path_directory = child_directory
                    else:
                        child_directory = directory / module if module else directory
                        child_path_directory = child_directory if module else path_directory
                    inner = block
                    if is_item:
                        self.blocks.append(Block(index, closing, block, tuple(attrs), tuple(head)))
                        inner = len(self.blocks) - 1
                    self.walk(index + 1, closing, active, child_directory, child_path_directory,
                              comma_items=comma_items or "struct" in head or "enum" in head,
                              block=inner)
                    index = closing
                active = enabled
                path_override = None
                attrs = []
                header = index + 1
            index += 1

    def findings(self):
        values = self.values
        for index, value in enumerate(values):
            if not self.live[index]:
                continue
            label = None
            if value == "use":
                end = values.index(";", index)
                if unix_use(values[index + 1:end]):
                    label = "Unix std import"
            elif values[index:index + 7] in (["std", ":", ":", "os", ":", ":", "unix"],
                                             ["std", ":", ":", "os", ":", ":", "linux"],
                                             ["std", ":", ":", "os", ":", ":", "macos"]):
                label = "Unix std path"
            elif value in METHODS and values[index + 1:index + 2] == ["("] and (
                values[index - 1:index] == ["."] or values[max(0, index - 2):index] == [":", ":"]
            ):
                label = f"Unix extension call {value}()"
            elif (value in UNIX_TRAITS and values[index + 1:index + 3] == [":", ":"]
                  and values[index + 3:index + 4] in (["mode"], ["set_mode"], ["blocks"])):
                label = f"Unix extension trait {value}"
            elif values[index:index + 6] == ["permissions", "(", ")", ".", "mode", "("]:
                label = "Unix permissions().mode()"
            if label:
                line = self.text.count("\n", 0, self.items[index].offset) + 1
                yield line, label


def package_manifests(root, directories):
    """Manifests of the packages below each directory (or the directory itself)."""
    for name in directories:
        if (root / name / "Cargo.toml").is_file():
            yield root / name / "Cargo.toml"
        yield from sorted((root / name).glob("*/Cargo.toml"))


def cargo_roots(root, directories=("crates",)):
    """An imported file can also be an independent Cargo test/bin target."""
    result = set()
    for manifest in package_manifests(root, directories):
        config = tomllib.loads(manifest.read_text())
        directory = manifest.parent
        for kind in ("lib", "bin", "test", "example", "bench"):
            entries = config.get(kind, [])
            if isinstance(entries, dict):
                entries = [entries]
            for entry in entries:
                if "path" in entry:
                    result.add((directory / entry["path"]).resolve())
        for kind, folder in (("bin", "src/bin"), ("test", "tests"),
                             ("example", "examples"), ("bench", "benches")):
            auto_key = "autobenches" if kind == "bench" else f"auto{kind}s"
            if config.get("package", {}).get(auto_key, True):
                result.update(path.resolve() for pattern in ("*.rs", "*/main.rs")
                              for path in (directory / folder).glob(pattern))
    return result


def scan(root, platform, directories=("crates",)):
    """Return sources and platform-reachable ModuleContext keys.

    Index declarations even when gated off, so excluded descendants do not
    become orphan roots. Parse each actual directory-ownership context on its
    own; edges from an excluded context must not leak into a reachable one.
    """
    paths = {path.resolve() for name in directories for path in (root / name).rglob("*.rs")
             if "target" not in path.relative_to(root).parts}
    targets = cargo_roots(root, directories) & paths
    targets.update(path for path in paths if path.name in ("lib.rs", "main.rs", "build.rs"))
    sources = {}

    def index_contexts(contexts):
        pending = list(contexts)
        while pending:
            context = pending.pop()
            if context in sources or context.path not in paths:
                continue
            source = sources[context] = Source(context, platform=platform)
            pending.extend(child for child, _ in source.edges)

    roots = {module_context(path, parent_owned=True) for path in targets}
    index_contexts(roots)
    # Check unreferenced files too, without inventing ordinary contexts for
    # files already owned by a #[path] tree. Infer the remaining ordinary
    # roots, then follow their real contexts before considering more orphans.
    orphan_roots = set()
    while remaining := paths - {context.path for context in sources}:
        defaults = {module_context(path): Source(module_context(path), platform=platform)
                    for path in remaining}
        incoming = {child.path for source in defaults.values() for child, _ in source.edges}
        inferred = {context for context in defaults if context.path not in incoming}
        if not inferred:
            break
        orphan_roots.update(inferred)
        index_contexts(inferred)
    incoming = {child.path for source in sources.values() for child, _ in source.edges}
    roots.update(context for context in orphan_roots if context.path not in incoming)
    reachable = set()
    pending = list(roots)
    while pending:
        context = pending.pop()
        if context in reachable or context not in sources:
            continue
        reachable.add(context)
        pending.extend(child for child, enabled in sources[context].edges if enabled)
    return sources, reachable


def check(root):
    sources, reachable = scan(root, "windows")
    failures = set()
    for context in reachable:
        for line, label in sources[context].findings():
            failures.add(f"{context.path.relative_to(root)}:{line}: {label} needs an enclosing cfg that excludes Windows")
    return sorted(failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        failures = check(args.root.resolve())
    except (OSError, ValueError) as error:
        print(f"windows portability: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("windows portability: no ungated Unix std APIs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
