#!/usr/bin/env python3
"""Fail closed when CodeQL exclusions hide a production Rust source.

This is a naming guard, not a Rust compiler: only direct cfg(test) and
cfg(feature = "test-support") declarations prove a module is test-only.
Indirect/composite gates must use a non-excluded filename instead.
"""

from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent


def ignore_patterns(root):
    """Read the same top-level block-list layout the local extractor accepts."""
    patterns = []
    seen = False
    active = False
    for line in (root / ".github/codeql/codeql-config.yml").read_text().splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if line.startswith("paths-ignore:"):
            if seen or not re.fullmatch(r"paths-ignore:\s*(?:#.*)?", line):
                raise ValueError("paths-ignore must be one top-level block list")
            seen = active = True
        elif active:
            if not line[0].isspace():
                active = False
                continue
            match = re.fullmatch(r'''\s+-\s+(?:"([^"\\]+)"|'([^']+)'|([^\s#]+))\s*(?:#.*)?''', line)
            if not match:
                raise ValueError(f"unsupported paths-ignore entry: {line.strip()}")
            pattern = next(value for value in match.groups() if value is not None)
            # CodeQL supports * and **; ** must occupy an entire component.
            if any(c in pattern for c in "?[]\\") or any(
                "**" in part and part != "**" for part in pattern.split("/")
            ):
                raise ValueError(f"unsupported paths-ignore pattern: {pattern}")
            patterns.append(pattern)
    if not seen or not patterns:
        raise ValueError("paths-ignore must be a non-empty top-level block list")
    return patterns


def matches(path, pattern):
    components = pattern.split("/")
    expression = ""
    for index, component in enumerate(components):
        last = index == len(components) - 1
        if component == "**":
            expression += ".*" if last else "(?:[^/]+/)*"
        else:
            expression += re.escape(component).replace(r"\*", "[^/]*")
            if not last:
                expression += "/"
    return re.fullmatch(expression, path.as_posix()) is not None


def tokens(source):
    """Keep literals atomic and discard comments, including nested blocks."""
    index = 0
    token = re.compile(r'''(?:br|cr|r)\#*"|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\\n])'|r\#[A-Za-z_]\w*|[A-Za-z_]\w*|\S''', re.DOTALL)
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
        else:
            match = token.match(source, index)
            if match is None:
                raise ValueError("cannot tokenize Rust source")
            value = match[0]
            index = match.end()
            if re.fullmatch(r'(?:br|cr|r)\#*"', value):
                terminator = '"' + "#" * value.count("#")
                end = source.find(terminator, index)
                if end < 0:
                    raise ValueError("unterminated Rust raw string")
                index = end + len(terminator)
                value = "<raw-string>"
            yield value


def declarations(source):
    items = list(tokens(source))
    attributes = []
    index = 0
    while index < len(items):
        if items[index:index + 2] == ["#", "["]:
            start = index + 2
            index = start
            depth = 1
            while index < len(items) and depth:
                if items[index] == "[":
                    depth += 1
                elif items[index] == "]":
                    depth -= 1
                index += 1
            attributes.append(items[start:index - 1])
            continue
        if items[index] == "pub":
            index += 1
            if index < len(items) and items[index] == "(":
                while index < len(items) and items[index] != ")":
                    index += 1
                index += 1
            continue
        if items[index] == "mod" and index + 2 < len(items) and items[index + 2] == ";":
            yield items[index + 1].removeprefix("r#"), attributes
        attributes = []
        index += 1


def test_only(root, path):
    directory = path.parent
    owners = {directory / name for name in ("mod.rs", "lib.rs", "main.rs")}
    if directory.name:
        owners.add(directory.with_suffix(".rs"))
    if path.name == "mod.rs":
        owners.update(directory.parent / name for name in ("mod.rs", "lib.rs", "main.rs"))
    gates = []
    for owner in sorted(owners):
        if owner == path or not (root / owner).is_file():
            continue
        for name, attributes in declarations((root / owner).read_text()):
            explicit = [a[2] for a in attributes if len(a) == 3 and a[:2] == ["path", "="]]
            if explicit:
                targets = [owner.parent / value.strip('"') for value in explicit]
                relevant = path in targets
            else:
                base = owner.parent if owner.name in ("mod.rs", "lib.rs", "main.rs") else owner.with_suffix("")
                relevant = path in (base / f"{name}.rs", base / name / "mod.rs")
            if relevant:
                gates.append(any(a in (
                    ["cfg", "(", "test", ")"],
                    ["cfg", "(", "feature", "=", '"test-support"', ")"],
                ) for a in attributes))
    # An unrecognized owner or any production declaration fails closed.
    return bool(gates) and all(gates)


def check(root):
    patterns = ignore_patterns(root)
    listing = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "*.rs"],
        cwd=root, check=True, capture_output=True,
    )
    errors = []
    for name in sorted(set(listing.stdout.decode().split("\0")) - {""}):
        path = Path(name)
        if not (root / path).is_file() or any(part in ("tests", "examples", "benches") for part in path.parts):
            continue
        excluded = [pattern for pattern in patterns if matches(path, pattern)]
        if excluded and not test_only(root, path):
            errors.extend(f"{path}: production Rust source matches CodeQL paths-ignore pattern {pattern}" for pattern in excluded)
    return errors


def main():
    try:
        errors = check(ROOT)
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        print("CodeQL production paths: no production Rust sources excluded")
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"CodeQL production paths: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
