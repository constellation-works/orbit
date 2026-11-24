#!/usr/bin/env python3
"""Fail when a code identifier removed in the diff is still named in live docs.

From the delivery diff against the guardrails base (`CI_TEST_BASE`, with the
merge-base fallback `scripts/ci-guardrails.sh` uses), take the identifiers whose
definition the diff deletes:

- Rust `const`, `static`, `fn`, `struct`, `enum`, `trait` and `type` names;
- Rust `pub use` re-exports, named by their crate-qualified public path
  (`orbit_policy::resolve_symlinks` for `pub use ...::resolve_symlinks;` in
  `crates/orbit-policy/src/lib.rs`). A re-export in a file outside
  `crates/<crate>/src` has no public path here and is not judged;
- CSS class selectors, named with their dot (`.ws-badge`), including a class in
  a compound or element-qualified selector (`.active.retired`, `a.retired`).
  Text in a CSS comment, a quoted string, a `url(...)` or a declaration value
  names no class.

Drop any identifier the same diff adds back, or that the working tree still
defines, re-exports or styles. Fail when a removed identifier is named as a
token inside an inline code span of a live document. Live documents are `docs/`
(except `docs/design/*/4_decisions.md`, `docs/dev/rca/` and `docs/standards/`),
`website/src/content`, the shipped skills, `ARCHITECTURE.md` and the per-crate
`AGENTS.md` and `CLAUDE.md` files. Legitimate keeps are recorded in
`scripts/removed-code-identifiers-allowlist.json` with a reason. An entry is
judged only when this diff removes that identifier: it is stale when the named
document no longer names it.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts/removed-code-identifiers-allowlist.json"
SOURCE_SUFFIXES = (".rs", ".css")
DOC_SUFFIXES = (".md", ".mdx")
DOC_PREFIXES = ("docs/", "website/src/content/", "crates/orbit-core/assets/skills/",
                "plugin/skills/")
DOC_FILES = ("ARCHITECTURE.md",)
CRATE_GUIDE = re.compile(r"crates/[^/]+/(?:AGENTS|CLAUDE)\.md")
# Decision records, RCAs and vendored standards keep history on purpose.
HISTORICAL_DOCS = re.compile(r"docs/design/[^/]+/4_decisions\.md|docs/dev/rca/|docs/standards/")
CRATE_SOURCE = re.compile(r"crates/([^/]+)/src/(.+)\.rs")
# A definition starts its line, after visibility and qualifier keywords, so
# `*const T`, `&'static str` and `fn(u8)` pointer types do not count.
RUST_ITEM = re.compile(
    r"^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?"
    r"(?:(?:const|async|unsafe|extern)(?:[ \t]+\"[^\"\n]*\")?[ \t]+)*"
    r"(?:const|static|fn|struct|enum|trait|type)[ \t]+(?:mut[ \t]+)?([A-Za-z_]\w*)",
    re.MULTILINE)
PUB_USE = re.compile(r"\bpub(?:\([^)\n]*\))?[ \t]+use[ \t]+([^;]+);")
# CSS comments, quoted strings and `url(...)` tokens, which name no class even
# in a selector (`a[href$=".pdf"]`), and selector preludes: the text before
# each `{` since the last `;`, `{` or `}`. Declarations are never preludes.
CSS_OPAQUE = re.compile(
    r"""/\*.*?\*/|"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|\burl\([^)\n]*\)""", re.DOTALL)
CSS_PRELUDE = re.compile(r"[^{};]*\{")
# A class in a prelude, including one after another selector part (`a.retired`,
# `.active.retired`). A letter must follow the dot, so `1.5` is not a class.
CSS_CLASS = re.compile(r"\.(-?[A-Za-z_][\w-]*)")
CODE_SPAN = re.compile(r"(?<!`)(?P<ticks>`+)(?!`)(?P<code>.*?)(?<!`)(?P=ticks)(?!`)",
                       re.DOTALL)
FENCE = re.compile(r"[ ]{0,3}(?P<marker>`{3,}|~{3,})(?P<info>.*)")
QUOTE = re.compile(r"[ ]{0,3}>[ ]?")
LIST_ITEM = re.compile(r"([ ]{0,3})(?:[-+*]|[0-9]{1,9}[.)])([ ]{1,4})")


def git(*arguments):
    return subprocess.run(
        ["git", "-C", str(ROOT), "-c", "core.quotepath=off", *arguments],
        env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
        capture_output=True, text=True)


def comparison_base():
    explicit = os.environ.get("CI_TEST_BASE")
    if explicit:
        result = git("rev-parse", "--verify", f"{explicit}^{{commit}}")
        if result.returncode == 0:
            return result.stdout.strip()
        raise ValueError(f"cannot resolve CI_TEST_BASE {explicit!r} to a commit")
    for branch in ("refs/remotes/origin/agent-main", "refs/heads/agent-main"):
        if git("rev-parse", "--verify", "--quiet", branch).returncode == 0:
            result = git("merge-base", "HEAD", branch)
            if result.returncode == 0 and result.stdout.strip():
                return result.stdout.strip()
            break
    raise ValueError("cannot resolve agent-main; set CI_TEST_BASE to the delivery base commit")


def diff_path(text):
    text = text.rstrip("\t")
    if text == "/dev/null" or text[:2] not in ("a/", "b/"):
        return text
    return text[2:]


def hunks(base):
    """Return (path, removed text, added text) for each hunk of the diff.

    Header lines are read only before a file's first hunk, so a removed line
    that begins with `--` is still content."""
    result = git("diff", "--no-color", "--no-ext-diff", "--no-renames", "--unified=0",
                 "--src-prefix=a/", "--dst-prefix=b/", base, "--")
    if result.returncode != 0:
        raise ValueError(f"git diff against {base} failed: {result.stderr.strip()}")
    blocks = []
    path = old_path = ""
    current = None
    for line in result.stdout.splitlines():
        if line.startswith("diff --git "):
            if current is not None:
                blocks.append(current)
            current = None
            path = old_path = ""
        elif line.startswith("@@"):
            if current is not None:
                blocks.append(current)
            current = (path, [], [])
        elif current is not None and line.startswith("-"):
            current[1].append(line[1:])
        elif current is not None and line.startswith("+"):
            current[2].append(line[1:])
        elif current is None and line.startswith("--- "):
            old_path = diff_path(line[4:])
        elif current is None and line.startswith("+++ "):
            new_path = diff_path(line[4:])
            path = old_path if new_path == "/dev/null" else new_path
    if current is not None:
        blocks.append(current)
    return [(path, "\n".join(removed), "\n".join(added)) for path, removed, added in blocks]


def module_of(path):
    """Crate-qualified module path of a crate source file, or None elsewhere."""
    match = CRATE_SOURCE.fullmatch(path)
    if match is None:
        return None
    segments = match.group(2).split("/")
    if segments[-1] == "mod" or (len(segments) == 1 and segments[0] in ("lib", "main")):
        segments.pop()
    return "::".join([match.group(1).replace("-", "_"), *segments])


def top_level_commas(text):
    parts, depth, start = [], 0, 0
    for index, char in enumerate(text):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(text[start:index])
            start = index + 1
    parts.append(text[start:])
    return [part for part in parts if part.strip()]


def use_items(tree, prefix=""):
    """Return (path, public name) for each item of a `use` tree; globs are skipped."""
    tree = " ".join(tree.split())
    brace = tree.find("{")
    if brace >= 0:
        head = prefix + tree[:brace]
        items = []
        for part in top_level_commas(tree[brace + 1:tree.rfind("}")]):
            items.extend(use_items(part, head))
        return items
    target, _, alias = tree.partition(" as ")
    target = target.strip()
    if not target or target.endswith("*"):
        return []
    if target == "self":
        full = prefix[:-2] if prefix.endswith("::") else prefix
    else:
        full = prefix + target
    name = alias.strip() or full.rsplit("::", 1)[-1]
    if not full or name == "_":
        return []
    return [(full, name)]


def public_paths(path, text):
    module = module_of(path)
    if module is None:
        return set()
    return {f"{module}::{name}"
            for match in PUB_USE.finditer(text)
            for _, name in use_items(match.group(1))}


def css_classes(text):
    """CSS class selectors a stylesheet names, with their dot."""
    text = CSS_OPAQUE.sub(" ", text)
    return {"." + match.group(1)
            for prelude in CSS_PRELUDE.finditer(text)
            for match in CSS_CLASS.finditer(prelude.group())}


def names_in(path, text):
    """Identifiers a source text defines or re-exports, or the CSS classes it styles."""
    if path.endswith(".css"):
        return css_classes(text)
    names = {match.group(1) for match in RUST_ITEM.finditer(text)}
    names.discard("_")
    return names | public_paths(path, text)


def read_text(relative):
    try:
        return (ROOT / relative).read_text(errors="replace")
    except OSError:
        return None


def worktree_files(*options):
    result = git("ls-files", *options, "--exclude-standard", "-z")
    if result.returncode != 0:
        raise ValueError(f"git ls-files failed: {result.stderr.strip()}")
    return [name for name in result.stdout.split("\0") if name]


def changed_identifiers(base):
    """Return {identifier: source paths} for identifiers the diff removes and does not re-add."""
    removed = {}
    added = set()
    changed = set()
    for path, removed_text, added_text in hunks(base):
        if not path.endswith(SOURCE_SUFFIXES):
            continue
        changed.add(path)
        # A CSS hunk can cut a selector or comment in half, so CSS is judged
        # from whole files below.
        if path.endswith(".rs"):
            for name in names_in(path, removed_text):
                removed.setdefault(name, set()).add(path)
            added |= names_in(path, added_text)
    # A zero-context hunk that deletes one member of a multiline `pub use` group
    # leaves the `pub use` opening and `;` outside the removed text, so compare
    # the whole file's public paths before and after the diff.
    for path in sorted(changed):
        if path.endswith(".css"):
            old = git("show", f"{base}:{path}")
            old_classes = css_classes(old.stdout) if old.returncode == 0 else set()
            for name in old_classes - css_classes(read_text(path) or ""):
                removed.setdefault(name, set()).add(path)
            continue
        if module_of(path) is None:
            continue
        old = git("show", f"{base}:{path}")
        old_paths = public_paths(path, old.stdout) if old.returncode == 0 else set()
        for name in old_paths - public_paths(path, read_text(path) or ""):
            removed.setdefault(name, set()).add(path)
    for path in worktree_files("--others"):
        text = read_text(path) if path.endswith(SOURCE_SUFFIXES) else None
        if text is not None:
            added |= names_in(path, text)
    return {name: files for name, files in removed.items() if name not in added}


def tree_names():
    """Identifiers the working tree still defines, re-exports or styles."""
    names = set()
    for path in worktree_files("--cached", "--others"):
        text = read_text(path) if path.endswith(SOURCE_SUFFIXES) else None
        if text is not None:
            names |= names_in(path, text)
    return names


def live_docs():
    docs = []
    for path in worktree_files("--cached", "--others"):
        if not path.endswith(DOC_SUFFIXES) or HISTORICAL_DOCS.match(path):
            continue
        if path in DOC_FILES or CRATE_GUIDE.fullmatch(path) or path.startswith(DOC_PREFIXES):
            docs.append(path)
    return docs


def name_pattern(name):
    """Whole-token match for a Rust name; a CSS class also matches as a compound's tail."""
    before = "" if name.startswith(".") else r"(?<![\w-])"
    return re.compile(before + re.escape(name) + r"(?![\w-])")


def doc_hits(identifiers, docs):
    """Return {identifier: {(document, line)}} for inline code spans that name it."""
    patterns = {name: name_pattern(name) for name in identifiers}
    hits = {name: set() for name in identifiers}
    for doc in docs:
        text = read_text(doc)
        if text is None:
            continue
        fence = None
        list_indents = []
        paragraph = []

        def scan_paragraph():
            if not paragraph:
                return
            content = "\n".join(line for _, line in paragraph)
            for span in CODE_SPAN.finditer(content):
                for name, pattern in patterns.items():
                    for match in pattern.finditer(span["code"]):
                        offset = span.start("code") + match.start()
                        number = paragraph[content.count("\n", 0, offset)][0]
                        hits[name].add((doc, number))
            paragraph.clear()

        for number, line in enumerate(text.splitlines(), 1):
            # Strip blockquote containers, then list indentation. A fence may
            # start on the list marker's line or a later continuation line.
            rest = line
            quote_depth = 0
            while (quote := QUOTE.match(rest)) is not None:
                rest = rest[quote.end():]
                quote_depth += 1
            indent = len(rest) - len(rest.lstrip(" "))
            while list_indents and (quote_depth != list_indents[-1][0]
                                    or (rest.strip() and indent < list_indents[-1][1])):
                list_indents.pop()
            if list_indents:
                rest = rest[list_indents[-1][1]:]
            while fence is None and (item := LIST_ITEM.match(rest)) is not None:
                consumed = item.end()
                indent = (list_indents[-1][1] if list_indents else 0) + consumed
                list_indents.append((quote_depth, indent))
                rest = rest[consumed:]
            opening = FENCE.match(rest)
            if fence is not None:
                if quote_depth < fence[2] or (fence[3] and
                        (not list_indents or list_indents[-1][1] < fence[3])):
                    fence = None
                else:
                    if (opening is not None and opening["marker"][0] == fence[0]
                            and len(opening["marker"]) >= fence[1] and not opening["info"].strip()):
                        fence = None
                    continue
            if opening is not None:
                scan_paragraph()
                fence = (opening["marker"][0], len(opening["marker"]), quote_depth,
                         list_indents[-1][1] if list_indents else 0)
                continue
            if not rest.strip():
                scan_paragraph()
            else:
                paragraph.append((number, rest))
        scan_paragraph()
    return hits


def normalize_identifier(value):
    return str(value).strip().strip("`").strip()


def load_allowlist():
    entries = json.loads(ALLOWLIST.read_text())
    if not isinstance(entries, list):
        raise ValueError("allowlist must be a JSON list")
    allowlist = {}
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("each allowlist entry must be an object")
        name = normalize_identifier(entry["identifier"])
        doc = str(entry["doc"])
        if not str(entry["reason"]).strip():
            raise ValueError(f"allowlist entry for {name!r} in {doc} needs a reason")
        if (name, doc) in allowlist:
            raise ValueError(f"duplicate allowlist entry for {name!r} in {doc}")
        allowlist[(name, doc)] = entry["reason"]
    return allowlist


def main():
    try:
        allowlist = load_allowlist()
        base = comparison_base()
        removed = changed_identifiers(base)
        if removed:
            defined = tree_names()
            removed = {name: files for name, files in removed.items() if name not in defined}
        if not removed:
            print("removed code identifiers: no removed code identifiers in the diff")
            return 0
        hits = doc_hits(sorted(removed), live_docs())
        errors = []
        for name in sorted(removed):
            files = ", ".join(sorted(removed[name]))
            for doc, number in sorted(hits[name]):
                if (name, doc) not in allowlist:
                    errors.append(
                        f'removed code identifier "{name}" (removed in {files}) '
                        f"still named in {doc}:{number}")
        # An entry is judged only when this diff removes its identifier. It is
        # stale when the named document no longer names the identifier.
        hit_pairs = {(name, doc) for name, found in hits.items() for doc, _ in found}
        for name, doc in sorted(allowlist):
            if name in removed and (name, doc) not in hit_pairs:
                errors.append(
                    f'stale allowlist entry: "{name}" in {doc} no longer matches '
                    "a removed code identifier and a doc line")
        if errors:
            print("\n".join(errors), file=sys.stderr)
            return 1
        print(f"removed code identifiers: no stale references to {len(removed)} removed identifier(s)")
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"removed code identifiers: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
