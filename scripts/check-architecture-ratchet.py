#!/usr/bin/env python3
"""Ratchet the old shape of the architecture restructure.

The restructure (docs/design/architecture-restructure/2_design.md §10) moves
about a thousand operations over many weeks while unrelated work lands daily,
so the old shape can regrow. This check holds twelve counts of that shape at
or below scripts/architecture-ratchet-baseline.json, the pattern of
scripts/check-dashboard-css-literals.py:

  check-architecture-ratchet.py                    check against the baseline
  check-architecture-ratchet.py --list            also print every counted location
  check-architecture-ratchet.py --write-baseline   lower the baseline to the current counts

A count above its baseline fails and names the count. A count below its
baseline also fails until the baseline is lowered in the same change, so the
baseline always states the truth. --write-baseline refuses to raise any count.
The script reports only: it changes no source, and no count is gated at zero.

Scope: every non-test Rust source under crates/*/src. A file is test code when
a path component is `tests`, or its name is `tests.rs` or ends in `_tests.rs`.
Inside other files, items gated by `#[cfg(test)]` are removed before counting.
Comments and string and char literals are blanked first, so they neither match
nor hide a name. A function's parameters are read from its signature, so a
multi-line signature counts.
"""

import argparse
import bisect
import json
import re
import sys
from collections import namedtuple
from pathlib import Path

SCHEMA_VERSION = 1
DEFAULT_ROOT = Path(__file__).resolve().parent.parent
BASELINE = "scripts/architecture-ratchet-baseline.json"
CRATES = "crates"
SURFACE_CRATES = ("orbit-cli", "orbit-web", "orbit-cmd")
RUNTIME_SRC = "crates/orbit-runtime/src"
RUNTIME_LAYERS = ("base", "stored", "host", "run")
APPLICATION_PORT = "crates/orbit-runtime/src/application_port.rs"
RUNTIME_HOST_DIR = "crates/orbit-engine/src/context/hosts/runtime_host"
RUNTIME_HOST_MOD = f"{RUNTIME_HOST_DIR}/mod.rs"
DEFINITIONS = {
    "impl_orbit_runtime_files":
        "non-test files with an inherent `impl OrbitRuntime` block",
    "orbit_runtime_naming_files.orbit-cli":
        "orbit-cli files that name `OrbitRuntime` in code",
    "orbit_runtime_naming_files.orbit-web":
        "orbit-web files that name `OrbitRuntime` in code",
    "orbit_runtime_naming_files.orbit-cmd":
        "orbit-cmd files that name `OrbitRuntime` in code",
    "orbit_runtime_ref_param_fns":
        "functions with a `&OrbitRuntime` or `&mut OrbitRuntime` parameter",
    "runtime_host_required_methods":
        "required methods of `RuntimeHost`: declarations with no default body",
    "application_port_methods":
        "methods of the `ApplicationPort` trait",
    "from_kernel_ref_uses":
        "calls of `from_kernel_ref` (its definition is not counted)",
    "env_global_accessor_calls":
        "calls of `composition_env()`, `captured_env()` and the free `process_env()` "
        "that return the captured `Env` from the static `CAPTURED_ENV` "
        "(their definitions are not counted)",
    "env_process_env_method_calls":
        "calls of a `process_env()` method on a runtime or host value (`.process_env()`)",
    "worker_or_owner_reads":
        "calls that read the worker invocation or the coordination owner",
    "runtime_top_level_modules_outside_layers":
        "top-level modules of orbit-runtime/src outside base, stored, host and run",
}

# Mechanics. Rust source is masked first: comments and literals become spaces
# (newlines kept), so every later pattern and bracket match sees code only.
TOKEN = re.compile(r"//|/\*|(?<!\w)[bc]?r(#*)\"|\"|'")
CHAR_LITERAL = re.compile(r"'(?:\\(?:u\{[^}\n]*\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
STRING_CLOSE = re.compile(r"\\[\s\S]|\"")
BLOCK_DELIMITER = re.compile(r"/\*|\*/")
CFG_TEST = re.compile(r"#\[cfg\(test\)\]")
ITEM_DELIMITER = re.compile(r"[;{(\[]")
BRACKET = {"{": re.compile(r"[{}]"), "(": re.compile(r"[()]"), "[": re.compile(r"[\[\]]")}
CLOSER = {"{": "}", "(": ")", "[": "]"}
FN_NAME = re.compile(r"\bfn\s+([A-Za-z_]\w*)")
FN_TOKEN = re.compile(r"->|[<>(;{]")
FN_BEFORE = re.compile(r"\bfn\s+$")
MACRO_CALL = re.compile(r"(\w+)::(\w+)!\s*\(\s*\)\s*;")
FnDecl = namedtuple("FnDecl", "name offset params body")

# Count patterns.
IMPL_ORBIT_RUNTIME = re.compile(r"\bimpl\s*(?:<[^{;]*?>\s*)?(?:\w+\s*::\s*)*OrbitRuntime\s*\{")
ORBIT_RUNTIME_NAME = re.compile(r"\bOrbitRuntime\b")
RUNTIME_PARAM = re.compile(r"&\s*(?:'\w+\s+)?(?:mut\s+)?(?:\w+\s*::\s*)*OrbitRuntime\b")
FROM_KERNEL_REF = re.compile(r"\bfrom_kernel_ref\s*\(")
OWNER_READ = re.compile(
    r"\b(worker_invocation|coordination_write_owner|coordination_task_reads_visible"
    r"|coordination_task_write_refusal)\s*\(")
# The two replacements for direct `std::env` reads (§8): a call that fetches
# the whole captured `Env` at the point of use. The global shape is a free
# call of `composition_env()`, `captured_env()` or `process_env()`; a method
# call through `.` is the other shape and is not counted here. Definitions
# (`fn composition_env`, `fn captured_env`, `fn process_env`) are excluded by
# `preceded_by_fn`, as with `from_kernel_ref`.
ENV_GLOBAL_ACCESSOR = re.compile(r"\b(composition_env|captured_env|process_env)\s*\(")
ENV_METHOD = re.compile(r"\.\s*process_env\s*\(")


def blank(text):
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
            if char is None:  # a lifetime or a label: keep the quote
                pieces.append("'")
                pos = match.end()
                continue
            end = char.end()
        pieces.append(blank(text[match.start():end]))
        pos = end
    pieces.append(text[pos:])
    return "".join(pieces)


def _closing(text, open_index, stop=None):
    """Offset of the bracket closing the one at open_index (stop - 1 when unclosed)."""
    stop = len(text) if stop is None else stop
    opener = text[open_index]
    depth = 0
    for match in BRACKET[opener].finditer(text, open_index, stop):
        depth += 1 if match.group() == opener else -1
        if depth == 0:
            return match.start()
    return stop - 1


def _top_delimiter(text, start, stop):
    """Offset of the first `;` or `{` outside parentheses and brackets, or -1."""
    index = start
    while True:
        match = ITEM_DELIMITER.search(text, index, stop)
        if match is None:
            return -1
        if match.group() in ";{":
            return match.start()
        index = _closing(text, match.start(), stop) + 1


def strip_test_items(masked):
    """Blank every item gated by #[cfg(test)]; only production code is counted."""
    spans = []
    for match in CFG_TEST.finditer(masked):
        delimiter = _top_delimiter(masked, match.end(), len(masked))
        if delimiter == -1:
            end = len(masked)
        elif masked[delimiter] == ";":
            end = delimiter + 1
        else:
            end = _closing(masked, delimiter) + 1
        spans.append((match.start(), end))
    pieces = []
    pos = 0
    for start, end in sorted(spans):
        if end <= pos:
            continue
        start = max(start, pos)
        pieces.append(masked[pos:start])
        pieces.append(blank(masked[start:end]))
        pos = end
    pieces.append(masked[pos:])
    return "".join(pieces)


def fn_decls(masked, start=0, stop=None):
    """Each fn declaration in masked[start:stop], with its parameter list and body delimiter.

    The body delimiter is "{" for a body and ";" for a required declaration.
    """
    stop = len(masked) if stop is None else stop
    for name in FN_NAME.finditer(masked, start, stop):
        index = name.end()
        depth = 0
        params = None
        while True:
            token = FN_TOKEN.search(masked, index, stop)
            if token is None:
                break
            text = token.group()
            if text == "<":
                depth += 1
            elif text == ">":
                depth = max(depth - 1, 0)
            elif depth == 0 and text == "(":
                params = (token.start(), _closing(masked, token.start(), stop))
                break
            elif depth == 0 and text in (";", "{"):
                break
            index = token.end()
        if params is None:
            continue
        delimiter = _top_delimiter(masked, params[1] + 1, stop)
        body = masked[delimiter] if delimiter != -1 else ""
        yield FnDecl(name.group(1), name.start(), masked[params[0]:params[1] + 1], body)


def _trait_body(masked, trait):
    match = re.search(rf"\btrait\s+{re.escape(trait)}\b", masked)
    if match is None:
        raise ValueError(f"trait {trait} is missing")
    open_index = _top_delimiter(masked, match.end(), len(masked))
    if open_index == -1 or masked[open_index] != "{":
        raise ValueError(f"trait {trait} has no body")
    return open_index + 1, _closing(masked, open_index)


def _macro_body(masked, name):
    match = re.search(rf"macro_rules!\s+{re.escape(name)}\s*\{{", masked)
    if match is None:
        raise ValueError(f"macro {name} is missing")
    return match.end(), _closing(masked, match.end() - 1)


def preceded_by_fn(masked, start):
    return FN_BEFORE.search(masked, max(0, start - 16), start) is not None


def preceded_by_dot(masked, start):
    """Whether the call at start is a method call through `.` (spaces allowed)."""
    index = start - 1
    while index >= 0 and masked[index] in " \t\n\r":
        index -= 1
    return index >= 0 and masked[index] == "."


class Source:
    def __init__(self, path, crate, masked):
        self.path = path
        self.crate = crate
        self.masked = masked
        self._newlines = [match.start() for match in re.finditer("\n", masked)]

    def location(self, offset, detail=""):
        line = bisect.bisect_right(self._newlines, offset) + 1
        return f"{self.path}:{line}" + (f" {detail}" if detail else "")


def is_test_name(name):
    return name in ("tests", "tests.rs") or name.endswith("_tests.rs")


def rust_sources(root):
    sources = []
    for path in sorted((root / CRATES).glob("*/src/**/*.rs")):
        relative = path.relative_to(root).as_posix()
        if any(is_test_name(part) for part in relative.split("/")):
            continue
        masked = strip_test_items(mask_source(path.read_text(encoding="utf-8")))
        sources.append(Source(relative, relative.split("/")[1], masked))
    return sources


def _source(by_path, path):
    source = by_path.get(path)
    if source is None:
        raise ValueError(f"{path} is missing")
    return source


def files_matching(sources, pattern):
    entries = []
    for source in sources:
        match = pattern.search(source.masked)
        if match is not None:
            entries.append(source.location(match.start()))
    return entries


def runtime_host_required(by_path):
    host = _source(by_path, RUNTIME_HOST_MOD)
    start, end = _trait_body(host.masked, "RuntimeHost")
    entries = [host.location(decl.offset, decl.name)
               for decl in fn_decls(host.masked, start, end) if decl.body == ";"]
    for call in MACRO_CALL.finditer(host.masked, start, end):
        module = _source(by_path, f"{RUNTIME_HOST_DIR}/{call.group(1)}.rs")
        body_start, body_end = _macro_body(module.masked, call.group(2))
        entries.extend(module.location(decl.offset, decl.name)
                       for decl in fn_decls(module.masked, body_start, body_end)
                       if decl.body == ";")
    return entries


def application_port_methods(by_path):
    port = _source(by_path, APPLICATION_PORT)
    start, end = _trait_body(port.masked, "ApplicationPort")
    return [port.location(decl.offset, decl.name) for decl in fn_decls(port.masked, start, end)]


def runtime_top_level_modules(root):
    entries = []
    for path in sorted((root / RUNTIME_SRC).iterdir()):
        if is_test_name(path.name) or path.stem in RUNTIME_LAYERS:
            continue
        if path.is_dir() or (path.suffix == ".rs" and path.name != "lib.rs"):
            entries.append(f"{RUNTIME_SRC}/{path.name}")
    return entries


def measure(root):
    """Return {count key: [location, ...]}; each count is the length of its list."""
    sources = rust_sources(root)
    by_path = {source.path: source for source in sources}
    counts = {"impl_orbit_runtime_files": files_matching(sources, IMPL_ORBIT_RUNTIME)}
    for crate in SURFACE_CRATES:
        counts[f"orbit_runtime_naming_files.{crate}"] = files_matching(
            [source for source in sources if source.crate == crate], ORBIT_RUNTIME_NAME)
    counts["orbit_runtime_ref_param_fns"] = [
        source.location(decl.offset, decl.name)
        for source in sources for decl in fn_decls(source.masked)
        if RUNTIME_PARAM.search(decl.params)]
    counts["runtime_host_required_methods"] = runtime_host_required(by_path)
    counts["application_port_methods"] = application_port_methods(by_path)
    counts["from_kernel_ref_uses"] = [
        source.location(match.start())
        for source in sources for match in FROM_KERNEL_REF.finditer(source.masked)
        if not preceded_by_fn(source.masked, match.start())]
    counts["env_global_accessor_calls"] = [
        source.location(match.start(), match.group(1))
        for source in sources for match in ENV_GLOBAL_ACCESSOR.finditer(source.masked)
        if not preceded_by_fn(source.masked, match.start())
        and not preceded_by_dot(source.masked, match.start())]
    counts["env_process_env_method_calls"] = [
        source.location(match.start())
        for source in sources for match in ENV_METHOD.finditer(source.masked)]
    counts["worker_or_owner_reads"] = [
        source.location(match.start(), match.group(1))
        for source in sources for match in OWNER_READ.finditer(source.masked)
        if not preceded_by_fn(source.masked, match.start())]
    counts["runtime_top_level_modules_outside_layers"] = runtime_top_level_modules(root)
    return counts


def read_baseline(path):
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict) or document.get("schema_version") != SCHEMA_VERSION:
        raise ValueError(f"{BASELINE}: expected schema_version {SCHEMA_VERSION}")
    counts = document.get("counts")
    if not isinstance(counts, dict):
        raise ValueError(f"{BASELINE}: expected a counts object")
    for key, value in counts.items():
        if type(value) is not int or value < 0:
            raise ValueError(f"{BASELINE}: {key} must be a non-negative integer")
    return counts


def write_baseline(path, counts):
    document = {
        "schema_version": SCHEMA_VERSION,
        "counts": {key: len(locations) for key, locations in sorted(counts.items())},
    }
    path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT,
                        help="repository root (default: this checkout)")
    parser.add_argument("--list", action="store_true",
                        help="print every counted location")
    parser.add_argument("--write-baseline", action="store_true",
                        help="lower the baseline to the current counts; refuses any rise")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    baseline_path = root / BASELINE

    try:
        counts = measure(root)
    except (OSError, ValueError) as error:
        print(f"architecture ratchet: cannot scan: {error}", file=sys.stderr)
        return 2

    if args.write_baseline:
        try:
            existing = read_baseline(baseline_path) if baseline_path.exists() else {}
        except (OSError, ValueError) as error:
            print(f"architecture ratchet: cannot read baseline: {error}", file=sys.stderr)
            return 2
        rises = [(key, len(locations), existing[key]) for key, locations in counts.items()
                 if key in existing and len(locations) > existing[key]]
        if rises:
            print("architecture ratchet: refusing to raise the baseline; remove the new "
                  "occurrences (--list shows each one):", file=sys.stderr)
            print("\n".join(f"architecture ratchet: {key} is {current}, above its baseline "
                            f"of {admitted}" for key, current, admitted in rises),
                  file=sys.stderr)
            return 1
        write_baseline(baseline_path, counts)
        print(f"architecture ratchet: wrote {BASELINE}")
        return 0

    try:
        baseline = read_baseline(baseline_path)
    except (OSError, ValueError) as error:
        print(f"architecture ratchet: cannot read baseline: {error}", file=sys.stderr)
        return 2
    unlisted = sorted(set(counts) - set(baseline))
    stale = sorted(set(baseline) - set(counts))
    if unlisted or stale:
        print(f"architecture ratchet: {BASELINE} does not list the current counts "
              f"(unlisted: {unlisted or 'none'}; stale: {stale or 'none'})", file=sys.stderr)
        return 2

    if args.list:
        for key, locations in counts.items():
            print(f"{key} ({DEFINITIONS[key]}): {len(locations)}")
            print("\n".join(f"  {location}" for location in locations))

    rises = [(key, len(locations), baseline[key]) for key, locations in counts.items()
             if len(locations) > baseline[key]]
    falls = [(key, len(locations), baseline[key]) for key, locations in counts.items()
             if len(locations) < baseline[key]]
    if rises or falls:
        messages = [f"architecture ratchet: {key} is {current}, above its baseline of {admitted}; "
                    "remove or move the new occurrence (--list shows each counted location)"
                    for key, current, admitted in rises]
        messages += [f"architecture ratchet: {key} is {current}, below its baseline of {admitted}; "
                     "lower the baseline in this change: scripts/check-architecture-ratchet.py "
                     "--write-baseline"
                     for key, current, admitted in falls]
        print("\n".join(messages), file=sys.stderr)
        return 1

    print(f"architecture ratchet: {len(counts)} counts match the baseline")
    for key, locations in counts.items():
        print(f"  {key}: {len(locations)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
