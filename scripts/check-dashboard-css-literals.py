#!/usr/bin/env python3
"""Ratchet colour literals and raw font sizes in the dashboard stylesheets.

Every stylesheet under crates/orbit-web/assets/dashboard/ except css/base.css
is scanned, file by file, for:

- colour literals: hex (#rgb, #rgba, #rrggbb, #rrggbbaa), rgb(), rgba(),
  hsl() and hsla();
- raw font sizes: `font-size` values that are not a `var(--...)` token. The
  CSS-wide keywords (inherit, initial, unset, revert, revert-layer) set no
  size and are not counted.

base.css holds the design tokens, so it is exempt. Comments and selectors (an
id such as `#face`) are not counted. The `font` shorthand and a size held in a
custom property are not counted either.

scripts/dashboard-css-literals-baseline.json records each file's counts. A
count above its baseline fails and lists every occurrence in that file, each
with the base.css token of the same value or a pointer to the design doc. A
count below its baseline also fails until the baseline is lowered in the same
change:

  check-dashboard-css-literals.py                    check against the baseline
  check-dashboard-css-literals.py --write-baseline   lower the baseline to the current counts

--write-baseline refuses to raise any count, so the numbers only go down. The
scan reads CSS text only: it renders nothing and reads no JS, HTML or UI copy.
"""

import argparse
import json
import re
import sys
from pathlib import Path

SCHEMA_VERSION = 1
KINDS = ("colour_literals", "raw_font_sizes")
LABELS = {"colour_literals": "colour literals", "raw_font_sizes": "raw font sizes"}
DEFAULT_ROOT = Path(__file__).resolve().parent.parent
DASHBOARD = "crates/orbit-web/assets/dashboard"
BASE_STYLESHEET = f"{DASHBOARD}/css/base.css"
BASELINE = "scripts/dashboard-css-literals-baseline.json"
THEME_DOC = "docs/design/user-interface/specs/theme.md"

COMMENT = re.compile(r"/\*.*?\*/", re.S)
HEX = re.compile(r"#(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})(?![0-9A-Za-z_-])")
# One level of nesting covers `rgb(var(--x) …)`; no stylesheet nests deeper.
FUNCTIONAL_COLOUR = re.compile(r"\b(?:rgba?|hsla?)\((?:[^()]|\([^()]*\))*\)", re.I)
FONT_SIZE = re.compile(r"(?<![\w-])font-size\s*:([^;{}]*)", re.I)
IMPORTANT = re.compile(r"\s*!\s*important\s*$", re.I)
CSS_WIDE = {"inherit", "initial", "unset", "revert", "revert-layer"}
DECLARATION_END = re.compile(r"[;{}]")
CUSTOM_PROPERTY = re.compile(r"--([\w-]+)\s*:\s*([^;{}]+?)\s*;")
RGB_SEPARATORS = re.compile(r"[\s,/]+")


def strip_comments(text):
    """Blank each comment, keeping its newlines so line numbers still hold."""
    return COMMENT.sub(lambda match: re.sub(r"[^\n]", " ", match.group(0)), text)


def is_selector(text, end):
    """A match followed by `{` before any `;` or `}` names a selector, not a value."""
    stop = DECLARATION_END.search(text, end)
    return stop is not None and stop.group() == "{"


def colour_key(literal):
    """Canonical (red, green, blue, alpha) of a hex or rgb()/rgba() literal, else None."""
    text = literal.strip().lower()
    if text.startswith("#"):
        digits = text[1:]
        if len(digits) in (3, 4):
            digits = "".join(digit * 2 for digit in digits)
        if len(digits) not in (6, 8):
            return None
        channels = [int(digits[i:i + 2], 16) for i in range(0, len(digits), 2)]
        alpha = 1.0
        if len(channels) == 4:
            alpha = channels.pop() / 255
        return (*channels, round(alpha, 2))
    match = re.fullmatch(r"rgba?\((.*)\)", text)
    if match is None:
        return None
    try:
        parts = [float(part) for part in RGB_SEPARATORS.split(match.group(1).strip()) if part]
    except ValueError:
        return None
    if len(parts) not in (3, 4):
        return None
    alpha = parts[3] if len(parts) == 4 else 1.0
    return (*(round(part) for part in parts[:3]), round(alpha, 2))


def colour_tokens(root):
    """Map each colour value in base.css to the first custom property holding it."""
    tokens = {}
    text = strip_comments((root / BASE_STYLESHEET).read_text(encoding="utf-8"))
    for name, value in CUSTOM_PROPERTY.findall(text):
        key = colour_key(value)
        if key is not None:
            tokens.setdefault(key, f"--{name}")
    return tokens


def scan_stylesheet(text):
    """Return (line, kind, value) for each colour literal and raw font size, in source order."""
    cleaned = strip_comments(text)
    found = []
    for match in HEX.finditer(cleaned):
        if not is_selector(cleaned, match.end()):
            found.append((match.start(), "colour_literals", match.group(0)))
    for match in FUNCTIONAL_COLOUR.finditer(cleaned):
        found.append((match.start(), "colour_literals", match.group(0)))
    for match in FONT_SIZE.finditer(cleaned):
        value = IMPORTANT.sub("", match.group(1).strip()).strip()
        if value and value.lower() not in CSS_WIDE and not value.startswith("var(--"):
            found.append((match.start(), "raw_font_sizes", value))
    return [(cleaned.count("\n", 0, offset) + 1, kind, value)
            for offset, kind, value in sorted(found)]


def scan(root):
    """Return {path: {kind: count}} and {path: [(line, kind, value)]} for each stylesheet."""
    dashboard = root / DASHBOARD
    if not dashboard.is_dir():
        raise ValueError(f"{DASHBOARD} is missing")
    counts = {}
    occurrences = {}
    for path in sorted(dashboard.rglob("*.css")):
        relative = path.relative_to(root).as_posix()
        if relative == BASE_STYLESHEET:
            continue
        found = scan_stylesheet(path.read_text(encoding="utf-8"))
        occurrences[relative] = found
        counts[relative] = {kind: sum(1 for _, found_kind, _ in found if found_kind == kind)
                            for kind in KINDS}
    return counts, occurrences


def load_baseline(path):
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict) or document.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"{path}: expected schema_version {SCHEMA_VERSION}")
    files = document.get("files")
    if not isinstance(files, dict):
        raise ValueError(f"{path}: expected a files object")
    baseline = {}
    for relative, entry in files.items():
        if not isinstance(entry, dict) or not set(entry) <= set(KINDS):
            raise ValueError(f"{path}: {relative}: expected only {' and '.join(KINDS)} counts")
        counts = {kind: entry.get(kind, 0) for kind in KINDS}
        if any(type(count) is not int or count < 0 for count in counts.values()):
            raise ValueError(f"{path}: {relative}: counts must be non-negative integers")
        baseline[relative] = counts
    return baseline


def differences(counts, baseline):
    """Return (rises, falls) as (path, kind, current, admitted) tuples."""
    rises = []
    falls = []
    for relative in sorted(set(counts) | set(baseline)):
        for kind in KINDS:
            current = counts.get(relative, {}).get(kind, 0)
            admitted = baseline.get(relative, {}).get(kind, 0)
            if current > admitted:
                rises.append((relative, kind, current, admitted))
            elif current < admitted:
                falls.append((relative, kind, current, admitted))
    return rises, falls


def describe(relative, line, kind, value, tokens):
    if kind == "colour_literals":
        token = tokens.get(colour_key(value))
        if token:
            use = f"var({token}), the base.css token with this value"
        else:
            use = f"a colour token from {BASE_STYLESHEET} (var(--...)); see {THEME_DOC}"
        return f"{relative}:{line}: colour literal {value}; use {use}"
    return (f"{relative}:{line}: raw font size {value}; use a var(--...) font-size token; "
            f"see {THEME_DOC}")


def rise_messages(rises, occurrences, tokens):
    messages = []
    for relative, kind, current, admitted in rises:
        messages.append(
            f"dashboard css literals: {relative} has {current} {LABELS[kind]}, above its "
            f"baseline of {admitted}. Every occurrence in the file is listed; replace the new "
            "one with the token named:")
        messages.extend(
            f"  {describe(relative, line, found_kind, value, tokens)}"
            for line, found_kind, value in occurrences[relative] if found_kind == kind)
    return messages


def fall_messages(falls):
    return [
        f"dashboard css literals: {relative} has {current} {LABELS[kind]}, below its baseline "
        f"of {admitted}. Lower the baseline in this change: "
        "scripts/check-dashboard-css-literals.py --write-baseline"
        for relative, kind, current, admitted in falls
    ]


def write_baseline(path, counts):
    document = {
        "schema_version": SCHEMA_VERSION,
        "files": {
            relative: {kind: file_counts[kind] for kind in KINDS}
            for relative, file_counts in sorted(counts.items())
            if any(file_counts.values())
        },
    }
    path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT,
                        help="repository root (default: this checkout)")
    parser.add_argument("--write-baseline", action="store_true",
                        help="lower the baseline to the current counts; refuses any rise")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    baseline_path = root / BASELINE

    try:
        counts, occurrences = scan(root)
        tokens = colour_tokens(root)
    except (OSError, ValueError) as error:
        print(f"dashboard css literals: cannot scan: {error}", file=sys.stderr)
        return 2

    if args.write_baseline:
        try:
            existing = load_baseline(baseline_path) if baseline_path.exists() else None
        except (OSError, ValueError) as error:
            print(f"dashboard css literals: cannot read baseline: {error}", file=sys.stderr)
            return 2
        if existing is not None:
            rises, _ = differences(counts, existing)
            if rises:
                print("dashboard css literals: refusing to raise the baseline; remove the new "
                      "value or use the token named instead:", file=sys.stderr)
                print("\n".join(rise_messages(rises, occurrences, tokens)), file=sys.stderr)
                return 1
        write_baseline(baseline_path, counts)
        print(f"dashboard css literals: wrote {baseline_path.relative_to(root).as_posix()}")
        return 0

    try:
        baseline = load_baseline(baseline_path)
    except (OSError, ValueError) as error:
        print(f"dashboard css literals: cannot read baseline: {error}", file=sys.stderr)
        return 2

    rises, falls = differences(counts, baseline)
    if rises or falls:
        messages = rise_messages(rises, occurrences, tokens) + fall_messages(falls)
        print("\n".join(messages), file=sys.stderr)
        return 1

    colours = sum(file_counts["colour_literals"] for file_counts in counts.values())
    font_sizes = sum(file_counts["raw_font_sizes"] for file_counts in counts.values())
    print(f"dashboard css literals: {colours} colour literals and {font_sizes} raw font sizes "
          f"in {len(counts)} stylesheets match the baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())
