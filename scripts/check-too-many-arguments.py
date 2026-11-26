#!/usr/bin/env python3
"""Ratchet `allow`/`expect` attributes for `clippy::too_many_arguments` per crate.

STD-02@10 §R51 forbids functions with more than seven parameters (receiver
counted) and forbids `#[allow]` or `#[expect]` of `clippy::too_many_arguments`.
`clippy.toml` sets `too-many-arguments-threshold = 7`, so `-D warnings`
Clippy rejects any new unannotated offender. This guard closes the attribute
gap by holding each crate's outer attribute count at or below
`scripts/too-many-arguments-baseline.json`, failing on any inner attribute
(`#![...]`) or Cargo manifest lint setting that allows the lint, and failing
when a crate's count drops below its baseline until the baseline is lowered:

  check-too-many-arguments.py                    check against the baseline
  check-too-many-arguments.py --list            also print every counted location
  check-too-many-arguments.py --write-baseline  lower the baseline to current counts
"""

import argparse
import json
import os
import re
import stat
import sys
import tempfile
import tomllib
from collections import namedtuple
from pathlib import Path

SCHEMA_VERSION = 1
DEFAULT_ROOT = Path(__file__).resolve().parent.parent
BASELINE = "scripts/too-many-arguments-baseline.json"
DESKTOP_CRATE = "orbit-desktop"
LINT_DIRECTIVES = frozenset(("allow", "expect", "warn", "deny", "forbid"))
ALLOW_DIRECTIVES = frozenset(("allow", "expect"))
MANIFEST_LINT_NAMES = frozenset(("too_many_arguments", "complexity", "all"))

TOKEN = re.compile(r"//|/\*|(?<!\w)[bc]?r(#*)\"|\"|'")
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[^}\n]*\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
STRING_CLOSE = re.compile(r"\\[\s\S]|\"")
BLOCK_DELIMITER = re.compile(r"/\*|\*/")
ATTR_TOKEN = re.compile(r"[A-Za-z_]\w*|::|[()\[\],=]|\S")

AttributeHit = namedtuple("AttributeHit", "path line directive inner")
ManifestViolation = namedtuple("ManifestViolation", "path table lint")


def _blank(text):
    return re.sub(r"[^\n]", " ", text)


def _block_comment_end(text, start):
    depth = 0
    for match in BLOCK_DELIMITER.finditer(text, start):
        depth += 1 if match.group() == "/*" else -1
        if depth == 0:
            return match.end()
    return len(text)


def _string_end(text, start):
    for match in STRING_CLOSE.finditer(text, start):
        if match.group() == '"':
            return match.end()
    return len(text)


def mask_source(text):
    """Blank comments and literals, keeping every offset and newline."""
    pieces = []
    pos = 0
    while True:
        match = TOKEN.search(text, pos)
        if match is None:
            break
        pieces.append(text[pos:match.start()])
        token = match.group(0)
        if token == "//":
            end = text.find("\n", match.end())
            end = len(text) if end == -1 else end
        elif token == "/*":
            end = _block_comment_end(text, match.start())
        elif match.group(1) is not None:
            terminator = '"' + match.group(1)
            close = text.find(terminator, match.end())
            end = len(text) if close == -1 else close + len(terminator)
        elif token == '"':
            end = _string_end(text, match.end())
        else:
            char = CHAR_LITERAL.match(text, match.start())
            if char is None:
                pieces.append("'")
                pos = match.end()
                continue
            end = char.end()
        pieces.append(_blank(text[match.start():end]))
        pos = end
    pieces.append(text[pos:])
    return "".join(pieces)


def _find_closing_bracket(text, open_index):
    depth = 0
    for index in range(open_index, len(text)):
        char = text[index]
        if char == "[":
            depth += 1
        elif char == "]":
            depth -= 1
            if depth == 0:
                return index
    return -1


def _attribute_directives(body):
    """Yield each `allow`/`expect` directive enclosing `clippy::too_many_arguments`."""
    tokens = ATTR_TOKEN.findall(body)
    paren_stack = []
    index = 0
    count = len(tokens)
    while index < count:
        token = tokens[index]
        if token == "(":
            callee = tokens[index - 1] if index > 0 and tokens[index - 1].isidentifier() else None
            paren_stack.append(callee)
            index += 1
            continue
        if token == ")":
            if paren_stack:
                paren_stack.pop()
            index += 1
            continue
        if index + 2 < count and tokens[index:index + 3] == ["clippy", "::", "too_many_arguments"]:
            directive = next(
                (frame for frame in reversed(paren_stack) if frame in LINT_DIRECTIVES),
                None,
            )
            if directive in ALLOW_DIRECTIVES:
                yield directive
            index += 3
            continue
        index += 1


def scan_rust_file(text, relative_path):
    """Return AttributeHit entries for `allow`/`expect` of `clippy::too_many_arguments`."""
    masked = mask_source(text)
    hits = []
    index = 0
    length = len(masked)
    while index < length:
        hash_index = masked.find("#", index)
        if hash_index < 0:
            break
        cursor = hash_index + 1
        while cursor < length and masked[cursor].isspace():
            cursor += 1
        inner = False
        if cursor < length and masked[cursor] == "!":
            inner = True
            cursor += 1
            while cursor < length and masked[cursor].isspace():
                cursor += 1
        if cursor >= length or masked[cursor] != "[":
            index = hash_index + 1
            continue
        close_index = _find_closing_bracket(masked, cursor)
        if close_index < 0:
            break
        line = masked.count("\n", 0, hash_index) + 1
        body = masked[cursor + 1:close_index]
        for directive in _attribute_directives(body):
            hits.append(AttributeHit(relative_path, line, directive, inner))
        index = close_index + 1
    return hits


def _lint_level(spec):
    if isinstance(spec, str):
        return spec
    if isinstance(spec, dict):
        return spec.get("level")
    return None


def scan_manifest(path, relative_path):
    """Return ManifestViolation entries for any `[lints.clippy]` allow of the lint."""
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    violations = []
    tables = (
        ("lints.clippy", data.get("lints", {}).get("clippy", {})),
        ("workspace.lints.clippy", data.get("workspace", {}).get("lints", {}).get("clippy", {})),
    )
    for table_name, table in tables:
        if not isinstance(table, dict):
            continue
        for lint_name, spec in sorted(table.items()):
            normalized = lint_name.replace("-", "_")
            if normalized in MANIFEST_LINT_NAMES and _lint_level(spec) == "allow":
                violations.append(ManifestViolation(relative_path, table_name, lint_name))
    return violations


def _iter_crate_files(base_dir, pattern):
    for path in sorted(base_dir.rglob(pattern)):
        if "target" in path.relative_to(base_dir).parts:
            continue
        if path.is_file():
            yield path


def scan(root):
    """Scan crates/ and desktop/ sources and Cargo manifests."""
    crates_dir = root / "crates"
    desktop_dir = root / "desktop"
    if not crates_dir.is_dir() and not desktop_dir.is_dir():
        raise ValueError(f"{root}: expected crates/ or desktop/ directory")

    counts = {}
    locations = {}
    inner_violations = []
    manifest_violations = []

    crate_roots = []
    if crates_dir.is_dir():
        for entry in sorted(crates_dir.iterdir()):
            if entry.is_dir():
                crate_roots.append((entry.name, entry))
    if desktop_dir.is_dir():
        crate_roots.append((DESKTOP_CRATE, desktop_dir))

    root_manifest = root / "Cargo.toml"
    if root_manifest.is_file():
        manifest_violations.extend(scan_manifest(root_manifest, "Cargo.toml"))

    for crate_name, crate_dir in crate_roots:
        crate_hits = []
        for rs_path in _iter_crate_files(crate_dir, "*.rs"):
            relative = rs_path.relative_to(root).as_posix()
            for hit in scan_rust_file(rs_path.read_text(encoding="utf-8"), relative):
                if hit.inner:
                    inner_violations.append(hit)
                else:
                    crate_hits.append(hit)
        counts[crate_name] = len(crate_hits)
        locations[crate_name] = crate_hits
        for manifest in _iter_crate_files(crate_dir, "Cargo.toml"):
            relative = manifest.relative_to(root).as_posix()
            manifest_violations.extend(scan_manifest(manifest, relative))

    return counts, locations, inner_violations, manifest_violations


def load_baseline(path):
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict) or document.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"{path}: expected schema_version {SCHEMA_VERSION}")
    crates = document.get("crates")
    if not isinstance(crates, dict):
        raise ValueError(f"{path}: expected a crates object")
    for crate, value in crates.items():
        if not isinstance(crate, str) or not crate or type(value) is not int or value < 0:
            raise ValueError(f"{path}: {crate!r}: count must be a non-negative integer")
    return crates


def write_baseline_atomic(path, counts):
    destination = path
    if destination.is_symlink():
        destination = destination.resolve(strict=True)
    if destination.exists() and not destination.is_file():
        raise OSError(f"{destination} is not a regular file")
    destination.parent.mkdir(parents=True, exist_ok=True)
    document = {
        "schema_version": SCHEMA_VERSION,
        "crates": {crate: count for crate, count in sorted(counts.items()) if count > 0},
    }
    payload = json.dumps(document, indent=2, sort_keys=True) + "\n"
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            delete=False,
        ) as stream:
            temporary = Path(stream.name)
            if destination.is_file():
                os.fchmod(stream.fileno(), stat.S_IMODE(destination.stat().st_mode))
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, destination)
        directory_fd = os.open(destination.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if temporary is not None and temporary.exists():
            try:
                temporary.unlink()
            except OSError:
                pass


def differences(counts, baseline):
    rises = []
    falls = []
    for crate in sorted(set(counts) | set(baseline)):
        current = counts.get(crate, 0)
        admitted = baseline.get(crate, 0)
        if current > admitted:
            rises.append((crate, current, admitted))
        elif current < admitted:
            falls.append((crate, current, admitted))
    return rises, falls


def format_policy_violations(inner_violations, manifest_violations):
    messages = []
    for hit in inner_violations:
        messages.append(
            f"too-many-arguments ratchet: {hit.path}:{hit.line}: forbidden inner "
            f"#![{hit.directive}(clippy::too_many_arguments)] attribute (STD-02 §R51)"
        )
    for violation in manifest_violations:
        messages.append(
            f"too-many-arguments ratchet: {violation.path}: [{violation.table}] "
            f"{violation.lint} = \"allow\" is forbidden (STD-02 §R51)"
        )
    return messages


def format_rise_messages(rises, locations):
    messages = []
    for crate, current, admitted in rises:
        messages.append(
            f"too-many-arguments ratchet: {crate} has {current} allow/expect attribute(s) "
            f"for clippy::too_many_arguments, above its baseline of {admitted}; refactor "
            "functions to take at most 7 parameters instead of adding an attribute:"
        )
        for hit in locations.get(crate, ()):
            messages.append(f"  {hit.path}:{hit.line} #[{hit.directive}(clippy::too_many_arguments)]")
    return messages


def format_fall_messages(falls):
    return [
        f"too-many-arguments ratchet: {crate} has {current} allow/expect attribute(s) "
        f"for clippy::too_many_arguments, below its baseline of {admitted}; lower the "
        "baseline in this change: scripts/check-too-many-arguments.py --write-baseline"
        for crate, current, admitted in falls
    ]


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT, help="repository root (default: this checkout)")
    parser.add_argument("--baseline", type=Path, default=None, help=f"baseline JSON path (default: <root>/{BASELINE})")
    parser.add_argument("--list", action="store_true", help="print every counted attribute location")
    parser.add_argument(
        "--write-baseline",
        action="store_true",
        help="lower the baseline to the current counts; refuses any rise or forbidden allow",
    )
    args = parser.parse_args(argv)
    root = args.root.resolve()
    baseline_path = args.baseline.resolve() if args.baseline else root / BASELINE

    try:
        counts, locations, inner_violations, manifest_violations = scan(root)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"too-many-arguments ratchet: cannot scan: {error}", file=sys.stderr)
        return 2

    policy_messages = format_policy_violations(inner_violations, manifest_violations)

    if args.write_baseline:
        if policy_messages:
            print("\n".join(policy_messages), file=sys.stderr)
            return 1
        try:
            existing = load_baseline(baseline_path) if baseline_path.exists() else None
        except (OSError, ValueError) as error:
            print(f"too-many-arguments ratchet: cannot read baseline: {error}", file=sys.stderr)
            return 2
        if existing is not None:
            rises, _ = differences(counts, existing)
            if rises:
                print(
                    "too-many-arguments ratchet: refusing to raise the baseline; remove the new attributes:",
                    file=sys.stderr,
                )
                print("\n".join(format_rise_messages(rises, locations)), file=sys.stderr)
                return 1
        try:
            write_baseline_atomic(baseline_path, counts)
        except OSError as error:
            print(f"too-many-arguments ratchet: cannot write baseline {baseline_path}: {error}", file=sys.stderr)
            return 2
        print(f"too-many-arguments ratchet: wrote {baseline_path}")
        return 0

    try:
        baseline = load_baseline(baseline_path)
    except (OSError, ValueError) as error:
        print(f"too-many-arguments ratchet: cannot read baseline: {error}", file=sys.stderr)
        return 2

    if args.list:
        for crate in sorted(counts):
            if counts[crate] == 0:
                continue
            print(f"{crate}: {counts[crate]}")
            for hit in locations[crate]:
                print(f"  {hit.path}:{hit.line} {hit.directive}")

    rises, falls = differences(counts, baseline)
    failures = policy_messages + format_rise_messages(rises, locations) + format_fall_messages(falls)
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1

    total = sum(counts.values())
    active = sum(1 for count in counts.values() if count > 0)
    print(f"too-many-arguments ratchet: {total} attributes across {active} crates match the baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())
